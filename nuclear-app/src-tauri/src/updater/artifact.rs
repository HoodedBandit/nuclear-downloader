use super::{parse_semver, validate_sha256};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[cfg(test)]
use crate::lifecycle::UpdateRunError;

const UPDATE_DIRECTORY_NAME: &str = "updater";
pub(super) const UPDATE_LOCK_FILE_NAME: &str = "update.lock";
const OWNER_RECORD_SUFFIX: &str = ".nuclear-owner.json";
pub(super) const INSTALLER_LIMIT: u64 = 1024 * 1024 * 1024;

/// A cryptographically verified installer whose open file handle prevents
/// write/delete sharing until Windows has successfully created the process.
/// The path alone is never treated as the authorization to execute.
pub(super) struct VerifiedInstaller {
    path: PathBuf,
    _read_lease: std::fs::File,
}

impl VerifiedInstaller {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

pub(super) enum CachedInstaller {
    Vacant,
    Verified(VerifiedInstaller),
    UnownedCollision,
}

/// A fully verified installer whose file and updater-directory leases remain
/// held until the caller has persisted the handoff and created the process.
pub(crate) struct InstallerHandoff {
    pub(super) expected_version: String,
    pub(super) installer_name: String,
    pub(super) installer_size: u64,
    pub(super) installer: VerifiedInstaller,
    pub(super) _directory_lock: UpdateDirectoryLock,
}

impl InstallerHandoff {
    pub(crate) fn expected_version(&self) -> &str {
        &self.expected_version
    }

    pub(crate) fn installer_name(&self) -> &str {
        &self.installer_name
    }

    pub(crate) fn installer_size(&self) -> u64 {
        self.installer_size
    }

