use super::artifact::{
    cleanup_current_artifact, cleanup_file_if_exists, create_prepared_directory,
    ensure_no_reparse_components, open_or_quarantine_cached_installer, open_verified_installer,
    owner_record_path, write_owner_record, CachedInstaller, VerifiedInstaller, INSTALLER_LIMIT,
};
use super::emit_install_progress;
use super::network::{read_http_error, validate_download_url};
use super::release::{sanitize_file_name, VerifiedUpdate};
use crate::lifecycle::{PublicationKind, UpdateRunError, UpdateTaskContext};
use crate::models::UpdateInstallProgress;
use crate::notifications::UpdateProgressSink;
use futures_util::StreamExt;
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub(super) async fn download_installer(
    progress: &UpdateProgressSink,
    client: &Client,
    target_dir: &Path,
    verified: &VerifiedUpdate<'_>,
    context: &UpdateTaskContext,
) -> Result<VerifiedInstaller, UpdateRunError> {
    ensure_no_reparse_components(target_dir)?;
    let installer = verified.installer_asset;
    validate_download_url(&installer.browser_download_url)?;
    let file_name = sanitize_file_name(&verified.manifest.installer.file_name)?;
    let canonical_path = target_dir.join(&file_name);
    let publication_dir = match open_or_quarantine_cached_installer(
        target_dir,
        &canonical_path,
        verified.manifest.installer.size,
        &verified.manifest.installer.sha256,
    )
    .await?
    {
        CachedInstaller::Verified(installer) => {
            context.check_cancelled()?;
            return Ok(installer);
        }
        CachedInstaller::Vacant => target_dir.to_path_buf(),
        CachedInstaller::UnownedCollision => {
            create_prepared_directory(
                target_dir,
                verified.manifest.installer.size,
                &verified.manifest.installer.sha256,
            )
            .await?
        }
    };
    let final_path = publication_dir.join(&file_name);
    let part_path = publication_dir.join(format!("{file_name}.{}.part", uuid::Uuid::new_v4()));
    let response = tokio::select! {
        _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
        result = client.get(&installer.browser_download_url).send() => result,
    }
    .map_err(|error| format!("Failed to download update installer: {error}"))?;
    if !response.status().is_success() {
        let error = tokio::select! {
            _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
            error = read_http_error(response, "update installer") => error,
        };
        return Err(error.into());
    }
    if response
        .content_length()
        .is_some_and(|length| length != verified.manifest.installer.size)
    {
        return Err("Installer Content-Length does not match the signed size.".into());
    }

    let total_bytes = Some(verified.manifest.installer.size);
    let mut stream = response.bytes_stream();
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&part_path)
        .await
        .map_err(|error| format!("Failed to create installer temp file: {error}"))?;
    if let Err(error) = write_owner_record(
        &part_path,
        verified.manifest.installer.size,
        &verified.manifest.installer.sha256,
    )
    .await
    {
        drop(file);
        cleanup_file_if_exists(&part_path).await;
        return Err(error.into());
    }
    let mut downloaded_bytes = 0u64;
    let mut hasher = Sha256::new();
    emit_install_progress(
        progress,
        UpdateInstallProgress {
            status: "downloading".into(),
            version: verified.version.to_string(),
            downloaded_bytes,
            total_bytes,
            message: Some(format!("Downloading {file_name}...")),
        },
    );

    loop {
        let chunk_result = tokio::select! {
            _ = context.cancelled() => {
                cleanup_current_artifact(&part_path).await;
                return Err(UpdateRunError::Cancelled);
            }
            result = stream.next() => result,
        };
        let Some(chunk_result) = chunk_result else {
            break;
        };
        let chunk = match chunk_result {
            Ok(chunk) => chunk,
            Err(error) => {
                cleanup_current_artifact(&part_path).await;
                return Err(format!("Failed while downloading update installer: {error}").into());
            }
        };
        downloaded_bytes = downloaded_bytes
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| "Installer byte count overflowed.".to_string())?;
        if downloaded_bytes > verified.manifest.installer.size || downloaded_bytes > INSTALLER_LIMIT
        {
            cleanup_current_artifact(&part_path).await;
            return Err("Installer exceeded its signed size or the 1 GiB limit.".into());
        }
        if let Err(error) = file.write_all(&chunk).await {
            cleanup_current_artifact(&part_path).await;
            return Err(format!("Failed to write installer download: {error}").into());
        }
        hasher.update(&chunk);
        emit_install_progress(
            progress,
            UpdateInstallProgress {
                status: "downloading".into(),
                version: verified.version.to_string(),
                downloaded_bytes,
                total_bytes,
                message: Some(format!("Downloading {file_name}...")),
            },
        );
    }

    file.flush()
        .await
        .map_err(|error| format!("Failed to finalize installer download: {error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("Failed to sync installer download: {error}"))?;
    drop(file);
    if downloaded_bytes != verified.manifest.installer.size {
        cleanup_current_artifact(&part_path).await;
        return Err(format!(
            "Downloaded installer size mismatch: expected {} bytes, got {downloaded_bytes} bytes.",
            verified.manifest.installer.size
        )
        .into());
    }
    let actual_checksum = format!("{:x}", hasher.finalize());
    emit_install_progress(
        progress,
        UpdateInstallProgress {
            status: "verifying".into(),
            version: verified.version.to_string(),
            downloaded_bytes,
            total_bytes,
            message: Some(format!("Verifying {file_name}...")),
        },
    );
    if actual_checksum != verified.manifest.installer.sha256 {
        cleanup_current_artifact(&part_path).await;
        return Err("Downloaded installer SHA-256 does not match the signed manifest.".into());
    }
    let publication = match context.enter_publication(PublicationKind::InstallerCache) {
        Ok(publication) => publication,
        Err(error) => {
            cleanup_current_artifact(&part_path).await;
            return Err(error);
        }
    };
    if let Err(error) = ensure_no_reparse_components(target_dir) {
        cleanup_current_artifact(&part_path).await;
        return Err(error.into());
    }
    if fs::try_exists(&final_path).await.unwrap_or(false) {
        cleanup_current_artifact(&part_path).await;
        return Err("The updater destination appeared while publishing the installer.".into());
    }
    if let Err(error) = fs::hard_link(&part_path, &final_path).await {
        cleanup_current_artifact(&part_path).await;
        return Err(format!(
            "Failed to publish the verified installer without replacement: {error}"
        )
        .into());
    }
    let installer = open_verified_installer(
        &final_path,
        verified.manifest.installer.size,
        &verified.manifest.installer.sha256,
    )
    .await?
    .ok_or_else(|| {
        "The published installer changed before its execution lease was acquired.".to_string()
    })?;
    if let Err(error) = write_owner_record(
        &final_path,
        verified.manifest.installer.size,
        &verified.manifest.installer.sha256,
    )
    .await
    {
        cleanup_current_artifact(&part_path).await;
        return Err(error.into());
    }
    if let Err(error) = fs::remove_file(&part_path).await {
        eprintln!(
            "Published the verified installer, but failed to remove its partial link: {error}"
        );
    }
    if let Ok(record_path) = owner_record_path(&part_path) {
        cleanup_file_if_exists(&record_path).await;
    }
    drop(publication);
    context.check_cancelled()?;
    Ok(installer)
}
