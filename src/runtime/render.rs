// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::model::{
    Content, Draw, Geometry, GroupTransform, ImageAsset, Layer, RepeaterComposite, Shape, fixed,
};
use super::{
    Composition, EvaluatedComposition, EvaluatedLayer, EvaluationError, FilterEffect,
    FilterLayerResult,
};
use kurbo::{Affine, PathEl, Rect};
use peniko::Mix;
use std::mem::swap;
use std::ops::Range;

pub trait RenderSink {
    /// Begins a layer that is composited onto what is below it, with `blend` and `alpha`, when it
    /// is popped. Content drawn into the layer is clipped to `shape`, and blends only with other
    /// content of the layer.
    fn push_layer(
        &mut self,
        blend: impl Into<peniko::BlendMode>,
        alpha: f32,
        transform: Affine,
        shape: &impl kurbo::Shape,
    );

    fn push_clip_layer(&mut self, transform: Affine, shape: &impl kurbo::Shape);

    fn pop_layer(&mut self);

    fn push_filter(&mut self, _transform: Affine, _effect: &FilterEffect) -> FilterLayerResult {
        FilterLayerResult::Unsupported
    }

    fn draw(
        &mut self,
        stroke: Option<&fixed::Stroke>,
        transform: Affine,
        brush: &fixed::Brush,
        shape: &impl kurbo::Shape,
    );

    /// Draws a previously loaded raster image asset.
    fn draw_image(&mut self, image: &ImageAsset, transform: Affine, alpha: f64);

    /// Called before rendering a Lottie layer.
    ///
    /// - `name` is the layer's `nm` field.
    /// - `index` is the layer's position in the layer array.
    fn begin_layer_group(&mut self, _name: &str, _index: usize) {}

    /// Called after rendering a Lottie layer.
    fn end_layer_group(&mut self) {}
}

/// Renders a composition into a scene.
#[derive(Debug, Default)]
pub struct Renderer {
    batch: Batch,
    /// Batches to reuse while the shapes of one layer are held for another layer to render.
    spare_batches: Vec<Batch>,
    mask_elements: Vec<PathEl>,
}

impl Renderer {
    /// Creates a new renderer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Renders and appends the animation at a given frame to the provided scene.
    ///
    /// Panics on evaluation errors. Use [`Self::try_append`] to handle them explicitly.
    ///
    /// Repeater expansion is unbounded. Callers handling untrusted animations should validate copy
    /// counts and nesting, and benchmark representative workloads to establish their own limits.
    pub fn append(
        &mut self,
        animation: &Composition,
        frame: f64,
        transform: Affine,
        alpha: f64,
        scene: &mut impl RenderSink,
    ) {
        self.try_append(animation, frame, transform, alpha, scene)
            .expect("Lottie hierarchy evaluation failed");
    }

    /// Renders a frame, leaving the sink unchanged if evaluation fails.
    pub fn try_append(
        &mut self,
        animation: &Composition,
        frame: f64,
        transform: Affine,
        alpha: f64,
        scene: &mut impl RenderSink,
    ) -> Result<(), EvaluationError> {
        let evaluated = animation.evaluate(frame)?;
        self.append_evaluated(&evaluated, transform, alpha, scene);
        Ok(())
    }

    /// Draws a saved evaluation without recalculating layer hierarchy, timing or transforms.
    ///
    /// Shape and paint properties are still sampled during drawing.
    ///
    /// NOTE: this is not a frame cache.
    pub fn append_evaluated(
        &mut self,
        evaluated: &EvaluatedComposition<'_>,
        transform: Affine,
        alpha: f64,
        scene: &mut impl RenderSink,
    ) {
        let animation = evaluated.composition;
        self.batch.clear();
        let alpha = unit_alpha(alpha);
        if alpha <= 0.0 {
            return;
        }
        let clip = Rect::new(0.0, 0.0, animation.width as _, animation.height as _);
        let clip_bounds = transform.transform_rect_bbox(clip);
        // The caller's opacity applies to the animation as a whole, so layers that may overlap
        // are composited first, and layers with a blend mode blend with the animation only.
        // That layer also serves as the composition clip.
        let isolate_opacity = (alpha != 1.0 && drawing_layers(&evaluated.layers) > 1)
            || self.has_blending_layer(animation, &evaluated.layers, transform);
        if isolate_opacity {
            scene.push_layer(Mix::Normal, alpha as f32, transform, &clip);
        } else {
            scene.push_clip_layer(transform, &clip);
        }
        let alpha = if isolate_opacity { 1.0 } else { alpha };
        for (layer_index, layer) in evaluated.layers.iter().enumerate().rev() {
            if !layer.visible {
                continue;
            }
            self.render_layer(
                animation,
                &evaluated.layers,
                layer,
                layer_index,
                transform,
                alpha,
                &clip_bounds,
                scene,
            );
        }
        scene.pop_layer();
    }

