// Copyright 2026 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Helpers for tests that assert what the renderer emits.

use crate::model::{ImageAsset, fixed};
use crate::{Composition, RenderSink, Renderer};
use kurbo::{Affine, Rect, Shape};
use peniko::{BlendMode, Brush, Compose, Fill, Mix};
use serde_json::{Value, json};
use std::fmt;

/// A call received by a [`RecordingSink`].
///
/// The [`Display`](fmt::Display) form is a short summary for comparing whole sequences.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Event {
    /// `push_layer`. `bounds` is the bounding box of the layer's shape in output coordinates.
    PushLayer {
        blend: BlendMode,
        alpha: f32,
        bounds: Rect,
    },
    /// `push_clip_layer`, with the bounding box of the clip in output coordinates.
    PushClip(Rect),
    /// `pop_layer`
    Pop,
    /// `draw`. `alpha` is the alpha of a solid brush, or the largest stop alpha of a gradient.
    /// `bounds` is the bounding box of the path in output coordinates, before stroking.
    Draw {
        stroked: bool,
        fill_rule: Fill,
        alpha: f32,
        bounds: Rect,
    },
    /// `draw_image`
    Image { id: String, alpha: f64 },
    /// `begin_layer_group`
    BeginLayer(String),
    /// `end_layer_group`
    EndLayer,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PushLayer { blend, alpha, .. } => {
                write!(f, "layer({alpha}")?;
                if blend.mix != Mix::Normal {
                    write!(f, ", {:?}", blend.mix)?;
                }
                if blend.compose != Compose::SrcOver {
                    write!(f, ", {:?}", blend.compose)?;
                }
                write!(f, ")")
            }
            Self::PushClip(_) => write!(f, "clip"),
            Self::Pop => write!(f, "pop"),
            Self::Draw {
                stroked: false,
                fill_rule: Fill::NonZero,
                alpha,
                ..
            } => write!(f, "fill({alpha})"),
            Self::Draw {
                stroked: false,
                fill_rule: Fill::EvenOdd,
                alpha,
                ..
            } => write!(f, "fill({alpha}, EvenOdd)"),
            Self::Draw {
                stroked: true,
                alpha,
                ..
            } => write!(f, "stroke({alpha})"),
            Self::Image { id, alpha } => write!(f, "image({id}, {alpha})"),
            Self::BeginLayer(name) => write!(f, "begin({name})"),
            Self::EndLayer => write!(f, "end"),
        }
    }
}

/// A [`RenderSink`] that records every call it receives.
#[derive(Debug, Default)]
pub(crate) struct RecordingSink {
    pub(crate) events: Vec<Event>,
}

impl RecordingSink {
    /// Panics if a layer is popped that was never pushed, or if any layer is left open.
    pub(crate) fn assert_balanced(&self) {
        let mut depth = 0_usize;
        for (index, event) in self.events.iter().enumerate() {
            match event {
                Event::PushLayer { .. } | Event::PushClip(_) => depth += 1,
                Event::Pop => {
                    assert!(depth > 0, "event {index} pops a layer that was not pushed");
                    depth -= 1;
                }
                _ => {}
            }
        }
        assert_eq!(depth, 0, "{depth} layers were left open");
    }
}

impl RenderSink for RecordingSink {
    fn push_layer(
        &mut self,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        shape: &impl Shape,
    ) {
        self.events.push(Event::PushLayer {
            blend: blend.into(),
            alpha,
            bounds: transform.transform_rect_bbox(shape.bounding_box()),
        });
    }

    fn push_clip_layer(&mut self, transform: Affine, shape: &impl Shape) {
        self.events.push(Event::PushClip(
            transform.transform_rect_bbox(shape.bounding_box()),
        ));
    }

    fn pop_layer(&mut self) {
        self.events.push(Event::Pop);
    }

    fn draw(
        &mut self,
        stroke: Option<&fixed::Stroke>,
        fill_rule: Fill,
        transform: Affine,
        brush: &fixed::Brush,
        shape: &impl Shape,
    ) {
        let alpha = match brush {
            Brush::Solid(color) => color.components[3],
            Brush::Gradient(gradient) => gradient
                .stops
                .iter()
                .map(|stop| stop.color.components[3])
                .fold(0.0, f32::max),
            Brush::Image(_) => 1.0,
        };
        self.events.push(Event::Draw {
            stroked: stroke.is_some(),
            fill_rule,
            alpha,
            bounds: transform.transform_rect_bbox(shape.bounding_box()),
        });
    }

    fn draw_image(&mut self, image: &ImageAsset, _transform: Affine, alpha: f64) {
        self.events.push(Event::Image {
            id: image.id.clone(),
            alpha,
        });
    }

    fn begin_layer_group(&mut self, name: &str, _index: usize) {
        self.events.push(Event::BeginLayer(name.to_owned()));
    }

    fn end_layer_group(&mut self) {
        self.events.push(Event::EndLayer);
    }
}

/// Renders a frame with an identity transform and returns every call the sink received.
///
/// Panics if evaluation fails or if layer pushes and pops do not balance.
pub(crate) fn render(composition: &Composition, frame: f64, alpha: f64) -> Vec<Event> {
    let mut sink = RecordingSink::default();
    Renderer::new()
        .try_append(composition, frame, Affine::IDENTITY, alpha, &mut sink)
        .expect("evaluation failed");
    sink.assert_balanced();
    sink.events
}

/// Renders a frame at full opacity and summarises the calls, leaving out layer group hooks.
pub(crate) fn trace(composition: &Composition, frame: f64) -> Vec<String> {
    trace_with_alpha(composition, frame, 1.0)
}

