// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::builders::{setup_layer_base, setup_precomp_layer, setup_shape_layer};
use super::defaults::{
    FLOAT_VALUE_ONE_HUNDRED, FLOAT_VALUE_ZERO, MULTIDIM_ONE_HUNDRED, POSITION_ZERO,
};
use crate::import::builders::LayerSetupParams;
use crate::runtime::model::animated::{self, Position};
use crate::runtime::model::{
    self, Content, Draw, Easing, EasingHandle, GroupTransform, Layer, RepeaterComposite,
    SpatialKeyframe, SpatialPosition, SplineToPath, Time, Tween, Value,
};
use crate::runtime::{self};
use crate::schema::animated_properties::keyframe::Keyframe;
use crate::schema::animated_properties::keyframe_bezier_handle::{
    KeyframeBezierHandle, KeyframeComponent,
};
use crate::schema::animated_properties::multi_dimensional::MultiDimensional;
use crate::schema::animated_properties::split_vector::SplitVector;
use crate::schema::animated_properties::value::FloatValue;
use crate::schema::constants::gradient_type::GradientType;
use crate::schema::helpers::transform::{AnyTransformP, AnyTransformR};
use crate::schema::{helpers::int_boolean::BoolInt, layers::AnyLayer};
use crate::{Composition, schema};
use kurbo::{Cap, Join, Point, Size, Vec2};
use peniko::{BlendMode, Color, Compose, Mix};
use std::collections::HashMap;

fn process_layers(source_layers: &[AnyLayer], idmap: &mut HashMap<usize, usize>) -> Vec<Layer> {
    idmap.clear();

    let mut converted: Vec<(Layer, usize, Option<BlendMode>, Option<usize>)> = vec![];

    for layer in source_layers {
        if let Some((layer, id, matte_mode, matte_layer_index)) = conv_layer(layer) {
            let index = converted.len();
            idmap.insert(id, index);
            converted.push((layer, id, matte_mode, matte_layer_index));
        }
    }

    let resolve = |id| {
        idmap.get(&id).copied().map_or(
            model::LayerReference::Unresolved(id),
            model::LayerReference::Resolved,
        )
    };

    let mut layers: Vec<Layer> = Vec::with_capacity(converted.len());

    for (idx, (mut layer, _id, matte_mode, explicit_matte_index)) in
        converted.into_iter().enumerate()
    {
        if let Some(model::LayerReference::Unresolved(id)) = layer.parent {
            layer.parent = Some(resolve(id));
        }

        if let Some(matte_mode) = matte_mode.filter(|mode| *mode != Mix::Normal.into()) {
            let matte_layer_idx = if let Some(explicit_idx) = explicit_matte_index {
                Some(resolve(explicit_idx))
            } else {
                idx.checked_sub(1).map(model::LayerReference::Resolved)
            };

            if let Some(matte_idx) = matte_layer_idx {
                layer.matte = Some((matte_mode, matte_idx));
            }
        }

        layers.push(layer);
    }

    // A layer's matte source is selected by `tp`, or defaults to the preceding layer when `tt`
    // enables a matte. The source need not be marked `td` for this selection. Treat resolved
    // sources as matte-only to avoid drawing their content separately.
    //
    // This matches ThorVG (also used by dotLottie-web), which marks resolved sources as matte-only
    // regardless of `td`, the Lottie docs do not state this explicitly.
    for index in 0..layers.len() {
        if let Some((_, model::LayerReference::Resolved(source))) = layers[index].matte {
            layers[source].is_matte_source = true;
        }
    }

    layers
}

pub fn conv_animation(source: schema::Animation) -> Result<Composition, crate::Error> {
    if !source.frame_rate.is_finite() || source.frame_rate <= 0.0 {
        return Err(crate::Error::InvalidFrameRate(source.frame_rate));
    }
    let mut target = Composition {
        frames: source.in_point..source.out_point,
        frame_rate: source.frame_rate,
        width: source.width,
        height: source.height,
        assets: Default::default(),
        images: Default::default(),
        layers: Default::default(),
    };

    // Collect assets and layers
    let mut idmap: HashMap<usize, usize> = HashMap::default();

    if let Some(assets) = source.assets {
        for asset in assets {
            match asset {
                schema::assets::AnyAsset::Precomposition(precomp) => {
                    let layers = process_layers(&precomp.composition.layers, &mut idmap);
                    target.assets.insert(precomp.asset.id.clone(), layers);
                }
                schema::assets::AnyAsset::Image(image) => {
                    let file = image.file_asset;
                    let id = file.asset.id;
                    target.images.insert(
                        id.clone(),
                        model::ImageAsset {
                            id,
                            width: image.width,
                            height: image.height,
                            directory: file.dir,
                            file_name: file.file_name,
                            embedded: file.embedded == Some(BoolInt::True),
                        },
                    );
                }
            }
        }
    }

    target.layers = process_layers(&source.composition.layers, &mut idmap);

    Ok(target)
}

pub fn conv_layer(source: &AnyLayer) -> Option<(Layer, usize, Option<BlendMode>, Option<usize>)> {
    let mut layer = Layer {
        hidden: is_layer_hidden(source),
        ..Layer::default()
    };

    let params = match source {
        AnyLayer::Null(null_layer) => setup_layer_base(&null_layer.visual_layer, &mut layer),
        AnyLayer::Precomposition(precomp_layer) => {
            let params = setup_precomp_layer(precomp_layer, &mut layer);
            let name = precomp_layer.ref_id.clone();
            let time_remap = precomp_layer.time_remap.as_ref().map(conv_scalar);
            layer.content = Content::Instance { name, time_remap };
            params
        }
        AnyLayer::Shape(shape_layer) => {
            let params = setup_shape_layer(shape_layer, &mut layer);
            let mut shapes = vec![];
            for shape in &shape_layer.shapes {
                if let Some(shape) = conv_shape(shape) {
                    shapes.push(shape);
                }
            }
            layer.content = Content::Shape(shapes);
            params
        }
        AnyLayer::Solid(solid_color_layer) => {
            setup_layer_base(&solid_color_layer.visual_layer, &mut layer)
        }
        AnyLayer::Image(image_layer) => {
            let params = setup_layer_base(&image_layer.visual_layer, &mut layer);
            layer.content = Content::Image {
                asset_id: image_layer.ref_id.clone(),
            };
            params
        }
    };

    {
        let visual = match source {
            AnyLayer::Null(l) => &l.visual_layer,
            AnyLayer::Precomposition(l) => &l.visual_layer,
            AnyLayer::Shape(l) => &l.visual_layer,
            AnyLayer::Solid(l) => &l.visual_layer,
            AnyLayer::Image(l) => &l.visual_layer,
        };
        for effect in visual.effects.iter().flatten() {
            if effect.get("en").is_some_and(|enabled| {
                enabled.as_bool() == Some(false) || enabled.as_u64() == Some(0)
            }) {
                continue;
            }
            if let Some(effect) = conv_layer_effect(effect) {
                layer.effects.push(effect);
            } else {
                layer.unsupported_effects.push(model::UnsupportedEffect {
                    name: effect
                        .get("nm")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    effect_type: effect.get("ty").and_then(serde_json::Value::as_u64),
                });
            }
        }
    }

    let LayerSetupParams {
        layer_index: id,
        matte_mode,
        matte_layer_index,
    } = params;

    Some((layer, id, matte_mode, matte_layer_index))
}

