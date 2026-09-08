use serde_json::{Map, Value};

pub fn merge_external_bins(
    existing: Option<&str>,
    external_bins: &[&str],
) -> Result<String, String> {
    let mut config = match existing {
        Some(raw) if !raw.trim().is_empty() => serde_json::from_str::<Value>(raw)
            .map_err(|error| format!("TAURI_CONFIG is not valid JSON: {error}"))?,
        _ => Value::Object(Map::new()),
    };

    let root = config
        .as_object_mut()
        .ok_or_else(|| "TAURI_CONFIG must be a JSON object.".to_string())?;
    let bundle = root
        .entry("bundle")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| "TAURI_CONFIG.bundle must be a JSON object.".to_string())?;

    let mut merged_bins = match bundle.remove("externalBin") {
        Some(Value::Array(values)) => values
            .into_iter()
            .map(|value| match value {
                Value::String(value) => Ok(value),
                _ => Err("TAURI_CONFIG.bundle.externalBin entries must be strings.".to_string()),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err("TAURI_CONFIG.bundle.externalBin must be an array.".to_string());
        }
        None => Vec::new(),
    };

    for external_bin in external_bins {
        if !merged_bins.iter().any(|value| value == external_bin) {
            merged_bins.push((*external_bin).to_string());
        }
    }

    bundle.insert(
        "externalBin".to_string(),
        Value::Array(merged_bins.into_iter().map(Value::String).collect()),
    );

    serde_json::to_string(&config)
        .map_err(|error| format!("Failed to serialize merged TAURI_CONFIG: {error}"))
}

pub fn validate_update_key_configuration(
    profile: &str,
    current_id: &str,
    current_key: &str,
    next_id: &str,
    next_key: &str,
) -> Result<(), String> {
    if profile == "release" && (current_id.is_empty() || current_key.is_empty()) {
        return Err(
            "Release builds require NUCLEAR_UPDATE_KEY_ID and NUCLEAR_UPDATE_PUBLIC_KEY so update manifests are authenticated."
                .into(),
        );
    }
    if current_id.is_empty() != current_key.is_empty() {
        return Err(
            "The current updater key ID and public key must be configured together.".into(),
        );
    }
    if next_id.is_empty() != next_key.is_empty() {
        return Err("The next updater key ID and public key must be configured together.".into());
    }
    for (label, key_id) in [("current", current_id), ("next", next_id)] {
        if !key_id.is_empty() && !crate::artifact_contract::is_canonical_update_key_id(key_id) {
            return Err(format!(
                "The {label} updater key ID must use 1-64 ASCII letters, digits, '.', '_', or '-'."
            ));
        }
    }
    if !next_id.is_empty() && next_id == current_id {
        return Err("The current and next updater key IDs must be different.".into());
    }
    for (label, public_key) in [("current", current_key), ("next", next_key)] {
        if !public_key.is_empty() {
            crate::artifact_contract::parse_tauri_update_public_key(public_key).map_err(
                |error| format!("The embedded {label} updater public key is invalid: {error}"),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{merge_external_bins, validate_update_key_configuration};
    use serde_json::Value;

    #[test]
    fn preserves_existing_overlay_fields() {
        let merged = merge_external_bins(
            Some(r#"{"productName":"Preview","bundle":{"targets":["nsis"]}}"#),
            &["binaries/yt-dlp", "binaries/ffmpeg"],
        )
        .expect("overlay should merge");
        let value: Value = serde_json::from_str(&merged).expect("merged config should be JSON");

        assert_eq!(value["productName"], "Preview");
        assert_eq!(value["bundle"]["targets"][0], "nsis");
        assert_eq!(value["bundle"]["externalBin"][0], "binaries/yt-dlp");
    }

    #[test]
    fn rejects_non_object_overlays() {
        let error = merge_external_bins(Some("[]"), &["binaries/yt-dlp"])
            .expect_err("array overlay should fail");
        assert!(error.contains("JSON object"));
    }

    #[test]
    fn preserves_and_deduplicates_existing_external_bins() {
        let merged = merge_external_bins(
            Some(r#"{"bundle":{"externalBin":["binaries/custom","binaries/yt-dlp"]}}"#),
            &["binaries/yt-dlp", "binaries/ffmpeg"],
        )
        .expect("external bins should merge");
        let value: Value = serde_json::from_str(&merged).expect("merged config should be JSON");

        assert_eq!(
            value["bundle"]["externalBin"],
            serde_json::json!(["binaries/custom", "binaries/yt-dlp", "binaries/ffmpeg"])
        );
    }

    #[test]
    fn rejects_non_array_external_bins() {
        let error = merge_external_bins(
            Some(r#"{"bundle":{"externalBin":"binaries/yt-dlp"}}"#),
            &["binaries/ffmpeg"],
        )
        .expect_err("non-array externalBin should fail");

        assert!(error.contains("externalBin must be an array"));
    }

    #[test]
    fn release_key_configuration_rejects_malformed_public_key_wrapper() {
        let error = validate_update_key_configuration(
            "release",
            "release-key-1",
            "not-tauri-base64",
            "",
            "",
        )
        .unwrap_err();

        assert!(error.contains("public key"));
    }

    #[test]
    fn release_key_configuration_accepts_valid_public_key_wrapper() {
        validate_update_key_configuration(
            "release",
            "release-key-1",
            crate::artifact_contract::TEST_TAURI_UPDATE_PUBLIC_KEY,
            "",
            "",
        )
        .expect("the repository Minisign fixture should be accepted");
    }

    #[test]
    fn debug_key_configuration_accepts_empty_rotation_slots() {
        validate_update_key_configuration("debug", "", "", "", "")
            .expect("debug builds may omit updater keys");
    }

    #[test]
    fn release_key_configuration_rejects_malformed_optional_next_key() {
        let error = validate_update_key_configuration(
            "release",
            "release-key-1",
            crate::artifact_contract::TEST_TAURI_UPDATE_PUBLIC_KEY,
            "release-key-2",
            "not-tauri-base64",
        )
        .unwrap_err();

        assert!(error.contains("next updater public key"));
    }
}
