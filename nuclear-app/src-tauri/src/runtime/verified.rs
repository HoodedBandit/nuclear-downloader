use super::cache::{RuntimeCache, RuntimeCacheRead};
use super::manifest::{
    discover_managed_runtime_at_with_verifier, ensure_no_reparse_components, is_reparse_or_symlink,
    tool_exe_name, validate_canonical_sha256, RuntimeSignatureVerifier, REQUIRED_TOOLS,
    RUNTIME_ENTRY_SIZE_LIMIT,
};
#[cfg(test)]
use super::{
    record_scoped_hash_bytes, record_scoped_hash_invocation, TEST_RUNTIME_RESOLUTION_CALLS,
    TEST_RUNTIME_SUCCESSFUL_RESOLUTIONS, TEST_TOOL_HASH_BYTES, TEST_TOOL_HASH_INVOCATIONS,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::LazyLock;
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Debug)]
pub(crate) struct RuntimeToolLease {
    pub(super) path: PathBuf,
    pub(super) source: String,
    pub(super) runtime_version: Option<String>,
    _read_lease: Option<std::fs::File>,
    _cache_read: Option<RuntimeCacheRead<VerifiedRuntimeSnapshot>>,
}

#[derive(Debug)]
pub(super) struct VerifiedRuntimeTool {
    pub(super) path: PathBuf,
    pub(super) file: File,
}