fn conv_layer_effect(effect: &serde_json::Value) -> Option<model::LayerEffect> {
    let parameters = effect.get("ef")?.as_array()?;
    let scalar = |index: usize| {
        let value = parameters.get(index)?.get("v")?;
        let value =
            serde_json::from_value::<schema::animated_properties::value::FloatValue>(value.clone())
                .ok()?;
        Some(conv_scalar(&value))
    };
    let color = |index: usize| {
        let value = parameters.get(index)?.get("v")?;
        let value = serde_json::from_value::<schema::animated_properties::color_value::ColorValue>(
            value.clone(),
        )
        .ok()?;
        Some(conv_color(&value))
    };
    Some(match effect.get("ty")?.as_u64()? {
        29 => model::LayerEffect::GaussianBlur {
            blurriness: scalar(0)?,
            dimensions: scalar(1).unwrap_or(Value::Fixed(1.0)),
            wrap: scalar(2).unwrap_or(Value::Fixed(0.0)),
        },
        25 => model::LayerEffect::DropShadow {
            color: color(0)?,
            opacity: scalar(1)?,
            angle: scalar(2)?,
            distance: scalar(3)?,
            softness: scalar(4)?,
        },
        21 => model::LayerEffect::Fill {
            color: color(2)?,
            opacity: scalar(6)?,
        },
        20 => model::LayerEffect::Tint {
            black: color(0)?,
            white: color(1)?,
            amount: scalar(2)?,
        },
        _ => return None,
    })
}

fn is_layer_hidden(source: &AnyLayer) -> bool {
    let hidden = match source {
        AnyLayer::Null(l) => l.visual_layer.layer.hidden,
        AnyLayer::Precomposition(l) => l.visual_layer.layer.hidden,
        AnyLayer::Shape(l) => l.visual_layer.layer.hidden,
        AnyLayer::Solid(l) => l.visual_layer.layer.hidden,
        AnyLayer::Image(l) => l.visual_layer.layer.hidden,
    };
    hidden == Some(true)
}

/// Returns the rotation in the plane of the composition.
///
/// A split rotation also turns the layer about the X and Y axes, which a 2D transform cannot
/// express, so only its Z component is used.
fn transform_rotation(rotation: &Option<AnyTransformR>) -> &FloatValue {
    match rotation {
        Some(AnyTransformR::Rotation(value)) => value,
        Some(AnyTransformR::SplitRotation { z_rotation, .. }) => z_rotation,
        None => &FLOAT_VALUE_ZERO,
    }
}

fn conv_transform_position(position: &AnyTransformP) -> Position {
    match position {
        AnyTransformP::Position(position) => conv_pos_point(position),
        AnyTransformP::SplitPosition(SplitVector { x, y, .. }) => {
            Position::SplitValues((conv_scalar(x), conv_scalar(y)))
        }
    }
}

pub fn conv_transform(
    value: &schema::helpers::transform::Transform,
) -> (runtime::model::Transform, Value<f64>) {
    let transform = animated::Transform {
        anchor: conv_pos_point(value.anchor_point.as_ref().unwrap_or(&POSITION_ZERO)),
        position: conv_transform_position(&value.position),
        scale: conv_vec2(value.scale.as_ref().unwrap_or(&MULTIDIM_ONE_HUNDRED)),
        rotation: conv_scalar(transform_rotation(&value.rotation)),
        skew: conv_scalar(value.skew.as_ref().unwrap_or(&FLOAT_VALUE_ZERO)),
        skew_angle: conv_scalar(value.skew_axis.as_ref().unwrap_or(&FLOAT_VALUE_ZERO)),
    };
    let opacity = conv_scalar(value.opacity.as_ref().unwrap_or(&FLOAT_VALUE_ONE_HUNDRED));
    (transform.into_model(), opacity)
}

pub fn conv_shape_transform(value: &schema::shapes::transform::TransformShape) -> GroupTransform {
    let transform = animated::Transform {
        anchor: conv_pos_point(
            value
                .transform
                .anchor_point
                .as_ref()
                .unwrap_or(&POSITION_ZERO),
        ),
        position: conv_transform_position(&value.transform.position),
        scale: conv_vec2(
            value
                .transform
                .scale
                .as_ref()
                .unwrap_or(&MULTIDIM_ONE_HUNDRED),
        ),
        rotation: conv_scalar(transform_rotation(&value.transform.rotation)),
        skew: conv_scalar(value.transform.skew.as_ref().unwrap_or(&FLOAT_VALUE_ZERO)),
        skew_angle: conv_scalar(
            value
                .transform
                .skew_axis
                .as_ref()
                .unwrap_or(&FLOAT_VALUE_ZERO),
        ),
    };

    let opacity = conv_scalar(
        value
            .transform
            .opacity
            .as_ref()
            .unwrap_or(&FLOAT_VALUE_ONE_HUNDRED),
    );

    GroupTransform {
        transform: transform.into_model(),
        opacity,
    }
}

/// Returns the start value of each keyframe.
///
/// A keyframe without a start value takes the previous keyframe's end value, or its start value
/// when that has no end value either. Older exporters end every property with such a keyframe.
fn keyframe_values<'a>(keyframes: impl Iterator<Item = &'a Keyframe>) -> Vec<&'a [f64]> {
    let mut values: Vec<&[f64]> = vec![];
    let mut previous: Option<&Keyframe> = None;
    for keyframe in keyframes {
        let value = if keyframe.value.is_empty() {
            previous
                .and_then(|previous| previous.end_value_deprecated.as_deref())
                .filter(|end| !end.is_empty())
                .or(values.last().copied())
                .unwrap_or_default()
        } else {
            keyframe.value.as_slice()
        };
        values.push(value);
        previous = Some(keyframe);
    }
    values
}

pub fn conv_keyframes<'a, T: Tween>(
    keyframes: impl Iterator<Item = &'a Keyframe>,
    f: impl Fn(&[f64]) -> T,
) -> Value<T> {
    let keyframes: Vec<_> = keyframes.collect();
    let start_values = keyframe_values(keyframes.iter().copied());
    let mut frames = vec![];
    let mut values = vec![];

    let collect_tangents = |handle: &Option<KeyframeBezierHandle>| {
        let mut handles = vec![];
        let Some(handle) = handle else { return handles };

        match (&handle.x_coordinate, &handle.y_coordinate) {
            (KeyframeComponent::ArrayOfValues(xarr), KeyframeComponent::ArrayOfValues(yarr)) => {
                handles.extend(
                    xarr.iter()
                        .take(2)
                        .zip(yarr.iter().take(2))
                        .map(|(x, y)| EasingHandle { x: *x, y: *y }),
                );
            }
            (KeyframeComponent::ArrayOfValues(xarr), KeyframeComponent::SingleValue(y)) => {
                handles.extend(xarr.iter().take(2).map(|x| EasingHandle { x: *x, y: *y }));
            }
            (KeyframeComponent::SingleValue(x), KeyframeComponent::ArrayOfValues(yarr)) => {
                handles.extend(yarr.iter().take(2).map(|y| EasingHandle { x: *x, y: *y }));
            }
            (KeyframeComponent::SingleValue(x), KeyframeComponent::SingleValue(y)) => {
                handles.push(EasingHandle { x: *x, y: *y });
            }
        }
        handles
    };

    for (keyframe, start_value) in keyframes.into_iter().zip(start_values) {
        let hold = keyframe
            .base
            .hold
            .as_ref()
            .map(|b| b.eq(&BoolInt::True))
            .unwrap_or(false);
        let in_tangents = collect_tangents(&keyframe.base.in_tangent);
        let out_tangents = collect_tangents(&keyframe.base.out_tangent);
        let tangents: Vec<_> = in_tangents.into_iter().zip(out_tangents).collect();
        if tangents.is_empty() {
            frames.push(Time {
                frame: keyframe.base.time,
                in_tangent: None,
                out_tangent: None,
                hold,
            });
            values.push(f(start_value));
        } else {
            for (in_tangent, out_tangent) in tangents {
                frames.push(Time {
                    frame: keyframe.base.time,
                    in_tangent: Some(in_tangent),
                    out_tangent: Some(out_tangent),
                    hold,
                });
                values.push(f(start_value));
            }
        }
    }
    Value::Animated(runtime::model::Animated {
        times: frames,
        values,
    })
}

