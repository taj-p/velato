// Copyright 2026 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::Composition;
use super::model::{Content, Layer, LayerReference, PathCheck, Shape, TransformComponents};
use super::render::{check_fixed_shapes, count_shapes, evaluate_paths};
use crate::limits::{Limit, Limits, ShapeCheck, ShapeCost, ShapeError};
use kurbo::{Affine, PathEl, Point};
use peniko::BlendMode;

/// Nested layer indices, not Lottie IDs; valid only for this composition layout.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OccurrencePath(pub Vec<usize>);

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvaluationError {
    NonFiniteTime,
    InvalidStretch(OccurrencePath),
    InvalidTransform(OccurrencePath),
    InvalidParent(OccurrencePath),
    UnresolvedParent {
        path: OccurrencePath,
        id: usize,
    },
    ParentCycle(OccurrencePath),
    InvalidMatte(OccurrencePath),
    UnresolvedMatte {
        path: OccurrencePath,
        id: usize,
    },
    MatteCycle(OccurrencePath),
    MissingAsset(String),
    PrecompCycle(String),
    /// The animation asks for more at this frame than a [limit](crate::limits) allows.
    LimitExceeded {
        /// The limit that is exceeded.
        limit: Limit,
        /// The most that the limit allows.
        maximum: usize,
    },
    /// A shape or a mask of this layer is not finite at this frame: a rectangle or ellipse
    /// has a size that is not finite, or a path of the layer has a coordinate that is not.
    ///
    /// A size is checked for every layer, and a path for the layers that are rendered at
    /// the frame.
    InvalidShape(OccurrencePath),
}

impl std::fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnresolvedParent { path, id } => write!(
                f,
                "No parent layer with ind {id} in the composition containing {path:?}"
            ),
            Self::UnresolvedMatte { path, id } => write!(
                f,
                "No matte layer with ind {id} in the composition containing {path:?}"
            ),
            Self::LimitExceeded { limit, maximum } => write!(
                f,
                "Lottie evaluation failed: the animation has more than {maximum} {limit}"
            ),
            _ => write!(f, "Lottie evaluation failed: {self:?}"),
        }
    }
}
impl std::error::Error for EvaluationError {}

/// One layer instance at a frame. Coordinates are pixels, with Y pointing down.
#[derive(Debug)]
pub struct EvaluatedLayer<'a> {
    pub path: OccurrencePath,
    pub layer: &'a Layer,
    /// Transform-parent index in the containing composition, not a Lottie ID.
    pub parent: Option<usize>,
    /// Matte mode and source index in the containing composition, not a Lottie ID.
    pub matte: Option<(BlendMode, usize)>,
    /// Frame in the containing composition.
    pub frame: f64,
    /// Frame is at or after the layer's in-point and before its out-point.
    pub in_range: bool,
    /// Active in the normal render tree, excluding hidden layers, matte sources, and inactive precomps.
    /// Does not account for opacity or clipping.
    pub visible: bool,
    /// None for matrix-only transforms, whose original components cannot be recovered.
    pub authored_components: Option<TransformComponents>,
    /// Layer-to-parent transform, or layer-to-composition if parentless.
    pub local_transform: Affine,
    /// Places layer coordinates in the main composition, before renderer scaling or positioning.
    pub full_transform: Affine,
    /// Layer's own opacity (1 = fully opaque), excluding parents and enclosing precomps.
    pub opacity: f64,
    /// Layers inside this precomp instance, not layers linked by transform parenting.
    pub children: Vec<EvaluatedLayer<'a>>,
}

