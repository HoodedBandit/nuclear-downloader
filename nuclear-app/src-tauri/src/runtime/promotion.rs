use super::cache::RuntimeCache;
use super::manifest::{
    ensure_no_reparse_components, installed_runtime_is_owned, is_reparse_or_symlink,
    validate_manifest_at, validate_runtime_auth_contract, validate_runtime_version,
    version_sort_key, RuntimeCurrentPointer, RuntimeSignatureVerifier, RUNTIME_CURRENT_POINTER,
    RUNTIME_UPDATE_OWNER_MARKER,
};
use super::verified::{
    build_verified_runtime_snapshot_async, VerifiedRuntimeSnapshot, VERIFIED_RUNTIME_CACHE,
};
use crate::bounded_read::{read_bounded, read_bounded_async};
use crate::lifecycle::{PublicationKind, UpdateRunError, UpdateTaskContext};
use crate::runtime_transaction::{
    self, RuntimeMutationLock, RuntimeTransaction, RuntimeTransactionCheckpoint,
};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

const RUNTIME_UPDATE_OWNER_MARKER_BYTES: &[u8] = b"schemaVersion=1\n";

pub(super) async fn recover_runtime_update_transaction_at<F>(
    managed_root: &Path,
    validate: &F,
) -> Result<(), String>
where
    F: Fn(&Path, &str) -> bool,
{
    let _mutation_lock = RuntimeMutationLock::acquire(managed_root)?;
    let active = runtime_transaction::load(managed_root)?;
    let protected = runtime_transaction::protected_update_ids(managed_root)?;
    if protected.iter().any(|update_id| {
        active
            .as_ref()
            .is_none_or(|active| update_id != &active.update_id)
    }) {
        return Err(
            "A quarantined runtime transaction requires manual recovery before updates can continue."
                .into(),
        );
    }
    let Some(mut transaction) = active else {
        if !protected.is_empty() {
            return Err(
                "A quarantined runtime transaction requires manual recovery before updates can continue."
                    .into(),
            );
        }
        return Ok(());
    };
    let paths = transaction.paths(managed_root);
    let candidate_valid = runtime_update_work_root_is_owned(&paths.work_root)
        && validate(&paths.candidate, &transaction.runtime_version);
    let final_valid = validate(&paths.final_dir, &transaction.runtime_version);
    let backup_valid = validate(&paths.backup, &transaction.runtime_version);
    let candidate_exists = paths.candidate.exists();
    let final_exists = paths.final_dir.exists();
    let backup_exists = paths.backup.exists();

    let should_finish_candidate = candidate_valid
        && (transaction.checkpoint == RuntimeTransactionCheckpoint::CandidateVerified
            || !final_valid
            || (final_exists && !backup_exists));
    if should_finish_candidate {
        if final_exists {
            if !installed_runtime_is_owned(&paths.final_dir, &transaction.runtime_version)? {
                return quarantine_runtime_transaction(
                    managed_root,
                    &transaction,
                    "An unowned directory occupies the runtime destination.",
                );
            }
            if backup_exists {
                return quarantine_runtime_transaction(
                    managed_root,
                    &transaction,
                    "Both the runtime destination and backup exist before recovery can stage the old runtime.",
                );
            }
            fs::rename(&paths.final_dir, &paths.backup)
                .await
                .map_err(|error| {
                    format!("Failed to stage the old runtime during recovery: {error}")
                })?;
            transaction.set_checkpoint(RuntimeTransactionCheckpoint::OldMoved);
            runtime_transaction::store(managed_root, &transaction)?;
        } else if transaction.had_existing && !backup_exists {
            return quarantine_runtime_transaction(
                managed_root,
                &transaction,
                "The previous runtime is missing during transaction recovery.",
            );
        }
        if paths.final_dir.exists() {
            return quarantine_runtime_transaction(
                managed_root,
                &transaction,
                "The runtime destination reappeared during transaction recovery.",
            );
        }
        fs::rename(&paths.candidate, &paths.final_dir)
            .await
            .map_err(|error| {
                format!("Failed to publish the recovered runtime candidate: {error}")
            })?;
        transaction.set_checkpoint(RuntimeTransactionCheckpoint::NewPublished);
        runtime_transaction::store(managed_root, &transaction)?;
        return finish_recovered_runtime(managed_root, transaction, validate).await;
    }

    if final_valid && !candidate_exists {
        if transaction.checkpoint == RuntimeTransactionCheckpoint::CandidateVerified
            && transaction.had_existing
            && !backup_exists
        {
            return quarantine_runtime_transaction(
                managed_root,
                &transaction,
                "The verified candidate and previous-runtime backup are both missing.",
            );
        }
        transaction.set_checkpoint(RuntimeTransactionCheckpoint::NewPublished);
        runtime_transaction::store(managed_root, &transaction)?;
        return finish_recovered_runtime(managed_root, transaction, validate).await;
    }

    if backup_valid {
        if final_exists {
            if !installed_runtime_is_owned(&paths.final_dir, &transaction.runtime_version)? {
                return quarantine_runtime_transaction(
                    managed_root,
                    &transaction,
                    "Recovery cannot replace an unowned runtime destination with the authenticated backup.",
                );
            }
            let quarantine = managed_root.join(format!(
                ".runtime-quarantine-{}-{}",
                transaction.runtime_version, transaction.update_id
            ));
            if quarantine.exists() {
                return quarantine_runtime_transaction(
                    managed_root,
                    &transaction,
                    "The runtime quarantine destination already exists.",
                );
            }
            fs::rename(&paths.final_dir, &quarantine)
                .await
                .map_err(|error| format!("Failed to quarantine the invalid runtime: {error}"))?;
        }
        fs::rename(&paths.backup, &paths.final_dir)
            .await
            .map_err(|error| {
                format!("Failed to restore the authenticated runtime backup: {error}")
            })?;
        write_current_pointer(managed_root, &transaction.runtime_version).await?;
        runtime_transaction::clear(managed_root)?;
        return Ok(());
    }

    let reason = if candidate_exists || final_exists || backup_exists {
        "Runtime transaction artifacts exist, but none is an authenticated, integrity-valid recovery source."
    } else {
        "All runtime transaction artifacts are missing."
    };
    quarantine_runtime_transaction(managed_root, &transaction, reason)
}