/// Like [`trace`], with the opacity passed to the renderer.
pub(crate) fn trace_with_alpha(composition: &Composition, frame: f64, alpha: f64) -> Vec<String> {
    render(composition, frame, alpha)
        .iter()
        .filter(|event| !matches!(event, Event::BeginLayer(_) | Event::EndLayer))
        .map(Event::to_string)
        .collect()
}

/// Imports a 100×100 animation that runs for frames 0 to 60 with the given `layers`.
pub(crate) fn composition(layers: Value) -> Composition {
    composition_with_assets(json!([]), layers)
}

/// Like [`composition`], with an `assets` array.
pub(crate) fn composition_with_assets(assets: Value, layers: Value) -> Composition {
    Composition::from_json(json!({
        "v": "5.7.14",
        "fr": 60,
        "ip": 0,
        "op": 60,
        "w": 100,
        "h": 100,
        "assets": assets,
        "layers": layers,
    }))
    .expect("the document should import")
}

/// Returns `base` with the keys of `overrides` added or replaced.
pub(crate) fn with(mut base: Value, overrides: Value) -> Value {
    let (Value::Object(base_map), Value::Object(overrides)) = (&mut base, overrides) else {
        panic!("both values must be objects");
    };
    base_map.extend(overrides);
    base
}

/// A layer transform (`ks`) that only sets the opacity.
pub(crate) fn transform(opacity: f64) -> Value {
    json!({
        "a": { "a": 0, "k": [0, 0] },
        "p": { "a": 0, "k": [0, 0] },
        "s": { "a": 0, "k": [100, 100] },
        "r": { "a": 0, "k": 0 },
        "o": { "a": 0, "k": opacity },
    })
}

/// A layer of type `ty` that is active for the whole animation, with an identity transform.
pub(crate) fn layer(ty: u8, ind: u32) -> Value {
    json!({
        "ty": ty,
        "ind": ind,
        "nm": format!("layer {ind}"),
        "ip": 0,
        "op": 60,
        "st": 0,
        "ks": transform(100.0),
    })
}

/// A layer transform (`ks`) with the given opacity, for [`with`].
pub(crate) fn opacity(opacity: f64) -> Value {
    json!({ "ks": transform(opacity) })
}

/// A shape layer holding `shapes`.
pub(crate) fn shape_layer(ind: u32, shapes: Value) -> Value {
    with(layer(4, ind), json!({ "shapes": shapes }))
}

/// A solid layer of the given size and color.
pub(crate) fn solid_layer(ind: u32, width: f64, height: f64, color: &str) -> Value {
    with(
        layer(1, ind),
        json!({ "sw": width, "sh": height, "sc": color }),
    )
}

/// A precomposition layer that instances the asset `ref_id`.
pub(crate) fn precomp_layer(ind: u32, ref_id: &str) -> Value {
    with(
        layer(0, ind),
        json!({ "refId": ref_id, "w": 100, "h": 100 }),
    )
}

/// A precomposition asset.
pub(crate) fn precomp(id: &str, layers: Value) -> Value {
    json!({ "id": id, "layers": layers })
}

/// A rectangle shape spanning `x0..x1` and `y0..y1`.
pub(crate) fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!({
        "ty": "rc",
        "p": { "a": 0, "k": [(x0 + x1) / 2.0, (y0 + y1) / 2.0] },
        "s": { "a": 0, "k": [x1 - x0, y1 - y0] },
        "r": { "a": 0, "k": 0 },
    })
}

/// A solid fill with the given opacity.
pub(crate) fn fill(opacity: f64) -> Value {
    json!({
        "ty": "fl",
        "c": { "a": 0, "k": [1, 0, 0] },
        "o": { "a": 0, "k": opacity },
    })
}

/// A solid stroke with the given opacity.
pub(crate) fn stroke(opacity: f64) -> Value {
    json!({
        "ty": "st",
        "c": { "a": 0, "k": [0, 0, 1] },
        "o": { "a": 0, "k": opacity },
        "w": { "a": 0, "k": 2 },
    })
}

/// A linear gradient from red to black, for a gradient fill (`gf`) or gradient stroke (`gs`).
pub(crate) fn gradient(ty: &str, opacity: f64) -> Value {
    json!({
        "ty": ty,
        "o": { "a": 0, "k": opacity },
        "w": { "a": 0, "k": 2 },
        "t": 1,
        "s": { "a": 0, "k": [0, 0] },
        "e": { "a": 0, "k": [10, 0] },
        "g": { "p": 2, "k": { "a": 0, "k": [0, 1, 0, 0, 1, 0, 0, 0] } },
    })
}

/// A shape group holding `items`, followed by a group transform that only sets the opacity.
pub(crate) fn group(mut items: Vec<Value>, opacity: f64) -> Value {
    items.push(with(transform(opacity), json!({ "ty": "tr" })));
    json!({ "ty": "gr", "it": items })
}

/// A repeater that offsets each copy by `dx` and fades from `start_opacity` to `end_opacity`.
pub(crate) fn repeater(copies: u32, dx: f64, start_opacity: f64, end_opacity: f64) -> Value {
    json!({
        "ty": "rp",
        "c": { "a": 0, "k": copies },
        "o": { "a": 0, "k": 0 },
        "m": 1,
        "tr": {
            "ty": "tr",
            "a": { "a": 0, "k": [0, 0] },
            "p": { "a": 0, "k": [dx, 0] },
            "s": { "a": 0, "k": [100, 100] },
            "r": { "a": 0, "k": 0 },
            "so": { "a": 0, "k": start_opacity },
            "eo": { "a": 0, "k": end_opacity },
        },
    })
}