    pub(crate) fn installer_path(&self) -> &Path {
        self.installer.path()
    }
}

/// Holds an open, non-shareable file on Windows. A crashed process releases the
/// OS handle, so a stale marker cannot permanently wedge the updater.
pub(super) struct UpdateDirectoryLock {
    file: Option<std::fs::File>,
}

impl UpdateDirectoryLock {
    pub(super) fn acquire(directory: &Path) -> Result<Self, String> {
        if let Some(parent) = directory.parent() {
            ensure_no_reparse_components(parent)?;
        }
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("Failed to create update temp folder: {error}"))?;
        ensure_no_reparse_components(directory)?;
        let directory_metadata = std::fs::symlink_metadata(directory)
            .map_err(|error| format!("Failed to inspect update temp folder: {error}"))?;
        if !directory_metadata.is_dir() || is_reparse_or_symlink(directory)? {
            return Err("The updater directory must be a regular non-reparse directory.".into());
        }
        let path = directory.join(UPDATE_LOCK_FILE_NAME);
        if path.exists() {
            ensure_no_reparse_components(&path)?;
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("Failed to inspect updater lock: {error}"))?;
            if !metadata.is_file() || is_reparse_or_symlink(&path)? {
                return Err("The updater lock must be a regular non-reparse file.".into());
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options.open(&path).map_err(|error| {
            format!("Another updater process owns the update directory: {error}")
        })?;
        ensure_no_reparse_components(&path)?;
        if is_reparse_or_symlink(&path)? {
            return Err("The updater lock became a reparse point.".into());
        }
        Ok(Self { file: Some(file) })
    }
}

impl Drop for UpdateDirectoryLock {
    fn drop(&mut self) {
        drop(self.file.take());
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnedArtifactRecord {
    schema_version: u32,
    artifact_name: String,
    installer_size: u64,
    installer_sha256: String,
}

pub(crate) async fn cleanup_owned_installer_stages() -> Result<(), String> {
    let target_dir = updater_directory();
    let _directory_lock = UpdateDirectoryLock::acquire(&target_dir)?;
    cleanup_owned_prepared_directories(&target_dir).await?;
    cleanup_owned_partial_installers(&target_dir).await
}

#[cfg(test)]
pub(crate) async fn test_installer_handoff(
    root: &Path,
    expected_version: &str,
) -> Result<InstallerHandoff, UpdateRunError> {
    let version = parse_semver(expected_version)?;
    if version.to_string() != expected_version {
        return Err("The test installer version must be canonical SemVer.".into());
    }

    let directory_lock = UpdateDirectoryLock::acquire(root)?;
    let installer_name = format!("Nuclear.Downloader_{expected_version}_x64-setup.exe");
    let installer_path = root.join(&installer_name);
    const INSTALLER_BYTES: &[u8] = b"nuclear-test-installer";
    let installer_sha256 = format!("{:x}", Sha256::digest(INSTALLER_BYTES));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&installer_path)
        .await
        .map_err(|error| format!("Failed to create test installer: {error}"))?;
    let write_result = async {
        file.write_all(INSTALLER_BYTES).await?;
        file.sync_all().await
    }
    .await;
    if let Err(error) = write_result {
        drop(file);
        cleanup_file_if_exists(&installer_path).await;
        return Err(format!("Failed to persist test installer: {error}").into());
    }
    drop(file);

    if let Err(error) = write_owner_record(
        &installer_path,
        INSTALLER_BYTES.len() as u64,
        &installer_sha256,
    )
    .await
    {
        cleanup_current_artifact(&installer_path).await;
        return Err(error.into());
    }
    let installer = match open_verified_installer(
        &installer_path,
        INSTALLER_BYTES.len() as u64,
        &installer_sha256,
    )
    .await
    {
        Ok(Some(installer)) => installer,
        Ok(None) => {
            cleanup_current_artifact(&installer_path).await;
            return Err("The test installer could not acquire its verified lease.".into());
        }
        Err(error) => {
            cleanup_current_artifact(&installer_path).await;
            return Err(error.into());
        }
    };

    Ok(InstallerHandoff {
        expected_version: expected_version.to_string(),
        installer_name,
        installer_size: INSTALLER_BYTES.len() as u64,
        installer,
        _directory_lock: directory_lock,
    })
}

pub(super) async fn cleanup_owned_partial_installers(target_dir: &Path) -> Result<(), String> {
    ensure_no_reparse_components(target_dir)?;
    let mut entries = fs::read_dir(target_dir)
        .await
        .map_err(|error| format!("Failed to inspect the updater directory: {error}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Failed to enumerate the updater directory: {error}"))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_owned_partial_installer_name(&name) {
            continue;
        }
        let path = entry.path();
        let Ok(Some((record_path, _record))) = read_owner_record(&path).await else {
            continue;
        };
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(|error| format!("Failed to inspect abandoned updater data: {error}"))?;
        if !metadata.is_file() || is_reparse_or_symlink(&path)? {
            continue;
        }
        ensure_no_reparse_components(&path)?;
        if is_reparse_or_symlink(&path)? {
            continue;
        }
        fs::remove_file(path).await.map_err(|error| {
            format!("Failed to remove an abandoned updater partial file: {error}")
        })?;
        cleanup_file_if_exists(&record_path).await;
    }
    Ok(())
}

pub(super) async fn cleanup_owned_prepared_directories(target_dir: &Path) -> Result<(), String> {
    ensure_no_reparse_components(target_dir)?;
    let mut entries = fs::read_dir(target_dir)
        .await
        .map_err(|error| format!("Failed to inspect the updater directory: {error}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Failed to enumerate the updater directory: {error}"))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_prefix("prepared-") else {
            continue;
        };
        if uuid::Uuid::parse_str(id).is_err() {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(|error| format!("Failed to inspect prepared updater data: {error}"))?;
        if !metadata.is_dir() || is_reparse_or_symlink(&path)? {
            continue;
        }
        let Ok(Some((directory_record_path, directory_record))) = read_owner_record(&path).await
        else {
            continue;
        };

        let mut children = fs::read_dir(&path)
            .await
            .map_err(|error| format!("Failed to inspect prepared updater contents: {error}"))?;
        let mut child_paths = Vec::new();
        let mut artifact_names = HashSet::new();
        let mut marker_names = HashSet::new();
        let mut safe_to_remove = true;
        while let Some(child) = children
            .next_entry()
            .await
            .map_err(|error| format!("Failed to enumerate prepared updater contents: {error}"))?
        {
            let child_path = child.path();
            let child_name = child.file_name().to_string_lossy().into_owned();
            let child_metadata = fs::symlink_metadata(&child_path).await.map_err(|error| {
                format!("Failed to inspect a prepared updater artifact: {error}")
            })?;
            if !child_metadata.is_file() || is_reparse_or_symlink(&child_path)? {
                safe_to_remove = false;
                break;
            }
            if child_name.ends_with(OWNER_RECORD_SUFFIX) {
                if let Some(artifact_name) = child_name.strip_suffix(OWNER_RECORD_SUFFIX) {
                    marker_names.insert(artifact_name.to_string());
                }
                child_paths.push(child_path);
                continue;
            }
            if !is_exact_owned_installer_name(&child_name)
                && !is_owned_partial_installer_name(&child_name)
            {
                safe_to_remove = false;
                break;
            }
            let Ok(Some((_record_path, record))) = read_owner_record(&child_path).await else {
                safe_to_remove = false;
                break;
            };
            if record.installer_size != directory_record.installer_size
                || record.installer_sha256 != directory_record.installer_sha256
            {
                safe_to_remove = false;
                break;
            }
            artifact_names.insert(child_name);
            child_paths.push(child_path);
        }
        drop(children);
        if !safe_to_remove || artifact_names != marker_names {
            continue;
        }
        child_paths.sort_by_key(|child_path| {
            child_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(OWNER_RECORD_SUFFIX))
        });
        for child_path in child_paths {
            fs::remove_file(&child_path)
                .await
                .map_err(|error| format!("Failed to remove prepared updater residue: {error}"))?;
        }
        fs::remove_dir(&path)
            .await
            .map_err(|error| format!("Failed to remove a prepared updater directory: {error}"))?;
        cleanup_file_if_exists(&directory_record_path).await;
    }
    Ok(())
}