pub(super) async fn finish_recovered_runtime<F>(
    managed_root: &Path,
    mut transaction: RuntimeTransaction,
    validate: &F,
) -> Result<(), String>
where
    F: Fn(&Path, &str) -> bool,
{
    let paths = transaction.paths(managed_root);
    if !validate(&paths.final_dir, &transaction.runtime_version) {
        return quarantine_runtime_transaction(
            managed_root,
            &transaction,
            "The published runtime failed authenticated integrity validation during recovery.",
        );
    }
    write_current_pointer(managed_root, &transaction.runtime_version).await?;
    transaction.set_checkpoint(RuntimeTransactionCheckpoint::CurrentPointerCommitted);
    runtime_transaction::store(managed_root, &transaction)?;
    if paths.backup.exists() {
        if !installed_runtime_is_owned(&paths.backup, &transaction.runtime_version)? {
            return quarantine_runtime_transaction(
                managed_root,
                &transaction,
                "Recovery retained an unowned runtime backup.",
            );
        }
        fs::remove_dir_all(&paths.backup)
            .await
            .map_err(|error| format!("Failed to remove the recovered runtime backup: {error}"))?;
    }
    transaction.set_checkpoint(RuntimeTransactionCheckpoint::BackupCleaned);
    runtime_transaction::store(managed_root, &transaction)?;
    runtime_transaction::clear(managed_root)
}

pub(super) fn authenticated_owned_runtime(path: &Path, version: &str) -> bool {
    let Ok(true) = installed_runtime_is_owned(path, version) else {
        return false;
    };
    let Ok(manifest) = validate_manifest_at(path, true) else {
        return false;
    };
    manifest.runtime_version == version && validate_runtime_auth_contract(path, &manifest).is_ok()
}

pub(super) fn runtime_update_work_root_is_owned(path: &Path) -> bool {
    if !path.is_dir() || ensure_no_reparse_components(path).is_err() {
        return false;
    }
    let marker = path.join(RUNTIME_UPDATE_OWNER_MARKER);
    marker.is_file()
        && is_reparse_or_symlink(&marker).ok() == Some(false)
        && runtime_update_owner_marker_is_exact(&marker)
}