#[derive(Debug)]
pub(super) struct VerifiedRuntimeSnapshot {
    pub(super) runtime_dir: Option<PathBuf>,
    pub(super) runtime_version: Option<String>,
    pub(super) source: String,
    pub(super) tools: HashMap<String, VerifiedRuntimeTool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BundledSidecarLock {
    schema_version: u32,
    platform: String,
    sidecars: Vec<BundledSidecarEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundledSidecarEntry {
    name: String,
    source_url: String,
    version: String,
    license: String,
    architecture: String,
    filename: String,
    sha256: String,
    #[serde(default)]
    archive_member_suffix: Option<String>,
}

pub(super) const BUNDLED_SIDECAR_LOCK: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/sidecars.lock.json"));

pub(super) static VERIFIED_RUNTIME_CACHE: LazyLock<RuntimeCache<VerifiedRuntimeSnapshot>> =
    LazyLock::new(RuntimeCache::new);

impl RuntimeToolLease {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn resolve_tool_lease(name: &str) -> Result<Option<RuntimeToolLease>, String> {
    resolve_tool_lease_from_cache(name, &VERIFIED_RUNTIME_CACHE, true)
}

pub(super) fn resolve_tool_lease_from_cache(
    name: &str,
    cache: &RuntimeCache<VerifiedRuntimeSnapshot>,
    allow_fallback: bool,
) -> Result<Option<RuntimeToolLease>, String> {
    #[cfg(test)]
    TEST_RUNTIME_RESOLUTION_CALLS.fetch_add(1, Ordering::SeqCst);
    if let Some(snapshot) = cache.get_initialized()? {
        let (path, file) = {
            let Some(tool) = snapshot.tools.get(name) else {
                return Ok(None);
            };
            let file = tool.file.try_clone().map_err(|error| {
                format!("Failed to clone verified runtime executable lease: {error}")
            })?;
            (tool.path.clone(), file)
        };
        let source = snapshot.source.clone();
        let runtime_version = snapshot.runtime_version.clone();
        #[cfg(test)]
        TEST_RUNTIME_SUCCESSFUL_RESOLUTIONS.fetch_add(1, Ordering::SeqCst);
        return Ok(Some(RuntimeToolLease {
            path,
            source,
            runtime_version,
            _read_lease: Some(file),
            _cache_read: Some(snapshot),
        }));
    }

    resolve_tool_fallback(name, allow_fallback)
}

fn resolve_tool_fallback(
    name: &str,
    allow_fallback: bool,
) -> Result<Option<RuntimeToolLease>, String> {
    if !allow_fallback {
        return Ok(None);
    }

    #[cfg(debug_assertions)]
    {
        #[cfg(test)]
        TEST_RUNTIME_SUCCESSFUL_RESOLUTIONS.fetch_add(1, Ordering::SeqCst);
        Ok(Some(RuntimeToolLease {
            path: PathBuf::from(name),
            source: "path".into(),
            runtime_version: None,
            _read_lease: None,
            _cache_read: None,
        }))
    }

    #[cfg(not(debug_assertions))]
    {
        Ok(None)
    }
}

#[cfg(test)]
pub(super) fn resolve_tool_lease_uncached_at(
    name: &str,
    managed_root: &Path,
    signature_verifier: RuntimeSignatureVerifier,
) -> Result<Option<RuntimeToolLease>, String> {
    TEST_RUNTIME_RESOLUTION_CALLS.fetch_add(1, Ordering::SeqCst);
    if let Some((runtime_dir, manifest)) =
        discover_managed_runtime_at_with_verifier(managed_root, false, signature_verifier)?
    {
        let tool = manifest
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .ok_or_else(|| format!("Managed runtime manifest does not contain {name}."))?;
        let path = runtime_dir.join(&tool.path);
        let file = open_verified_tool_file(&path, Some(&tool.sha256))?;
        TEST_RUNTIME_SUCCESSFUL_RESOLUTIONS.fetch_add(1, Ordering::SeqCst);
        return Ok(Some(RuntimeToolLease {
            path,
            source: "managed".into(),
            runtime_version: Some(manifest.runtime_version),
            _read_lease: Some(file),
            _cache_read: None,
        }));
    }
    Ok(None)
}

pub(super) async fn initialize_runtime_cache_at(
    cache: &RuntimeCache<VerifiedRuntimeSnapshot>,
    managed_root: PathBuf,
    bundled_root: Option<PathBuf>,
    signature_verifier: RuntimeSignatureVerifier,
    cancellation: CancellationToken,
) -> Result<Option<RuntimeCacheRead<VerifiedRuntimeSnapshot>>, String> {
    cache
        .get_or_initialize(move || {
            build_verified_runtime_snapshot_async(
                managed_root,
                bundled_root,
                signature_verifier,
                cancellation,
            )
        })
        .await
}

pub(super) async fn build_verified_runtime_snapshot_async(
    managed_root: PathBuf,
    bundled_root: Option<PathBuf>,
    signature_verifier: RuntimeSignatureVerifier,
    cancellation: CancellationToken,
) -> Result<Option<VerifiedRuntimeSnapshot>, String> {
    tokio::task::spawn_blocking(move || {
        build_verified_runtime_snapshot_at(
            &managed_root,
            bundled_root.as_deref(),
            signature_verifier,
            &cancellation,
        )
    })
    .await
    .map_err(|error| format!("Runtime verification worker failed: {error}"))?
}

pub(super) fn build_verified_runtime_snapshot_at(
    managed_root: &Path,
    bundled_root: Option<&Path>,
    signature_verifier: RuntimeSignatureVerifier,
    cancellation: &CancellationToken,
) -> Result<Option<VerifiedRuntimeSnapshot>, String> {
    if let Some((runtime_dir, manifest)) =
        discover_managed_runtime_at_with_verifier(managed_root, false, signature_verifier)?
    {
        let mut tools = HashMap::with_capacity(manifest.tools.len());
        for tool in &manifest.tools {
            if cancellation.is_cancelled() {
                return Err("Runtime verification was cancelled during shutdown.".into());
            }
            let path = runtime_dir.join(&tool.path);
            let file =
                open_verified_tool_file_cancellable(&path, Some(&tool.sha256), cancellation)?;
            tools.insert(tool.name.clone(), VerifiedRuntimeTool { path, file });
        }
        return Ok(Some(VerifiedRuntimeSnapshot {
            runtime_dir: Some(runtime_dir),
            runtime_version: Some(manifest.runtime_version),
            source: "managed".into(),
            tools,
        }));
    }

    build_verified_bundled_snapshot_at(bundled_root, cancellation)
}

pub(super) fn build_verified_bundled_snapshot_at(
    bundled_root: Option<&Path>,
    cancellation: &CancellationToken,
) -> Result<Option<VerifiedRuntimeSnapshot>, String> {
    build_verified_bundled_snapshot_from_lock_at(bundled_root, BUNDLED_SIDECAR_LOCK, cancellation)
}

pub(super) fn build_verified_bundled_snapshot_from_lock_at(
    bundled_root: Option<&Path>,
    lock_json: &str,
    cancellation: &CancellationToken,
) -> Result<Option<VerifiedRuntimeSnapshot>, String> {
    let Some(bundled_root) = bundled_root else {
        return Ok(None);
    };
    let lock: BundledSidecarLock = serde_json::from_str(lock_json)
        .map_err(|error| format!("Bundled sidecar lock is invalid: {error}"))?;
    validate_bundled_sidecar_lock(&lock)?;

    let mut tools = HashMap::with_capacity(lock.sidecars.len());
    for sidecar in lock.sidecars {
        if cancellation.is_cancelled() {
            return Err("Runtime verification was cancelled during shutdown.".into());
        }
        let path = bundled_root.join(tool_exe_name(&sidecar.name));
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to inspect bundled runtime executable {}: {error}",
                    path.display()
                ));
            }
        }
        let file = open_verified_tool_file_cancellable(&path, Some(&sidecar.sha256), cancellation)?;
        tools.insert(sidecar.name, VerifiedRuntimeTool { path, file });
    }

    if tools.is_empty() {
        return Ok(None);
    }
    Ok(Some(VerifiedRuntimeSnapshot {
        runtime_dir: Some(bundled_root.to_path_buf()),
        runtime_version: None,
        source: "bundled".into(),
        tools,
    }))
}

