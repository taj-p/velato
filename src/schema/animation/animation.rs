// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::schema::{
    animation::{composition::Composition, meta::Meta},
    assets::AnyAsset,
    helpers::{int_boolean::BoolInt, marker::Marker, visual_object::VisualObject},
    layers::AnyLayer,
};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt::Display;

/// Top level object, describing the animation
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct Animation {
    /// Visual object properties
    #[serde(flatten)]
    pub visual_object: VisualObject,
    /// Composition properties
    #[serde(flatten)]
    pub composition: Composition,
    /// Bodymovin version
    #[serde(rename = "v")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Specification version
    #[serde(rename = "ver")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ver: Option<u32>,
    /// Framerate in frames per second
    #[serde(rename = "fr")]
    pub frame_rate: f64,
    /// "In Point", which frame the animation starts at (usually 0)
    #[serde(rename = "ip")]
    pub in_point: f64,
    /// "Out Point", which frame the animation stops/loops at, which makes this
    /// the duration in frames when `ip` is 0
    #[serde(rename = "op")]
    pub out_point: f64,
    /// Width of the animation
    ///
    /// A fractional width is rounded up, so that the animation covers its authored size.
    #[serde(rename = "w", deserialize_with = "deserialize_size")]
    pub width: usize,
    /// Height of the animation
    ///
    /// A fractional height is rounded up, like the width.
    #[serde(rename = "h", deserialize_with = "deserialize_size")]
    pub height: usize,
    /// Whether the animation has 3D layers
    #[serde(rename = "ddd", default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub three_dimensional: Option<BoolInt>,
    /// List of assets that can be referenced by layers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets: Option<Vec<AnyAsset>>,
    /// Markers (named time spans)
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markers: Option<Vec<Marker>>,
    /// File metadata from the authoring tool (Bodymovin/LottieFiles extension)
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    /// User-defined metadata bag. Schema is consumer-defined.
    ///
    /// - Bodymovin uses keys like `filename`/`customProps`.
    /// - Lottielab embeds its interactivity extension here under `lottielabInteractivity`.
    /// - Custom embeds are permitted.
    ///
    /// Left as a raw JSON value so consumers own the schema.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    // TODO: comps
    // TODO: fonts
    // TODO: chars
    // TODO: mb
    // TODO: slots
}

fn deserialize_size<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let size = f64::deserialize(deserializer)?;
    if size.is_finite() && size >= 0.0 {
        Ok(size.ceil() as usize)
    } else {
        Err(D::Error::custom(format!(
            "invalid size {size}: expected a finite, non-negative number"
        )))
    }
}

impl Animation {
    pub fn from_slice(v: &[u8]) -> Result<Animation, serde_json::Error> {
        serde_json::from_slice(v)
    }

    /// Reads an animation from a JSON value.
    ///
    /// Fails for a value in which arrays and objects nest deeper than 127 levels, which is
    /// what the JSON parser takes when it reads a file with [`Self::from_slice`] or
    /// [`from_str`](std::str::FromStr::from_str). Reading a value recurses once for each
    /// level, as does dropping it, so a deeper one would overflow the stack; this neither
    /// reads nor drops such a value by recursing.
    ///
    /// The types of the schema can also be read from a value with [`serde_json::from_value`],
    /// which has no such guard.
    pub fn from_json(v: serde_json::Value) -> Result<Animation, serde_json::Error> {
        if !nests_within(&v, MAX_NESTING) {
            drop_without_recursion(v);
            return Err(Error::custom("recursion limit exceeded"));
        }
        serde_json::from_value(v)
    }
}

/// Most levels of arrays and objects that a JSON value may nest to be read. The JSON parser
/// takes as many in text: its recursion limit of 128 counts the value inside the innermost.
const MAX_NESTING: usize = 127;

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

/// Drops a value by taking it apart level by level, which dropping it as it is would do by
/// recursing.
fn drop_without_recursion(value: serde_json::Value) {
    let mut values = vec![value];
    while let Some(value) = values.pop() {
        match value {
            serde_json::Value::Array(items) => values.extend(items),
            serde_json::Value::Object(items) => {
                values.extend(items.into_iter().map(|(_, item)| item));
            }
            _ => {}
        }
    }
}

impl std::str::FromStr for Animation {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

impl Display for Animation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", serde_json::to_string(&self).unwrap())
    }
}