fn runtime_update_owner_marker_is_exact(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    read_bounded(&mut file, RUNTIME_UPDATE_OWNER_MARKER_BYTES.len() as u64)
        .ok()
        .as_deref()
        == Some(RUNTIME_UPDATE_OWNER_MARKER_BYTES)
}

async fn runtime_update_owner_marker_is_exact_async(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path).await else {
        return false;
    };
    read_bounded_async(&mut file, RUNTIME_UPDATE_OWNER_MARKER_BYTES.len() as u64)
        .await
        .ok()
        .as_deref()
        == Some(RUNTIME_UPDATE_OWNER_MARKER_BYTES)
}

pub(super) fn quarantine_runtime_transaction(
    managed_root: &Path,
    transaction: &RuntimeTransaction,
    reason: &str,
) -> Result<(), String> {
    runtime_transaction::quarantine(managed_root, transaction)?;
    Err(format!(
        "Runtime transaction was quarantined without deleting its artifacts: {reason}"
    ))
}

pub(super) async fn cleanup_abandoned_runtime_updates_at(
    updates_root: &Path,
    protected: &std::collections::HashSet<String>,
) -> Result<(), String> {
    if !fs::try_exists(&updates_root).await.unwrap_or(false) {
        return Ok(());
    }
    ensure_no_reparse_components(updates_root)?;
    if is_reparse_or_symlink(updates_root)? {
        return Err("Runtime update staging root is a reparse point; cleanup was refused.".into());
    }
    let mut entries = fs::read_dir(&updates_root)
        .await
        .map_err(|error| format!("Failed to inspect runtime update staging: {error}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Failed to enumerate runtime update staging: {error}"))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if protected.contains(&name) {
            continue;
        }
        let path = entry.path();
        if uuid::Uuid::parse_str(&name).is_err()
            || !entry
                .file_type()
                .await
                .map_err(|error| format!("Failed to inspect runtime staging entry: {error}"))?
                .is_dir()
            || is_reparse_or_symlink(&path)?
        {
            continue;
        }
        let marker = path.join(RUNTIME_UPDATE_OWNER_MARKER);
        if !marker.is_file()
            || is_reparse_or_symlink(&marker)?
            || !runtime_update_owner_marker_is_exact_async(&marker).await
        {
            continue;
        }
        remove_owned_runtime_update_dir(&path).await?;
    }
    Ok(())
}

pub(super) fn authorize_runtime_replacement(
    final_dir: &Path,
    version: &str,
) -> Result<bool, String> {
    let had_existing = final_dir.exists();
    if had_existing && !installed_runtime_is_owned(final_dir, version)? {
        return Err("Refusing to replace an unowned managed runtime directory.".into());
    }
    Ok(had_existing)
}

pub(super) struct RuntimePublicationRequest {
    pub(super) managed_root: PathBuf,
    pub(super) candidate_dir: PathBuf,
    pub(super) update_id: String,
    pub(super) runtime_version: String,
    pub(super) bundled_root: Option<PathBuf>,
    pub(super) signature_verifier: RuntimeSignatureVerifier,
}

pub(super) struct RuntimePublicationOutcome {
    pub(super) installed_version: String,
    pub(super) warnings: Vec<String>,
}

pub(super) async fn publish_verified_candidate(
    request: RuntimePublicationRequest,
    context: &UpdateTaskContext,
) -> Result<RuntimePublicationOutcome, UpdateRunError> {
    publish_verified_candidate_with_cache(request, context, &VERIFIED_RUNTIME_CACHE).await
}