pub(super) fn is_owned_partial_installer_name(name: &str) -> bool {
    name.strip_prefix("Nuclear.Downloader_")
        .and_then(|rest| rest.split_once("_x64-setup.exe."))
        .and_then(|(version, suffix)| {
            let operation_id = suffix.strip_suffix(".part")?;
            (parse_semver(version).is_ok() && uuid::Uuid::parse_str(operation_id).is_ok())
                .then_some(())
        })
        .is_some()
}

pub(super) async fn cleanup_owned_old_installers(
    target_dir: &Path,
    current_installer_name: &str,
) -> Result<(), String> {
    ensure_no_reparse_components(target_dir)?;
    let mut entries = fs::read_dir(target_dir)
        .await
        .map_err(|error| format!("Failed to inspect the updater directory: {error}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Failed to enumerate the updater directory: {error}"))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == current_installer_name || !is_exact_owned_installer_name(&name) {
            continue;
        }
        let path = entry.path();
        let Ok(Some((record_path, record))) = read_owner_record(&path).await else {
            continue;
        };
        let Ok(Some(lease)) =
            open_verified_installer(&path, record.installer_size, &record.installer_sha256).await
        else {
            continue;
        };
        drop(lease);
        fs::remove_file(&path)
            .await
            .map_err(|error| format!("Failed to remove an old updater installer: {error}"))?;
        cleanup_file_if_exists(&record_path).await;
    }
    Ok(())
}

pub(super) fn is_exact_owned_installer_name(name: &str) -> bool {
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

pub(super) fn owner_record_path(artifact_path: &Path) -> Result<PathBuf, String> {
    let name = artifact_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "The updater artifact name is invalid.".to_string())?;
    Ok(artifact_path.with_file_name(format!("{name}{OWNER_RECORD_SUFFIX}")))
}