/// Converts an easing handle of a property that is eased as a whole.
///
/// A handle may hold one value per dimension. The first is used, and a handle without values
/// counts as missing.
fn conv_keyframe_handle(handle: &KeyframeBezierHandle) -> Option<EasingHandle> {
    fn first(component: &KeyframeComponent) -> Option<f64> {
        match component {
            KeyframeComponent::SingleValue(value) => Some(*value),
            KeyframeComponent::ArrayOfValues(values) => values.first().copied(),
        }
    }
    Some(EasingHandle {
        x: first(&handle.x_coordinate)?,
        y: first(&handle.y_coordinate)?,
    })
}

fn conv_gradient_colors(
    value: &schema::animated_properties::gradient_colors::GradientColors,
) -> runtime::model::ColorStops {
    use schema::animated_properties::animated_property::AnimatedPropertyK::*;

    let count = value.count;
    match &value.colors.animated_property.value {
        Static(value) => runtime::model::ColorStops::Fixed({
            let mut stops = runtime::model::fixed::ColorStops::new();
            let raw = conv_stops(value, count);
            for values in raw {
                stops.push(
                    (
                        values[0] as f32,
                        runtime::model::fixed::Color::new([
                            values[1] as f32,
                            values[2] as f32,
                            values[3] as f32,
                            values[4] as f32,
                        ]),
                    )
                        .into(),
                );
            }
            stops
        }),
        AnimatedValue(animated) => {
            let mut frames = vec![];
            let mut values: Vec<Vec<f64>> = vec![];
            for (value, start_value) in animated.iter().zip(keyframe_values(animated.iter())) {
                let hold = value
                    .base
                    .hold
                    .as_ref()
                    .map(|b| b.eq(&BoolInt::True))
                    .unwrap_or(false);
                frames.push(Time {
                    frame: value.base.time,
                    in_tangent: value
                        .base
                        .in_tangent
                        .as_ref()
                        .and_then(conv_keyframe_handle),
                    out_tangent: value
                        .base
                        .out_tangent
                        .as_ref()
                        .and_then(conv_keyframe_handle),
                    hold,
                });

                let stops = conv_stops(start_value, count)
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();

                values.push(stops);
            }
            runtime::model::ColorStops::Animated(animated::ColorStops {
                frames,
                values,
                count,
            })
        }
    }
}

fn conv_draw(value: &schema::shapes::AnyShape) -> Option<runtime::model::Draw> {
    use schema::constants::line_cap::LineCap;
    use schema::constants::line_join::LineJoin;
    use schema::shapes::AnyShape;

    match value {
        AnyShape::Fill(value) => {
            let color = conv_color(&value.color);
            let brush = animated::Brush::Solid(color).into_model();
            let opacity = conv_scalar(&value.shape_style.opacity);
            Some(runtime::model::Draw {
                stroke: None,
                brush,
                opacity,
            })
        }
        AnyShape::Stroke(value) => {
            let stroke = animated::Stroke {
                width: conv_scalar(&value.base_stroke.width),
                join: match value
                    .base_stroke
                    .line_join
                    .as_ref()
                    .unwrap_or(&LineJoin::Bevel)
                {
                    LineJoin::Bevel => Join::Bevel,
                    LineJoin::Round => Join::Round,
                    LineJoin::Miter => Join::Miter,
                },
                miter_limit: value.base_stroke.miter_limit,
                cap: match value
                    .base_stroke
                    .line_cap
                    .as_ref()
                    .unwrap_or(&LineCap::Butt)
                {
                    LineCap::Butt => Cap::Butt,
                    LineCap::Round => Cap::Round,
                    LineCap::Square => Cap::Square,
                },
            };
            let color = conv_color(&value.stroke_color);
            let brush = animated::Brush::Solid(color).into_model();
            let opacity = conv_scalar(&value.shape_style.opacity);
            Some(runtime::model::Draw {
                stroke: Some(stroke.into_model()),
                brush,
                opacity,
            })
        }
        AnyShape::GradientFill(value) => {
            let is_radial = matches!(value.gradient.gradient_type, Some(GradientType::Radial));
            let start_point = conv_multi_point(&value.gradient.start_point);
            let end_point = conv_multi_point(&value.gradient.end_point);
            let gradient = animated::Gradient {
                is_radial,
                start_point,
                end_point,
                stops: conv_gradient_colors(&value.gradient.colors),
            };
            let brush = animated::Brush::Gradient(gradient).into_model();
            Some(Draw {
                stroke: None,
                brush,
                opacity: conv_scalar(&value.shape_style.opacity),
            })
        }
        AnyShape::GradientStroke(value) => {
            let stroke = animated::Stroke {
                width: conv_scalar(&value.base_stroke.width),
                join: match value
                    .base_stroke
                    .line_join
                    .as_ref()
                    .unwrap_or(&LineJoin::Round)
                {
                    LineJoin::Bevel => Join::Bevel,
                    LineJoin::Round => Join::Round,
                    LineJoin::Miter => Join::Miter,
                },
                miter_limit: value.base_stroke.miter_limit,
                cap: match value
                    .base_stroke
                    .line_cap
                    .as_ref()
                    .unwrap_or(&LineCap::Round)
                {
                    LineCap::Butt => Cap::Butt,
                    LineCap::Round => Cap::Round,
                    LineCap::Square => Cap::Square,
                },
            };
            let is_radial = matches!(
                value
                    .gradient
                    .gradient_type
                    .as_ref()
                    .unwrap_or(&GradientType::Linear),
                GradientType::Radial
            );
            let start_point = conv_multi_point(&value.gradient.start_point);
            let end_point = conv_multi_point(&value.gradient.end_point);
            let gradient = animated::Gradient {
                is_radial,
                start_point,
                end_point,
                stops: conv_gradient_colors(&value.gradient.colors),
            };
            let brush = animated::Brush::Gradient(gradient).into_model();
            Some(Draw {
                stroke: Some(stroke.into_model()),
                brush,
                opacity: conv_scalar(&value.shape_style.opacity),
            })
        }
        _ => None,
    }
}

/// Returns true if the shape is marked as hidden (`hd`).
fn is_shape_hidden(value: &schema::shapes::AnyShape) -> bool {
    use schema::shapes::AnyShape::*;
    let element = match value {
        Group(value) => &value.graphic_element,
        Rectangle(value) => &value.shape.graphic_element,
        Ellipse(value) => &value.shape.graphic_element,
        Path(value) => &value.shape.graphic_element,
        PolyStar(value) => &value.shape.graphic_element,
        Transform(value) => &value.graphic_element,
        Fill(value) => &value.shape_style.graphic_element,
        Stroke(value) => &value.shape_style.graphic_element,
        GradientFill(value) => &value.shape_style.graphic_element,
        GradientStroke(value) => &value.shape_style.graphic_element,
        Repeater(value) => &value.modifier.graphic_element,
        Trim(value) => &value.modifier.graphic_element,
        PuckerBloat(value) => &value.modifier.graphic_element,
        OffsetPath(value) => &value.modifier.graphic_element,
        Twist(value) => &value.modifier.graphic_element,
        RoundedCorners(value) => &value.modifier.graphic_element,
        Merge(value) => &value.graphic_element,
    };
    element.hidden == Some(true)
}

fn conv_shape(value: &schema::shapes::AnyShape) -> Option<crate::runtime::model::Shape> {
    use crate::runtime::model::Shape;
    let shape = conv_visible_shape(value)?;
    if !is_shape_hidden(value) {
        return Some(shape);
    }
    // A hidden shape takes no part in rendering: hidden geometry is not drawn, a hidden style
    // draws nothing and a hidden group is left out with its content. These stay in the model
    // for queries of the layer's paths. A hidden modifier is not applied at all.
    match shape {
        Shape::Repeater(_) | Shape::Trim(_) => None,
        shape => Some(Shape::Hidden(Box::new(shape))),
    }
}