pub(super) async fn publish_verified_candidate_with_cache(
    request: RuntimePublicationRequest,
    context: &UpdateTaskContext,
    runtime_cache: &RuntimeCache<VerifiedRuntimeSnapshot>,
) -> Result<RuntimePublicationOutcome, UpdateRunError> {
    let _mutation_lock = RuntimeMutationLock::acquire(&request.managed_root)?;
    let final_dir = request.managed_root.join(&request.runtime_version);
    let had_existing = authorize_runtime_replacement(&final_dir, &request.runtime_version)?;
    let mut transaction = RuntimeTransaction::new(
        request.update_id,
        request.runtime_version.clone(),
        had_existing,
    )?;
    let cache_mutation = tokio::select! {
        biased;
        _ = context.cancelled() => return Err(UpdateRunError::Cancelled),
        mutation = runtime_cache.begin_mutation() => mutation,
    };
    context.check_cancelled()?;
    let publication = context.enter_publication(PublicationKind::RuntimeCommit)?;
    runtime_transaction::store(&request.managed_root, &transaction)?;
    let paths = transaction.paths(&request.managed_root);
    let mut warnings = Vec::new();
    let promotion = promote_runtime_atomically(
        &request.managed_root,
        &mut transaction,
        &request.candidate_dir,
    )
    .await?;
    if let Err(pointer_error) =
        write_current_pointer(&request.managed_root, &request.runtime_version).await
    {
        let rollback_error = rollback_runtime_promotion(
            &request.candidate_dir,
            &paths.final_dir,
            &paths.backup,
            promotion.had_existing,
        )
        .await
        .err();
        if rollback_error.is_none() {
            let _ = runtime_transaction::clear(&request.managed_root);
        }
        return Err(match rollback_error {
            Some(rollback_error) => format!(
                "{pointer_error} Runtime publication rollback also failed: {rollback_error}"
            )
            .into(),
            None => pointer_error.into(),
        });
    }
    transaction.set_checkpoint(RuntimeTransactionCheckpoint::CurrentPointerCommitted);
    let pointer_checkpoint_error =
        runtime_transaction::store(&request.managed_root, &transaction).err();
    publication.commit();
    if let Some(error) = pointer_checkpoint_error {
        warnings.push(format!(
            "Runtime is active, but its pointer checkpoint could not be persisted: {error}"
        ));
    } else {
        if let Some(warning) =
            cleanup_runtime_promotion_backup(&paths.backup, &request.runtime_version, promotion)
                .await?
        {
            warnings.push(warning);
        }
        transaction.set_checkpoint(RuntimeTransactionCheckpoint::BackupCleaned);
        if let Err(error) = runtime_transaction::store(&request.managed_root, &transaction) {
            warnings.push(format!(
                "Runtime is active, but its cleanup checkpoint could not be persisted: {error}"
            ));
        } else if let Err(error) = runtime_transaction::clear(&request.managed_root) {
            warnings.push(format!(
                "Runtime is active, but its completed transaction record remains: {error}"
            ));
        }
    }
    if let Err(warning) =
        cleanup_old_runtime_versions(&request.managed_root, &request.runtime_version).await
    {
        warnings.push(warning);
    }
    let refreshed_cache = build_verified_runtime_snapshot_async(
        request.managed_root,
        request.bundled_root,
        request.signature_verifier,
        CancellationToken::new(),
    )
    .await;
    if let Err(error) = &refreshed_cache {
        warnings.push(format!(
            "Runtime was published, but its verified runtime cache could not be refreshed: {error}"
        ));
    }
    cache_mutation.publish(refreshed_cache);
    Ok(RuntimePublicationOutcome {
        installed_version: request.runtime_version,
        warnings,
    })
}

pub(super) async fn finalize_update_workspace(
    managed_root: &Path,
    work_root: &Path,
    update_id: &str,
) -> Option<String> {
    let cleanup_lock = RuntimeMutationLock::acquire(managed_root);
    let preserve_work_root = match &cleanup_lock {
        Ok(_) => match runtime_transaction::protected_update_ids(managed_root) {
            Ok(protected) => protected.contains(update_id),
            Err(error) => {
                eprintln!(
                    "Runtime staging was retained because transaction protection could not be read: {error}"
                );
                true
            }
        },
        Err(error) => {
            eprintln!(
                "Runtime staging was retained because the mutation lock was unavailable: {error}"
            );
            true
        }
    };
    let cleanup_warning = if preserve_work_root {
        None
    } else {
        remove_owned_runtime_update_dir(work_root).await.err()
    };
    drop(cleanup_lock);
    cleanup_warning
}

#[derive(Clone, Copy)]
pub(super) struct RuntimePromotion {
    pub(super) had_existing: bool,
}