impl EvaluatedLayer<'_> {
    /// Returns this layer's paths after moving, scaling, rotating, trimming, and repeating them.
    ///
    /// Coordinates are pixels in the main animation. Hidden and transparent paths are included,
    /// but colors, stroke thickness, masks, and effects are not applied.
    ///
    /// The visible paths are the ones that are drawn. A trim that covers hidden paths as well
    /// gives them their share of its range, here and when rendering.
    ///
    /// Returns nothing for non-shape layers; for nested animations, query their child layers.
    pub fn evaluated_paths(&self) -> Vec<kurbo::BezPath> {
        match &self.layer.content {
            Content::Shape(shapes) => evaluate_paths(shapes, self.full_transform, self.frame),
            _ => Vec::new(),
        }
    }

    /// Anchor in main-composition pixels; None for matrix-only transforms.
    pub fn anchor(&self) -> Option<Point> {
        self.authored_components
            .map(|c| self.full_transform * c.anchor)
    }

    /// Original shape outlines at this frame, before trimming, repetition, masks, fills, strokes,
    /// or effects. These may differ from the final visible shapes.
    pub fn authored_paths(&self) -> Vec<AuthoredPath> {
        fn collect(
            shapes: &[Shape],
            frame: f64,
            transform: Affine,
            indices: &mut Vec<usize>,
            paths: &mut Vec<AuthoredPath>,
        ) {
            for (index, shape) in shapes.iter().enumerate() {
                indices.push(index);
                let shape = match shape {
                    Shape::Hidden(shape) => shape.as_ref(),
                    shape => shape,
                };
                match shape {
                    Shape::Geometry(geometry) => {
                        let mut elements = Vec::new();
                        geometry.evaluate(frame, &mut elements);
                        paths.push(AuthoredPath {
                            indices: indices.clone(),
                            elements,
                            transform,
                        });
                    }
                    Shape::Group(children, group) => {
                        let local = group.as_ref().map_or(Affine::IDENTITY, |g| {
                            g.transform.evaluate(frame).into_owned()
                        });
                        collect(children, frame, transform * local, indices, paths);
                    }
                    _ => {}
                }
                indices.pop();
            }
        }
        let mut paths = Vec::new();
        if let Content::Shape(shapes) = &self.layer.content {
            collect(
                shapes,
                self.frame,
                self.full_transform,
                &mut Vec::new(),
                &mut paths,
            );
        }
        paths
    }
}

/// A shape outline before visual modifications, as returned by [`EvaluatedLayer::authored_paths`].
#[derive(Clone, Debug)]
pub struct AuthoredPath {
    /// Runtime array positions through nested groups, not Lottie IDs or JSON positions.
    ///
    /// E.g. `[2, 0, 3]` selects group 2, then group 0 inside it, then shape 3.
    pub indices: Vec<usize>,
    pub elements: Vec<PathEl>,
    pub transform: Affine,
}

#[derive(Debug)]
pub struct EvaluatedComposition<'a> {
    pub(crate) composition: &'a Composition,
    pub layers: Vec<EvaluatedLayer<'a>>,
}

impl EvaluatedComposition<'_> {
    pub fn get(&self, path: &OccurrencePath) -> Option<&EvaluatedLayer<'_>> {
        let mut layers = &self.layers;
        let mut result = None;
        for index in &path.0 {
            let layer = layers.get(*index)?;
            result = Some(layer);
            layers = &layer.children;
        }
        result
    }
}