pub(super) fn validate_bundled_sidecar_lock(lock: &BundledSidecarLock) -> Result<(), String> {
    if lock.schema_version != 1 || lock.platform != "windows-x86_64" {
        return Err("Bundled sidecar lock has an unsupported schema or platform.".into());
    }
    let expected_names = REQUIRED_TOOLS
        .iter()
        .map(|tool| tool.name)
        .collect::<HashSet<_>>();
    if lock.sidecars.len() != expected_names.len() {
        return Err("Bundled sidecar lock must contain exactly the supported tools.".into());
    }
    let mut names = HashSet::with_capacity(lock.sidecars.len());
    for entry in &lock.sidecars {
        if !expected_names.contains(entry.name.as_str()) || !names.insert(entry.name.as_str()) {
            return Err("Bundled sidecar lock contains an unknown or duplicate tool.".into());
        }
        let url = Url::parse(&entry.source_url)
            .map_err(|error| format!("Bundled sidecar URL is invalid: {error}"))?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return Err("Bundled sidecar URL must use HTTPS without credentials.".into());
        }
        if entry.version.trim().is_empty() || entry.license.trim().is_empty() {
            return Err("Bundled sidecar version and license must be non-empty.".into());
        }
        if entry.architecture != "x86_64-pc-windows-msvc"
            || entry.filename != format!("{}-x86_64-pc-windows-msvc.exe", entry.name)
        {
            return Err("Bundled sidecar architecture or filename is invalid.".into());
        }
        validate_canonical_sha256(&entry.sha256)?;
        if let Some(suffix) = &entry.archive_member_suffix {
            if suffix.is_empty()
                || suffix.contains("..")
                || suffix.starts_with('/')
                || suffix.starts_with('\\')
            {
                return Err("Bundled sidecar archive member suffix is unsafe.".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) async fn initialize_test_runtime_cache_at(
    cache: &RuntimeCache<VerifiedRuntimeSnapshot>,
    managed_root: PathBuf,
    signature_verifier: RuntimeSignatureVerifier,
) -> Result<Option<RuntimeCacheRead<VerifiedRuntimeSnapshot>>, String> {
    initialize_runtime_cache_at(
        cache,
        managed_root,
        None,
        signature_verifier,
        CancellationToken::new(),
    )
    .await
}

#[cfg(test)]
pub(super) fn open_verified_tool_file(
    path: &Path,
    expected_sha256: Option<&str>,
) -> Result<File, String> {
    open_verified_tool_file_cancellable(path, expected_sha256, &CancellationToken::new())
}

fn open_verified_tool_file_cancellable(
    path: &Path,
    expected_sha256: Option<&str>,
    cancellation: &CancellationToken,
) -> Result<File, String> {
    ensure_no_reparse_components(path)?;
    let path_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect runtime executable: {error}"))?;
    if !path_metadata.is_file()
        || is_reparse_or_symlink(path)?
        || path_metadata.len() == 0
        || path_metadata.len() > RUNTIME_ENTRY_SIZE_LIMIT
    {
        return Err("Runtime executable must be a bounded regular non-reparse file.".into());
    }

    let mut options = std::fs::OpenOptions::new();
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
        .map_err(|error| format!("Failed to acquire runtime executable lease: {error}"))?;
    ensure_no_reparse_components(path)?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("Failed to inspect leased runtime executable: {error}"))?;
    if !opened_metadata.is_file()
        || opened_metadata.len() != path_metadata.len()
        || metadata_is_reparse(&opened_metadata)
    {
        return Err("Runtime executable identity changed while acquiring its lease.".into());
    }

    if let Some(expected_sha256) = expected_sha256 {
        #[cfg(test)]
        TEST_TOOL_HASH_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
        #[cfg(test)]
        record_scoped_hash_invocation(path, true);
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            if cancellation.is_cancelled() {
                return Err(
                    "Runtime executable verification was cancelled during shutdown.".into(),
                );
            }
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("Failed to hash leased runtime executable: {error}"))?;
            if read == 0 {
                break;
            }
            #[cfg(test)]
            TEST_TOOL_HASH_BYTES.fetch_add(read as u64, Ordering::SeqCst);
            #[cfg(test)]
            record_scoped_hash_bytes(path, true, read as u64);
            hasher.update(&buffer[..read]);
        }
        if format!("{:x}", hasher.finalize()) != expected_sha256 {
            return Err("Runtime executable failed signed integrity validation.".into());
        }
    }

    let final_metadata = file
        .metadata()
        .map_err(|error| format!("Failed to re-inspect leased runtime executable: {error}"))?;
    if final_metadata.len() != opened_metadata.len() || metadata_is_reparse(&final_metadata) {
        return Err("Runtime executable changed during integrity validation.".into());
    }
    Ok(file)
}

fn metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
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