    /// Renders one layer. `alpha` is opacity that the caller or an enclosing precomposition left
    /// for this layer to apply, because the layer is the only one that draws inside it.
    #[expect(clippy::too_many_arguments, reason = "Deferred")]
    fn render_layer(
        &mut self,
        animation: &Composition,
        layer_set: &[EvaluatedLayer<'_>],
        evaluated: &EvaluatedLayer<'_>,
        layer_index: usize,
        output_transform: Affine,
        alpha: f64,
        clip_bounds: &Rect,
        scene: &mut impl RenderSink,
    ) {
        if !evaluated.in_range {
            return;
        }
        let mut alpha = alpha * unit_alpha(evaluated.opacity);
        if alpha <= 0.0 {
            return;
        }
        let layer = evaluated.layer;
        let frame = evaluated.frame;
        scene.begin_layer_group(&layer.name, layer_index);
        let parent_transform = output_transform;
        let transform = output_transform * evaluated.full_transform;
        let full_rect = Rect::new(0.0, 0.0, animation.width as f64, animation.height as f64);
        // Shapes are evaluated before anything is pushed, so that the number of draw calls is
        // known.
        let draw_calls = match &layer.content {
            Content::None => 0,
            Content::Instance { .. } => drawing_layers(&evaluated.children),
            Content::Image { asset_id } => usize::from(animation.images.contains_key(asset_id)),
            Content::Shape(shapes) => {
                self.batch.evaluate(shapes, transform, frame);
                self.batch.draw_calls()
            }
        };
        // A blend mode composites the layer as a whole, and a track matte is applied inside a
        // layer of its own. One layer serves both and carries the opacity of the layer as well.
        // A layer that draws nothing has nothing to blend.
        let draws = match &layer.content {
            Content::Instance { .. } if layer.blend_mode.is_some() => {
                self.any_layer_draws(animation, &evaluated.children, output_transform)
            }
            _ => draw_calls > 0,
        };
        let blend = layer.blend_mode.filter(|_| draws);
        let grouped = blend.is_some() || evaluated.matte.is_some();
        if grouped {
            let blend = blend.unwrap_or(Mix::Normal.into());
            scene.push_layer(blend, alpha as f32, parent_transform, &full_rect);
            alpha = 1.0;
        }
        if let Some((mode, mask_index)) = evaluated.matte {
            // The matte source evaluates shapes of its own, so those of this layer are set aside.
            let spare = self.spare_batches.pop().unwrap_or_default();
            let batch = std::mem::replace(&mut self.batch, spare);
            // The matte source is not content of this layer, so it does not take its opacity.
            self.render_layer(
                animation,
                layer_set,
                &layer_set[mask_index],
                mask_index,
                output_transform,
                1.0,
                clip_bounds,
                scene,
            );
            let spare = std::mem::replace(&mut self.batch, batch);
            self.spare_batches.push(spare);
            scene.push_layer(mode, 1.0, parent_transform, &full_rect);
        }

        // Layers of a precomposition that have a blend mode blend with the precomposition only.
        let isolate_blending = !grouped
            && matches!(layer.content, Content::Instance { .. })
            && self.has_blending_layer(animation, &evaluated.children, output_transform);

        // Opacity applies to the layer as a whole and to the output of its effects, so content
        // that may overlap itself is composited first. A single draw call gives the same result
        // with the opacity in its brush, and a precomposition with a single drawing layer by
        // handing the opacity to that layer; neither needs a layer.
        let isolate_opacity =
            (alpha != 1.0 && (!layer.effects.is_empty() || draw_calls > 1)) || isolate_blending;

        // A precomposition shows nothing outside its own bounds. The opacity layer can clip to
        // them as well.
        let precomp_clip = match &layer.content {
            Content::Instance { .. } => precomposition_clip(layer, transform, clip_bounds),
            _ => None,
        };
        match (isolate_opacity, &precomp_clip) {
            (true, Some(clip)) => scene.push_layer(Mix::Normal, alpha as f32, transform, clip),
            (true, None) => {
                scene.push_layer(Mix::Normal, alpha as f32, parent_transform, &full_rect);
            }
            (false, Some(clip)) => scene.push_clip_layer(transform, clip),
            (false, None) => {}
        }

        let alpha = if isolate_opacity { 1.0 } else { alpha };
        let mut filter_layers = usize::from(isolate_opacity || precomp_clip.is_some());

        for effect in layer
            .effects
            .iter()
            .rev()
            .filter_map(|effect| effect.evaluate(frame))
        {
            match scene.push_filter(transform, &effect) {
                FilterLayerResult::Pushed => filter_layers += 1,
                FilterLayerResult::Unsupported => {}
            }
        }
        for mask in &layer.masks {
            mask.geometry.evaluate(frame, &mut self.mask_elements);
            scene.push_clip_layer(transform, &self.mask_elements.as_slice());
            self.mask_elements.clear();
        }
        match &layer.content {
            Content::None => {}
            Content::Instance { .. } => {
                for (index, child) in evaluated.children.iter().enumerate().rev() {
                    if child.layer.is_matte_source || child.layer.hidden {
                        continue;
                    }
                    self.render_layer(
                        animation,
                        &evaluated.children,
                        child,
                        index,
                        output_transform,
                        alpha,
                        clip_bounds,
                        scene,
                    );
                }
            }
            Content::Image { asset_id } => {
                if let Some(image) = animation.images.get(asset_id) {
                    // Lottie requires visuals to stay within authored image bounds.
                    let bounds = image
                        .width
                        .zip(image.height)
                        .filter(|(w, h)| *w > 0.0 && *h > 0.0);
                    if let Some((width, height)) = bounds {
                        scene.push_clip_layer(transform, &Rect::new(0.0, 0.0, width, height));
                    }
                    scene.draw_image(image, transform, alpha);
                    if bounds.is_some() {
                        scene.pop_layer();
                    }
                }
            }
            Content::Shape(_) => {
                self.batch.render(scene, clip_bounds, alpha);
                self.batch.clear();
            }
        }
        let group_layers = usize::from(grouped) + usize::from(evaluated.matte.is_some());
        for _ in 0..layer.masks.len() + filter_layers + group_layers {
            scene.pop_layer();
        }
        scene.end_layer_group();
    }

    /// Whether a layer that draws at this frame has a blend mode.
    fn has_blending_layer(
        &mut self,
        animation: &Composition,
        layers: &[EvaluatedLayer<'_>],
        output_transform: Affine,
    ) -> bool {
        layers.iter().any(|evaluated| {
            evaluated.layer.blend_mode.is_some()
                && may_draw(evaluated)
                && self.layer_draws(animation, evaluated, output_transform)
        })
    }

    fn any_layer_draws(
        &mut self,
        animation: &Composition,
        layers: &[EvaluatedLayer<'_>],
        output_transform: Affine,
    ) -> bool {
        layers.iter().any(|evaluated| {
            may_draw(evaluated) && self.layer_draws(animation, evaluated, output_transform)
        })
    }

    /// Whether the content of a layer makes at least one draw call. This evaluates the shapes
    /// of a shape layer, so it is only asked of layers with a blend mode.
    fn layer_draws(
        &mut self,
        animation: &Composition,
        evaluated: &EvaluatedLayer<'_>,
        output_transform: Affine,
    ) -> bool {
        match &evaluated.layer.content {
            Content::None => false,
            Content::Instance { .. } => {
                self.any_layer_draws(animation, &evaluated.children, output_transform)
            }
            Content::Image { asset_id } => animation.images.contains_key(asset_id),
            Content::Shape(shapes) => {
                let transform = output_transform * evaluated.full_transform;
                self.batch.evaluate(shapes, transform, evaluated.frame);
                let draws = self.batch.draw_calls() > 0;
                self.batch.clear();
                draws
            }
        }
    }
}

/// Returns the bounds, in layer coordinates, that a precomposition layer has to be clipped to.
///
/// Returns `None` when they already cover everything inside the composition clip, which saves
/// the clip layer for the common precomposition that fills the composition.
fn precomposition_clip(layer: &Layer, transform: Affine, clip_bounds: &Rect) -> Option<Rect> {
    let bounds = Rect::new(0.0, 0.0, layer.width.max(0.0), layer.height.max(0.0));
    let to_layer = transform.inverse();
    let covered = to_layer.is_finite() && {
        // Tolerates rounding in the transform when the edges coincide.
        let bounds = bounds.inflate(1e-6, 1e-6);
        [
            (clip_bounds.x0, clip_bounds.y0),
            (clip_bounds.x1, clip_bounds.y0),
            (clip_bounds.x1, clip_bounds.y1),
            (clip_bounds.x0, clip_bounds.y1),
        ]
        .into_iter()
        .all(|corner| bounds.contains(to_layer * kurbo::Point::from(corner)))
    };
    (!covered).then_some(bounds)
}

/// Limits an opacity to the range it can be composited with. `NaN` counts as transparent.
fn unit_alpha(alpha: f64) -> f64 {
    if alpha.is_nan() {
        0.0
    } else {
        alpha.clamp(0.0, 1.0)
    }
}

/// Counts the layers that may draw at this frame. Opacity shared by more than one of them has to
/// be composited for all of them together.
fn drawing_layers(layers: &[EvaluatedLayer<'_>]) -> usize {
    layers.iter().filter(|layer| may_draw(layer)).count()
}

fn may_draw(evaluated: &EvaluatedLayer<'_>) -> bool {
    let layer = evaluated.layer;
    evaluated.in_range
        && !layer.hidden
        && !layer.is_matte_source
        && evaluated.opacity > 0.0
        && !matches!(layer.content, Content::None)
}

#[derive(Clone, Debug)]
struct DrawData {
    stroke: Option<fixed::Stroke>,
    brush: fixed::Brush,
    /// Opacity of the style itself.
    alpha: f64,
    transform: Affine,
    /// Range into `ShapeBatch::geometries`
    geometry: Range<usize>,
    // Only repeaters that copied this style may share its opacity layers with other styles.
    repeater_depth: usize,
    /// Translucent groups that hold this style and repeater copies that include it, outermost
    /// first. The opacity of each applies to everything inside it at once.
    scopes: Vec<OpacityScope>,
}

impl DrawData {
    fn new(draw: &Draw, geometry: Range<usize>, frame: f64) -> Self {
        Self {
            stroke: draw
                .stroke
                .as_ref()
                .map(|stroke| stroke.evaluate(frame).into_owned()),
            brush: draw.brush.evaluate(1.0, frame).into_owned(),
            alpha: unit_alpha(draw.opacity.evaluate(frame) / 100.0),
            transform: Affine::IDENTITY,
            geometry,
            repeater_depth: 0,
            scopes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct GeometryData {
    /// Range into `ShapeBatch::elements`
    elements: Range<usize>,
    transform: Affine,
    // Sequential trimming follows copy indices independently of paint stacking.
    trim_order: usize,
    // Inherited styles need copy opacity without inheriting the repeated styles' stacking position.
    copies: Vec<OpacityScope>,
}

impl GeometryData {
    fn opacity_copies(&self) -> &[OpacityScope] {
        // Opaque ancestors still distinguish separate instances of translucent descendants.
        let end = self
            .copies
            .iter()
            .rposition(|copy| copy.alpha != 1.0)
            .map_or(0, |index| index + 1);
        &self.copies[..end]
    }
}

/// A shape group or repeater copy whose opacity applies to its content as a whole.
#[derive(Clone, Copy, Debug, PartialEq)]
struct OpacityScope {
    id: usize,
    alpha: f64,
}

/// One call to [`RenderSink::draw`]: a style and a run of its geometry in the same repeater
/// copies.
#[derive(Clone, Debug)]
struct DrawCall {
    /// Index into `Batch::draws`
    draw: usize,
    /// Range into `Batch::geometries`
    geometries: Range<usize>,
}

#[derive(Debug, Default)]
struct Batch {
    elements: Vec<PathEl>,
    geometries: Vec<GeometryData>,
    draws: Vec<DrawData>,
    /// Draw calls in paint order.
    calls: Vec<DrawCall>,
    repeat_geometries: Vec<GeometryData>,
    repeat_draws: Vec<DrawData>,
    next_scope: usize,
    trim_elements: Vec<PathEl>,
}

pub(crate) fn evaluate_paths(
    shapes: &[Shape],
    transform: Affine,
    frame: f64,
) -> Vec<kurbo::BezPath> {
    let mut batch = Batch::default();
    batch.evaluate_shapes(shapes, transform, frame, false);
    batch
        .geometries
        .iter()
        .filter(|geometry| !geometry.elements.is_empty())
        .map(|geometry| {
            batch.elements[geometry.elements.clone()]
                .iter()
                .map(|element| geometry.transform * *element)
                .collect()
        })
        .collect()
}

impl Batch {
    /// Evaluates the shapes of a layer and works out the draw calls they need.
    fn evaluate(&mut self, shapes: &[Shape], transform: Affine, frame: f64) {
        self.evaluate_shapes(shapes, transform, frame, true);
        self.collect_calls();
    }

    fn evaluate_shapes(
        &mut self,
        shapes: &[Shape],
        transform: Affine,
        frame: f64,
        collect_draws: bool,
    ) {
        let geometry_start = self.geometries.len();
        let draw_start = self.draws.len();
        for shape in shapes {
            // A hidden shape is not drawn, but its paths are part of the layer's paths.
            let shape = match shape {
                Shape::Hidden(_) if collect_draws => continue,
                Shape::Hidden(shape) => shape.as_ref(),
                shape => shape,
            };
            match shape {
                // Nothing wraps a hidden shape a second time.
                Shape::Hidden(_) => {}
                Shape::Group(shapes, group_transform) => {
                    let (group_transform, group_alpha) =
                        if let Some(GroupTransform { transform, opacity }) = group_transform {
                            (
                                transform.evaluate(frame).into_owned(),
                                if collect_draws {
                                    unit_alpha(opacity.evaluate(frame) / 100.0)
                                } else {
                                    1.0
                                },
                            )
                        } else {
                            (Affine::IDENTITY, 1.0)
                        };
                    let group_draws = self.draws.len();
                    self.evaluate_shapes(shapes, group_transform, frame, collect_draws);
                    if group_alpha != 1.0 {
                        let scope = OpacityScope {
                            id: self.next_scope,
                            alpha: group_alpha,
                        };
                        self.next_scope += 1;
                        for draw in &mut self.draws[group_draws..] {
                            draw.scopes.insert(0, scope);
                        }
                    }
                }
                Shape::Geometry(geometry) => {
                    self.push_geometry(geometry, Affine::IDENTITY, frame);
                }
                Shape::Draw(draw) => {
                    if collect_draws {
                        self.push_draw(draw, geometry_start, frame);
                    }
                }
                Shape::Repeater(repeater) => {
                    let repeater = repeater.evaluate(frame);
                    self.repeat(repeater.as_ref(), geometry_start, draw_start);
                }
                Shape::Trim(trim) => {
                    let trim = trim.evaluate(frame);
                    self.apply_trim(trim.as_ref(), geometry_start);
                }
            }
        }
        for geometry in &mut self.geometries[geometry_start..] {
            geometry.transform = transform * geometry.transform;
        }
        for draw in &mut self.draws[draw_start..] {
            draw.transform = transform * draw.transform;
        }
    }

    fn push_geometry(&mut self, geometry: &Geometry, transform: Affine, frame: f64) {
        let start = self.elements.len();
        geometry.evaluate(frame, &mut self.elements);
        let end = self.elements.len();
        self.geometries.push(GeometryData {
            elements: start..end,
            transform,
            trim_order: self.geometries.len(),
            copies: Vec::new(),
        });
    }

    fn push_draw(&mut self, draw: &Draw, geometry_start: usize, frame: f64) {
        self.draws.push(DrawData::new(
            draw,
            geometry_start..self.geometries.len(),
            frame,
        ));
    }

    fn repeat(&mut self, repeater: &fixed::Repeater, geometry_start: usize, draw_start: usize) {
        // A copy count whose geometry is too large to hold in memory draws nothing, like no copies.
        let geometry_count = self.geometries.len() - geometry_start;
        let fits = repeater
            .copies
            .checked_mul(geometry_count)
            .is_some_and(|total| self.geometries.try_reserve(total).is_ok());
        if repeater.copies == 0 || !fits {
            self.geometries.truncate(geometry_start);
            self.draws.truncate(draw_start);
            return;
        }
        if geometry_count == 0 {
            return;
        }

        // First move the relevant ranges of geometries and draws into side
        // buffers
        self.repeat_geometries
            .extend(self.geometries.drain(geometry_start..));
        self.repeat_draws.extend(self.draws.drain(draw_start..));
        let start_alpha = repeater.start_opacity / 100.0;
        let end_alpha = repeater.end_opacity / 100.0;
        let delta_alpha = if repeater.copies > 1 {
            (end_alpha - start_alpha) / (repeater.copies - 1) as f64
        } else {
            0.0
        };
        let first_scope = self.next_scope;
        self.next_scope = first_scope.wrapping_add(repeater.copies);
        let copy_scope = |copy: usize, i: usize| OpacityScope {
            id: first_scope.wrapping_add(copy),
            alpha: unit_alpha(start_alpha + delta_alpha * i as f64),
        };
        // Later styles traverse geometry directly, so it must be in paint order too.
        for copy in 0..repeater.copies {
            let i = match repeater.composite {
                RepeaterComposite::Below => repeater.copies - 1 - copy,
                RepeaterComposite::Above => copy,
            };
            let transform = repeater.transform(i);
            let copy_layer = copy_scope(copy, i);
            for geometry in &self.repeat_geometries {
                let mut geometry = geometry.clone();
                geometry.trim_order += i * geometry_count;
                geometry.transform = transform * geometry.transform;
                geometry.copies.insert(0, copy_layer);
                self.geometries.push(geometry);
            }
        }

        // Draws are consumed in reverse, unlike geometries.
        for copy in (0..repeater.copies).rev() {
            let copy_start = geometry_start + copy * geometry_count;
            let i = match repeater.composite {
                RepeaterComposite::Below => repeater.copies - 1 - copy,
                RepeaterComposite::Above => copy,
            };
            let transform = repeater.transform(i);
            for mut draw in self.repeat_draws.iter().cloned() {
                let start = draw.geometry.start - geometry_start;
                let end = draw.geometry.end - geometry_start;
                draw.geometry = copy_start + start..copy_start + end;
                draw.transform = transform * draw.transform;
                draw.repeater_depth += 1;
                // The copy holds the styles it repeats, along with any groups inside it.
                draw.scopes.insert(0, copy_scope(copy, i));
                self.draws.push(draw);
            }
        }
        // Clear the side buffers
        self.repeat_geometries.clear();
        self.repeat_draws.clear();
    }

    fn apply_trim(&mut self, trim: &fixed::Trim, geometry_start: usize) {
        let Some((first, second)) = trim.normalized() else {
            for geometry in &mut self.geometries[geometry_start..] {
                geometry.elements = 0..0;
            }
            return;
        };

        if first.0 <= 1e-9 && first.1 >= 1.0 - 1e-9 && second.is_none() {
            return;
        }

        for geometry in &self.geometries[..geometry_start] {
            self.trim_elements
                .extend(self.elements[geometry.elements.clone()].iter().cloned());
        }

        let sequential = trim.mode == super::model::TrimMode::Sequential;
        let paths: Vec<Vec<PathEl>> = self.geometries[geometry_start..]
            .iter()
            .map(|geometry| {
                self.elements[geometry.elements.clone()]
                    .iter()
                    .map(|element| {
                        if sequential {
                            *element
                        } else {
                            geometry.transform * *element
                        }
                    })
                    .collect()
            })
            .collect();
        let mut intervals = vec![(0.0, 0.0); paths.len()];
        if sequential {
            let lengths: Vec<_> = paths.iter().map(|path| super::trim::length(path)).collect();
            let total_length = lengths.iter().sum();
            let mut order: Vec<_> = (0..paths.len()).collect();
            order.sort_unstable_by_key(|index| self.geometries[geometry_start + index].trim_order);
            let mut offset = 0.0;
            for index in order {
                intervals[index] = (offset, total_length);
                offset += lengths[index];
            }
        }
        let ranges: Vec<_> = [Some(first), second].into_iter().flatten().collect();
        for ((geometry, path), interval) in self.geometries[geometry_start..]
            .iter_mut()
            .zip(paths)
            .zip(intervals)
        {
            let new_start = self.trim_elements.len();
            super::trim::trim(
                &path,
                &ranges,
                sequential.then_some(interval),
                &mut self.trim_elements,
            );
            geometry.elements = new_start..self.trim_elements.len();
            if !sequential {
                geometry.transform = Affine::IDENTITY;
            }
        }

        let mut offset = 0;
        for geometry in &mut self.geometries[..geometry_start] {
            let len = geometry.elements.len();
            geometry.elements = offset..offset + len;
            offset += len;
        }

        swap(&mut self.elements, &mut self.trim_elements);
        self.trim_elements.clear();
    }

    /// Lists the draw calls in paint order, leaving out those that would draw nothing.
    fn collect_calls(&mut self) {
        for (draw_index, draw) in self.draws.iter().enumerate().rev() {
            if draw.alpha <= 0.0
                || !draw.transform.is_finite()
                || draw.scopes.iter().any(|scope| scope.alpha <= 0.0)
            {
                continue;
            }
            let style_inverse = draw.transform.inverse();
            // A singular style transform collapses both fill and stroke to zero area.
            if !style_inverse.is_finite() {
                continue;
            }
            let mut start = draw.geometry.start;
            // Compound paths must be assembled after group transforms and modifiers are resolved.
            for geometries in self.geometries[draw.geometry.clone()]
                .chunk_by(|a, b| a.opacity_copies() == b.opacity_copies())
            {
                let range = start..start + geometries.len();
                start = range.end;
                if geometries[0]
                    .opacity_copies()
                    .iter()
                    .any(|copy| copy.alpha <= 0.0)
                {
                    continue;
                }
                let has_path = geometries.iter().any(|geometry| {
                    !geometry.elements.is_empty()
                        && (geometry.transform == draw.transform
                            || (style_inverse * geometry.transform).is_finite())
                });
                if has_path {
                    self.calls.push(DrawCall {
                        draw: draw_index,
                        geometries: range,
                    });
                }
            }
        }
    }

    /// Number of calls to [`RenderSink::draw`] that [`Self::render`] makes.
    fn draw_calls(&self) -> usize {
        self.calls.len()
    }

    /// Opacity scopes around a draw call, outermost first: those of the style, then the repeater
    /// copies of the geometry that did not copy the style itself.
    fn scopes(&self, call: &DrawCall) -> (&[OpacityScope], &[OpacityScope]) {
        let draw = &self.draws[call.draw];
        let copies = self.geometries[call.geometries.start].opacity_copies();
        (
            &draw.scopes,
            &copies[draw.repeater_depth.min(copies.len())..],
        )
    }

    /// Number of opacity scopes, counted from the outermost, that hold both draw calls.
    fn shared_scopes(&self, a: &DrawCall, b: &DrawCall) -> usize {
        fn common(a: &[OpacityScope], b: &[OpacityScope]) -> usize {
            a.iter().zip(b).take_while(|(a, b)| a == b).count()
        }
        let (a_style, a_copies) = self.scopes(a);
        let (b_style, b_copies) = self.scopes(b);
        if a.draw == b.draw {
            a_style.len() + common(a_copies, b_copies)
        } else {
            // Copies that did not copy the style belong to that style alone.
            common(a_style, b_style)
        }
    }

    /// Emits the draw calls. `alpha` is opacity of the layer that is left to the brushes.
    ///
    /// The opacity of a group or repeater copy applies to its content as a whole. A scope that
    /// holds several draw calls becomes a layer around them. A scope that holds one draw call is
    /// multiplied into its brush instead, which gives the same result without a layer.
    fn render(&self, scene: &mut impl RenderSink, clip_bounds: &Rect, alpha: f64) {
        let mut path = Vec::new();
        // Scopes shared with the previous draw call are open layers.
        let mut shared_previous = 0;
        for (index, call) in self.calls.iter().enumerate() {
            let draw = &self.draws[call.draw];
            let (style_scopes, copy_scopes) = self.scopes(call);
            let scopes = || style_scopes.iter().chain(copy_scopes).enumerate();
            let shared_next = self
                .calls
                .get(index + 1)
                .map_or(0, |next| self.shared_scopes(call, next));
            let mut brush_alpha = alpha * draw.alpha;
            for (depth, scope) in scopes().skip(shared_previous) {
                if scope.alpha == 1.0 {
                    continue;
                }
                if depth < shared_next {
                    scene.push_layer(Mix::Normal, scope.alpha as _, Affine::IDENTITY, clip_bounds);
                } else {
                    brush_alpha *= scope.alpha;
                }
            }
            self.render_draw(
                draw,
                brush_alpha,
                &self.geometries[call.geometries.clone()],
                &mut path,
                scene,
            );
            let open = shared_previous.max(shared_next);
            for (_, scope) in scopes().take(open).skip(shared_next) {
                if scope.alpha != 1.0 {
                    scene.pop_layer();
                }
            }
            shared_previous = shared_next;
        }
    }

    fn render_draw(
        &self,
        draw: &DrawData,
        alpha: f64,
        geometries: &[GeometryData],
        path: &mut Vec<PathEl>,
        scene: &mut impl RenderSink,
    ) {
        let style_inverse = draw.transform.inverse();
        let modified_brush = if alpha != 1.0 {
            Some(draw.brush.clone().multiply_alpha(alpha as _))
        } else {
            None
        };
        let brush = modified_brush.as_ref().unwrap_or(&draw.brush);
        let geometry = &geometries[0];
        let path = if geometries.len() == 1 && geometry.transform == draw.transform {
            &self.elements[geometry.elements.clone()]
        } else {
            path.clear();
            for geometry in geometries {
                let elements = &self.elements[geometry.elements.clone()];
                if geometry.transform == draw.transform {
                    path.extend_from_slice(elements);
                } else {
                    let transform = style_inverse * geometry.transform;
                    if transform.is_finite() {
                        path.extend(elements.iter().map(|element| transform * *element));
                    }
                }
            }
            path.as_slice()
        };
        if !path.is_empty() {
            scene.draw(draw.stroke.as_ref(), draw.transform, brush, &path);
        }
    }

    fn clear(&mut self) {
        self.elements.clear();
        self.geometries.clear();
        self.draws.clear();
        self.calls.clear();
        self.repeat_geometries.clear();
        self.repeat_draws.clear();
        self.next_scope = 0;
        self.trim_elements.clear();
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::*;
    use kurbo::Rect;
    use peniko::{BlendMode, Compose, Mix};
    use serde_json::json;

    #[test]
    fn layers_paint_bottom_to_top_inside_the_composition_clip() {
        let composition = composition(json!([
            shape_layer(1, json!([rect(0.0, 0.0, 10.0, 10.0), fill(100.0)])),
            shape_layer(2, json!([rect(50.0, 50.0, 60.0, 60.0), fill(100.0)])),
        ]));
        let draw = |bounds| Event::Draw {
            stroked: false,
            alpha: 1.0,
            bounds,
        };
        assert_eq!(
            render(&composition, 0.0, 1.0),
            [
                Event::PushClip(Rect::new(0.0, 0.0, 100.0, 100.0)),
                Event::BeginLayer("layer 2".into()),
                draw(Rect::new(50.0, 50.0, 60.0, 60.0)),
                Event::EndLayer,
                Event::BeginLayer("layer 1".into()),
                draw(Rect::new(0.0, 0.0, 10.0, 10.0)),
                Event::EndLayer,
                Event::Pop,
            ]
        );
    }

    #[test]
    fn styles_paint_in_reverse_order_with_their_own_opacity() {
        let composition = composition(json!([shape_layer(
            1,
            json!([rect(0.0, 0.0, 10.0, 10.0), fill(40.0), stroke(100.0)]),
        )]));
        assert_eq!(
            trace(&composition, 0.0),
            ["clip", "stroke(1)", "fill(0.4)", "pop"]
        );
    }

    #[test]
    fn hidden_and_inactive_layers_emit_nothing() {
        let shapes = || json!([rect(0.0, 0.0, 10.0, 10.0), fill(100.0)]);
        let composition = composition(json!([
            with(shape_layer(1, shapes()), json!({ "hd": true })),
            with(shape_layer(2, shapes()), json!({ "ip": 10, "op": 20 })),
        ]));
        assert_eq!(render(&composition, 0.0, 1.0).len(), 2);
        assert_eq!(trace(&composition, 0.0), ["clip", "pop"]);
        assert_eq!(trace(&composition, 10.0), ["clip", "fill(1)", "pop"]);
        assert_eq!(trace(&composition, 20.0), ["clip", "pop"]);
    }

    /// A document without translucent layers, groups or repeater copies. It covers parenting, a
    /// group, a track matte, a mask, a precomposition, a repeater and an image.
    fn opaque_document() -> crate::Composition {
        let mask_path = json!({
            "a": 0,
            "k": {
                "c": true,
                "v": [[0, 0], [40, 0], [40, 40], [0, 40]],
                "i": [[0, 0], [0, 0], [0, 0], [0, 0]],
                "o": [[0, 0], [0, 0], [0, 0], [0, 0]],
            },
        });
        composition_with_assets(
            json!([
                { "id": "image", "w": 8, "h": 8, "u": "", "p": "image.png", "e": 0 },
                precomp(
                    "comp",
                    json!([shape_layer(
                        1,
                        json!([
                            rect(0.0, 0.0, 10.0, 10.0),
                            fill(100.0),
                            repeater(2, 20.0, 100.0, 100.0),
                        ]),
                    )]),
                ),
            ]),
            json!([
                with(
                    shape_layer(
                        1,
                        json!([group(
                            vec![rect(10.0, 10.0, 30.0, 30.0), fill(100.0), stroke(50.0)],
                            100.0,
                        )]),
                    ),
                    json!({ "parent": 6 }),
                ),
                with(
                    shape_layer(2, json!([rect(0.0, 0.0, 50.0, 50.0), fill(100.0)])),
                    json!({ "td": 1 }),
                ),
                with(
                    shape_layer(3, json!([rect(25.0, 25.0, 75.0, 75.0), fill(100.0)])),
                    json!({ "tt": 1 }),
                ),
                with(
                    precomp_layer(4, "comp"),
                    json!({
                        "masksProperties": [
                            { "mode": "a", "pt": mask_path, "o": { "a": 0, "k": 100 } },
                        ],
                    }),
                ),
                with(layer(2, 5), json!({ "refId": "image" })),
                with(
                    layer(3, 6),
                    json!({
                        "ks": with(transform(100.0), json!({ "p": { "a": 0, "k": [5, 5] } })),
                    }),
                ),
            ]),
        )
    }

    // Pins the complete sequence for a document that needs no opacity compositing, so that
    // changes to how opacity is composited cannot alter what such documents emit.
    #[test]
    fn opaque_document_emits_a_fixed_sequence() {
        let full = Rect::new(0.0, 0.0, 100.0, 100.0);
        let begin = |index: u32| Event::BeginLayer(format!("layer {index}"));
        let draw = |stroked, alpha, bounds| Event::Draw {
            stroked,
            alpha,
            bounds,
        };
        let layer = |blend: BlendMode| Event::PushLayer {
            blend,
            alpha: 1.0,
            bounds: full,
        };
        assert_eq!(
            render(&opaque_document(), 0.0, 1.0),
            [
                Event::PushClip(full),
                // The null layer draws nothing.
                begin(6),
                Event::EndLayer,
                // The image is clipped to its authored size.
                begin(5),
                Event::PushClip(Rect::new(0.0, 0.0, 8.0, 8.0)),
                Event::Image {
                    id: "image".into(),
                    alpha: 1.0,
                },
                Event::Pop,
                Event::EndLayer,
                // The mask clips the precomposition, whose repeater paints later copies first.
                begin(4),
                Event::PushClip(Rect::new(0.0, 0.0, 40.0, 40.0)),
                begin(1),
                draw(false, 1.0, Rect::new(20.0, 0.0, 30.0, 10.0)),
                draw(false, 1.0, Rect::new(0.0, 0.0, 10.0, 10.0)),
                Event::EndLayer,
                Event::Pop,
                Event::EndLayer,
                // The matte source renders into its own layer and the matted layer keeps only
                // what falls inside it.
                begin(3),
                layer(Mix::Normal.into()),
                begin(2),
                draw(false, 1.0, Rect::new(0.0, 0.0, 50.0, 50.0)),
                Event::EndLayer,
                layer(Compose::SrcIn.into()),
                draw(false, 1.0, Rect::new(25.0, 25.0, 75.0, 75.0)),
                Event::Pop,
                Event::Pop,
                Event::EndLayer,
                // The parent null offsets the group; the stroke is listed last so it paints first.
                begin(1),
                draw(true, 0.5, Rect::new(15.0, 15.0, 35.0, 35.0)),
                draw(false, 1.0, Rect::new(15.0, 15.0, 35.0, 35.0)),
                Event::EndLayer,
                Event::Pop,
            ]
        );
    }

    /// Two overlapping rectangles with one fill each, listed as two groups.
    fn two_fills() -> Vec<serde_json::Value> {
        vec![
            group(vec![rect(0.0, 0.0, 20.0, 20.0), fill(100.0)], 100.0),
            group(vec![rect(10.0, 10.0, 30.0, 30.0), fill(100.0)], 100.0),
        ]
    }

    #[test]
    fn translucent_layer_with_overlapping_content_is_composited_as_a_group() {
        let layer = with(shape_layer(1, json!(two_fills())), opacity(50.0));
        assert_eq!(
            trace(&composition(json!([layer])), 0.0),
            ["clip", "layer(0.5)", "fill(1)", "fill(1)", "pop", "pop"]
        );
    }

    #[test]
    fn translucent_layer_with_one_draw_call_fades_its_brush() {
        let shapes = json!([rect(0.0, 0.0, 20.0, 20.0), fill(50.0)]);
        let layer = with(shape_layer(1, shapes), opacity(50.0));
        assert_eq!(
            trace(&composition(json!([layer])), 0.0),
            ["clip", "fill(0.25)", "pop"]
        );
    }

    #[test]
    fn translucent_group_with_overlapping_content_is_composited_as_a_group() {
        let shapes = json!([
            group(two_fills(), 50.0),
            rect(0.0, 0.0, 5.0, 5.0),
            fill(100.0),
        ]);
        assert_eq!(
            trace(&composition(json!([shape_layer(1, shapes)])), 0.0),
            [
                "clip",
                "fill(1)",
                "layer(0.5)",
                "fill(1)",
                "fill(1)",
                "pop",
                "pop"
            ]
        );
    }

    #[test]
    fn translucent_group_with_one_draw_call_fades_its_brush() {
        let shapes = json!([
            group(vec![rect(0.0, 0.0, 20.0, 20.0), fill(50.0)], 50.0),
            group(vec![rect(10.0, 10.0, 30.0, 30.0), stroke(100.0)], 20.0),
        ]);
        assert_eq!(
            trace(&composition(json!([shape_layer(1, shapes)])), 0.0),
            ["clip", "stroke(0.2)", "fill(0.25)", "pop"]
        );
    }

    #[test]
    fn opacities_around_one_draw_call_multiply_into_its_brush() {
        let shapes = json!([group(
            vec![group(vec![rect(0.0, 0.0, 20.0, 20.0), fill(50.0)], 50.0)],
            50.0
        )]);
        let layer = with(shape_layer(1, shapes), opacity(50.0));
        assert_eq!(
            trace(&composition(json!([layer])), 0.0),
            ["clip", "fill(0.0625)", "pop"]
        );
    }

    #[test]
    fn nested_translucent_groups_nest_their_layers() {
        // The outer group holds three draw calls and the inner group two of them.
        let shapes = json!([group(
            vec![
                group(two_fills(), 50.0),
                rect(0.0, 0.0, 5.0, 5.0),
                stroke(100.0),
            ],
            25.0
        )]);
        assert_eq!(
            trace(&composition(json!([shape_layer(1, shapes)])), 0.0),
            [
                "clip",
                "layer(0.25)",
                "stroke(1)",
                "layer(0.5)",
                "fill(1)",
                "fill(1)",
                "pop",
                "pop",
                "pop"
            ]
        );
    }

    #[test]
    fn translucent_precomposition_is_composited_as_a_group() {
        let child = |ind| shape_layer(ind, json!([rect(0.0, 0.0, 20.0, 20.0), fill(100.0)]));
        let composition = composition_with_assets(
            json!([precomp(
                "comp",
                json!([child(1), with(child(2), opacity(50.0))])
            )]),
            json!([with(precomp_layer(1, "comp"), opacity(50.0))]),
        );
        // The children do not take the opacity of the precomposition layer themselves.
        assert_eq!(
            trace(&composition, 0.0),
            ["clip", "layer(0.5)", "fill(0.5)", "fill(1)", "pop", "pop"]
        );
    }

    #[test]
    fn translucent_precomposition_with_one_drawing_layer_hands_down_its_opacity() {
        let shapes = || json!([rect(0.0, 0.0, 20.0, 20.0), fill(100.0)]);
        let assets = json!([
            precomp(
                "single",
                json!([
                    with(shape_layer(1, shapes()), opacity(50.0)),
                    // Neither a null layer nor an inactive layer draws.
                    layer(3, 2),
                    with(shape_layer(3, shapes()), json!({ "ip": 30 })),
                ])
            ),
            precomp("overlapping", json!([shape_layer(1, json!(two_fills()))])),
        ]);
        let single = composition_with_assets(
            assets.clone(),
            json!([with(precomp_layer(1, "single"), opacity(50.0))]),
        );
        assert_eq!(trace(&single, 0.0), ["clip", "fill(0.25)", "pop"]);
        // Once the second shape layer is active the precomposition needs a layer.
        assert_eq!(
            trace(&single, 30.0),
            ["clip", "layer(0.5)", "fill(1)", "fill(0.5)", "pop", "pop"]
        );

        // The layer that takes the opacity composites its own overlapping content.
        let overlapping = composition_with_assets(
            assets,
            json!([with(precomp_layer(1, "overlapping"), opacity(50.0))]),
        );
        assert_eq!(
            trace(&overlapping, 0.0),
            ["clip", "layer(0.5)", "fill(1)", "fill(1)", "pop", "pop"]
        );
    }

    #[test]
    fn caller_alpha_applies_to_the_animation_as_a_whole() {
        let shapes = || json!([rect(0.0, 0.0, 20.0, 20.0), fill(100.0)]);
        let two_layers = composition(json!([shape_layer(1, shapes()), shape_layer(2, shapes())]));
        assert_eq!(
            trace_with_alpha(&two_layers, 0.0, 0.5),
            ["layer(0.5)", "fill(1)", "fill(1)", "pop"]
        );
        // The opacity layer takes the place of the composition clip.
        assert!(matches!(
            &render(&two_layers, 0.0, 0.5)[0],
            Event::PushLayer { bounds, .. } if *bounds == Rect::new(0.0, 0.0, 100.0, 100.0)
        ));

        let one_layer = composition(json!([shape_layer(1, shapes())]));
        assert_eq!(
            trace_with_alpha(&one_layer, 0.0, 0.5),
            ["clip", "fill(0.5)", "pop"]
        );

        let one_layer_two_fills = composition(json!([shape_layer(1, json!(two_fills()))]));
        assert_eq!(
            trace_with_alpha(&one_layer_two_fills, 0.0, 0.5),
            ["clip", "layer(0.5)", "fill(1)", "fill(1)", "pop", "pop"]
        );

        assert!(render(&two_layers, 0.0, 0.0).is_empty());
    }

    #[test]
    fn transparent_content_is_skipped() {
        let shapes = || json!([rect(0.0, 0.0, 20.0, 20.0), fill(100.0)]);
        let transparent_layer = composition(json!([
            with(shape_layer(1, json!(two_fills())), opacity(0.0)),
            shape_layer(2, shapes()),
        ]));
        assert_eq!(
            render(&transparent_layer, 0.0, 1.0)
                .iter()
                .map(Event::to_string)
                .collect::<Vec<_>>(),
            ["clip", "begin(layer 2)", "fill(1)", "end", "pop"]
        );

        let transparent_group = composition(json!([shape_layer(
            1,
            json!([
                group(two_fills(), 0.0),
                rect(0.0, 0.0, 5.0, 5.0),
                fill(100.0)
            ]),
        )]));
        assert_eq!(trace(&transparent_group, 0.0), ["clip", "fill(1)", "pop"]);

        let transparent_style = composition(json!([shape_layer(
            1,
            json!([rect(0.0, 0.0, 5.0, 5.0), fill(0.0), stroke(100.0)]),
        )]));
        assert_eq!(trace(&transparent_style, 0.0), ["clip", "stroke(1)", "pop"]);
    }

    #[test]
    fn translucent_layer_with_empty_content_pushes_no_layer() {
        let composition = composition(json!([
            with(shape_layer(1, json!([fill(100.0)])), opacity(50.0)),
            with(layer(3, 2), opacity(50.0)),
        ]));
        assert_eq!(trace(&composition, 0.0), ["clip", "pop"]);
    }

    #[test]
    fn matte_source_keeps_its_own_opacity() {
        let source = with(
            shape_layer(1, json!([rect(0.0, 0.0, 50.0, 50.0), fill(100.0)])),
            json!({ "td": 1 }),
        );
        let matted = |opacity_percent: f64| {
            with(
                with(shape_layer(2, json!(two_fills())), json!({ "tt": 1 })),
                opacity(opacity_percent),
            )
        };

        // The layer that holds the matte carries the opacity of the matted layer. The source is
        // drawn at its own opacity.
        let translucent = composition(json!([source.clone(), matted(50.0)]));
        assert_eq!(
            trace(&translucent, 0.0),
            [
                "clip",
                "layer(0.5)",
                "fill(1)",
                "layer(1, SrcIn)",
                "fill(1)",
                "fill(1)",
                "pop",
                "pop",
                "pop"
            ]
        );
        let translucent_source =
            composition(json!([with(source.clone(), opacity(50.0)), matted(50.0)]));
        assert_eq!(
            trace(&translucent_source, 0.0)[1..4],
            ["layer(0.5)", "fill(0.5)", "layer(1, SrcIn)"]
        );

        // A transparent source leaves the matte layers empty but balanced.
        let transparent_source =
            composition(json!([with(source.clone(), opacity(0.0)), matted(100.0)]));
        assert_eq!(
            trace(&transparent_source, 0.0),
            [
                "clip",
                "layer(1)",
                "layer(1, SrcIn)",
                "fill(1)",
                "fill(1)",
                "pop",
                "pop",
                "pop"
            ]
        );

        // A transparent matted layer is skipped along with its matte.
        let transparent_target = composition(json!([source, matted(0.0)]));
        assert_eq!(trace(&transparent_target, 0.0), ["clip", "pop"]);
    }

    #[test]
    fn translucent_layer_with_mask_is_composited_before_it_is_clipped() {
        let mask = json!({
            "mode": "a",
            "o": { "a": 0, "k": 100 },
            "pt": {
                "a": 0,
                "k": {
                    "c": true,
                    "v": [[0, 0], [40, 0], [40, 40], [0, 40]],
                    "i": [[0, 0], [0, 0], [0, 0], [0, 0]],
                    "o": [[0, 0], [0, 0], [0, 0], [0, 0]],
                },
            },
        });
        let masked = |shapes| {
            with(
                with(
                    shape_layer(1, shapes),
                    json!({ "masksProperties": [mask.clone()] }),
                ),
                opacity(50.0),
            )
        };
        assert_eq!(
            trace(&composition(json!([masked(json!(two_fills()))])), 0.0),
            [
                "clip",
                "layer(0.5)",
                "clip",
                "fill(1)",
                "fill(1)",
                "pop",
                "pop",
                "pop"
            ]
        );
        let one_fill = json!([rect(0.0, 0.0, 20.0, 20.0), fill(100.0)]);
        assert_eq!(
            trace(&composition(json!([masked(one_fill)])), 0.0),
            ["clip", "clip", "fill(0.5)", "pop", "pop"]
        );
    }

    #[test]
    fn translucent_layer_with_effects_is_always_composited_as_a_group() {
        let blur = json!({ "ty": 29, "nm": "blur", "ef": [{ "v": { "a": 0, "k": 4 } }] });
        let layer = with(
            with(
                shape_layer(1, json!([rect(0.0, 0.0, 20.0, 20.0), fill(100.0)])),
                json!({ "ef": [blur] }),
            ),
            opacity(50.0),
        );
        // The opacity applies to the output of the effect.
        assert_eq!(
            trace(&composition(json!([layer])), 0.0),
            ["clip", "layer(0.5)", "fill(1)", "pop", "pop"]
        );
    }

    #[test]
    fn translucent_image_layer_fades_the_image() {
        let composition = composition_with_assets(
            json!([{ "id": "image", "w": 8, "h": 8, "u": "", "p": "image.png", "e": 0 }]),
            json!([with(
                with(layer(2, 1), json!({ "refId": "image" })),
                opacity(50.0)
            )]),
        );
        assert_eq!(
            trace(&composition, 0.0),
            ["clip", "clip", "image(image, 0.5)", "pop", "pop"]
        );
    }

    #[test]
    fn repeater_copy_with_one_draw_call_fades_its_brush() {
        // The copies fade from opaque to transparent; the last one is not drawn.
        let shapes = json!([
            rect(0.0, 0.0, 10.0, 10.0),
            fill(100.0),
            repeater(3, 20.0, 100.0, 0.0),
        ]);
        assert_eq!(
            trace(&composition(json!([shape_layer(1, shapes)])), 0.0),
            ["clip", "fill(0.5)", "fill(1)", "pop"]
        );
    }

    #[test]
    fn repeater_copy_with_overlapping_content_is_composited_as_a_group() {
        let shapes = json!([
            rect(0.0, 0.0, 10.0, 10.0),
            fill(100.0),
            stroke(100.0),
            repeater(2, 20.0, 100.0, 50.0),
        ]);
        assert_eq!(
            trace(&composition(json!([shape_layer(1, shapes)])), 0.0),
            [
                "clip",
                "layer(0.5)",
                "stroke(1)",
                "fill(1)",
                "pop",
                "stroke(1)",
                "fill(1)",
                "pop"
            ]
        );
    }

    #[test]
    fn translucent_group_inside_repeater_copy_shares_the_copy_with_its_siblings() {
        let shapes = json!([
            group(two_fills(), 50.0),
            rect(0.0, 0.0, 5.0, 5.0),
            stroke(100.0),
            repeater(2, 40.0, 25.0, 25.0),
        ]);
        // Each copy composites the stroke together with the group, which is composited first.
        let copy = [
            "layer(0.25)",
            "stroke(1)",
            "layer(0.5)",
            "fill(1)",
            "fill(1)",
            "pop",
            "pop",
        ];
        let expected: Vec<_> = ["clip"]
            .into_iter()
            .chain(copy)
            .chain(copy)
            .chain(["pop"])
            .collect();
        assert_eq!(
            trace(&composition(json!([shape_layer(1, shapes)])), 0.0),
            expected
        );
    }

    #[test]
    fn style_after_a_repeater_takes_the_opacity_of_each_copy() {
        let shapes = json!([
            rect(0.0, 0.0, 10.0, 10.0),
            repeater(2, 20.0, 100.0, 50.0),
            fill(100.0),
        ]);
        let events = render(&composition(json!([shape_layer(1, shapes)])), 0.0, 1.0);
        let draws: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::Draw { alpha, bounds, .. } => Some((*alpha, *bounds)),
                _ => None,
            })
            .collect();
        assert_eq!(
            draws,
            [
                (0.5, Rect::new(20.0, 0.0, 30.0, 10.0)),
                (1.0, Rect::new(0.0, 0.0, 10.0, 10.0)),
            ]
        );
        assert_eq!(events.len(), 6, "no layer is pushed for a single draw call");
    }

    #[test]
    fn layers_stay_balanced_while_opacities_animate() {
        let fade = |from: f64, to: f64| {
            json!({
                "a": 1,
                "k": [
                    { "t": 0, "s": [from], "i": { "x": [1], "y": [1] }, "o": { "x": [0], "y": [0] } },
                    { "t": 30, "s": [to] },
                ],
            })
        };
        let mut faded_group = group(two_fills(), 100.0);
        faded_group["it"][2]["o"] = fade(100.0, 0.0);
        let mut shapes = shape_layer(
            1,
            json!([
                faded_group,
                rect(0.0, 0.0, 5.0, 5.0),
                stroke(100.0),
                repeater(3, 10.0, 100.0, 0.0),
            ]),
        );
        shapes["ks"]["o"] = fade(0.0, 100.0);
        let mut instance = precomp_layer(1, "comp");
        instance["ks"]["o"] = fade(100.0, 0.0);
        let composition = composition_with_assets(
            json!([precomp(
                "comp",
                json!([shapes.clone(), with(shapes, json!({ "ind": 2 }))])
            )]),
            json!([instance]),
        );
        // `render` panics if pushes and pops do not balance.
        let mut layers = 0;
        for frame in 0..=60 {
            let events = render(&composition, f64::from(frame) / 2.0, 0.5);
            layers += events
                .iter()
                .filter(|event| matches!(event, Event::PushLayer { .. }))
                .count();
        }
        assert!(layers > 0, "the document should need opacity layers");
    }

    #[test]
    fn opacity_is_limited_to_the_range_from_zero_to_one() {
        // Opacity above 100 is full opacity, which needs no layer and leaves brushes alone.
        let layer = with(
            shape_layer(
                1,
                json!([
                    group(two_fills(), 150.0),
                    rect(0.0, 0.0, 5.0, 5.0),
                    fill(200.0)
                ]),
            ),
            opacity(150.0),
        );
        let composition = composition(json!([layer]));
        let opaque = ["clip", "fill(1)", "fill(1)", "fill(1)", "pop"];
        assert_eq!(trace(&composition, 0.0), opaque);
        assert_eq!(trace_with_alpha(&composition, 0.0, 2.0), opaque);

        let copies = json!([
            rect(0.0, 0.0, 10.0, 10.0),
            fill(100.0),
            stroke(100.0),
            repeater(2, 20.0, 300.0, 300.0),
        ]);
        let repeated = crate::test_support::composition(json!([shape_layer(1, copies)]));
        assert_eq!(
            trace(&repeated, 0.0),
            [
                "clip",
                "stroke(1)",
                "fill(1)",
                "stroke(1)",
                "fill(1)",
                "pop"
            ]
        );

        // Opacity below zero is no opacity.
        let negative = with(shape_layer(1, json!(two_fills())), opacity(-50.0));
        let negative = crate::test_support::composition(json!([negative]));
        assert_eq!(trace(&negative, 0.0), ["clip", "pop"]);
        assert!(render(&composition, 0.0, -1.0).is_empty());
    }

    #[test]
    fn matte_source_does_not_count_as_a_drawing_layer() {
        let layers = || {
            json!([
                with(
                    shape_layer(1, json!([rect(0.0, 0.0, 50.0, 50.0), fill(100.0)])),
                    json!({ "td": 1 }),
                ),
                with(
                    shape_layer(2, json!([rect(0.0, 0.0, 20.0, 20.0), fill(100.0)])),
                    json!({ "tt": 1 }),
                ),
            ])
        };
        // The matted layer is the only one that draws, so it takes the opacity, on the layer
        // that holds its matte. No further layer is needed around the two.
        let expected = [
            "clip",
            "layer(0.5)",
            "fill(1)",
            "layer(1, SrcIn)",
            "fill(1)",
            "pop",
            "pop",
            "pop",
        ];
        let precomposed = composition_with_assets(
            json!([precomp("comp", layers())]),
            json!([with(precomp_layer(1, "comp"), opacity(50.0))]),
        );
        assert_eq!(trace(&precomposed, 0.0), expected);
        assert_eq!(trace_with_alpha(&composition(layers()), 0.0, 0.5), expected);
    }

    #[test]
    fn repeater_with_too_many_copies_draws_nothing() {
        let mut too_many = repeater(1, 10.0, 100.0, 100.0);
        too_many["c"]["k"] = json!(1e30);
        let shapes = json!([
            rect(0.0, 0.0, 10.0, 10.0),
            rect(20.0, 0.0, 30.0, 10.0),
            fill(100.0),
            too_many,
        ]);
        assert_eq!(
            trace(&composition(json!([shape_layer(1, shapes)])), 0.0),
            ["clip", "pop"]
        );
    }

    #[test]
    fn trim_of_an_extremely_long_curve_renders() {
        let shapes = json!([
            {
                "ty": "sh",
                "ks": {
                    "a": 0,
                    "k": {
                        "c": false,
                        "v": [[0, 0], [1e25, 0]],
                        "i": [[0, 0], [0, 1e24]],
                        "o": [[0, 1e24], [0, 0]],
                    },
                },
            },
            stroke(100.0),
            {
                "ty": "tm",
                "s": { "a": 0, "k": 25 },
                "e": { "a": 0, "k": 75 },
                "o": { "a": 0, "k": 0 },
                "m": 1,
            },
        ]);
        let events = render(&composition(json!([shape_layer(1, shapes)])), 0.0, 1.0);
        let Event::Draw { bounds, .. } = &events[2] else {
            panic!("expected a draw call, got {events:?}");
        };
        assert!(bounds.x0 > 1e24 && bounds.x1 < 9e24, "{bounds:?}");
    }

    /// A shape layer with one fill and the given blend mode (`bm`).
    fn blended_layer(ind: u32, mode: u8) -> serde_json::Value {
        with(
            shape_layer(ind, json!([rect(0.0, 0.0, 20.0, 20.0), fill(100.0)])),
            json!({ "bm": mode }),
        )
    }

    #[test]
    fn layer_with_blend_mode_is_composited_with_it() {
        let plain = shape_layer(2, json!([rect(0.0, 0.0, 20.0, 20.0), fill(100.0)]));
        let composition = composition(json!([blended_layer(1, 1), plain]));
        let events = render(&composition, 0.0, 1.0);
        // The animation is isolated so that the layer blends with the animation only. That
        // layer takes the place of the composition clip.
        assert_eq!(
            events[0],
            Event::PushLayer {
                blend: Mix::Normal.into(),
                alpha: 1.0,
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            }
        );
        assert_eq!(
            trace(&composition, 0.0),
            [
                "layer(1)",
                "fill(1)",
                "layer(1, Multiply)",
                "fill(1)",
                "pop",
                "pop"
            ]
        );
    }

    #[test]
    fn add_blend_mode_sums_colors() {
        let composition = composition(json!([blended_layer(1, 16)]));
        assert_eq!(
            trace(&composition, 0.0),
            ["layer(1)", "layer(1, PlusLighter)", "fill(1)", "pop", "pop"]
        );
    }

    #[test]
    fn hard_mix_blend_mode_renders_as_normal() {
        let composition = composition(json!([blended_layer(1, 17)]));
        assert_eq!(trace(&composition, 0.0), ["clip", "fill(1)", "pop"]);
    }

    #[test]
    fn blend_layer_carries_the_opacity_of_the_layer() {
        let layer = with(
            with(shape_layer(1, json!(two_fills())), json!({ "bm": 2 })),
            opacity(50.0),
        );
        assert_eq!(
            trace(&composition(json!([layer])), 0.0),
            [
                "layer(1)",
                "layer(0.5, Screen)",
                "fill(1)",
                "fill(1)",
                "pop",
                "pop"
            ]
        );
    }

    #[test]
    fn blend_layer_holds_the_track_matte() {
        let source = with(
            shape_layer(1, json!([rect(0.0, 0.0, 50.0, 50.0), fill(100.0)])),
            json!({ "td": 1 }),
        );
        let matted = with(blended_layer(2, 1), json!({ "tt": 1 }));
        assert_eq!(
            trace(&composition(json!([source, matted])), 0.0),
            [
                "layer(1)",
                "layer(1, Multiply)",
                "fill(1)",
                "layer(1, SrcIn)",
                "fill(1)",
                "pop",
                "pop",
                "pop"
            ]
        );
    }

    #[test]
    fn precomposition_isolates_layers_with_blend_modes() {
        let assets = json!([precomp("comp", json!([blended_layer(1, 1)]))]);
        // The layer blends with the content of the precomposition only.
        let opaque = composition_with_assets(assets.clone(), json!([precomp_layer(1, "comp")]));
        assert_eq!(
            trace(&opaque, 0.0),
            [
                "clip",
                "layer(1)",
                "layer(1, Multiply)",
                "fill(1)",
                "pop",
                "pop",
                "pop"
            ]
        );
        // The opacity of the precomposition is not handed to a layer that blends.
        let translucent = composition_with_assets(
            assets,
            json!([with(precomp_layer(1, "comp"), opacity(50.0))]),
        );
        assert_eq!(
            trace(&translucent, 0.0),
            [
                "clip",
                "layer(0.5)",
                "layer(1, Multiply)",
                "fill(1)",
                "pop",
                "pop",
                "pop"
            ]
        );
    }

    #[test]
    fn caller_alpha_is_not_handed_to_a_layer_with_a_blend_mode() {
        let composition = composition(json!([blended_layer(1, 1)]));
        assert_eq!(
            trace_with_alpha(&composition, 0.0, 0.5),
            ["layer(0.5)", "layer(1, Multiply)", "fill(1)", "pop", "pop"]
        );
    }

    #[test]
    fn blend_mode_of_a_layer_that_does_not_draw_is_ignored() {
        // Transparent, hidden and null layers do not draw.
        let composition = composition(json!([
            with(blended_layer(1, 1), opacity(0.0)),
            with(blended_layer(2, 1), json!({ "hd": true })),
            with(layer(3, 3), json!({ "bm": 1 })),
        ]));
        assert_eq!(trace(&composition, 0.0), ["clip", "pop"]);
    }

    #[test]
    fn blend_mode_of_a_layer_without_draw_calls_pushes_no_layer() {
        // A fill without a path, and a fill that is transparent.
        let no_path = with(shape_layer(1, json!([fill(100.0)])), json!({ "bm": 1 }));
        let transparent = with(
            shape_layer(2, json!([rect(0.0, 0.0, 20.0, 20.0), fill(0.0)])),
            json!({ "bm": 1 }),
        );
        let empty = composition(json!([no_path.clone(), transparent.clone()]));
        // Neither a blend layer nor a layer to isolate the animation is pushed.
        assert_eq!(trace(&empty, 0.0), ["clip", "pop"]);

        // A precomposition whose layers make no draw call does not blend either.
        let assets = json!([precomp("comp", json!([no_path, transparent]))]);
        let precomposed = composition_with_assets(
            assets,
            json!([with(precomp_layer(1, "comp"), json!({ "bm": 1 }))]),
        );
        assert_eq!(trace(&precomposed, 0.0), ["clip", "pop"]);

        // With a track matte the layer that holds the matte remains, without the blend mode.
        let source = with(
            shape_layer(1, json!([rect(0.0, 0.0, 50.0, 50.0), fill(100.0)])),
            json!({ "td": 1 }),
        );
        let matted = with(
            shape_layer(2, json!([fill(100.0)])),
            json!({ "bm": 1, "tt": 1 }),
        );
        assert_eq!(
            trace(&composition(json!([source, matted])), 0.0),
            [
                "clip",
                "layer(1)",
                "fill(1)",
                "layer(1, SrcIn)",
                "pop",
                "pop",
                "pop"
            ]
        );
    }

    #[test]
    fn matte_source_and_matted_layer_keep_their_own_shapes() {
        // Both layers have several draw calls, and the matted layer is evaluated before its
        // source is rendered.
        let source = with(
            shape_layer(
                1,
                json!([
                    rect(0.0, 0.0, 50.0, 50.0),
                    fill(100.0),
                    rect(50.0, 50.0, 60.0, 60.0),
                    stroke(100.0),
                ]),
            ),
            json!({ "td": 1 }),
        );
        let matted = with(blended_layer(2, 1), json!({ "tt": 1 }));
        let events = render(&composition(json!([source, matted])), 0.0, 1.0);
        let draws: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::Draw {
                    stroked, bounds, ..
                } => Some((*stroked, *bounds)),
                _ => None,
            })
            .collect();
        assert_eq!(
            draws,
            [
                (true, Rect::new(0.0, 0.0, 60.0, 60.0)),
                (false, Rect::new(0.0, 0.0, 50.0, 50.0)),
                (false, Rect::new(0.0, 0.0, 20.0, 20.0)),
            ]
        );
    }

    fn draw_bounds(events: &[Event]) -> Vec<Rect> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Draw { bounds, .. } => Some(*bounds),
                _ => None,
            })
            .collect()
    }

    fn hidden(shape: serde_json::Value) -> serde_json::Value {
        with(shape, json!({ "hd": true }))
    }

    #[test]
    fn hidden_geometry_is_not_drawn() {
        let shapes = json!([
            hidden(rect(0.0, 0.0, 10.0, 10.0)),
            rect(20.0, 20.0, 30.0, 30.0),
            fill(100.0),
        ]);
        let events = render(&composition(json!([shape_layer(1, shapes)])), 0.0, 1.0);
        assert_eq!(draw_bounds(&events), [Rect::new(20.0, 20.0, 30.0, 30.0)]);
    }

    #[test]
    fn hidden_style_draws_nothing() {
        let shapes = json!([
            rect(0.0, 0.0, 10.0, 10.0),
            hidden(fill(100.0)),
            stroke(100.0),
            hidden(gradient("gf", 100.0)),
        ]);
        assert_eq!(
            trace(&composition(json!([shape_layer(1, shapes)])), 0.0),
            ["clip", "stroke(1)", "pop"]
        );
    }

    #[test]
    fn hidden_group_is_not_rendered() {
        let shapes = json!([
            hidden(group(vec![rect(0.0, 0.0, 10.0, 10.0), fill(100.0)], 100.0)),
            rect(20.0, 20.0, 30.0, 30.0),
            fill(100.0),
        ]);
        let events = render(&composition(json!([shape_layer(1, shapes)])), 0.0, 1.0);
        // The geometry of the hidden group does not reach the fill after it either.
        assert_eq!(draw_bounds(&events), [Rect::new(20.0, 20.0, 30.0, 30.0)]);
    }

    #[test]
    fn hidden_trim_leaves_paths_whole() {
        let line = json!({
            "ty": "sh",
            "ks": {
                "a": 0,
                "k": {
                    "c": false,
                    "v": [[0, 0], [40, 0]],
                    "i": [[0, 0], [0, 0]],
                    "o": [[0, 0], [0, 0]],
                },
            },
        });
        let trim = json!({
            "ty": "tm",
            "s": { "a": 0, "k": 0 },
            "e": { "a": 0, "k": 50 },
            "o": { "a": 0, "k": 0 },
            "m": 1,
        });
        let bounds = |trim: serde_json::Value| {
            let shapes = json!([line.clone(), stroke(100.0), trim]);
            draw_bounds(&render(
                &composition(json!([shape_layer(1, shapes)])),
                0.0,
                1.0,
            ))
        };
        assert_eq!(bounds(trim.clone()), [Rect::new(0.0, 0.0, 20.0, 0.0)]);
        assert_eq!(bounds(hidden(trim)), [Rect::new(0.0, 0.0, 40.0, 0.0)]);
    }

    #[test]
    fn hidden_shapes_stay_in_path_queries() {
        use kurbo::Shape as _;
        let shapes = json!([
            hidden(rect(0.0, 0.0, 10.0, 10.0)),
            hidden(group(
                vec![rect(20.0, 20.0, 30.0, 30.0), fill(100.0)],
                100.0
            )),
            hidden(fill(100.0)),
            rect(40.0, 40.0, 50.0, 50.0),
            fill(100.0),
        ]);
        let composition = composition(json!([shape_layer(1, shapes)]));
        // Only the visible rectangle is drawn.
        let events = render(&composition, 0.0, 1.0);
        assert_eq!(draw_bounds(&events), [Rect::new(40.0, 40.0, 50.0, 50.0)]);

        // The paths of the layer include the hidden rectangle and that of the hidden group.
        let all = [
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Rect::new(20.0, 20.0, 30.0, 30.0),
            Rect::new(40.0, 40.0, 50.0, 50.0),
        ];
        let evaluated = composition.evaluate(0.0).unwrap();
        let layer = &evaluated.layers[0];
        let bounds = |path: &kurbo::BezPath| path.bounding_box();
        let evaluated_bounds: Vec<_> = layer.evaluated_paths().iter().map(bounds).collect();
        assert_eq!(evaluated_bounds, all);
        let authored = layer.authored_paths();
        let authored_bounds: Vec<_> = authored
            .iter()
            .map(|path| path.elements.as_slice().bounding_box())
            .collect();
        assert_eq!(authored_bounds, all);
        // Hidden shapes keep their place among the shapes of the layer.
        let indices: Vec<_> = authored.iter().map(|path| path.indices.clone()).collect();
        assert_eq!(indices, [vec![0], vec![1, 0], vec![3]]);
    }

    #[test]
    fn hidden_trim_is_not_applied_to_queried_paths() {
        use kurbo::Shape as _;
        let line = json!({
            "ty": "sh",
            "ks": {
                "a": 0,
                "k": {
                    "c": false,
                    "v": [[0, 0], [40, 0]],
                    "i": [[0, 0], [0, 0]],
                    "o": [[0, 0], [0, 0]],
                },
            },
        });
        let trim = json!({
            "ty": "tm",
            "s": { "a": 0, "k": 0 },
            "e": { "a": 0, "k": 50 },
            "o": { "a": 0, "k": 0 },
            "m": 1,
        });
        let bounds = |trim: serde_json::Value| {
            let composition = composition(json!([shape_layer(1, json!([line.clone(), trim]))]));
            let evaluated = composition.evaluate(0.0).unwrap();
            evaluated.layers[0].evaluated_paths()[0].bounding_box()
        };
        assert_eq!(bounds(trim.clone()), Rect::new(0.0, 0.0, 20.0, 0.0));
        assert_eq!(bounds(hidden(trim)), Rect::new(0.0, 0.0, 40.0, 0.0));
    }

    #[test]
    fn gradient_opacity_fades_the_gradient() {
        let shapes = json!([
            rect(0.0, 0.0, 10.0, 10.0),
            gradient("gf", 50.0),
            gradient("gs", 25.0),
        ]);
        assert_eq!(
            trace(&composition(json!([shape_layer(1, shapes)])), 0.0),
            ["clip", "stroke(0.25)", "fill(0.5)", "pop"]
        );
    }

    /// A precomposition with one layer that fills the whole composition.
    fn filled_precomp(id: &str) -> serde_json::Value {
        precomp(
            id,
            json!([shape_layer(
                1,
                json!([rect(0.0, 0.0, 100.0, 100.0), fill(100.0)])
            )]),
        )
    }

    fn moved(layer: serde_json::Value, x: f64, y: f64) -> serde_json::Value {
        let ks = with(transform(100.0), json!({ "p": { "a": 0, "k": [x, y] } }));
        with(layer, json!({ "ks": ks }))
    }

    #[test]
    fn precomposition_is_clipped_to_its_size() {
        let instance = with(precomp_layer(1, "comp"), json!({ "w": 40, "h": 30 }));
        let composition = composition_with_assets(
            json!([filled_precomp("comp")]),
            json!([moved(instance, 10.0, 20.0)]),
        );
        let events = render(&composition, 0.0, 1.0);
        assert_eq!(
            trace(&composition, 0.0),
            ["clip", "clip", "fill(1)", "pop", "pop"]
        );
        // The clip follows the transform of the precomposition layer.
        assert!(events.contains(&Event::PushClip(Rect::new(10.0, 20.0, 50.0, 50.0))));
    }

    #[test]
    fn precomposition_that_covers_the_composition_needs_no_clip() {
        let assets = json!([filled_precomp("comp")]);
        let covering = composition_with_assets(
            assets.clone(),
            json!([with(
                precomp_layer(1, "comp"),
                json!({ "w": 300, "h": 300 })
            )]),
        );
        assert_eq!(trace(&covering, 0.0), ["clip", "fill(1)", "pop"]);
        // Once it moves, part of the composition lies outside it.
        let moved =
            composition_with_assets(assets, json!([moved(precomp_layer(1, "comp"), 50.0, 0.0)]));
        assert_eq!(
            trace(&moved, 0.0),
            ["clip", "clip", "fill(1)", "pop", "pop"]
        );
        assert!(
            render(&moved, 0.0, 1.0).contains(&Event::PushClip(Rect::new(50.0, 0.0, 150.0, 100.0)))
        );
    }

    #[test]
    fn translucent_precomposition_clips_with_its_opacity_layer() {
        let child = |ind| shape_layer(ind, json!([rect(0.0, 0.0, 100.0, 100.0), fill(100.0)]));
        let instance = with(
            with(precomp_layer(1, "comp"), json!({ "w": 40, "h": 30 })),
            opacity(50.0),
        );
        let composition = composition_with_assets(
            json!([precomp("comp", json!([child(1), child(2)]))]),
            json!([instance]),
        );
        let events = render(&composition, 0.0, 1.0);
        assert_eq!(
            trace(&composition, 0.0),
            ["clip", "layer(0.5)", "fill(1)", "fill(1)", "pop", "pop"]
        );
        assert!(matches!(
            &events[2],
            Event::PushLayer { bounds, .. } if *bounds == Rect::new(0.0, 0.0, 40.0, 30.0)
        ));
    }

    fn positioned(layer: serde_json::Value, x: f64, y: f64) -> serde_json::Value {
        let mut layer = layer;
        layer["ks"]["p"] = json!({ "a": 0, "k": [x, y] });
        layer
    }

    #[test]
    fn solid_layer_draws_a_rectangle_in_the_layer_transform() {
        let composition = composition(json!([positioned(
            solid_layer(1, 40.0, 30.0, "#ff8000"),
            10.0,
            20.0
        )]));
        assert_eq!(
            render(&composition, 0.0, 1.0),
            [
                Event::PushClip(Rect::new(0.0, 0.0, 100.0, 100.0)),
                Event::BeginLayer("layer 1".into()),
                Event::Draw {
                    stroked: false,
                    alpha: 1.0,
                    bounds: Rect::new(10.0, 20.0, 50.0, 50.0),
                },
                Event::EndLayer,
                Event::Pop,
            ]
        );
    }

    #[test]
    fn translucent_solid_layer_fades_its_brush() {
        // A solid is a single draw call, so its opacity needs no layer.
        let solid = with(solid_layer(1, 40.0, 30.0, "#ff8000"), opacity(50.0));
        assert_eq!(
            trace(&composition(json!([solid])), 0.0),
            ["clip", "fill(0.5)", "pop"]
        );
        // The alpha of the color itself is kept as well.
        let translucent_color = with(solid_layer(1, 40.0, 30.0, "#ff800080"), opacity(50.0));
        let events = render(&composition(json!([translucent_color])), 0.0, 1.0);
        let Event::Draw { alpha, .. } = &events[2] else {
            panic!("expected a draw call, got {events:?}");
        };
        assert!((alpha - 0.25).abs() < 0.005, "{alpha}");
    }

    #[test]
    fn solid_layer_can_be_a_track_matte_source_and_a_matted_layer() {
        let shape = |ind| shape_layer(ind, json!([rect(0.0, 0.0, 20.0, 20.0), fill(100.0)]));
        let matte = [
            "clip",
            "layer(1)",
            "fill(1)",
            "layer(1, SrcIn)",
            "fill(1)",
            "pop",
            "pop",
            "pop",
        ];
        let bounds = |events: &[Event]| draw_bounds(events);

        // The solid is the matte of the shape layer below it.
        let solid_source = composition(json!([
            with(solid_layer(1, 40.0, 30.0, "#ffffff"), json!({ "td": 1 })),
            with(shape(2), json!({ "tt": 1 })),
        ]));
        assert_eq!(trace(&solid_source, 0.0), matte);
        assert_eq!(
            bounds(&render(&solid_source, 0.0, 1.0)),
            [
                Rect::new(0.0, 0.0, 40.0, 30.0),
                Rect::new(0.0, 0.0, 20.0, 20.0)
            ]
        );

        // The solid is matted by the shape layer above it.
        let solid_matted = composition(json!([
            with(shape(1), json!({ "td": 1 })),
            with(solid_layer(2, 40.0, 30.0, "#ffffff"), json!({ "tt": 1 })),
        ]));
        assert_eq!(trace(&solid_matted, 0.0), matte);
        assert_eq!(
            bounds(&render(&solid_matted, 0.0, 1.0)),
            [
                Rect::new(0.0, 0.0, 20.0, 20.0),
                Rect::new(0.0, 0.0, 40.0, 30.0)
            ]
        );
    }

    #[test]
    fn solid_layer_takes_masks_blend_mode_range_and_parenting() {
        let mask = json!({
            "mode": "a",
            "o": { "a": 0, "k": 100 },
            "pt": {
                "a": 0,
                "k": {
                    "c": true,
                    "v": [[0, 0], [20, 0], [20, 20], [0, 20]],
                    "i": [[0, 0], [0, 0], [0, 0], [0, 0]],
                    "o": [[0, 0], [0, 0], [0, 0], [0, 0]],
                },
            },
        });
        let masked = with(
            solid_layer(1, 40.0, 30.0, "#ff8000"),
            json!({ "masksProperties": [mask] }),
        );
        assert_eq!(
            trace(&composition(json!([masked])), 0.0),
            ["clip", "clip", "fill(1)", "pop", "pop"]
        );

        let blended = with(solid_layer(1, 40.0, 30.0, "#ff8000"), json!({ "bm": 1 }));
        assert_eq!(
            trace(&composition(json!([blended])), 0.0),
            ["layer(1)", "layer(1, Multiply)", "fill(1)", "pop", "pop"]
        );

        let ranged = with(
            solid_layer(1, 40.0, 30.0, "#ff8000"),
            json!({ "ip": 10, "op": 20 }),
        );
        let ranged = composition(json!([ranged]));
        assert_eq!(trace(&ranged, 9.0), ["clip", "pop"]);
        assert_eq!(trace(&ranged, 10.0), ["clip", "fill(1)", "pop"]);
        assert_eq!(trace(&ranged, 20.0), ["clip", "pop"]);

        // A solid follows its parent, and a layer parented to a solid follows the solid.
        let family = composition(json!([
            with(
                shape_layer(1, json!([rect(0.0, 0.0, 5.0, 5.0), fill(100.0)])),
                json!({ "parent": 2 }),
            ),
            with(
                positioned(solid_layer(2, 10.0, 10.0, "#ff8000"), 1.0, 2.0),
                json!({ "parent": 3 }),
            ),
            positioned(layer(3, 3), 30.0, 40.0),
        ]));
        assert_eq!(
            draw_bounds(&render(&family, 0.0, 1.0)),
            [
                Rect::new(31.0, 42.0, 41.0, 52.0),
                Rect::new(31.0, 42.0, 36.0, 47.0)
            ]
        );
    }
}