async fn read_owner_record(
    artifact_path: &Path,
) -> Result<Option<(PathBuf, OwnedArtifactRecord)>, String> {
    let record_path = owner_record_path(artifact_path)?;
    let metadata = match fs::symlink_metadata(&record_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Failed to inspect updater ownership data: {error}")),
    };
    if !metadata.is_file() || metadata.len() > 4 * 1024 || is_reparse_or_symlink(&record_path)? {
        return Err("The updater ownership data is not a regular bounded file.".into());
    }
    ensure_no_reparse_components(&record_path)?;
    let bytes = fs::read(&record_path)
        .await
        .map_err(|error| format!("Failed to read updater ownership data: {error}"))?;
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

pub(super) async fn write_owner_record(
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

pub(super) async fn open_or_quarantine_cached_installer(
    target_dir: &Path,
    final_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<CachedInstaller, String> {
    ensure_no_reparse_components(target_dir)?;
    match fs::symlink_metadata(final_path).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let record_path = owner_record_path(final_path)?;
            return match fs::symlink_metadata(record_path).await {
                Ok(_) => Ok(CachedInstaller::UnownedCollision),
                Err(record_error) if record_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(CachedInstaller::Vacant)
                }
                Err(record_error) => Err(format!(
                    "Failed to inspect cached installer ownership data: {record_error}"
                )),
            };
        }
        Err(error) => return Err(format!("Failed to inspect the cached installer: {error}")),
    }

    match open_verified_installer(final_path, expected_size, expected_sha256).await {
        Ok(Some(installer)) => return Ok(CachedInstaller::Verified(installer)),
        Ok(None) => {}
        Err(_) => {}
    }

    let owner = match read_owner_record(final_path).await {
        Ok(owner) => owner,
        Err(_) => return Ok(CachedInstaller::UnownedCollision),
    };
    let Some((record_path, record)) = owner else {
        return Ok(CachedInstaller::UnownedCollision);
    };
    if record.installer_size != expected_size || record.installer_sha256 != expected_sha256 {
        return Ok(CachedInstaller::UnownedCollision);
    }

    quarantine_cached_installer(target_dir, final_path, &record_path).await?;
    Ok(CachedInstaller::Vacant)
}

pub(super) async fn quarantine_cached_installer(
    target_dir: &Path,
    final_path: &Path,
    record_path: &Path,
) -> Result<PathBuf, String> {
    ensure_no_reparse_components(target_dir)?;
    if final_path.parent() != Some(target_dir) {
        return Err("The cached installer is outside the updater directory.".into());
    }
    let file_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "The cached installer name is invalid.".to_string())?;
    if !is_exact_owned_installer_name(file_name) {
        return Err("The cached installer is not an owned canonical installer.".into());
    }
    fs::symlink_metadata(final_path)
        .await
        .map_err(|error| format!("Failed to inspect the invalid cached installer: {error}"))?;

    let quarantine_path = target_dir.join(format!(
        "{file_name}.invalid.{}.quarantine",
        uuid::Uuid::new_v4()
    ));
    let quarantine_record_path = owner_record_path(&quarantine_path)?;
    match fs::symlink_metadata(&quarantine_path).await {
        Ok(_) => return Err("The updater quarantine destination already exists.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "Failed to inspect the quarantine destination: {error}"
            ));
        }
    }
    fs::rename(final_path, &quarantine_path)
        .await
        .map_err(|error| format!("Failed to quarantine the invalid cached installer: {error}"))?;
    if let Err(record_error) = fs::rename(record_path, &quarantine_record_path).await {
        return match fs::rename(&quarantine_path, final_path).await {
            Ok(()) => Err(format!(
                "Failed to quarantine installer ownership data: {record_error}"
            )),
            Err(rollback_error) => Err(format!(
                "Failed to quarantine installer ownership data ({record_error}) and failed to restore the installer ({rollback_error})."
            )),
        };
    }
    Ok(quarantine_path)
}

