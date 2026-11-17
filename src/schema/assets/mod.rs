// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

pub mod asset;
pub mod file_asset;
pub mod image;
pub mod precomposition;

use self::image::Image;
use self::precomposition::Precomposition;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum AnyAsset {
    Image(Image),
    Precomposition(Precomposition),
    // unimplemented - Sound(Sound),
    // unimplemented - DataSource(DataSource),
    /// An asset of a kind that is not modeled, such as a sound or a data source, kept as
    /// written.
    Unknown(serde_json::Value),
}

impl<'de> Deserialize<'de> for AnyAsset {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;

        let value = serde_json::Value::deserialize(deserializer)?;
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>")
            .to_owned();

        // Lottie assets have no single discriminator field
        if value.get("layers").is_some() {
            serde_json::from_value(value)
                .map(AnyAsset::Precomposition)
                .map_err(|e| D::Error::custom(format!("asset id={id:?} (Precomposition): {e}")))
        } else if value.get("t").and_then(|v| v.as_u64()) == Some(3) {
            // A data source.
            Ok(AnyAsset::Unknown(value))
        } else if value.get("w").is_some() || value.get("h").is_some() {
            serde_json::from_value(value)
                .map(AnyAsset::Image)
                .map_err(|e| D::Error::custom(format!("asset id={id:?} (Image): {e}")))
        } else {
            // A sound, or another kind of file without a size.
            Ok(AnyAsset::Unknown(value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AnyAsset;
    use serde_json::json;

    #[test]
    fn asset_of_unmodeled_kind_is_kept_as_written() {
        let sound = json!({ "id": "sound", "u": "sounds/", "p": "voice.mp3", "e": 0 });
        let data_source = json!({ "id": "data", "p": "data.json", "t": 3 });
        for source in [sound, data_source] {
            let asset: AnyAsset = serde_json::from_value(source.clone()).unwrap();
            assert_eq!(asset, AnyAsset::Unknown(source.clone()));
            assert_eq!(serde_json::to_value(&asset).unwrap(), source);
        }
    }
}
