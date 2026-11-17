// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use serde::{Deserialize, Deserializer};
use serde_repr::Serialize_repr;

/// Drawing direction of the shape curve, useful for trim path
///
/// Only `3` reverses a shape. Any other value is read as [`Self::Normal`]; some exporters write
/// `2`.
#[repr(u8)]
#[derive(Serialize_repr, Debug, Clone, PartialEq)]
pub enum ShapeDirection {
    /// Usually clockwise
    Normal = 1,
    /// Usually counter clockwise
    Reversed = 3,
}

impl<'de> Deserialize<'de> for ShapeDirection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match u8::deserialize(deserializer)? {
            3 => Self::Reversed,
            _ => Self::Normal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ShapeDirection;
    use serde_json::json;

    #[test]
    fn only_three_reverses_the_direction() {
        let parse = |value| serde_json::from_value::<ShapeDirection>(json!(value)).unwrap();
        assert_eq!(parse(1), ShapeDirection::Normal);
        assert_eq!(parse(2), ShapeDirection::Normal);
        assert_eq!(parse(3), ShapeDirection::Reversed);
        assert_eq!(serde_json::to_value(parse(3)).unwrap(), json!(3));
        assert_eq!(serde_json::to_value(parse(2)).unwrap(), json!(1));
    }
}