fn conv_visible_shape(value: &schema::shapes::AnyShape) -> Option<crate::runtime::model::Shape> {
    match conv_draw(value) {
        Some(draw) => {
            return Some(crate::runtime::model::Shape::Draw(draw));
        }
        _ => {
            if let Some(geometry) = conv_geometry(value) {
                return Some(crate::runtime::model::Shape::Geometry(geometry));
            }
        }
    }

    match value {
        schema::shapes::AnyShape::Group(value) => {
            let mut shapes = vec![];
            let mut group_transform = None;
            for item in &value.shapes {
                match item {
                    schema::shapes::AnyShape::Transform(transform) => {
                        group_transform = Some(conv_shape_transform(transform));
                    }
                    _ => {
                        if let Some(shape) = conv_shape(item) {
                            shapes.push(shape);
                        }
                    }
                }
            }
            if !shapes.is_empty() {
                Some(crate::runtime::model::Shape::Group(shapes, group_transform))
            } else {
                None
            }
        }
        schema::shapes::AnyShape::Repeater(value) => {
            use schema::constants::composite::Composite;

            let transform = &value.transform.transform.transform;
            let repeater = animated::Repeater {
                copies: conv_scalar(&value.copies),
                offset: conv_scalar(value.offset.as_ref().unwrap_or(&FLOAT_VALUE_ZERO)),
                anchor_point: conv_pos_point(
                    transform.anchor_point.as_ref().unwrap_or(&POSITION_ZERO),
                ),
                position: conv_transform_position(&transform.position),
                rotation: conv_scalar(transform_rotation(&transform.rotation)),
                scale: conv_vec2(transform.scale.as_ref().unwrap_or(&MULTIDIM_ONE_HUNDRED)),
                start_opacity: conv_scalar(
                    value
                        .transform
                        .start_opacity
                        .as_ref()
                        .unwrap_or(&FLOAT_VALUE_ONE_HUNDRED),
                ),
                end_opacity: conv_scalar(
                    value
                        .transform
                        .end_opacity
                        .as_ref()
                        .unwrap_or(&FLOAT_VALUE_ONE_HUNDRED),
                ),
                composite: match value.composite.as_ref().unwrap_or(&Composite::Below) {
                    Composite::Below => RepeaterComposite::Below,
                    Composite::Above => RepeaterComposite::Above,
                },
            };
            Some(crate::runtime::model::Shape::Repeater(
                repeater.into_model(),
            ))
        }
        schema::shapes::AnyShape::Trim(value) => {
            let trim = animated::Trim {
                mode: match value.multiple {
                    Some(
                        schema::constants::trim_multiple_shapes::TrimMultipleShapes::Sequential,
                    ) => model::TrimMode::Sequential,
                    _ => model::TrimMode::Parallel,
                },
                start: conv_scalar(&value.start),
                end: conv_scalar(&value.end),
                offset: conv_scalar(&value.offset),
            };
            Some(crate::runtime::model::Shape::Trim(trim.into_model()))
        }
        _ => None,
    }
}

fn conv_geometry(value: &schema::shapes::AnyShape) -> Option<crate::runtime::model::Geometry> {
    use schema::shapes::AnyShape;
    match value {
        AnyShape::Ellipse(value) => {
            let ellipse = animated::Ellipse {
                is_ccw: matches!(
                    value.shape.direction,
                    Some(schema::constants::shape_direction::ShapeDirection::Reversed)
                ),
                position: conv_pos_point(&value.position),
                size: conv_size(&value.size),
            };
            Some(crate::runtime::model::Geometry::Ellipse(ellipse))
        }
        AnyShape::Rectangle(value) => {
            let rect = animated::Rect {
                is_ccw: matches!(
                    value.shape.direction,
                    Some(schema::constants::shape_direction::ShapeDirection::Reversed)
                ),
                position: conv_pos_point(&value.position),
                size: conv_size(&value.size),
                corner_radius: conv_scalar(&value.rounded_corner_radius),
            };
            Some(crate::runtime::model::Geometry::Rect(rect))
        }
        AnyShape::Path(value) => conv_shape_geometry(&value.shape_property),
        AnyShape::PolyStar(value) => conv_polystar_geometry(value),
        _ => None,
    }
}

fn conv_polystar_geometry(
    value: &schema::shapes::polystar::PolyStarShape,
) -> Option<runtime::model::Geometry> {
    use schema::constants::star_type::StarType;

    let is_polygon = matches!(value.star_type, StarType::Polygon);
    let position = conv_pos_point(&value.position);
    let outer_radius = conv_scalar(&value.outer_radius);
    let outer_roundness = conv_scalar(&value.outer_roundness);
    let rotation = conv_scalar(&value.rotation);
    let points = conv_scalar(&value.points);

    let (inner_radius, inner_roundness) = if is_polygon {
        (Value::Fixed(0.0), Value::Fixed(0.0))
    } else {
        let ir = value
            .inner_radius
            .as_ref()
            .map(conv_scalar)
            .unwrap_or(Value::Fixed(0.0));
        let is = value
            .inner_roundness
            .as_ref()
            .map(conv_scalar)
            .unwrap_or(Value::Fixed(0.0));
        (ir, is)
    };

    let star = animated::Star {
        is_polygon,
        direction: 1.0,
        position,
        inner_radius,
        inner_roundness,
        outer_radius,
        outer_roundness,
        rotation,
        points,
    };
    Some(star.into_model())
}

pub fn conv_shape_geometry(
    value: &schema::animated_properties::shape_property::ShapeProperty,
) -> Option<runtime::model::Geometry> {
    use schema::animated_properties::shape_property::ShapePropertyK::*;
    let mut is_closed = false;
    match &value.value {
        Static(value) => {
            let (points, is_closed) = conv_spline(value);
            let mut path = vec![];
            points.as_slice().to_path(is_closed, &mut path);
            Some(runtime::model::Geometry::Fixed(path))
        }
        Animated(animated) => {
            // Build frames & values for the animated spline
            let mut frames = vec![];
            let mut values = vec![];

            for (idx, value) in animated.iter().enumerate() {
                // Extract hold from base key properties
                let hold = value
                    .base
                    .hold
                    .as_ref()
                    .map(|b| b.eq(&BoolInt::True))
                    .unwrap_or(false);
                // Add frame time and easing
                frames.push(Time {
                    frame: value.base.time,
                    in_tangent: value
                        .base
                        .in_tangent
                        .as_ref()
                        .and_then(conv_keyframe_handle),
                    out_tangent: value
                        .base
                        .out_tangent
                        .as_ref()
                        .and_then(conv_keyframe_handle),
                    hold,
                });
                // Get converted spline points for keyframe bezier at the start
                let start_bezier = {
                    if let Some(start) = &value.start {
                        start.first()?.clone()
                    } else {
                        // No keyframe was present - we look for the previous end/start. Without
                        // either, the path has no geometry.
                        let prev = animated.get(idx.checked_sub(1)?)?;
                        // Take the previous end if present, otherwise the previous start.
                        if let Some(prev_end) = prev.end.as_ref() {
                            if let Some(prev_end_bezier) = prev_end.first() {
                                prev_end_bezier.clone()
                            } else {
                                prev.start.as_ref()?.first()?.clone()
                            }
                        } else {
                            prev.start.as_ref()?.first()?.clone()
                        }
                    }
                };
                let (points, is_frame_closed) = conv_spline(&start_bezier);
                // Get converted spline points for keyframe bezier at the end
                let end_points = value.end.as_ref().and_then(|end_beziers| {
                    let end_bezier = end_beziers.first()?;
                    let (end_points, is_end_frame_closed) = conv_spline(end_bezier);
                    is_closed |= is_end_frame_closed;
                    Some(end_points)
                });
                // Add the keyframe values
                values.push(animated::SplineKeyframeValues {
                    start: points,
                    end: end_points,
                });
                is_closed |= is_frame_closed;
            }
            Some(runtime::model::Geometry::Spline(animated::Spline {
                is_closed,
                times: frames,
                values,
            }))
        }
    }
}

