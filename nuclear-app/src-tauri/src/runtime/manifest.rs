use super::archive::validate_runtime_path_component;
use super::release::{parse_runtime_descriptor, SignedRuntimeDescriptor};
#[cfg(test)]
use super::{
    record_scoped_hash_bytes, record_scoped_hash_invocation, TEST_MANIFEST_HASH_BYTES,
    TEST_MANIFEST_HASH_INVOCATIONS, TEST_TOOL_HASH_BYTES, TEST_TOOL_HASH_INVOCATIONS,
};
use crate::bounded_read::{read_bounded, BoundedReadError};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::Ordering;

pub(super) const RUNTIME_DESCRIPTOR_LIMIT: u64 = 64 * 1024;
pub(super) const RUNTIME_MANIFEST_LIMIT: u64 = 64 * 1024;
pub(super) const RUNTIME_SIGNATURE_LIMIT: u64 = 8 * 1024;
pub(super) const RUNTIME_ENTRY_SIZE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
pub(super) const RUNTIME_CURRENT_POINTER: &str = "current.json";
pub(super) const RUNTIME_UPDATE_OWNER_MARKER: &str = ".nuclear-runtime-update-v1";
pub(super) const RUNTIME_INSTALL_OWNER_MARKER: &str = ".nuclear-runtime-install-v1";
pub(super) const RUNTIME_AUTH_DESCRIPTOR: &str = ".nuclear-runtime-descriptor-v1.json";
pub(super) const RUNTIME_AUTH_SIGNATURE: &str = ".nuclear-runtime-descriptor-v1.json.sig";

pub(super) type RuntimeSignatureVerifier = fn(&str, &[u8], &[u8]) -> Result<(), String>;

#[derive(Clone, Copy)]
pub(super) struct ToolSpec {
    pub(super) name: &'static str,
    pub(super) required: bool,
}

