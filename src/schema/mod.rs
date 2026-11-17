// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The schema for a Lottie `Animation`.
//!
//! Raw schema: <https://lottiefiles.github.io/lottie-docs/schema/lottie.schema.json>

pub mod animated_properties;
pub mod animation;
pub mod assets;
pub mod constants;
// todo effect-values
// todo effects
pub mod helpers;
pub mod layers;
pub mod shapes;
pub mod styles;
//todo text

pub use animation::animation::Animation;

#[cfg(test)]
mod tests {
    use crate::schema::{
        animation::composition::Composition, helpers::visual_object::VisualObject,
    };

    use super::{Animation, helpers::int_boolean::BoolInt};
    use serde_json::json;
    use std::sync::LazyLock;

    static JSON: LazyLock<serde_json::Value> = LazyLock::new(|| {
        json!(
            {
                "v": "5.5.2",
                "nm": "Example",
                "fr": 60,
                "ip": 0,
                "op": 60,
                "w": 512,
                "h": 512,
                "ddd": 0,
                "layers": []
            }
        )
    });

    static LOTTIE: LazyLock<Animation> = LazyLock::new(|| Animation {
        ver: None,
        visual_object: VisualObject {
            name: Some("Example".to_string()),
            ..Default::default()
        },
        composition: Composition { layers: vec![] },
        version: Some("5.5.2".to_string()),
        frame_rate: 60.0,
        in_point: 0.0,
        out_point: 60.0,
        width: 512,
        height: 512,
        three_dimensional: Some(BoolInt::False),
        assets: None,
        markers: None,
        meta: None,
        metadata: None,
    });

    #[test]
    fn test_serde_deserialize() {
        let actual = serde_json::from_value(JSON.to_owned());

        match actual {
            Ok(actual) => assert_eq!(*LOTTIE, actual),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn test_deserialize() {
        let actual = Animation::from_json(JSON.to_owned());

        match actual {
            Ok(actual) => assert_eq!(*LOTTIE, actual),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn test_can_serialize() {
        serde_json::to_value(&*LOTTIE).unwrap();
    }

    #[test]
    fn fractional_size_is_rounded_up() {
        let mut source = JSON.to_owned();
        source["w"] = json!(58.84);
        source["h"] = json!(100.0);
        let animation = Animation::from_json(source).unwrap();
        assert_eq!((animation.width, animation.height), (59, 100));
    }

    #[test]
    fn negative_size_is_rejected() {
        let mut source = JSON.to_owned();
        source["w"] = json!(-1);
        assert!(Animation::from_json(source).is_err());
    }

    #[test]
    fn value_that_nests_too_deep_is_refused_without_recursion() {
        // Arrays and objects in turn, a hundred thousand deep. Reading such a value, or
        // only dropping it, recurses once per level.
        let nested = |levels: usize| {
            let mut value = json!(1);
            for level in 0..levels {
                // Put together by hand: `json!` would copy the value, level by level.
                value = if level % 2 == 0 {
                    serde_json::Value::Array(vec![value])
                } else {
                    serde_json::Value::Object([("layers".to_owned(), value)].into_iter().collect())
                };
            }
            value
        };
        // On a stack that a few hundred levels of recursion fit.
        let refused = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let error = Animation::from_json(nested(100_000)).unwrap_err();
                assert!(error.to_string().contains("recursion limit"), "{error}");
                let composition = crate::Composition::from_json(nested(100_000));
                assert!(matches!(composition, Err(crate::Error::Json(_))));
            })
            .unwrap()
            .join();
        assert!(refused.is_ok());

        // What the parser takes as text is taken as a value, and no more.
        let mut at_limit = JSON.to_owned();
        let mut one_more = JSON.to_owned();
        // The document is the first level and the array of layers the second.
        at_limit["layers"] = serde_json::Value::Array(vec![nested(125)]);
        one_more["layers"] = serde_json::Value::Array(vec![nested(126)]);
        let text = |value: &serde_json::Value| serde_json::to_string(value).unwrap();
        let recursion = |error: serde_json::Error| error.to_string().contains("recursion limit");
        // A layer without a type is kept as it is, so the first is an animation.
        assert!(Animation::from_json(at_limit.clone()).is_ok());
        assert!(text(&at_limit).parse::<Animation>().is_ok());
        assert!(recursion(
            Animation::from_json(one_more.clone()).unwrap_err()
        ));
        assert!(recursion(text(&one_more).parse::<Animation>().unwrap_err()));
    }

    #[test]
    fn size_round_trips() {
        let value = serde_json::to_value(&*LOTTIE).unwrap();
        assert_eq!((&value["w"], &value["h"]), (&json!(512), &json!(512)));
        assert_eq!(Animation::from_json(value).unwrap(), *LOTTIE);
    }
}