pub fn conv_spline(value: &schema::helpers::bezier::Bezier) -> (Vec<Point>, bool) {
    use core::iter::repeat;
    let mut points = Vec::with_capacity(value.vertices.len() * 3);
    let is_closed = value.closed.unwrap_or(false);

    for ((v, i), o) in value
        .vertices
        .iter()
        .zip(value.in_tangents.iter().chain(repeat(&[0.0, 0.0])))
        .zip(value.out_tangents.iter().chain(repeat(&[0.0, 0.0])))
    {
        points.push((v[0], v[1]).into());
        points.push((i[0], i[1]).into());
        points.push((o[0], o[1]).into());
    }
    (points, is_closed)
}

pub fn conv_blend_mode(
    value: &crate::schema::constants::blend_mode::BlendMode,
) -> Option<BlendMode> {
    use crate::schema::constants::blend_mode::BlendMode::*;

    Some(match value {
        Normal => return None,
        Multiply => BlendMode::from(Mix::Multiply),
        Screen => BlendMode::from(Mix::Screen),
        Overlay => BlendMode::from(Mix::Overlay),
        Darken => BlendMode::from(Mix::Darken),
        Lighten => BlendMode::from(Mix::Lighten),
        ColorDodge => BlendMode::from(Mix::ColorDodge),
        ColorBurn => BlendMode::from(Mix::ColorBurn),
        HardLight => BlendMode::from(Mix::HardLight),
        SoftLight => BlendMode::from(Mix::SoftLight),
        Difference => BlendMode::from(Mix::Difference),
        Exclusion => BlendMode::from(Mix::Exclusion),
        Hue => BlendMode::from(Mix::Hue),
        Saturation => BlendMode::from(Mix::Saturation),
        Color => BlendMode::from(Mix::Color),
        Luminosity => BlendMode::from(Mix::Luminosity),
        Add => BlendMode::from(Compose::PlusLighter),
        // There is no equivalent mode; the layer blends normally.
        HardMix => return None,
    })
}

pub fn conv_scalar(float_value: &schema::animated_properties::value::FloatValue) -> Value<f64> {
    use crate::schema::animated_properties::animated_property::AnimatedPropertyK::*;
    match &float_value.animated_property.value {
        Static(number) => Value::Fixed(*number),
        AnimatedValue(keyframes) => {
            let mut frames = vec![];
            let mut values = vec![];
            for (keyframe, start_value) in keyframes.iter().zip(keyframe_values(keyframes.iter())) {
                let start_time = keyframe.base.time;
                let data = start_value.first().copied().unwrap_or_default();
                let hold = keyframe
                    .base
                    .hold
                    .as_ref()
                    .map(|b| b.eq(&BoolInt::True))
                    .unwrap_or(false);
                frames.push(crate::runtime::model::Time {
                    frame: start_time,
                    in_tangent: keyframe
                        .base
                        .in_tangent
                        .as_ref()
                        .and_then(conv_keyframe_handle),
                    out_tangent: keyframe
                        .base
                        .out_tangent
                        .as_ref()
                        .and_then(conv_keyframe_handle),
                    hold,
                });
                values.push(data);
            }
            Value::Animated(model::Animated {
                times: frames,
                values,
            })
        }
    }
}

pub fn conv_multi<T: Tween>(
    multidimensional: &schema::animated_properties::multi_dimensional::MultiDimensional,
    f: impl Fn(&[f64]) -> T,
) -> Value<T> {
    use crate::schema::animated_properties::animated_property::AnimatedPropertyK::*;

    match &multidimensional.animated_property.value {
        Static(components) => Value::Fixed(f(components)),
        AnimatedValue(keyframes) => conv_keyframes(keyframes.iter(), &f),
    }
}

pub fn conv_multi_color<T: Tween>(
    color: &schema::animated_properties::color_value::ColorValue,
    f: impl Fn(&[f64]) -> T,
) -> Value<T> {
    use crate::schema::animated_properties::animated_property::AnimatedPropertyK::*;

    match &color.animated_property.value {
        Static(components) => Value::Fixed(f(components)),
        AnimatedValue(keyframes) => conv_keyframes(keyframes.iter(), &f),
    }
}

pub fn conv_pos_point(value: &schema::animated_properties::position::Position) -> Position {
    use crate::schema::animated_properties::position::PositionValueK::*;

    fn point(value: &[f64]) -> Point {
        match value.get(..2) {
            Some([x, y]) => Point::new(*x, *y),
            _ => Point::ORIGIN,
        }
    }

    match &value.value {
        Static(components) => Position::Value(Value::Fixed(point(components))),
        Animated(keyframes)
            if keyframes.iter().any(|keyframe| {
                keyframe.value_in_tangent.is_some() || keyframe.value_out_tangent.is_some()
            }) =>
        {
            let start_values = keyframe_values(keyframes.iter().map(|keyframe| &keyframe.keyframe));
            let spatial_keyframes = keyframes
                .iter()
                .zip(start_values)
                .map(|(keyframe, start_value)| {
                    let base = &keyframe.keyframe.base;
                    SpatialKeyframe {
                        time: Time {
                            frame: base.time,
                            // The first easing component gives the distance along the spatial path.
                            in_tangent: base.in_tangent.as_ref().and_then(conv_keyframe_handle),
                            out_tangent: base.out_tangent.as_ref().and_then(conv_keyframe_handle),
                            hold: base.hold == Some(BoolInt::True),
                        },
                        value: point(start_value),
                        out_tangent: keyframe
                            .value_out_tangent
                            .as_ref()
                            .map(|v| point(v).to_vec2()),
                        in_tangent: keyframe
                            .value_in_tangent
                            .as_ref()
                            .map(|v| point(v).to_vec2()),
                    }
                })
                .collect();
            Position::Spatial(SpatialPosition::new(spatial_keyframes))
        }
        Animated(keyframes) => Position::Value(conv_keyframes(
            keyframes.iter().map(|keyframe| &keyframe.keyframe),
            point,
        )),
    }
}

pub fn conv_multi_point(
    value: &schema::animated_properties::multi_dimensional::MultiDimensional,
) -> Value<Point> {
    conv_multi(value, |x| {
        let (x0, x1) = match x.get(0..=1) {
            Some([x0, x1]) => (*x0, *x1),
            _ => (0.0, 0.0),
        };
        Point::new(x0, x1)
    })
}

pub fn conv_color(value: &schema::animated_properties::color_value::ColorValue) -> Value<Color> {
    conv_multi_color(value, |x| {
        let (x0, x1, x2) = match x.get(0..=2) {
            Some([x0, x1, x2]) => (*x0, *x1, *x2),
            _ => (0.0, 0.0, 0.0),
        };
        Color::new([x0 as f32, x1 as f32, x2 as f32, 1.0])
    })
}

pub fn conv_vec2(value: &MultiDimensional) -> Value<Vec2> {
    conv_multi(value, |x| {
        let (x0, x1) = match x.get(0..=1) {
            Some([x0, x1]) => (*x0, *x1),
            _ => (0.0, 0.0),
        };
        Vec2::new(x0, x1)
    })
}

pub fn conv_size(value: &MultiDimensional) -> Value<Size> {
    conv_multi(value, |x| {
        let (x0, x1) = match x.get(0..=1) {
            Some([x0, x1]) => (*x0, *x1),
            _ => (0.0, 0.0),
        };
        Size::new(x0, x1)
    })
}