impl Composition {
    /// Samples layers at a frame without drawing; returns errors for invalid data.
    ///
    /// Fails if the animation asks for more at this frame than the [limits](crate::limits)
    /// allow. What it returns is within them.
    ///
    /// Fails with [`EvaluationError::InvalidShape`] if a layer that is rendered at this frame
    /// has a shape or a mask whose path is not finite: a path of the file itself, before the
    /// transform of a group, a repeater or a layer is applied to it. No file can write a
    /// number that is not finite, but a vertex and its tangent can add up to more than a float
    /// holds, as can an easing that overshoots.
    pub fn evaluate(&self, frame: f64) -> Result<EvaluatedComposition<'_>, EvaluationError> {
        self.evaluate_within(frame, &Limits::DEFAULT)
    }

    pub(crate) fn evaluate_within<'a>(
        &'a self,
        frame: f64,
        limits: &'a Limits,
    ) -> Result<EvaluatedComposition<'a>, EvaluationError> {
        let mut evaluation = Evaluation {
            animation: self,
            limits,
            layers: 0,
            passes: ShapeCost::default(),
            counted: ShapeCost::default(),
            prefix: Vec::new(),
            assets: Vec::new(),
        };
        let (layers, rendered) =
            evaluation.evaluate_layers(&self.layers, frame, Affine::IDENTITY, true, true)?;
        // Shapes that were counted for layers that are not rendered count with the ones that
        // are.
        let mut shapes = rendered.shapes;
        shapes
            .add(evaluation.counted, limits)
            .map_err(|limit| EvaluationError::LimitExceeded {
                limit,
                maximum: limits.maximum(limit),
            })?;
        // The paths are checked last, when it is known that the frame is within the limits,
        // which bound what evaluating the paths of the layers that are rendered takes.
        check_rendered_paths(&layers, &mut Vec::new())?;
        Ok(EvaluatedComposition {
            composition: self,
            layers,
        })
    }

    /// Checks what can be known about the limits without a frame, and records the count of
    /// the shapes that never change in number.
    pub(crate) fn check_limits(&mut self, limits: &Limits) -> Result<(), crate::Error> {
        let limit_exceeded = |limit| crate::Error::LimitExceeded {
            limit,
            maximum: limits.maximum(limit),
        };
        let exceeded = |error| match error {
            ShapeError::Limit(limit) => Some(limit_exceeded(limit)),
            // A fixed size is a number from the file, which is finite. A size that is animated
            // may be finite at another frame, so it is left to the evaluation of a frame.
            ShapeError::NonFiniteSize => None,
        };
        let mut scratch = Vec::new();
        for layer in self.assets.values_mut().flatten().chain(&mut self.layers) {
            let Content::Shape(shapes) = &layer.content else {
                layer.path_check = path_check(layer, &mut scratch);
                continue;
            };
            let check = check_fixed_shapes(shapes, limits, 0).and_then(|fixed| {
                Ok(if fixed {
                    ShapeCheck::Never(count_shapes(shapes, 0.0, limits)?)
                } else {
                    ShapeCheck::EveryFrame
                })
            });
            layer.shape_check = match check.map_err(exceeded) {
                Ok(check) => check,
                Err(None) => ShapeCheck::EveryFrame,
                Err(Some(error)) => return Err(error),
            };
            // The shapes are within the limits on a single shape here, which bounds what
            // evaluating their paths takes.
            layer.path_check = path_check(layer, &mut scratch);
        }
        let mut structure = Structure {
            animation: self,
            limits,
            layers: 0,
            assets: Vec::new(),
        };
        structure
            .visit_layers(&self.layers, 1)
            .map_err(limit_exceeded)
    }
}

/// Returns when the paths of a layer have to be checked for coordinates that are not finite.
fn path_check(layer: &Layer, scratch: &mut Vec<PathEl>) -> PathCheck {
    if layer.has_fixed_paths() && layer.has_finite_paths(0.0, scratch) {
        PathCheck::Never
    } else {
        PathCheck::EveryFrame
    }
}

/// Fails if a layer that is rendered at this frame has a shape or a mask with a path that is
/// not finite.
///
/// The layers that are rendered are those that [`Renderer`](crate::Renderer) draws, or would
/// draw if they were not transparent: the layers of the composition and of the
/// precompositions it shows that are within their in and out points and not hidden, and the
/// track matte sources of those. The limits bound the paths of these layers together, so
/// evaluating them all once more takes no more than drawing them does.
///
/// The paths of other layers can still be queried, and are not checked.
fn check_rendered_paths(
    layers: &[EvaluatedLayer<'_>],
    scratch: &mut Vec<PathEl>,
) -> Result<(), EvaluationError> {
    let mut rendered: Vec<_> = layers
        .iter()
        .map(|node| node.in_range && !node.layer.hidden && !node.layer.is_matte_source)
        .collect();
    // A matte source is rendered for a layer that is rendered. Sources come before the layers
    // that they matte in this order, so going through it backwards reaches the source of a
    // source in one pass. A cycle has failed the evaluation before.
    let matte_order = dependency_order(layers.len(), |index| {
        layers[index].matte.map(|(_, source)| source)
    })
    .unwrap_or_default();
    for &index in matte_order.iter().rev() {
        if rendered[index]
            && let Some((_, source)) = layers[index].matte
        {
            rendered[source] = layers[source].in_range;
        }
    }
    for (node, _) in layers
        .iter()
        .zip(rendered)
        .filter(|(_, rendered)| *rendered)
    {
        if node.layer.path_check == PathCheck::EveryFrame
            && !node.layer.has_finite_paths(node.frame, scratch)
        {
            return Err(EvaluationError::InvalidShape(node.path.clone()));
        }
        check_rendered_paths(&node.children, scratch)?;
    }
    Ok(())
}

/// Returns the index of the layer that mattes a layer, if it is one of `layers`.
fn matte_source(layers: &[Layer], index: usize) -> Option<usize> {
    match layers[index].matte {
        Some((_, LayerReference::Resolved(source))) if source < layers.len() => Some(source),
        _ => None,
    }
}

/// A walk over the layers of a composition in the way that rendering walks them, with every
/// layer taken to be active. It finds out whether the composition can be within the limits on
/// layers at any frame.
struct Structure<'a> {
    animation: &'a Composition,
    limits: &'a Limits,
    /// Layers visited.
    layers: usize,
    /// Precompositions that hold the layers being visited.
    assets: Vec<&'a str>,
}

