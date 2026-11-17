// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transform - <https://lottiefiles.github.io/lottie-docs/concepts/#transform>

use crate::schema::animated_properties::multi_dimensional::MultiDimensional;
use crate::schema::animated_properties::position::Position;
use crate::schema::animated_properties::split_vector::SplitVector;
use crate::schema::animated_properties::value::FloatValue;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Transform {
    /// Position (relative to its parent) around which transformations are
    /// applied (ie: center for rotation / scale)
    #[serde(rename = "a")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_point: Option<Position>,
    /// Position / Translation
    #[serde(rename = "p")]
    pub position: AnyTransformP,
    /// Scale factor, 100 for no scaling
    #[serde(rename = "s")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<MultiDimensional>,
    /// Rotation in degrees, clockwise
    // todo: need untagged enum for split vector variant
    #[serde(rename = "r")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<AnyTransformR>,
    /// Rotation about the X axis in degrees. A 3D layer has this, `ry`, `rz` and `or` in
    /// place of `r`.
    #[serde(rename = "rx")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_rotation: Option<FloatValue>,
    /// Rotation about the Y axis in degrees, for a 3D layer.
    #[serde(rename = "ry")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y_rotation: Option<FloatValue>,
    /// Rotation about the Z axis in degrees, clockwise, for a 3D layer. This is the rotation
    /// in the plane of the composition, like `r`.
    #[serde(rename = "rz")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z_rotation: Option<FloatValue>,
    /// Orientation of a 3D layer, as rotations about the X, Y and Z axes in degrees.
    #[serde(rename = "or")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<MultiDimensional>,
    /// Skew amount as an angle in degrees
    #[serde(rename = "sk")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skew: Option<FloatValue>,
    /// Direction at which skew is applied, in degrees (0 skews along the X
    /// axis, 90 along the Y axis)
    #[serde(rename = "sa")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skew_axis: Option<FloatValue>,
    /// Opacity, 100 for fully opaque
    #[serde(rename = "o")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<FloatValue>,
}

/// The possible values of "p" in a [`Transform`].
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum AnyTransformP {
    /// Position / Translation
    Position(Position),
    /// Position / Translation with split components
    SplitPosition(SplitVector),
}

/// The possible values of "r" in a [`Transform`].
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
#[expect(clippy::large_enum_variant, reason = "Deferred")]
pub enum AnyTransformR {
    /// Rotation in degrees, clockwise
    Rotation(FloatValue),
    /// Split rotation components
    SplitRotation {
        /// Split rotation X component.
        #[serde(rename = "x")]
        x_rotation: FloatValue,
        /// Split rotation Y component.
        #[serde(rename = "y")]
        y_rotation: FloatValue,
        /// Split rotation component, equivalent to r when not split.
        #[serde(rename = "z")]
        z_rotation: FloatValue,
        /// Orientation
        #[serde(rename = "or")]
        orientation: MultiDimensional,
    },
}
