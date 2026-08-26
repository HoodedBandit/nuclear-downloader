use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

mod build_config;

const LOCK_PATH: &str = "sidecars.lock.json";
const WINDOWS_X64_TARGET: &str = "x86_64-pc-windows-msvc";
const REQUIRED_SIDECARS: [(&str, &str); 4] = [
    ("yt-dlp", "binaries/yt-dlp"),
    ("ffmpeg", "binaries/ffmpeg"),
    ("ffprobe", "binaries/ffprobe"),
    ("deno", "binaries/deno"),
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SidecarLock {
    schema_version: u32,
    platform: String,
    sidecars: Vec<SidecarEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SidecarEntry {
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

fn main() {
    println!("cargo:rerun-if-changed={LOCK_PATH}");
    println!("cargo:rerun-if-changed=binaries");
    println!("cargo:rerun-if-env-changed=TAURI_CONFIG");
    for variable in [
        "NUCLEAR_UPDATE_KEY_ID",
        "NUCLEAR_UPDATE_PUBLIC_KEY",
        "NUCLEAR_UPDATE_NEXT_KEY_ID",
        "NUCLEAR_UPDATE_NEXT_PUBLIC_KEY",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    let target = env::var("TARGET").expect("TARGET should be set by cargo");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let profile = env::var("PROFILE").unwrap_or_default();

    if profile == "release" && (target_os != "windows" || target != WINDOWS_X64_TARGET) {
        panic!(
            "Nuclear Downloader 0.6.0 release builds support only {WINDOWS_X64_TARGET}; got {target}"
        );
    }
    validate_update_key_environment(&profile);

    let lock = load_sidecar_lock(Path::new(LOCK_PATH));
    let entries = validate_sidecar_lock(&lock, &target, &target_os);

    let mut available_sidecars = Vec::new();
    let mut missing_sidecars = Vec::new();
    for (name, base) in REQUIRED_SIDECARS {
        let path = sidecar_path(base, &target, &target_os);
        if path.exists() {
            let entry = entries
                .get(name)
                .unwrap_or_else(|| panic!("{LOCK_PATH} is missing required sidecar {name}"));
            verify_sidecar(&path, entry).unwrap_or_else(|error| panic!("{error}"));
            available_sidecars.push(base);
        } else {
            missing_sidecars.push(base);
        }
    }

    if !missing_sidecars.is_empty() {
        if profile == "release" {
            panic!(
                "Missing locked downloader runtime sidecars for release build: {}. Run scripts/fetch-sidecars.ps1.",
                missing_sidecars.join(", ")
            );
        }
        println!(
            "cargo:warning=Skipping missing local sidecars: {}",
            missing_sidecars.join(", ")
        );
    }

    let inherited_config = env::var("TAURI_CONFIG").ok();
    let merged_config =
        build_config::merge_external_bins(inherited_config.as_deref(), &available_sidecars)
            .unwrap_or_else(|error| panic!("Failed to merge Tauri build configuration: {error}"));

    env::set_var("TAURI_CONFIG", merged_config);
    tauri_build::build()
}

fn load_sidecar_lock(path: &Path) -> SidecarLock {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("Failed to read {LOCK_PATH}: {error}"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("Failed to parse {LOCK_PATH}: {error}"))
}

fn validate_sidecar_lock<'a>(
    lock: &'a SidecarLock,
    target: &str,
    target_os: &str,
) -> HashMap<&'a str, &'a SidecarEntry> {
    if lock.schema_version != 1 {
        panic!(
            "Unsupported {LOCK_PATH} schemaVersion {}",
            lock.schema_version
        );
    }
    if lock.platform != "windows-x86_64" {
        panic!("{LOCK_PATH} platform must be windows-x86_64");
    }

    let mut names = HashSet::new();
    let mut entries = HashMap::new();
    for entry in &lock.sidecars {
        if !names.insert(entry.name.as_str()) {
            panic!("{LOCK_PATH} contains duplicate sidecar {}", entry.name);
        }
        if !entry.source_url.starts_with("https://") {
            panic!("{} sourceUrl must use HTTPS", entry.name);
        }
        if entry.version.trim().is_empty() || entry.license.trim().is_empty() {
            panic!("{} version and license must be non-empty", entry.name);
        }
        if entry.architecture != WINDOWS_X64_TARGET {
            panic!(
                "{} has unsupported architecture {}",
                entry.name, entry.architecture
            );
        }
        if entry.sha256.len() != 64 || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            panic!(
                "{} sha256 must be exactly 64 hexadecimal characters",
                entry.name
            );
        }
        if let Some(suffix) = &entry.archive_member_suffix {
            if suffix.is_empty()
                || suffix.contains("..")
                || suffix.starts_with('/')
                || suffix.starts_with('\\')
            {
                panic!("{} archiveMemberSuffix is unsafe", entry.name);
            }
        }
        entries.insert(entry.name.as_str(), entry);
    }

    if target_os == "windows" && target == WINDOWS_X64_TARGET {
        for (name, base) in REQUIRED_SIDECARS {
            let entry = entries
                .get(name)
                .unwrap_or_else(|| panic!("{LOCK_PATH} is missing required sidecar {name}"));
            let expected = sidecar_path(base, target, target_os);
            let expected_name = expected
                .file_name()
                .and_then(|value| value.to_str())
                .expect("sidecar filename should be UTF-8");
            if entry.filename != expected_name {
                panic!(
                    "{} filename mismatch: expected {}, got {}",
                    entry.name, expected_name, entry.filename
                );
            }
        }
    }

    entries
}

fn validate_update_key_environment(profile: &str) {
    let current_id = env::var("NUCLEAR_UPDATE_KEY_ID").unwrap_or_default();
    let current_key = env::var("NUCLEAR_UPDATE_PUBLIC_KEY").unwrap_or_default();
    let next_id = env::var("NUCLEAR_UPDATE_NEXT_KEY_ID").unwrap_or_default();
    let next_key = env::var("NUCLEAR_UPDATE_NEXT_PUBLIC_KEY").unwrap_or_default();

    if profile == "release" && (current_id.is_empty() || current_key.is_empty()) {
        panic!(
            "Release builds require NUCLEAR_UPDATE_KEY_ID and NUCLEAR_UPDATE_PUBLIC_KEY so update manifests are authenticated."
        );
    }
    if current_id.is_empty() != current_key.is_empty() {
        panic!("The current updater key ID and public key must be configured together.");
    }
    if next_id.is_empty() != next_key.is_empty() {
        panic!("The next updater key ID and public key must be configured together.");
    }
    for (label, key_id) in [("current", &current_id), ("next", &next_id)] {
        if !key_id.is_empty()
            && (key_id.len() > 64
                || !key_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)))
        {
            panic!(
                "The {label} updater key ID must use 1-64 ASCII letters, digits, '.', '_', or '-'."
            );
        }
    }
    if !next_id.is_empty() && next_id == current_id {
        panic!("The current and next updater key IDs must be different.");
    }
}

