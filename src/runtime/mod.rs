// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

mod effects;
mod evaluate;
mod render;
mod trim;

use crate::Error;
use crate::import;
use crate::schema::Animation;
use std::collections::HashMap;
use std::ops::Range;

#[cfg(feature = "vello")]
mod vello;

#[cfg(test)]
mod limit_tests;

pub mod model;

pub use effects::{BlurEdgeMode, FilterEffect, FilterLayerResult};
pub use evaluate::{
    AuthoredPath, EvaluatedComposition, EvaluatedLayer, EvaluationError, OccurrencePath,
};
pub use render::{RenderSink, Renderer};

/// Model of a Lottie file.
#[derive(Clone, Default, Debug)]
pub struct Composition {
    /// Frames in which the animation is active.
    pub frames: Range<f64>,
    /// Frames per second.
    pub frame_rate: f64,
    /// Width of the animation.
    pub width: usize,
    /// Height of the animation.
    pub height: usize,
    /// Precomposed layers that may be instanced.
    pub assets: HashMap<String, Vec<model::Layer>>,
    /// Raster image assets, keyed by the identifier used by layer `refId`s.
    pub images: HashMap<String, model::ImageAsset>,
    /// Collection of layers.
    pub layers: Vec<model::Layer>,
    /// Layers that do not render as authored, in the main composition and in precomposition
    /// assets.
    ///
    /// This lists layers of an unsupported type, which are imported as empty layers, and 3D
    /// layers that are rotated out of the plane of the composition. Other features that are
    /// not supported, such as mask modes, are not reported here.
    pub unsupported_layers: Vec<model::UnsupportedLayer>,
    /// Assets that were left out because their kind is unsupported.
    pub unsupported_assets: Vec<model::UnsupportedAsset>,
}

impl Composition {
    /// Creates a new runtime composition from a buffer of Lottie file contents.
    pub fn from_slice(source: impl AsRef<[u8]>) -> Result<Composition, Error> {
        let source = Animation::from_slice(source.as_ref())?;
        import::conv_animation(source)
    }

    /// Creates a new runtime composition from a json object of Lottie file contents.
    ///
    /// Fails for a value in which arrays and objects nest deeper than 128 levels, which is
    /// what the JSON parser takes when it reads a file.
    pub fn from_json(v: serde_json::Value) -> Result<Composition, Error> {
        if !nests_within(&v, 128) {
            let error: serde_json::Error = serde::de::Error::custom("recursion limit exceeded");
            return Err(error.into());
        }
        let source = Animation::from_json(v)?;
        import::conv_animation(source)
    }
}

/// Whether the arrays and objects of a value nest no deeper than `depth` levels.
fn nests_within(value: &serde_json::Value, depth: usize) -> bool {
    let mut values = vec![(value, 1)];
    while let Some((value, level)) = values.pop() {
        let nested: Box<dyn Iterator<Item = &serde_json::Value>> = match value {
            serde_json::Value::Array(values) => Box::new(values.iter()),
            serde_json::Value::Object(values) => Box::new(values.values()),
            _ => continue,
        };
        if level > depth {
            return false;
        }
        values.extend(nested.map(|value| (value, level + 1)));
    }
    true
}

impl std::str::FromStr for Composition {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let source = Animation::from_str(s)?;
        import::conv_animation(source)
    }
}