impl<'a> Structure<'a> {
    fn visit_layers(&mut self, layers: &'a [Layer], depth: usize) -> Result<(), Limit> {
        // Evaluation fails for layers that matte each other in a cycle. That is its error to
        // report, so here such layers count as having no mattes.
        let mattes = dependency_order(layers.len(), |index| matte_source(layers, index)).is_ok();
        // A layer that mattes another is rendered for that layer only.
        let mut is_source = vec![false; layers.len()];
        if mattes {
            for index in 0..layers.len() {
                if let Some(source) = matte_source(layers, index) {
                    is_source[source] = true;
                }
            }
        }
        for (index, is_source) in is_source.into_iter().enumerate() {
            if !is_source {
                self.visit_layer(layers, index, mattes, depth)?;
            }
        }
        Ok(())
    }

    fn visit_layer(
        &mut self,
        layers: &'a [Layer],
        index: usize,
        mattes: bool,
        depth: usize,
    ) -> Result<(), Limit> {
        if depth > self.limits.nesting_depth {
            return Err(Limit::NestingDepth);
        }
        self.layers += 1;
        if self.layers > self.limits.layer_instances {
            return Err(Limit::LayerInstances);
        }
        // A matte source is rendered once more for each layer that it mattes.
        if mattes && let Some(source) = matte_source(layers, index) {
            self.visit_layer(layers, source, mattes, depth + 1)?;
        }
        // Likewise, evaluation reports a precomposition that holds itself, and one that is
        // missing.
        if let Content::Instance { name, .. } = &layers[index].content
            && !self.assets.contains(&name.as_str())
            && let Some(children) = self.animation.assets.get(name)
        {
            self.assets.push(name);
            self.visit_layers(children, depth + 1)?;
            self.assets.pop();
        }
        Ok(())
    }
}

/// What rendering a layer, or a set of layers, takes at a frame.
#[derive(Clone, Copy, Debug, Default)]
struct RenderCost {
    /// Layers rendered. A matte source counts once for each layer that it mattes.
    layers: usize,
    /// What the shapes of those layers take.
    shapes: ShapeCost,
    /// Levels of nesting below, through precompositions and mattes.
    depth: usize,
}

impl RenderCost {
    /// Adds the cost of something that is rendered as a part of this.
    fn nest(&mut self, inner: Self, limits: &Limits) -> Result<(), Limit> {
        self.layers = self.layers.saturating_add(inner.layers);
        self.depth = self.depth.max(inner.depth + 1);
        self.shapes.add(inner.shapes, limits)?;
        self.check(limits)
    }

    /// Adds the cost of something that is rendered next to this.
    fn join(&mut self, other: Self, limits: &Limits) -> Result<(), Limit> {
        self.layers = self.layers.saturating_add(other.layers);
        self.depth = self.depth.max(other.depth);
        self.shapes.add(other.shapes, limits)?;
        self.check(limits)
    }

