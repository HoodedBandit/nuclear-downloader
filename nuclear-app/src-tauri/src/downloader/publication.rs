mod destination;
mod resolution;
mod staging;

#[cfg(test)]
use super::process::DownloadJob;
#[cfg(test)]
use std::path::{Path, PathBuf};

pub(super) use destination::publish_staged_output;
pub(super) use resolution::resolve_staged_output;
#[cfg(test)]
use resolution::{
    final_output_record_path, resolve_recorded_output, MAX_FINAL_OUTPUT_RECORD_BYTES,
    TEST_FINAL_OUTPUT_RECORD_READ_BYTES,
};
pub(crate) use staging::cleanup_abandoned_download_stages;
pub(super) use staging::{build_staging_dir, cleanup_staging_with_warning, reset_staging_dir};
#[cfg(test)]
use staging::{
    cleanup_abandoned_download_stages_at, cleanup_staging_dir, verify_staging_marker,
    STAGING_MARKER_NAME, STAGING_ROOT_NAME, TEST_MARKER_INITIALIZATION_FAILURE,
    TEST_PARTIAL_MARKER_WRITE_FAILURE,
};

#[cfg(test)]
pub(crate) fn test_create_owned_stage(
    output_root: &Path,
    operation_id: &str,
) -> Result<PathBuf, String> {
    let stage = build_staging_dir(output_root, operation_id);
    reset_staging_dir(&stage, output_root, operation_id)?;
    Ok(stage)
}

#[cfg(test)]
pub(crate) async fn test_publish_staged_file(
    staged_output: &Path,
    desired_path: &Path,
    job: Option<&DownloadJob>,
) -> Result<PathBuf, String> {
    publish_staged_output(staged_output, desired_path, job).await
}

#[cfg(test)]
pub(crate) fn test_cleanup_owned_stage(
    output_root: &Path,
    operation_id: &str,
) -> Result<(), String> {
    cleanup_staging_dir(
        &build_staging_dir(output_root, operation_id),
        output_root,
        operation_id,
    )
}

#[cfg(test)]
pub(crate) fn test_cleanup_abandoned_stages(output_root: &Path) -> Result<(), String> {
    cleanup_abandoned_download_stages_at(output_root)
}

#[cfg(test)]
mod tests;
