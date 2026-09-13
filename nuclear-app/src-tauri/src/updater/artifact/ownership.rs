use super::{
    cleanup_file_if_exists, ensure_no_reparse_components, is_reparse_or_symlink, INSTALLER_LIMIT,
};
use crate::bounded_read::{read_bounded_async, BoundedReadError};
use crate::updater::release::{parse_semver, validate_sha256};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub(super) const OWNER_RECORD_SUFFIX: &str = ".nuclear-owner.json";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct OwnedArtifactRecord {
    pub(super) schema_version: u32,
    pub(super) artifact_name: String,
    pub(super) installer_size: u64,
    pub(super) installer_sha256: String,
}

pub(in crate::updater) fn is_owned_partial_installer_name(name: &str) -> bool {
    name.strip_prefix("Nuclear.Downloader_")
        .and_then(|rest| rest.split_once("_x64-setup.exe."))
        .and_then(|(version, suffix)| {
            let operation_id = suffix.strip_suffix(".part")?;
            (parse_semver(version).is_ok() && uuid::Uuid::parse_str(operation_id).is_ok())
                .then_some(())
        })
        .is_some()
}

pub(in crate::updater) fn is_exact_owned_installer_name(name: &str) -> bool {
    let Some(version_text) = name
        .strip_prefix("Nuclear.Downloader_")
        .and_then(|rest| rest.strip_suffix("_x64-setup.exe"))
    else {
        return false;
    };
    let Ok(version) = Version::parse(version_text) else {
        return false;
    };
    version.pre.is_empty() && version.build.is_empty() && version.to_string() == version_text
}

pub(in crate::updater) fn owner_record_path(artifact_path: &Path) -> Result<PathBuf, String> {
    let name = artifact_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "The updater artifact name is invalid.".to_string())?;
    Ok(artifact_path.with_file_name(format!("{name}{OWNER_RECORD_SUFFIX}")))
}

pub(super) async fn read_owner_record(
    artifact_path: &Path,
) -> Result<Option<(PathBuf, OwnedArtifactRecord)>, String> {
    let record_path = owner_record_path(artifact_path)?;
    let metadata = match fs::symlink_metadata(&record_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Failed to inspect updater ownership data: {error}")),
    };
    let bytes = read_owner_record_bytes(&record_path, &metadata).await?;
    let record: OwnedArtifactRecord = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Failed to parse updater ownership data: {error}"))?;
    let artifact_name = artifact_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "The updater artifact name is invalid.".to_string())?;
    if record.schema_version != 1
        || record.artifact_name != artifact_name
        || record.installer_size == 0
        || record.installer_size > INSTALLER_LIMIT
        || validate_sha256(&record.installer_sha256).is_err()
    {
        return Err("The updater ownership data does not match its artifact.".into());
    }
    Ok(Some((record_path, record)))
}

pub(in crate::updater) async fn read_owner_record_bytes(
    record_path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<Vec<u8>, String> {
    if !metadata.is_file() || metadata.len() > 4 * 1024 || is_reparse_or_symlink(record_path)? {
        return Err("The updater ownership data is not a regular bounded file.".into());
    }
    ensure_no_reparse_components(record_path)?;
    let mut file = fs::File::open(record_path)
        .await
        .map_err(|error| format!("Failed to read updater ownership data: {error}"))?;
    let opened_metadata = file
        .metadata()
        .await
        .map_err(|error| format!("Failed to inspect updater ownership data: {error}"))?;
    if !opened_metadata.is_file() || opened_metadata.len() > 4 * 1024 {
        return Err("The updater ownership data is not a regular bounded file.".into());
    }
    let bytes = match read_bounded_async(&mut file, 4 * 1024).await {
        Ok(bytes) => bytes,
        Err(BoundedReadError::LimitExceeded | BoundedReadError::InvalidLimit) => {
            return Err("The updater ownership data is not a regular bounded file.".into());
        }
        Err(BoundedReadError::Io(error)) => {
            return Err(format!("Failed to read updater ownership data: {error}"));
        }
    };
    Ok(bytes)
}

pub(in crate::updater) async fn write_owner_record(
    artifact_path: &Path,
    installer_size: u64,
    installer_sha256: &str,
) -> Result<PathBuf, String> {
    let record_path = owner_record_path(artifact_path)?;
    let artifact_name = artifact_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "The updater artifact name is invalid.".to_string())?;
    let record = OwnedArtifactRecord {
        schema_version: 1,
        artifact_name: artifact_name.to_string(),
        installer_size,
        installer_sha256: installer_sha256.to_string(),
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|error| format!("Failed to encode updater ownership data: {error}"))?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&record_path)
        .await
        .map_err(|error| format!("Failed to create updater ownership data: {error}"))?;
    if let Err(error) = file.write_all(&bytes).await {
        drop(file);
        cleanup_file_if_exists(&record_path).await;
        return Err(format!("Failed to write updater ownership data: {error}"));
    }
    if let Err(error) = file.sync_all().await {
        drop(file);
        cleanup_file_if_exists(&record_path).await;
        return Err(format!("Failed to sync updater ownership data: {error}"));
    }
    Ok(record_path)
}