pub(super) const REQUIRED_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "yt-dlp",
        required: true,
    },
    ToolSpec {
        name: "ffmpeg",
        required: true,
    },
    ToolSpec {
        name: "ffprobe",
        required: true,
    },
    ToolSpec {
        name: "deno",
        required: false,
    },
];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RuntimeManifest {
    #[serde(default)]
    pub(super) schema_version: u32,
    pub(super) runtime_version: String,
    pub(super) platform: String,
    pub(super) tools: Vec<RuntimeManifestTool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RuntimeManifestTool {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) path: String,
    pub(super) sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RuntimeCurrentPointer {
    pub(super) schema_version: u32,
    pub(super) runtime_version: String,
}

pub(super) fn validate_runtime_version(version: &str) -> Result<(), String> {
    if version.trim() != version || parse_dotted_numeric_version(version).is_none() {
        Err("Runtime version must contain exactly three numeric dotted components.".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn discover_managed_runtime_at(
    root: &Path,
    verify_hashes: bool,
) -> Result<Option<(PathBuf, RuntimeManifest)>, String> {
    discover_managed_runtime_at_with_verifier(
        root,
        verify_hashes,
        crate::updater::verify_release_signature_for_key,
    )
}

pub(super) fn discover_managed_runtime_at_with_verifier(
    root: &Path,
    verify_hashes: bool,
    signature_verifier: RuntimeSignatureVerifier,
) -> Result<Option<(PathBuf, RuntimeManifest)>, String> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Failed to inspect managed runtime root: {error}")),
    };
    ensure_no_reparse_components(root)?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(root)? {
        return Err("Managed runtime root must be a non-reparse directory.".into());
    }
    let pointer_path = root.join(RUNTIME_CURRENT_POINTER);
    let pointer_metadata = match std::fs::symlink_metadata(&pointer_path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "Failed to inspect managed runtime pointer: {error}"
            ))
        }
    };
    if let Some(metadata) = pointer_metadata {
        ensure_no_reparse_components(&pointer_path)?;
        if !metadata.is_file() || is_reparse_or_symlink(&pointer_path)? {
            return Err("Managed runtime pointer must be a regular non-reparse file.".into());
        }
        if metadata.len() > RUNTIME_MANIFEST_LIMIT {
            return Err("Managed runtime pointer exceeds the 64 KiB limit.".into());
        }
        let mut pointer_file = File::open(&pointer_path)
            .map_err(|error| format!("Failed to read managed runtime pointer: {error}"))?;
        let opened_metadata = pointer_file
            .metadata()
            .map_err(|error| format!("Failed to inspect managed runtime pointer: {error}"))?;
        if !opened_metadata.is_file() || metadata_is_reparse(&opened_metadata) {
            return Err("Managed runtime pointer must be a regular non-reparse file.".into());
        }
        let pointer_bytes = match read_bounded(&mut pointer_file, RUNTIME_MANIFEST_LIMIT) {
            Ok(bytes) => bytes,
            Err(BoundedReadError::LimitExceeded | BoundedReadError::InvalidLimit) => {
                return Err("Managed runtime pointer exceeds the 64 KiB limit.".into())
            }
            Err(BoundedReadError::Io(error)) => {
                return Err(format!("Failed to read managed runtime pointer: {error}"))
            }
        };
        let pointer: RuntimeCurrentPointer = serde_json::from_slice(&pointer_bytes)
            .map_err(|error| format!("Failed to parse managed runtime pointer: {error}"))?;
        if pointer.schema_version != 1
            || validate_runtime_version(&pointer.runtime_version).is_err()
        {
            return Err("Managed runtime pointer has an unsupported schema or version.".into());
        }
        let runtime_dir = root.join(&pointer.runtime_version);
        let manifest = validate_installed_runtime_at_with_verifier(
            &runtime_dir,
            verify_hashes,
            signature_verifier,
        )?;
        if manifest.runtime_version != pointer.runtime_version {
            return Err("Managed runtime pointer and manifest versions do not match.".into());
        }
        return Ok(Some((runtime_dir, manifest)));
    }

    // Compatibility is limited to unmarked pre-0.6 runtime directories. Once
    // a marker-owned 0.6 runtime exists, current.json is authoritative and its
    // absence is a repair condition rather than an invitation to scan/select.
    let mut candidates = Vec::new();
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("Failed to inspect managed runtime root: {error}"))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Failed to enumerate managed runtime root: {error}"))?;
        let path = entry.path();
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| "Managed runtime contains a non-UTF-8 entry name.".to_string())?
            .to_string();
        if name.starts_with('.') || validate_runtime_version(&name).is_err() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("Failed to inspect managed runtime entry: {error}"))?;
        if !metadata.is_dir() || is_reparse_or_symlink(&path)? {
            return Err("Managed runtime version entry must be a non-reparse directory.".into());
        }
        if installed_runtime_is_owned(&path, &name)? {
            return Err(
                "Managed runtime current.json is missing for a marker-owned runtime.".into(),
            );
        }
        let manifest = validate_manifest_at(&path, verify_hashes)?;
        if manifest.runtime_version != name {
            return Err(
                "Legacy managed runtime directory and manifest versions do not match.".into(),
            );
        }
        candidates.push((path, manifest));
    }

    candidates.sort_by(|(_, left), (_, right)| {
        version_sort_key(&left.runtime_version).cmp(&version_sort_key(&right.runtime_version))
    });
    Ok(candidates.pop())
}

pub(super) fn bundled_executable_root() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

pub(super) fn tool_exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