pub(super) async fn create_prepared_directory(
    target_dir: &Path,
    installer_size: u64,
    installer_sha256: &str,
) -> Result<PathBuf, String> {
    ensure_no_reparse_components(target_dir)?;
    for _ in 0..4 {
        let path = target_dir.join(format!("prepared-{}", uuid::Uuid::new_v4()));
        match fs::create_dir(&path).await {
            Ok(()) => {
                ensure_no_reparse_components(&path)?;
                if let Err(error) =
                    write_owner_record(&path, installer_size, installer_sha256).await
                {
                    let _ = fs::remove_dir(&path).await;
                    return Err(error);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to create an isolated installer directory: {error}"
                ));
            }
        }
    }
    Err("Failed to allocate a unique isolated installer directory.".into())
}

pub(super) async fn open_verified_installer(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<Option<VerifiedInstaller>, String> {
    let path = path.to_path_buf();
    let expected_sha256 = expected_sha256.to_string();
    tokio::task::spawn_blocking(move || {
        open_verified_installer_sync(&path, expected_size, &expected_sha256)
    })
    .await
    .map_err(|error| format!("Installer verification worker failed: {error}"))?
}

pub(super) fn open_verified_installer_sync(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<Option<VerifiedInstaller>, String> {
    ensure_no_reparse_components(path)?;
    let path_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect an existing updater artifact: {error}"))?;
    if !path_metadata.is_file()
        || is_reparse_or_symlink(path)?
        || path_metadata.len() != expected_size
        || path_metadata.len() > INSTALLER_LIMIT
    {
        return Ok(None);
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x00000001;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("Failed to open an existing updater artifact: {error}"))?;

    ensure_no_reparse_components(path)?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("Failed to inspect the opened updater artifact: {error}"))?;
    if !opened_metadata.is_file()
        || opened_metadata.len() != expected_size
        || opened_metadata.len() > INSTALLER_LIMIT
        || opened_file_is_reparse(&opened_metadata)
    {
        return Ok(None);
    }

    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Failed to verify an existing updater artifact: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let final_metadata = file
        .metadata()
        .map_err(|error| format!("Failed to re-inspect the opened updater artifact: {error}"))?;
    if final_metadata.len() != expected_size
        || opened_file_is_reparse(&final_metadata)
        || format!("{:x}", hasher.finalize()) != expected_sha256
    {
        return Ok(None);
    }

    Ok(Some(VerifiedInstaller {
        path: path.to_path_buf(),
        _read_lease: file,
    }))
}

fn opened_file_is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub(super) fn updater_directory() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("NuclearDownloader")
        .join(UPDATE_DIRECTORY_NAME)
}

pub(super) fn is_reparse_or_symlink(path: &Path) -> Result<bool, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        Ok(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    #[cfg(not(windows))]
    Ok(false)
}

pub(super) fn ensure_no_reparse_components(path: &Path) -> Result<(), String> {
    let mut ancestors = path.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for component_path in ancestors {
        if component_path.exists() && is_reparse_or_symlink(component_path)? {
            return Err(format!(
                "Updater path traverses a symbolic link or reparse point: {}",
                component_path.display()
            ));
        }
    }
    Ok(())
}

pub(super) async fn cleanup_file_if_exists(path: &Path) {
    if let Ok(metadata) = fs::symlink_metadata(path).await {
        if metadata.is_file()
            && is_reparse_or_symlink(path).ok() == Some(false)
            && ensure_no_reparse_components(path).is_ok()
        {
            let _ = fs::remove_file(path).await;
        }
    }
}

pub(super) async fn cleanup_current_artifact(path: &Path) {
    cleanup_file_if_exists(path).await;
    if let Ok(record_path) = owner_record_path(path) {
        cleanup_file_if_exists(&record_path).await;
    }
}