pub fn conv_stops(value: &[f64], count: usize) -> Vec<[f64; 5]> {
    let mut stops: Vec<[f64; 5]> = Vec::new();
    let mut alpha_stops: Vec<(f64, f64)> = Vec::new();
    for chunk in value.chunks_exact(4) {
        stops.push([chunk[0], chunk[1], chunk[2], chunk[3], 1.0]);
        if stops.len() >= count {
            // there is alpha data at the end of the list, which is a sequence
            // of (offset, alpha) pairs
            for chunk in value.chunks_exact(2).skip(count * 2) {
                let offset = chunk[0];
                let alpha = chunk[1];
                alpha_stops.push((offset, alpha));
            }

            for stop in stops.iter_mut() {
                let mut last: Option<(f64, f64)> = None;
                for &(b, alpha_b) in alpha_stops.iter() {
                    if let Some((a, alpha_a)) = last.take() {
                        let x = stop[0];
                        let t = normalize_to_range(a, b, x);

                        let alpha_interp = alpha_a.tween(&alpha_b, t, &Easing::LERP);
                        let alpha_interp = if (x >= a && x <= b) && (t <= 0.25) && (x <= 0.1) {
                            alpha_a
                        } else {
                            alpha_interp
                        }; // todo: this is a hack to get alpha rendering with a
                        // falloff similar to LottieFiles

                        let alpha_interp = if (x >= a && x <= b) && (t >= 0.75) && (x >= 0.9) {
                            alpha_b
                        } else {
                            alpha_interp
                        }; // todo: this is a hack to get alpha rendering with a
                        // falloff similar to LottieFiles

                        stop[4] = stop[4].min(alpha_interp);
                    }
                    last = Some((b, alpha_b));
                }
            }
            break;
        }
    }

    stops
}

pub fn normalize_to_range(a: f64, b: f64, x: f64) -> f64 {
    if a == b {
        // Avoid division by zero if a and b are the same
        return 0.0;
    }

    // Calculate the normalized value
    (x - a) / (b - a)
}

#[cfg(test)]
mod tests {
    use crate::{
        import::converters::{conv_layer, conv_shape},
        model::{self, Content, RepeaterComposite},
        schema::{
            animated_properties::{
                animated_property::{AnimatedProperty, AnimatedPropertyK},
                position::{Position, PositionValueK},
                value::FloatValue,
            },
            helpers::{
                int_boolean::BoolInt,
                transform::{AnyTransformP, AnyTransformR, Transform},
                visual_object::VisualObject,
            },
            layers::{AnyLayer, layer::Layer, shape::ShapeLayer, visual::VisualLayer},
        },
    };
    use kurbo::{Point, Vec2};
    use peniko::Color;
    use serde_json::json;

    #[expect(deprecated, reason = "Uses deprecated VisualLayer fields")]
    fn make_shape_layer(hidden: bool, matte_target: bool) -> AnyLayer {
        AnyLayer::Shape(ShapeLayer {
            visual_layer: VisualLayer {
                layer: Layer {
                    visual_object: VisualObject {
                        name: Some("test".to_string()),
                        match_name: None,
                    },
                    layer_type: 4,
                    three_dimensional: None,
                    index: Some(0),
                    start_time: Some(0.0),
                    in_point: 0.0,
                    out_point: 60.0,
                    hidden: if hidden { Some(true) } else { None },
                    parent_index: None,
                    time_stretch: None,
                },
                transform: Transform {
                    anchor_point: None,
                    position: AnyTransformP::Position(Position {
                        property_index: None,
                        animated: Some(BoolInt::False),
                        expression: None,
                        length: None,
                        value: PositionValueK::Static(vec![0.0, 0.0]),
                    }),
                    scale: None,
                    rotation: Some(AnyTransformR::Rotation(FloatValue {
                        animated_property: AnimatedProperty {
                            animated: Some(BoolInt::False),
                            property_index: None,
                            expression: None,
                            slot_id: None,
                            value: AnimatedPropertyK::Static(0.0),
                        },
                    })),
                    opacity: None,
                    skew: None,
                    skew_axis: None,
                },
                matte_mode: None,
                matte_target: if matte_target {
                    Some(BoolInt::True)
                } else {
                    None
                },
                masks_properties: None,
                effects: None,
                rotate_to_match_anim_pos_path: None,
                matte_layer_index: None,
                has_mask: None,
                motion_blur: None,
                blend_mode: None,
                css_class: None,
                id: None,
                tag_name: None,
                tranform_before_mask_deprecated: None,
                transform_before_mask: None,
            },
            shapes: vec![],
        })
    }

    #[test]
    fn hidden_layer_retains_content() {
        let source = make_shape_layer(true, false);
        let (layer, ..) = conv_layer(&source).unwrap();
        assert!(layer.hidden);
        assert!(matches!(layer.content, Content::Shape(_)));
    }

    #[test]
    fn hidden_matte_layer_retains_is_matte_source() {
        let source = make_shape_layer(true, true);
        let (layer, ..) = conv_layer(&source).unwrap();
        assert!(layer.hidden);
        assert!(layer.is_matte_source);
    }

    #[test]
    fn visible_matte_layer_has_is_matte_source_true() {
        let source = make_shape_layer(false, true);
        let (layer, ..) = conv_layer(&source).unwrap();
        assert!(layer.is_matte_source);
    }

    #[test]
    fn visible_layer_has_content() {
        let source = make_shape_layer(false, false);
        let (layer, ..) = conv_layer(&source).unwrap();
        assert!(matches!(layer.content, Content::Shape(_)));
    }

    #[test]
    fn image_assets_and_layer_references_are_retained() {
        let composition = crate::Composition::from_json(serde_json::json!({
            "v": "5.5.2",
            "fr": 60,
            "ip": 0,
            "op": 60,
            "w": 512,
            "h": 512,
            "assets": [{
                "id": "texture",
                "h": 32,
                "w": 64,
                "u": "i/",
                "p": "texture.png",
                "e": 0
            }],
            "layers": [{
                "ty": 2,
                "ind": 1,
                "ip": 0,
                "op": 60,
                "st": 0,
                "refId": "texture",
                "ks": {
                    "a": { "a": 0, "k": [0, 0] },
                    "p": { "a": 0, "k": [0, 0] },
                    "s": { "a": 0, "k": [100, 100] },
                    "r": { "a": 0, "k": 0 },
                    "o": { "a": 0, "k": 100 },
                    "sk": { "a": 0, "k": 0 },
                    "sa": { "a": 0, "k": 0 }
                }
            }]
        }))
        .unwrap();

        let image = composition.images.get("texture").unwrap();
        assert_eq!(image.width, Some(64.0));
        assert_eq!(image.height, Some(32.0));
        assert_eq!(image.location(), "i/texture.png");
        assert!(!image.embedded);
        assert!(matches!(
            &composition.layers[0].content,
            Content::Image { asset_id } if asset_id == "texture"
        ));
    }

    #[test]
    fn repeater_retains_split_position_identity_scale_and_composite() {
        let source: crate::schema::shapes::AnyShape = serde_json::from_value(serde_json::json!({
            "ty": "rp",
            "c": { "a": 0, "k": 3 },
            "m": 2,
            "tr": {
                "p": {
                    "s": true,
                    "x": { "a": 0, "k": 12 },
                    "y": { "a": 0, "k": 34 }
                }
            }
        }))
        .unwrap();

        let model::Shape::Repeater(model::Repeater::Fixed(repeater)) = conv_shape(&source).unwrap()
        else {
            panic!("expected a fixed repeater");
        };
        assert_eq!(repeater.position, Point::new(12.0, 34.0));
        assert_eq!(repeater.scale, Vec2::new(100.0, 100.0));
        assert_eq!(repeater.composite, RepeaterComposite::Above);
    }

    /// Keyframes as written by older exporters: each holds its end value in `e`, and the last
    /// holds only a time. The easing handles describe a straight line.
    fn legacy_keyframes(start: serde_json::Value, end: serde_json::Value) -> serde_json::Value {
        json!([
            { "t": 0, "s": start, "e": end, "i": { "x": [1], "y": [1] }, "o": { "x": [0], "y": [0] } },
            { "t": 10 },
        ])
    }