pub(super) fn validate_manifest_at(
    dir: &Path,
    verify_hashes: bool,
) -> Result<RuntimeManifest, String> {
    ensure_no_reparse_components(dir)?;
    let root_metadata = std::fs::symlink_metadata(dir)
        .map_err(|error| format!("Failed to inspect runtime root: {error}"))?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(dir)? {
        return Err("Runtime root must be a regular non-reparse directory.".into());
    }
    let canonical_root = std::fs::canonicalize(dir)
        .map_err(|error| format!("Failed to canonicalize runtime root: {error}"))?;
    let manifest_path = dir.join("runtime-manifest.json");
    ensure_no_reparse_components(&manifest_path)?;
    let metadata = std::fs::symlink_metadata(&manifest_path)
        .map_err(|error| format!("Failed to inspect runtime manifest: {error}"))?;
    if !metadata.is_file()
        || is_reparse_or_symlink(&manifest_path)?
        || metadata.len() > RUNTIME_MANIFEST_LIMIT
    {
        return Err(
            "Runtime manifest must be a regular non-reparse file no larger than 64 KiB.".into(),
        );
    }
    let mut manifest_file = File::open(&manifest_path)
        .map_err(|error| format!("Failed to read runtime manifest: {error}"))?;
    let opened_metadata = manifest_file
        .metadata()
        .map_err(|error| format!("Failed to inspect runtime manifest: {error}"))?;
    if !opened_metadata.is_file() || metadata_is_reparse(&opened_metadata) {
        return Err(
            "Runtime manifest must be a regular non-reparse file no larger than 64 KiB.".into(),
        );
    }
    let manifest_bytes = match read_bounded(&mut manifest_file, RUNTIME_MANIFEST_LIMIT) {
        Ok(bytes) => bytes,
        Err(BoundedReadError::LimitExceeded | BoundedReadError::InvalidLimit) => {
            return Err(
                "Runtime manifest must be a regular non-reparse file no larger than 64 KiB.".into(),
            )
        }
        Err(BoundedReadError::Io(error)) => {
            return Err(format!("Failed to read runtime manifest: {error}"))
        }
    };
    let manifest = serde_json::from_slice::<RuntimeManifest>(&manifest_bytes)
        .map_err(|error| format!("Failed to parse runtime manifest: {error}"))?;

    if manifest.schema_version != 1 {
        return Err(format!(
            "Unsupported runtime manifest schema version {}.",
            manifest.schema_version
        ));
    }
    validate_runtime_version(&manifest.runtime_version)?;

    if manifest.platform != runtime_platform() {
        return Err(format!(
            "Runtime bundle platform {} does not match {}.",
            manifest.platform,
            runtime_platform()
        ));
    }

    for required in REQUIRED_TOOLS.iter().filter(|tool| tool.required) {
        if !manifest.tools.iter().any(|tool| tool.name == required.name) {
            return Err(format!("Runtime manifest is missing {}.", required.name));
        }
    }

    let mut seen_names = std::collections::HashSet::new();
    for tool in &manifest.tools {
        if !REQUIRED_TOOLS.iter().any(|spec| spec.name == tool.name)
            || !seen_names.insert(tool.name.as_str())
        {
            return Err("Runtime manifest contains an unknown or duplicate tool.".into());
        }
        if tool.version.trim().is_empty()
            || tool.version.len() > 256
            || tool.version.chars().any(char::is_control)
        {
            return Err(format!("Runtime tool {} is missing a version.", tool.name));
        }
        validate_relative_manifest_path(&tool.path)?;
        validate_canonical_sha256(&tool.sha256)?;
        let path = dir.join(&tool.path);
        ensure_no_reparse_components(&path)?;
        if !path.is_file() {
            return Err(format!("Runtime tool {} was not found.", tool.name));
        }
        if is_reparse_or_symlink(&path)? {
            return Err(format!(
                "Runtime tool {} is a symbolic link or reparse point.",
                tool.name
            ));
        }
        let canonical_tool = std::fs::canonicalize(&path).map_err(|error| {
            format!("Failed to canonicalize runtime tool {}: {error}", tool.name)
        })?;
        let relative = canonical_tool.strip_prefix(&canonical_root).map_err(|_| {
            format!(
                "Runtime tool {} resolves outside the runtime root.",
                tool.name
            )
        })?;
        if relative.as_os_str().is_empty() {
            return Err(format!(
                "Runtime tool {} does not resolve to a file beneath the runtime root.",
                tool.name
            ));
        }
        if verify_hashes {
            let actual = sha256_runtime_tool_file_sync(&path)?;
            if !actual.eq_ignore_ascii_case(&tool.sha256) {
                return Err(format!(
                    "Runtime tool {} checksum mismatch: expected {}, got {}.",
                    tool.name, tool.sha256, actual
                ));
            }
        }
    }

    Ok(manifest)
}

pub(super) fn validate_installed_runtime_at_with_verifier(
    dir: &Path,
    verify_hashes: bool,
    signature_verifier: RuntimeSignatureVerifier,
) -> Result<RuntimeManifest, String> {
    let manifest = validate_manifest_at(dir, verify_hashes)?;
    if installed_runtime_is_owned(dir, &manifest.runtime_version)? {
        validate_runtime_auth_contract_with_verifier(dir, &manifest, signature_verifier)?;
    }
    Ok(manifest)
}

pub(super) fn validate_runtime_auth_contract(
    dir: &Path,
    manifest: &RuntimeManifest,
) -> Result<SignedRuntimeDescriptor, String> {
    validate_runtime_auth_contract_with_verifier(
        dir,
        manifest,
        crate::updater::verify_release_signature_for_key,
    )
}