pub(super) async fn promote_runtime_atomically(
    managed_root: &Path,
    transaction: &mut RuntimeTransaction,
    candidate_dir: &Path,
) -> Result<RuntimePromotion, String> {
    let paths = transaction.paths(managed_root);
    if candidate_dir != paths.candidate {
        return Err("Runtime candidate does not match its durable transaction path.".into());
    }
    if transaction.had_existing {
        if fs::try_exists(&paths.backup).await.unwrap_or(false) {
            return Err("Refusing to overwrite an unexpected runtime backup path.".into());
        }
        fs::rename(&paths.final_dir, &paths.backup)
            .await
            .map_err(|error| {
                format!("Failed to stage the existing runtime for replacement: {error}")
            })?;
        transaction.set_checkpoint(RuntimeTransactionCheckpoint::OldMoved);
        if let Err(checkpoint_error) = runtime_transaction::store(managed_root, transaction) {
            let rollback_error = fs::rename(&paths.backup, &paths.final_dir).await.err();
            if rollback_error.is_none() {
                let _ = runtime_transaction::clear(managed_root);
            }
            return Err(match rollback_error {
                Some(rollback_error) => format!(
                    "Failed to persist the old-runtime checkpoint: {checkpoint_error}. Restoring the previous runtime also failed: {rollback_error}"
                ),
                None => format!(
                    "Failed to persist the old-runtime checkpoint: {checkpoint_error}"
                ),
            });
        }
    }

    if let Err(error) = fs::rename(candidate_dir, &paths.final_dir).await {
        if transaction.had_existing {
            if let Err(rollback_error) = fs::rename(&paths.backup, &paths.final_dir).await {
                return Err(format!(
                    "Failed to publish runtime bundle: {error}. Restoring the previous runtime also failed: {rollback_error}"
                ));
            }
        }
        let _ = runtime_transaction::clear(managed_root);
        return Err(format!("Failed to publish runtime bundle: {error}"));
    }
    transaction.set_checkpoint(RuntimeTransactionCheckpoint::NewPublished);
    if let Err(checkpoint_error) = runtime_transaction::store(managed_root, transaction) {
        let rollback_error = rollback_runtime_promotion(
            candidate_dir,
            &paths.final_dir,
            &paths.backup,
            transaction.had_existing,
        )
        .await
        .err();
        if rollback_error.is_none() {
            let _ = runtime_transaction::clear(managed_root);
        }
        return Err(match rollback_error {
            Some(rollback_error) => format!(
                "Failed to persist the new-runtime checkpoint: {checkpoint_error}. Runtime rollback also failed: {rollback_error}"
            ),
            None => format!("Failed to persist the new-runtime checkpoint: {checkpoint_error}"),
        });
    }

    Ok(RuntimePromotion {
        had_existing: transaction.had_existing,
    })
}

pub(super) async fn rollback_runtime_promotion(
    candidate_dir: &Path,
    final_dir: &Path,
    backup_dir: &Path,
    had_existing: bool,
) -> Result<(), String> {
    fs::rename(final_dir, candidate_dir)
        .await
        .map_err(|error| format!("Failed to withdraw the newly published runtime: {error}"))?;
    if had_existing {
        if let Err(error) = fs::rename(backup_dir, final_dir).await {
            let restore_new_error = fs::rename(candidate_dir, final_dir).await.err();
            return Err(match restore_new_error {
                Some(restore_new_error) => format!(
                    "Failed to restore the previous runtime: {error}. Restoring the new runtime also failed: {restore_new_error}"
                ),
                None => format!("Failed to restore the previous runtime: {error}"),
            });
        }
    }
    Ok(())
}

pub(super) async fn cleanup_runtime_promotion_backup(
    backup_dir: &Path,
    final_version: &str,
    promotion: RuntimePromotion,
) -> Result<Option<String>, String> {
    if !promotion.had_existing {
        return Ok(None);
    }
    let warning = match installed_runtime_is_owned(backup_dir, final_version) {
        Ok(true) => fs::remove_dir_all(backup_dir).await.err().map(|error| {
            format!(
                "Runtime is ready, but cleanup of backup {} failed: {error}",
                backup_dir.display()
            )
        }),
        Ok(false) => Some(format!(
            "Runtime is ready, but backup ownership changed; cleanup of {} was refused.",
            backup_dir.display()
        )),
        Err(error) => Some(format!(
            "Runtime is ready, but backup validation failed and cleanup was refused: {error}"
        )),
    };
    Ok(warning)
}

