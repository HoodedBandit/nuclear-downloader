use super::{is_reparse_metadata, staging_root};
use std::path::{Path, PathBuf};

pub(super) fn validate_staging_layout(
    path: &Path,
    output_dir: &Path,
    operation_id: &str,
    require_operation: bool,
) -> Result<(PathBuf, PathBuf, String), String> {
    let normalized_id = uuid::Uuid::parse_str(operation_id.trim())
        .map_err(|_| "Staging operation ID was not a UUID.".to_string())?
        .to_string();
    let expected_lexical = staging_root(output_dir).join(&normalized_id);
    if path != expected_lexical {
        return Err("Refusing to use a staging folder outside the expected operation path.".into());
    }
    let output_metadata = std::fs::symlink_metadata(output_dir)
        .map_err(|error| format!("Failed to inspect output folder: {error}"))?;
    if !output_metadata.is_dir() || is_reparse_metadata(&output_metadata) {
        return Err("Output folder was not a regular non-reparse directory.".into());
    }
    let canonical_output = output_dir
        .canonicalize()
        .map_err(|error| format!("Failed to resolve output folder: {error}"))?;
    let root = staging_root(&canonical_output);
    if !require_operation {
        match std::fs::create_dir(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("Failed to create staging root: {error}")),
        }
    }
    match std::fs::symlink_metadata(&root) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_reparse_metadata(&metadata) {
                return Err("Staging root was not a regular non-reparse directory.".into());
            }
            let canonical_root = root
                .canonicalize()
                .map_err(|error| format!("Failed to resolve staging root: {error}"))?;
            if canonical_root.parent() != Some(canonical_output.as_path()) {
                return Err("Staging root escaped the output folder.".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("Staging root did not exist.".into());
        }
        Err(error) => return Err(format!("Failed to inspect staging root: {error}")),
    }
    let operation_path = root.join(&normalized_id);
    match std::fs::symlink_metadata(&operation_path) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_reparse_metadata(&metadata) {
                return Err("Staging operation was not a regular non-reparse directory.".into());
            }
            let canonical_operation = operation_path
                .canonicalize()
                .map_err(|error| format!("Failed to resolve staging operation: {error}"))?;
            if canonical_operation.parent() != Some(root.as_path()) {
                return Err("Staging operation escaped the staging root.".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !require_operation => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("Staging operation did not exist.".into());
        }
        Err(error) => return Err(format!("Failed to inspect staging operation: {error}")),
    }
    Ok((root, operation_path, normalized_id))
}
