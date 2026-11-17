// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::keyframe_base::KeyframeBase;
use serde::{Deserialize, Serialize};

/// Keyframes specifies the value at a specific time and the interpolation
/// function to reach the next keyframe.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct Keyframe {
    #[serde(flatten)]
    pub base: KeyframeBase,
    /// Value at this keyframe. Note that if the property is a scalar, keyframe
    /// values are still represented as arrays.
    ///
    /// Empty if the keyframe has no "s". Older exporters end a property with a keyframe
    /// that holds only a time, and its value is the previous keyframe's "e".
    #[serde(rename = "s")]
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub value: Vec<f64>,
    /// Value at the end of the keyframe. Note that this is deprecated, and you
    /// should use "s" from the next keyframe to get this value.
    #[serde(rename = "e")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_value_deprecated: Option<Vec<f64>>,
}

#[cfg(test)]
mod tests {
    use super::Keyframe;
    use serde_json::json;

    #[test]
    fn keyframe_without_start_value_round_trips() {
        let source = json!({ "t": 10.0 });
        let keyframe: Keyframe = serde_json::from_value(source.clone()).unwrap();
        assert!(keyframe.value.is_empty());
        assert_eq!(serde_json::to_value(&keyframe).unwrap(), source);
    }

    #[test]
    fn keyframe_with_start_value_round_trips() {
        let source = json!({ "t": 10.0, "s": [1.0, 2.0] });
        let keyframe: Keyframe = serde_json::from_value(source.clone()).unwrap();
        assert_eq!(keyframe.value, [1.0, 2.0]);
        assert_eq!(serde_json::to_value(&keyframe).unwrap(), source);
    }
}
