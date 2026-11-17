// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::animated_property::{AnimatedProperty, AnimatedPropertyK};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize};

/// aka Value, in the Schema.
///
/// An animatable property that holds a float.
///
/// Some exporters write a static value as an array of one number. It is read as that number.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct FloatValue {
    #[serde(flatten)]
    pub animated_property: AnimatedProperty<f64>,
}

/// A static scalar, which is a number or, for some exporters, an array that holds it.
#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum StaticValue {
    Number(f64),
    Numbers(Vec<f64>),
}

impl<'de> Deserialize<'de> for FloatValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let property = AnimatedProperty::<StaticValue>::deserialize(deserializer)?;
        let value = match property.value {
            AnimatedPropertyK::AnimatedValue(keyframes) => {
                AnimatedPropertyK::AnimatedValue(keyframes)
            }
            AnimatedPropertyK::Static(StaticValue::Number(number)) => {
                AnimatedPropertyK::Static(number)
            }
            AnimatedPropertyK::Static(StaticValue::Numbers(numbers)) => AnimatedPropertyK::Static(
                *numbers
                    .first()
                    .ok_or_else(|| D::Error::custom("expected a number or keyframes"))?,
            ),
        };
        Ok(Self {
            animated_property: AnimatedProperty {
                property_index: property.property_index,
                animated: property.animated,
                expression: property.expression,
                slot_id: property.slot_id,
                value,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::FloatValue;
    use crate::schema::animated_properties::animated_property::AnimatedPropertyK;
    use serde_json::json;

    #[test]
    fn static_value_round_trips() {
        let source = json!({ "a": 0, "k": 100.0 });
        let value: FloatValue = serde_json::from_value(source.clone()).unwrap();
        assert_eq!(
            value.animated_property.value,
            AnimatedPropertyK::Static(100.0)
        );
        assert_eq!(serde_json::to_value(&value).unwrap(), source);
    }

    #[test]
    fn static_value_may_be_an_array_of_one_number() {
        let value: FloatValue = serde_json::from_value(json!({ "a": 0, "k": [100] })).unwrap();
        assert_eq!(
            value.animated_property.value,
            AnimatedPropertyK::Static(100.0)
        );
        // It is written back as a number.
        assert_eq!(
            serde_json::to_value(&value).unwrap(),
            json!({ "a": 0, "k": 100.0 })
        );
    }

    #[test]
    fn keyframes_round_trip() {
        let source = json!({ "a": 1, "k": [{ "t": 0.0, "s": [1.0] }, { "t": 10.0, "s": [2.0] }] });
        let value: FloatValue = serde_json::from_value(source.clone()).unwrap();
        assert!(matches!(
            &value.animated_property.value,
            AnimatedPropertyK::AnimatedValue(keyframes) if keyframes.len() == 2
        ));
        assert_eq!(serde_json::to_value(&value).unwrap(), source);
    }
}