pub(super) fn validate_runtime_auth_contract_with_verifier(
    dir: &Path,
    manifest: &RuntimeManifest,
    signature_verifier: RuntimeSignatureVerifier,
) -> Result<SignedRuntimeDescriptor, String> {
    let descriptor_path = dir.join(RUNTIME_AUTH_DESCRIPTOR);
    let signature_path = dir.join(RUNTIME_AUTH_SIGNATURE);
    let descriptor_bytes = read_regular_bounded_file(
        &descriptor_path,
        RUNTIME_DESCRIPTOR_LIMIT,
        "installed runtime descriptor",
    )?;
    let signature_bytes = read_regular_bounded_file(
        &signature_path,
        RUNTIME_SIGNATURE_LIMIT,
        "installed runtime descriptor signature",
    )?;
    let descriptor = parse_runtime_descriptor(&descriptor_bytes)?;
    signature_verifier(&descriptor.key_id, &descriptor_bytes, &signature_bytes)?;
    if descriptor.runtime_version != manifest.runtime_version {
        return Err("Installed runtime descriptor and manifest versions do not match.".into());
    }
    let actual_manifest_hash = sha256_file_sync(&dir.join("runtime-manifest.json"))?;
    if actual_manifest_hash != descriptor.manifest_sha256 {
        return Err("Installed runtime manifest failed signed integrity validation.".into());
    }
    Ok(descriptor)
}

pub(super) fn read_regular_bounded_file(
    path: &Path,
    limit: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    ensure_no_reparse_components(path)?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect {label}: {error}"))?;
    if !metadata.is_file() || is_reparse_or_symlink(path)? || metadata.len() > limit {
        return Err(format!(
            "{label} must be a regular non-reparse file no larger than {limit} bytes."
        ));
    }
    let mut file = File::open(path).map_err(|error| format!("Failed to read {label}: {error}"))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("Failed to inspect {label}: {error}"))?;
    if !opened_metadata.is_file() || metadata_is_reparse(&opened_metadata) {
        return Err(format!(
            "{label} must be a regular non-reparse file no larger than {limit} bytes."
        ));
    }
    match read_bounded(&mut file, limit) {
        Ok(bytes) => Ok(bytes),
        Err(BoundedReadError::LimitExceeded | BoundedReadError::InvalidLimit) => Err(format!(
            "{label} must be a regular non-reparse file no larger than {limit} bytes."
        )),
        Err(BoundedReadError::Io(error)) => Err(format!("Failed to read {label}: {error}")),
    }
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

pub(super) fn validate_relative_manifest_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.as_os_str().len() > 512 || path.is_absolute() {
        return Err("Runtime manifest contains an unsafe tool path.".into());
    }
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err("Runtime manifest contains an unsafe tool path.".into());
        };
        let Some(name) = name.to_str() else {
            return Err("Runtime manifest tool paths must be UTF-8.".into());
        };
        validate_runtime_path_component(name)?;
    }
    Ok(())
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
        if !component_path.exists() {
            continue;
        }
        if is_reparse_or_symlink(component_path)? {
            return Err(format!(
                "Path traverses a symbolic link or reparse point: {}",
                component_path.display()
            ));
        }
    }
    Ok(())
}

pub(super) fn local_data_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("NuclearDownloader")
}

pub(super) fn managed_runtime_root() -> PathBuf {
    local_data_root().join("runtime")
}

pub(super) fn app_plugin_dir() -> PathBuf {
    local_data_root().join("plugins")
}

pub(super) fn runtime_platform() -> &'static str {
    if cfg!(all(windows, target_arch = "x86_64")) {
        "windows-x64"
    } else if cfg!(windows) {
        "windows"
    } else {
        "unknown"
    }
}

pub(super) fn version_sort_key(raw: &str) -> Version {
    let normalized = raw.trim().trim_start_matches('v');
    if let Some(version) = parse_dotted_numeric_version(normalized) {
        return version;
    }

    Version::parse(normalized).unwrap_or_else(|_| Version::new(0, 0, 0))
}

pub(super) fn parse_dotted_numeric_version(raw: &str) -> Option<Version> {
    let parts = raw.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| part.is_empty())
        || !parts
            .iter()
            .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }

    Some(Version::new(
        parts[0].parse::<u64>().ok()?,
        parts[1].parse::<u64>().ok()?,
        parts[2].parse::<u64>().ok()?,
    ))
}