pub(super) async fn write_current_pointer(
    root: &Path,
    runtime_version: &str,
) -> Result<(), String> {
    validate_runtime_version(runtime_version)?;
    ensure_no_reparse_components(root)?;
    let pointer = RuntimeCurrentPointer {
        schema_version: 1,
        runtime_version: runtime_version.to_string(),
    };
    let bytes = serde_json::to_vec(&pointer)
        .map_err(|error| format!("Failed to serialize runtime pointer: {error}"))?;
    let temporary = root.join(format!(".current-{}.json.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .await
        .map_err(|error| format!("Failed to create runtime pointer: {error}"))?;
    file.write_all(&bytes)
        .await
        .map_err(|error| format!("Failed to write runtime pointer: {error}"))?;
    file.flush()
        .await
        .map_err(|error| format!("Failed to flush runtime pointer: {error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("Failed to sync runtime pointer: {error}"))?;
    drop(file);
    let destination = root.join(RUNTIME_CURRENT_POINTER);
    if destination.exists() {
        ensure_no_reparse_components(&destination)?;
        if is_reparse_or_symlink(&destination)? {
            let _ = std::fs::remove_file(&temporary);
            return Err("Runtime pointer destination is a reparse point.".into());
        }
    }
    replace_file_atomically(&temporary, &destination).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("Failed to publish runtime pointer: {error}")
    })
}

#[cfg(windows)]
pub(super) fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both paths are valid, nul-terminated UTF-16 buffers for the
    // duration of this call. Flags request same-volume replacement and flush.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
pub(super) fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

pub(super) async fn cleanup_old_runtime_versions(root: &Path, current: &str) -> Result<(), String> {
    let mut candidates = Vec::new();
    let mut stale_backups = Vec::new();
    let mut entries = fs::read_dir(root)
        .await
        .map_err(|error| format!("Failed to inspect managed runtimes: {error}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Failed to enumerate managed runtimes: {error}"))?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|error| format!("Failed to inspect a managed runtime: {error}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_dir() && name.starts_with(".backup-") {
            if let Ok(manifest) = validate_manifest_at(&entry.path(), false) {
                if installed_runtime_is_owned(&entry.path(), &manifest.runtime_version)? {
                    stale_backups.push((entry.path(), manifest.runtime_version));
                }
            }
            continue;
        }
        if !file_type.is_dir()
            || name.starts_with('.')
            || name == current
            || validate_runtime_version(&name).is_err()
        {
            continue;
        }
        if validate_manifest_at(&entry.path(), false).is_ok()
            && installed_runtime_is_owned(&entry.path(), &name)?
        {
            candidates.push((version_sort_key(&name), entry.path(), name));
        }
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    for (_, path, version) in candidates.into_iter().skip(1) {
        if !installed_runtime_is_owned(&path, &version)? {
            return Err(format!(
                "Refusing to remove runtime {} because ownership changed.",
                path.display()
            ));
        }
        fs::remove_dir_all(&path).await.map_err(|error| {
            format!(
                "Runtime was installed, but cleanup of {} failed: {error}",
                path.display()
            )
        })?;
    }
    for (path, version) in stale_backups {
        if !installed_runtime_is_owned(&path, &version)? {
            return Err(format!(
                "Refusing to remove backup {} because ownership changed.",
                path.display()
            ));
        }
        fs::remove_dir_all(&path).await.map_err(|error| {
            format!(
                "Runtime is ready, but cleanup of stale backup {} failed: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

pub(super) async fn remove_owned_runtime_update_dir(path: &Path) -> Result<(), String> {
    if !fs::try_exists(path).await.unwrap_or(false) {
        return Ok(());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Runtime update staging has an invalid name.".to_string())?;
    if uuid::Uuid::parse_str(name).is_err() || ensure_no_reparse_components(path).is_err() {
        return Err("Refusing to remove an unowned or reparse runtime staging path.".into());
    }
    let marker = path.join(RUNTIME_UPDATE_OWNER_MARKER);
    if !marker.is_file()
        || is_reparse_or_symlink(&marker)?
        || !runtime_update_owner_marker_is_exact_async(&marker).await
    {
        return Err(
            "Refusing to remove runtime staging without its exact ownership marker.".into(),
        );
    }
    ensure_no_reparse_components(path)?;
    fs::remove_dir_all(path).await.map_err(|error| {
        format!(
            "Cleanup of owned runtime staging directory {} failed: {error}",
            path.display()
        )
    })
}