    /// Easing is solved numerically, so values between keyframes are not exact.
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn parse<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).expect("the property should parse")
    }

    #[test]
    fn scalar_keyframe_without_start_takes_previous_end() {
        let value = super::conv_scalar(&parse(
            json!({ "a": 1, "k": legacy_keyframes(json!([20]), json!([80])) }),
        ));
        assert_eq!(value.evaluate(0.0), 20.0);
        assert!(close(value.evaluate(5.0), 50.0));
        assert_eq!(value.evaluate(10.0), 80.0);
        assert_eq!(value.evaluate(15.0), 80.0);
    }

    #[test]
    fn multi_dimensional_keyframe_without_start_takes_previous_end() {
        let value = super::conv_vec2(&parse(json!({
            "a": 1,
            "k": legacy_keyframes(json!([0, 100, 100]), json!([50, 200, 100])),
        })));
        assert_eq!(value.evaluate(0.0), Vec2::new(0.0, 100.0));
        let middle = value.evaluate(5.0);
        assert!(
            close(middle.x, 25.0) && close(middle.y, 150.0),
            "{middle:?}"
        );
        assert_eq!(value.evaluate(10.0), Vec2::new(50.0, 200.0));
    }

    #[test]
    fn position_keyframe_without_start_takes_previous_end() {
        let position = super::conv_pos_point(&parse(json!({
            "a": 1,
            "k": legacy_keyframes(json!([0, 0, 0]), json!([10, 20, 0])),
        })));
        assert!(matches!(position, super::Position::Value(_)));
        assert_eq!(position.evaluate(0.0), Point::new(0.0, 0.0));
        let middle = position.evaluate(5.0);
        assert!(close(middle.x, 5.0) && close(middle.y, 10.0), "{middle:?}");
        assert_eq!(position.evaluate(10.0), Point::new(10.0, 20.0));
    }

    #[test]
    fn spatial_position_keyframe_without_start_takes_previous_end() {
        let mut keyframes = legacy_keyframes(json!([0, 0, 0]), json!([10, 0, 0]));
        keyframes[0]["to"] = json!([0, 10, 0]);
        keyframes[0]["ti"] = json!([0, 10, 0]);
        let position = super::conv_pos_point(&parse(json!({ "a": 1, "k": keyframes })));
        assert!(matches!(position, super::Position::Spatial(_)));
        assert_eq!(position.evaluate(0.0), Point::new(0.0, 0.0));
        // The motion path bulges away from the straight line between the two values.
        let middle = position.evaluate(5.0);
        assert!(close(middle.x, 5.0) && middle.y > 5.0, "{middle:?}");
        assert_eq!(position.evaluate(10.0), Point::new(10.0, 0.0));
        assert_eq!(position.evaluate(15.0), Point::new(10.0, 0.0));
    }

    #[test]
    fn color_keyframe_without_start_takes_previous_end() {
        let color = super::conv_color(&parse(json!({
            "a": 1,
            "k": legacy_keyframes(json!([1, 0, 0, 1]), json!([0, 0, 1, 1])),
        })));
        let at = |frame| color.evaluate_or(frame, Color::TRANSPARENT).components;
        assert_eq!(at(0.0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(at(5.0), [0.5, 0.0, 0.5, 1.0]);
        assert_eq!(at(10.0), [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn gradient_keyframe_without_start_takes_previous_end() {
        let stops = super::conv_gradient_colors(&parse(json!({
            "p": 2,
            "k": {
                "a": 1,
                "k": legacy_keyframes(
                    json!([0, 1, 0, 0, 1, 0, 0, 0]),
                    json!([0, 0, 0, 1, 1, 1, 1, 1]),
                ),
            },
        })));
        let colors = |frame| -> Vec<[f32; 4]> {
            stops
                .evaluate(frame)
                .as_ref()
                .iter()
                .map(|stop| stop.color.components)
                .collect()
        };
        assert_eq!(colors(0.0), [[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]]);
        assert_eq!(colors(5.0), [[0.5, 0.0, 0.5, 1.0], [0.5, 0.5, 0.5, 1.0]]);
        assert_eq!(colors(10.0), [[0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0]]);
    }

    #[test]
    fn keyframe_start_wins_over_previous_end() {
        let value = super::conv_scalar(&parse(json!({
            "a": 1,
            "k": [
                { "t": 0, "s": [0], "e": [50], "i": { "x": [1], "y": [1] }, "o": { "x": [0], "y": [0] } },
                { "t": 10, "s": [80] },
            ],
        })));
        assert!(close(value.evaluate(5.0), 40.0));
        assert_eq!(value.evaluate(10.0), 80.0);
    }

    #[test]
    fn keyframe_without_start_or_previous_end_repeats_previous_start() {
        let value = super::conv_scalar(&parse(json!({
            "a": 1,
            "k": [{ "t": 0, "s": [30], "h": 1 }, { "t": 10 }],
        })));
        assert_eq!(value.evaluate(5.0), 30.0);
        assert_eq!(value.evaluate(10.0), 30.0);
    }

    #[test]
    fn document_with_legacy_keyframes_imports_and_renders() {
        use crate::test_support::*;
        let mut layer = shape_layer(1, json!([rect(0.0, 0.0, 10.0, 10.0), fill(100.0)]));
        layer["ks"]["o"] = json!({ "a": 1, "k": legacy_keyframes(json!([100]), json!([50])) });
        layer["ks"]["p"] =
            json!({ "a": 1, "k": legacy_keyframes(json!([0, 0, 0]), json!([40, 0, 0])) });
        let composition = composition(json!([layer]));
        let bounds = |frame| {
            render(&composition, frame, 1.0)
                .into_iter()
                .find_map(|event| match event {
                    Event::Draw { bounds, .. } => Some(bounds),
                    _ => None,
                })
        };
        assert_eq!(bounds(0.0), Some(kurbo::Rect::new(0.0, 0.0, 10.0, 10.0)));
        assert_eq!(bounds(10.0), Some(kurbo::Rect::new(40.0, 0.0, 50.0, 10.0)));
        assert_eq!(trace(&composition, 10.0), ["clip", "fill(0.5)", "pop"]);
    }

    fn split_rotation(z: f64) -> serde_json::Value {
        json!({
            "x": { "a": 0, "k": 10 },
            "y": { "a": 0, "k": 20 },
            "z": { "a": 0, "k": z },
            "or": { "a": 0, "k": [0, 0, 0] },
        })
    }

    #[test]
    fn layer_split_rotation_uses_the_z_component() {
        use crate::test_support::*;
        let ks = with(transform(100.0), json!({ "r": split_rotation(90.0) }));
        let composition = composition(json!([with(layer(3, 1), json!({ "ks": ks }))]));
        let components = composition.layers[0].transform.components(0.0).unwrap();
        assert_eq!(components.rotation, 90.0);
    }

    #[test]
    fn group_transform_accepts_split_rotation_and_split_position() {
        use crate::test_support::*;
        let mut group = group(vec![rect(0.0, 0.0, 10.0, 10.0), fill(100.0)], 100.0);
        group["it"][2]["r"] = split_rotation(90.0);
        group["it"][2]["p"] = json!({
            "s": true,
            "x": { "a": 0, "k": 12 },
            "y": { "a": 0, "k": 34 },
        });
        let Some(model::Shape::Group(_, Some(group))) = conv_shape(&parse(group)) else {
            panic!("expected a group with a transform");
        };
        let components = group.transform.components(0.0).unwrap();
        assert_eq!(components.rotation, 90.0);
        assert_eq!(components.position, Point::new(12.0, 34.0));
    }

    /// Two keyframes from 0 to 100 over ten frames with the given easing handles.
    fn eased_keyframes(o: serde_json::Value, i: serde_json::Value) -> serde_json::Value {
        json!({
            "a": 1,
            "k": [{ "t": 0, "s": [0], "o": o, "i": i }, { "t": 10, "s": [100] }],
        })
    }

    #[test]
    fn easing_handle_with_several_values_uses_the_first() {
        let several = super::conv_scalar(&parse(eased_keyframes(
            json!({ "x": [0.6, 0.1], "y": [0.0, 0.9] }),
            json!({ "x": [0.4, 0.8], "y": [1.0, 0.2] }),
        )));
        let single = super::conv_scalar(&parse(eased_keyframes(
            json!({ "x": [0.6], "y": [0.0] }),
            json!({ "x": [0.4], "y": [1.0] }),
        )));
        assert_eq!(several.evaluate(3.0), single.evaluate(3.0));
        assert!(single.evaluate(3.0) < 20.0, "the easing starts slowly");
    }

    #[test]
    fn easing_handle_without_values_counts_as_missing() {
        let value = super::conv_scalar(&parse(eased_keyframes(
            json!({ "x": [], "y": [] }),
            json!({ "x": [], "y": [] }),
        )));
        assert!(close(value.evaluate(3.0), 30.0));
    }

    #[test]
    fn shape_keyframes_accept_easing_handles_with_several_values() {
        let bezier = |x: f64| {
            json!([{
                "c": false,
                "v": [[0, 0], [x, 0]],
                "i": [[0, 0], [0, 0]],
                "o": [[0, 0], [0, 0]],
            }])
        };
        let geometry = super::conv_shape_geometry(&parse(json!({
            "a": 1,
            "k": [
                {
                    "t": 0,
                    "s": bezier(10.0),
                    "o": { "x": [0.0, 0.5], "y": [0.0, 0.5] },
                    "i": { "x": [1.0, 0.5], "y": [1.0, 0.5] },
                },
                { "t": 10, "s": bezier(20.0) },
            ],
        })))
        .unwrap();
        let mut path = vec![];
        geometry.evaluate(5.0, &mut path);
        assert_eq!(
            path,
            [
                kurbo::PathEl::MoveTo(Point::new(0.0, 0.0)),
                kurbo::PathEl::LineTo(Point::new(15.0, 0.0)),
            ]
        );
    }

    #[test]
    fn gradient_keyframes_accept_easing_handles_with_several_values() {
        let stops = super::conv_gradient_colors(&parse(json!({
            "p": 1,
            "k": {
                "a": 1,
                "k": [
                    {
                        "t": 0,
                        "s": [0, 1, 0, 0],
                        "o": { "x": [0.0, 0.5], "y": [0.0, 0.5] },
                        "i": { "x": [1.0, 0.5], "y": [1.0, 0.5] },
                    },
                    { "t": 10, "s": [0, 0, 0, 1] },
                ],
            },
        })));
        let stops = stops.evaluate(5.0);
        assert_eq!(stops.as_ref()[0].color.components, [0.5, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn keyframe_with_empty_value_evaluates_to_zero() {
        let value = super::conv_scalar(&parse(json!({
            "a": 1,
            "k": [{ "t": 0, "s": [] }, { "t": 10, "s": [] }],
        })));
        assert_eq!(value.evaluate(5.0), 0.0);
    }

    #[test]
    fn layer_blend_modes_map_to_mix_and_compose_modes() {
        use crate::test_support::*;
        use peniko::{BlendMode, Compose, Mix};
        let mixes = [
            Mix::Multiply,
            Mix::Screen,
            Mix::Overlay,
            Mix::Darken,
            Mix::Lighten,
            Mix::ColorDodge,
            Mix::ColorBurn,
            Mix::HardLight,
            Mix::SoftLight,
            Mix::Difference,
            Mix::Exclusion,
            Mix::Hue,
            Mix::Saturation,
            Mix::Color,
            Mix::Luminosity,
        ];
        let mut expected: Vec<Option<BlendMode>> = vec![None];
        expected.extend(mixes.map(|mix| Some(mix.into())));
        // `Add` sums the colors. `HardMix` has no equivalent and blends normally.
        expected.push(Some(Compose::PlusLighter.into()));
        expected.push(None);
        for (mode, expected) in expected.into_iter().enumerate() {
            let composition = composition(json!([with(layer(3, 1), json!({ "bm": mode }))]));
            assert_eq!(composition.layers[0].blend_mode, expected, "bm {mode}");
        }
    }

    #[test]
    fn first_shape_keyframe_without_start_has_no_geometry() {
        use crate::schema::animated_properties::{
            keyframe_base::KeyframeBase,
            shape_keyframe::ShapeKeyframe,
            shape_property::{ShapeProperty, ShapePropertyK},
        };
        // Parsing rejects this, but the schema types can be built directly.
        let keyframe = |time| ShapeKeyframe {
            base: KeyframeBase {
                time,
                hold: None,
                in_tangent: None,
                out_tangent: None,
            },
            start: None,
            end: None,
        };
        let property = ShapeProperty {
            property_index: None,
            animated: None,
            expression: None,
            value: ShapePropertyK::Animated(vec![keyframe(0.0), keyframe(10.0)]),
        };
        assert!(super::conv_shape_geometry(&property).is_none());
    }

    fn polystar(star_type: u8, points: serde_json::Value) -> serde_json::Value {
        json!({
            "ty": "sr",
            "sy": star_type,
            "p": { "a": 0, "k": [50, 50] },
            "or": { "a": 0, "k": 20 },
            "os": { "a": 0, "k": 0 },
            "ir": { "a": 0, "k": 10 },
            "is": { "a": 0, "k": 0 },
            "r": { "a": 0, "k": 0 },
            "pt": points,
        })
    }

    #[test]
    fn polystar_with_too_many_points_has_no_outline() {
        use crate::test_support::*;
        let animated = json!({ "a": 1, "k": [{ "t": 0, "s": [1e30] }, { "t": 10, "s": [1e30] }] });
        for points in [json!({ "a": 0, "k": 1e30 }), animated] {
            for star_type in [1, 2] {
                let shapes = json!([polystar(star_type, points.clone()), fill(100.0)]);
                let composition = composition(json!([shape_layer(1, shapes)]));
                assert_eq!(trace(&composition, 0.0), ["clip", "pop"]);
            }
        }
        let shapes = json!([polystar(1, json!({ "a": 0, "k": 5 })), fill(100.0)]);
        let composition = composition(json!([shape_layer(1, shapes)]));
        assert_eq!(trace(&composition, 0.0), ["clip", "fill(1)", "pop"]);
    }

    #[test]
    fn spatial_position_along_an_extremely_long_path_evaluates() {
        let position = super::conv_pos_point(&parse(json!({
            "a": 1,
            "k": [
                { "t": 0, "s": [0, 0, 0], "to": [0, 1e24, 0], "ti": [0, 1e24, 0] },
                { "t": 10, "s": [1e25, 0, 0] },
            ],
        })));
        let middle = position.evaluate(5.0);
        assert!(middle.x > 4e24 && middle.x < 6e24, "{middle:?}");
    }

    #[test]
    fn last_of_several_keyframes_at_one_time_applies() {
        // The value jumps at frame 10, and the keyframe it jumps from holds its value.
        let value = super::conv_scalar(&parse(json!({
            "a": 1,
            "k": [
                { "t": 0, "s": [0] },
                { "t": 10, "s": [100], "h": 1 },
                { "t": 10, "s": [50] },
                { "t": 20, "s": [60] },
            ],
        })));
        assert_eq!(value.evaluate(10.0), 50.0);
        assert!(close(value.evaluate(15.0), 55.0));
        assert!(value.evaluate(9.0) > 50.0);
    }

    #[test]
    fn shape_keyframes_at_one_time_evaluate_to_a_finite_path() {
        let bezier = |x: f64| {
            json!([{
                "c": false,
                "v": [[0, 0], [x, 0]],
                "i": [[0, 0], [0, 0]],
                "o": [[0, 0], [0, 0]],
            }])
        };
        let geometry = super::conv_shape_geometry(&parse(json!({
            "a": 1,
            "k": [
                { "t": 0, "s": bezier(10.0) },
                { "t": 10, "s": bezier(20.0) },
                { "t": 10, "s": bezier(30.0) },
            ],
        })))
        .unwrap();
        let mut path = vec![];
        geometry.evaluate(10.0, &mut path);
        assert_eq!(
            path,
            [
                kurbo::PathEl::MoveTo(Point::new(0.0, 0.0)),
                kurbo::PathEl::LineTo(Point::new(30.0, 0.0)),
            ]
        );
    }
}