pub(super) fn validate_canonical_sha256(value: &str) -> Result<(), String> {
    if !crate::artifact_contract::is_canonical_sha256(value) {
        return Err("Runtime SHA-256 must be 64 lowercase hexadecimal digits.".into());
    }
    Ok(())
}

pub(super) fn sha256_file_sync(path: &Path) -> Result<String, String> {
    sha256_file_sync_with_kind(path, false)
}

pub(super) fn sha256_runtime_tool_file_sync(path: &Path) -> Result<String, String> {
    sha256_file_sync_with_kind(path, true)
}

pub(super) fn sha256_file_sync_with_kind(
    path: &Path,
    is_runtime_tool: bool,
) -> Result<String, String> {
    #[cfg(not(test))]
    let _ = is_runtime_tool;
    let mut file =
        File::open(path).map_err(|error| format!("Failed to open {}: {error}", path.display()))?;
    #[cfg(test)]
    if is_runtime_tool {
        TEST_TOOL_HASH_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
    } else {
        TEST_MANIFEST_HASH_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
    }
    #[cfg(test)]
    record_scoped_hash_invocation(path, is_runtime_tool);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        #[cfg(test)]
        if is_runtime_tool {
            TEST_TOOL_HASH_BYTES.fetch_add(read as u64, Ordering::SeqCst);
        } else {
            TEST_MANIFEST_HASH_BYTES.fetch_add(read as u64, Ordering::SeqCst);
        }
        #[cfg(test)]
        record_scoped_hash_bytes(path, is_runtime_tool, read as u64);
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn write_runtime_install_marker(dir: &Path, version: &str) -> Result<(), String> {
    validate_runtime_version(version)?;
    ensure_no_reparse_components(dir)?;
    let marker = dir.join(RUNTIME_INSTALL_OWNER_MARKER);
    let contents = format!("schemaVersion=1\nruntimeVersion={version}\n");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&marker)
        .map_err(|error| format!("Failed to create runtime ownership marker: {error}"))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| format!("Failed to write runtime ownership marker: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("Failed to sync runtime ownership marker: {error}"))
}

pub(super) fn write_runtime_auth_contract(
    dir: &Path,
    descriptor_bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    if descriptor_bytes.is_empty()
        || descriptor_bytes.len() as u64 > RUNTIME_DESCRIPTOR_LIMIT
        || signature_bytes.is_empty()
        || signature_bytes.len() as u64 > RUNTIME_SIGNATURE_LIMIT
    {
        return Err("Runtime authentication contract exceeds its size limits.".into());
    }
    let descriptor = parse_runtime_descriptor(descriptor_bytes)?;
    crate::updater::verify_release_signature_for_key(
        &descriptor.key_id,
        descriptor_bytes,
        signature_bytes,
    )?;
    ensure_no_reparse_components(dir)?;
    for (name, bytes) in [
        (RUNTIME_AUTH_DESCRIPTOR, descriptor_bytes),
        (RUNTIME_AUTH_SIGNATURE, signature_bytes),
    ] {
        let path = dir.join(name);
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("Failed to create {name}: {error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("Failed to write {name}: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("Failed to sync {name}: {error}"))?;
    }
    Ok(())
}

pub(super) fn installed_runtime_is_owned(dir: &Path, version: &str) -> Result<bool, String> {
    if validate_runtime_version(version).is_err()
        || !dir.is_dir()
        || ensure_no_reparse_components(dir).is_err()
    {
        return Ok(false);
    }
    let marker = dir.join(RUNTIME_INSTALL_OWNER_MARKER);
    if !marker.is_file() || is_reparse_or_symlink(&marker)? {
        return Ok(false);
    }
    let expected = format!("schemaVersion=1\nruntimeVersion={version}\n");
    let Ok(mut marker_file) = File::open(&marker) else {
        return Ok(false);
    };
    let Ok(opened_metadata) = marker_file.metadata() else {
        return Ok(false);
    };
    if !opened_metadata.is_file() || metadata_is_reparse(&opened_metadata) {
        return Ok(false);
    }
    Ok(read_bounded(&mut marker_file, expected.len() as u64)
        .ok()
        .as_deref()
        == Some(expected.as_bytes()))
}
