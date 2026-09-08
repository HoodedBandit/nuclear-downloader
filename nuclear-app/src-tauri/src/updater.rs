mod artifact;
mod installer;
mod network;
mod release;

use crate::lifecycle::{UpdateRunError, UpdateTaskContext};
use crate::models::{UpdateCheckResult, UpdateInstallProgress};
use crate::notifications::UpdateProgressSink;
use installer::download_installer;
use network::{build_client, fetch_latest_release, updater_user_agent};
pub(crate) use release::{is_canonical_update_key_id, verify_release_signature_for_key};
use release::{
    normalize_optional_text, parse_release_tag, parse_semver, signed_update_minimum,
    verify_release_contract,
};
use std::time::Duration;

const INSTALLER_OVERALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[cfg(test)]
use network::{read_bounded_response, validate_download_url, GitHubRelease, GitHubReleaseAsset};
#[cfg(test)]
use release::{
    expected_installer_name, parse_and_validate_manifest, select_public_key,
    select_signed_update_assets, validate_sha256, validate_timestamp, verify_with_public_key,
};

#[cfg(test)]
pub(crate) use artifact::test_installer_handoff;
#[cfg(test)]
use artifact::UPDATE_LOCK_FILE_NAME;
pub(crate) use artifact::{cleanup_owned_installer_stages, InstallerHandoff};
use artifact::{
    cleanup_owned_old_installers, cleanup_owned_partial_installers,
    cleanup_owned_prepared_directories, updater_directory, UpdateDirectoryLock,
};
#[cfg(test)]
use artifact::{
    create_prepared_directory, open_or_quarantine_cached_installer, open_verified_installer,
    owner_record_path, write_owner_record, CachedInstaller,
};

pub async fn check_for_app_update(current_version: &str) -> Result<UpdateCheckResult, String> {
    let current_semver = parse_semver(current_version)?;
    let client = build_client(updater_user_agent(current_version))?;
    let release = fetch_latest_release(&client).await?;
    let latest_semver = parse_release_tag(&release.tag_name)?;
    let has_update = latest_semver > current_semver;
    let installer_name = if has_update {
        if latest_semver < signed_update_minimum() {
            return Err("Unsigned app releases are not accepted by this updater.".into());
        }
        Some(
            verify_release_contract(&client, &release, &latest_semver)
                .await?
                .installer_asset
                .name
                .clone(),
        )
    } else {
        None
    };

    Ok(UpdateCheckResult {
        current_version: current_version.to_string(),
        has_update,
        latest_version: Some(latest_semver.to_string()),
        notes: normalize_optional_text(release.body),
        published_at: normalize_optional_text(release.published_at),
        installer_name,
    })
}

pub(crate) async fn prepare_app_update(
    current_version: &str,
    progress: UpdateProgressSink,
    expected_version: String,
    context: &UpdateTaskContext,
) -> Result<InstallerHandoff, UpdateRunError> {
    prepare_app_update_inner(
        current_version,
        &progress,
        expected_version.clone(),
        context,
    )
    .await
    .inspect_err(|error| {
        let (status, message) = match error {
            UpdateRunError::Cancelled => ("cancelled", "App update was cancelled.".to_string()),
            UpdateRunError::Failed(error) => ("error", error.summary.clone()),
        };
        emit_install_progress(
            &progress,
            UpdateInstallProgress {
                status: status.into(),
                version: normalize_version_label(&expected_version),
                downloaded_bytes: 0,
                total_bytes: None,
                message: Some(message),
            },
        );
    })
}

async fn prepare_app_update_inner(
    current_version: &str,
    progress: &UpdateProgressSink,
    expected_version: String,
    context: &UpdateTaskContext,
) -> Result<InstallerHandoff, UpdateRunError> {
    #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
    {
        let _ = current_version;
        let _ = progress;
        let _ = expected_version;
        let _ = context;
        return Err("Automatic updates are supported only on Windows x64 builds.".into());
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        let expected_semver = parse_semver(&expected_version)?;
        let current_semver = parse_semver(current_version)?;
        if expected_semver < signed_update_minimum() {
            return Err("Unsigned app releases are not accepted by this updater.".into());
        }
        if expected_semver <= current_semver {
            return Err(format!(
                "No newer update is available. Current version is {current_semver}."
            )
            .into());
        }
        let client = build_client(updater_user_agent(&current_semver.to_string()))?;
        let release = tokio::select! {
            _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
            result = fetch_latest_release(&client) => result,
        }?;
        let latest_semver = parse_release_tag(&release.tag_name)?;
        if latest_semver != expected_semver {
            return Err(format!(
                "The latest GitHub release changed from {expected_semver} to {latest_semver}. Please check for updates again."
            )
            .into());
        }
        let verified = tokio::select! {
            _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
            result = verify_release_contract(&client, &release, &latest_semver) => result,
        }?;
        context.check_cancelled()?;
        let target_dir = updater_directory();
        let directory_lock = UpdateDirectoryLock::acquire(&target_dir)?;
        cleanup_owned_prepared_directories(&target_dir).await?;
        cleanup_owned_partial_installers(&target_dir).await?;
        cleanup_owned_old_installers(&target_dir, &verified.manifest.installer.file_name).await?;
        let installer = tokio::time::timeout(
            INSTALLER_OVERALL_TIMEOUT,
            download_installer(progress, &client, &target_dir, &verified, context),
        )
        .await
        .map_err(|_| "Update installer download exceeded the 30-minute limit.".to_string())??;
        context.check_cancelled()?;
        Ok(InstallerHandoff {
            expected_version: latest_semver.to_string(),
            installer_name: verified.manifest.installer.file_name.clone(),
            installer_size: verified.manifest.installer.size,
            installer,
            _directory_lock: directory_lock,
        })
    }
}

fn normalize_version_label(raw: &str) -> String {
    parse_semver(raw)
        .map(|version| version.to_string())
        .unwrap_or_else(|_| raw.trim().trim_start_matches('v').to_string())
}

fn emit_install_progress(progress: &UpdateProgressSink, payload: UpdateInstallProgress) {
    progress(payload);
}

#[cfg(test)]
mod tests;