fn verify_sidecar(path: &Path, entry: &SidecarEntry) -> Result<(), String> {
    let actual_hash = sha256_file(path)?;
    if !actual_hash.eq_ignore_ascii_case(&entry.sha256) {
        return Err(format!(
            "Locked sidecar hash mismatch for {}: expected {}, got {}",
            path.display(),
            entry.sha256,
            actual_hash
        ));
    }
    verify_pe_x64(path)
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("Failed to open {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Failed to hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn verify_pe_x64(path: &Path) -> Result<(), String> {
    let mut file =
        File::open(path).map_err(|error| format!("Failed to open {}: {error}", path.display()))?;
    let mut dos_header = [0_u8; 64];
    file.read_exact(&mut dos_header)
        .map_err(|error| format!("{} is not a valid PE file: {error}", path.display()))?;
    if &dos_header[..2] != b"MZ" {
        return Err(format!("{} is not a Windows PE file", path.display()));
    }
    let pe_offset = u32::from_le_bytes(
        dos_header[0x3c..0x40]
            .try_into()
            .expect("DOS header slice should have four bytes"),
    ) as u64;
    file.seek(SeekFrom::Start(pe_offset))
        .map_err(|error| format!("Failed to seek {}: {error}", path.display()))?;
    let mut pe_header = [0_u8; 6];
    file.read_exact(&mut pe_header)
        .map_err(|error| format!("{} has a truncated PE header: {error}", path.display()))?;
    if &pe_header[..4] != b"PE\0\0" {
        return Err(format!("{} has an invalid PE signature", path.display()));
    }
    let machine = u16::from_le_bytes([pe_header[4], pe_header[5]]);
    if machine != 0x8664 {
        return Err(format!(
            "{} has PE machine 0x{machine:04x}; x86_64 (0x8664) is required",
            path.display()
        ));
    }
    Ok(())
}

fn sidecar_path(base: &str, target: &str, target_os: &str) -> PathBuf {
    let suffix = if target_os == "windows" {
        format!("-{target}.exe")
    } else {
        format!("-{target}")
    };
    PathBuf::from(format!("{base}{suffix}"))
}