    fn check(&self, limits: &Limits) -> Result<(), Limit> {
        if self.layers > limits.layer_instances {
            Err(Limit::LayerInstances)
        } else if self.depth > limits.nesting_depth {
            Err(Limit::NestingDepth)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum VisitState {
    Unvisited,
    Visiting,
    Visited,
}

fn dependency_order(
    count: usize,
    dependency: impl Fn(usize) -> Option<usize>,
) -> Result<Vec<usize>, usize> {
    let mut states = vec![VisitState::Unvisited; count];
    let mut stack = Vec::new();
    let mut order = Vec::with_capacity(count);
    for start in 0..count {
        let mut current = Some(start);
        while let Some(index) = current {
            match states[index] {
                VisitState::Visited => break,
                VisitState::Visiting => return Err(index),
                VisitState::Unvisited => {}
            }
            states[index] = VisitState::Visiting;
            stack.push(index);
            current = dependency(index);
        }
        while let Some(index) = stack.pop() {
            states[index] = VisitState::Visited;
            order.push(index);
        }
    }
    Ok(order)
}

/// The evaluation of a composition at a frame.
struct Evaluation<'a> {
    animation: &'a Composition,
    limits: &'a Limits,
    /// Layers evaluated so far.
    layers: usize,
    /// What the shapes take that were counted at this frame, for any layer. This stops an
    /// evaluation that counts more than a frame may take, before it has counted it all.
    passes: ShapeCost,
    /// The part of `passes` for layers that are not rendered in their own right. It counts
    /// toward the frame with what is rendered.
    counted: ShapeCost,
    /// Indices of the precomposition layers that hold the layers being evaluated.
    prefix: Vec<usize>,
    /// Their precompositions.
    assets: Vec<String>,
}

impl<'a> Evaluation<'a> {
    /// Evaluates the layers of the main composition or of a precomposition, and works out what
    /// rendering them takes.
    ///
    /// The number of layers and their nesting are checked before anything is evaluated for
    /// them, and the shapes of every layer are counted, so that everything this returns can be
    /// rendered and queried within the limits.
    ///
    /// `active` is whether the frame is within the in and out points of every precomposition
    /// layer that holds these layers. Layers that are not active are not rendered.
    fn evaluate_layers(
        &mut self,
        layers: &'a [Layer],
        frame: f64,
        outer: Affine,
        visible: bool,
        active: bool,
    ) -> Result<(Vec<EvaluatedLayer<'a>>, RenderCost), EvaluationError> {
        if !frame.is_finite() {
            return Err(EvaluationError::NonFiniteTime);
        }
        let limits = self.limits;
        // The layers being evaluated are one level below the layers in `prefix`.
        let exceeded = |limit| EvaluationError::LimitExceeded {
            limit,
            maximum: limits.maximum(limit),
        };
        if self.prefix.len() >= limits.nesting_depth {
            return Err(exceeded(Limit::NestingDepth));
        }
        self.layers = self.layers.saturating_add(layers.len());
        if self.layers > limits.layer_instances {
            return Err(exceeded(Limit::LayerInstances));
        }
        let mut result = Vec::with_capacity(layers.len());
        for (index, layer) in layers.iter().enumerate() {
            let mut indices = self.prefix.clone();
            indices.push(index);
            let path = OccurrencePath(indices);
            let parent = match layer.parent {
                Some(LayerReference::Resolved(index)) => {
                    if index >= layers.len() {
                        return Err(EvaluationError::InvalidParent(path));
                    }
                    Some(index)
                }
                Some(LayerReference::Unresolved(id)) => {
                    return Err(EvaluationError::UnresolvedParent { path, id });
                }
                None => None,
            };
            let matte = match layer.matte {
                Some((mode, LayerReference::Resolved(index))) => {
                    if index >= layers.len() {
                        return Err(EvaluationError::InvalidMatte(path));
                    }
                    Some((mode, index))
                }
                Some((_, LayerReference::Unresolved(id))) => {
                    return Err(EvaluationError::UnresolvedMatte { path, id });
                }
                None => None,
            };
            if matches!(layer.content, Content::Instance { .. })
                && (!layer.stretch.is_finite() || layer.stretch == 0.0)
            {
                return Err(EvaluationError::InvalidStretch(path));
            }
            let authored_components = layer.transform.components(frame);
            let local_transform = authored_components.map_or_else(
                || layer.transform.evaluate(frame).into_owned(),
                TransformComponents::matrix,
            );
            if !local_transform.is_finite() {
                return Err(EvaluationError::InvalidTransform(path));
            }
            let in_range = layer.frames.contains(&frame);
            result.push(EvaluatedLayer {
                path,
                layer,
                parent,
                matte,
                frame,
                in_range,
                visible: visible && in_range && !layer.hidden && !layer.is_matte_source,
                authored_components,
                local_transform,
                full_transform: local_transform,
                opacity: layer.opacity.evaluate(frame) / 100.0,
                children: Vec::new(),
            });
        }
        let parent_order = dependency_order(result.len(), |index| result[index].parent)
            .map_err(|index| EvaluationError::ParentCycle(result[index].path.clone()))?;
        for index in parent_order {
            let parent_transform = result[index]
                .parent
                .map_or(outer, |parent| result[parent].full_transform);
            let transform = parent_transform * result[index].local_transform;
            if !transform.is_finite() {
                return Err(EvaluationError::InvalidTransform(
                    result[index].path.clone(),
                ));
            }
            result[index].full_transform = transform;
        }
        let matte_order = dependency_order(result.len(), |index| {
            result[index].matte.map(|(_, source)| source)
        })
        .map_err(|index| EvaluationError::MatteCycle(result[index].path.clone()))?;
        // What the content of each layer takes to render: its shapes, or the layers of its
        // precomposition.
        let mut contents = vec![RenderCost::default(); result.len()];
        // Whether the shapes of each layer were counted at this frame.
        let mut counted = vec![false; result.len()];
        for (index, (node, content)) in result.iter_mut().zip(&mut contents).enumerate() {
            match &node.layer.content {
                Content::Instance { name, time_remap } => {
                    // Keep existing tm sampling; remap only child content.
                    let child_frame = if let Some(tm) = time_remap {
                        let sample_frame = frame / node.layer.stretch - node.layer.start_frame;
                        if !sample_frame.is_finite() {
                            return Err(EvaluationError::NonFiniteTime);
                        }
                        tm.evaluate(sample_frame) * self.animation.frame_rate
                    } else {
                        frame / node.layer.stretch - node.layer.start_frame / node.layer.stretch
                    };
                    if !child_frame.is_finite() {
                        return Err(EvaluationError::NonFiniteTime);
                    }
                    if self.assets.contains(name) {
                        return Err(EvaluationError::PrecompCycle(name.clone()));
                    }
                    let children = self
                        .animation
                        .assets
                        .get(name)
                        .ok_or_else(|| EvaluationError::MissingAsset(name.clone()))?;
                    self.assets.push(name.clone());
                    self.prefix.push(*node.path.0.last().unwrap());
                    (node.children, *content) = self.evaluate_layers(
                        children,
                        child_frame,
                        node.full_transform,
                        node.visible,
                        active && node.in_range,
                    )?;
                    self.prefix.pop();
                    self.assets.pop();
                }
                Content::Image { asset_id } => {
                    if !self.animation.images.contains_key(asset_id) {
                        return Err(EvaluationError::MissingAsset(asset_id.clone()));
                    }
                }
                Content::Shape(shapes) => {
                    content.shapes = match node.layer.shape_check {
                        ShapeCheck::Never(cost) => cost,
                        ShapeCheck::EveryFrame => {
                            let cost =
                                count_shapes(shapes, frame, limits).map_err(
                                    |error| match error {
                                        ShapeError::Limit(limit) => exceeded(limit),
                                        ShapeError::NonFiniteSize => {
                                            EvaluationError::InvalidShape(node.path.clone())
                                        }
                                    },
                                )?;
                            counted[index] = true;
                            self.passes.add(cost, limits).map_err(exceeded)?;
                            cost
                        }
                    };
                }
                Content::None => {}
            }
        }
        // What each layer takes to render. A layer renders its matte source, so sources come
        // before the layers they matte.
        let mut rendered = vec![RenderCost::default(); result.len()];
        let mut total = RenderCost::default();
        for index in matte_order {
            let node = &result[index];
            // The paths of a layer that is not rendered can still be queried. Where counting
            // them took a pass over the shapes at this frame, that pass is work of the frame,
            // and is held to the limits like the rest.
            let mut count_unrendered = || {
                if counted[index] {
                    self.counted
                        .add(contents[index].shapes, limits)
                        .map_err(exceeded)?;
                }
                Ok(())
            };
            if !(active && node.in_range) {
                count_unrendered()?;
                continue;
            }
            // Masks and effects are applied each time the layer is rendered.
            let mut cost = RenderCost {
                layers: 1,
                shapes: ShapeCost {
                    instances: node.layer.masks.len() + node.layer.effects.len(),
                    elements: node
                        .layer
                        .masks
                        .iter()
                        .map(|mask| {
                            mask.geometry
                                .max_elements(frame, limits)
                                .unwrap_or(0)
                                .max(1)
                        })
                        .fold(0, usize::saturating_add),
                    trimmed: 0,
                },
                depth: 0,
            };
            // The content of a layer and its matte source are one level below the layer.
            let source = node.matte.map(|(_, source)| rendered[source]);
            for inner in [Some(contents[index]), source].into_iter().flatten() {
                cost.nest(inner, limits).map_err(exceeded)?;
            }
            rendered[index] = cost;
            if node.layer.hidden || node.layer.is_matte_source {
                count_unrendered()?;
            } else {
                total.join(cost, limits).map_err(exceeded)?;
            }
        }
        Ok((result, total))
    }
}
