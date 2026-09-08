use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use zip::write::SimpleFileOptions;

fn start_http_fixture(parts: Vec<(Vec<u8>, Duration)>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        for (bytes, delay_after) in parts {
            stream.write_all(&bytes).unwrap();
            stream.flush().unwrap();
            if !delay_after.is_zero() {
                std::thread::sleep(delay_after);
            }
        }
    });
    format!("http://{address}/fixture")
}

fn release_with_assets(assets: Vec<(&str, u64)>) -> GitHubRelease {
    GitHubRelease {
        assets: assets
            .into_iter()
            .map(|(name, size)| GitHubReleaseAsset {
                name: name.into(),
                browser_download_url: format!("https://example.com/{name}"),
                size,
            })
            .collect(),
    }
}

#[test]
fn runtime_versions_require_three_numeric_components() {
    for valid in ["2026.06.09", "1.0.0"] {
        assert!(validate_runtime_version(valid).is_ok());
    }
    for invalid in [
        "2026.06",
        "2026.06.09.1",
        "v2026.06.09",
        "2026.06.beta",
        "２０２６.０６.０９",
    ] {
        assert!(
            validate_runtime_version(invalid).is_err(),
            "accepted {invalid}"
        );
    }
}

#[tokio::test]
async fn runtime_http_reader_rejects_streamed_overflow() {
    let url = start_http_fixture(vec![(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nabcde\r\n0\r\n\r\n"
                .to_vec(),
            Duration::ZERO,
        )]);
    let response = Client::new().get(url).send().await.unwrap();
    let error = read_response_limited(response, 4, "fixture")
        .await
        .unwrap_err();
    assert!(error.contains("exceeds the 4-byte limit"));
}

#[tokio::test]
async fn runtime_http_reader_enforces_idle_read_timeout() {
    let url = start_http_fixture(vec![
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\na".to_vec(),
            Duration::from_millis(250),
        ),
        (b"bcde".to_vec(), Duration::ZERO),
    ]);
    let client = Client::builder()
        .read_timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    let response = client.get(url).send().await.unwrap();
    let error = read_response_limited(response, 5, "fixture")
        .await
        .unwrap_err();
    assert!(error.contains("Failed while reading fixture"));
}

#[test]
fn stale_ytdlp_detection_uses_recommended_baseline() {
    assert!(is_ytdlp_stale("2026.03.17", "2026.06.09"));
    assert!(!is_ytdlp_stale("2026.06.09", "2026.06.09"));
    assert!(!is_ytdlp_stale("2026.07.01", "2026.06.09"));
}

#[test]
fn version_probe_args_match_tool_cli_contracts() {
    assert_eq!(tool_version_args("yt-dlp"), &["--version"]);
    assert_eq!(tool_version_args("deno"), &["--version"]);
    assert_eq!(tool_version_args("ffmpeg"), &["-version"]);
    assert_eq!(tool_version_args("ffprobe"), &["-version"]);
}

#[test]
fn deno_absence_degrades_instead_of_blocking_downloads() {
    let deno = REQUIRED_TOOLS
        .iter()
        .find(|tool| tool.name == "deno")
        .expect("deno tool spec");

    assert!(!deno.required);
}

#[test]
fn signed_descriptor_selects_one_exact_runtime_archive() {
    let release = release_with_assets(vec![
        ("nuclear-downloader-runtime-windows-x64.json", 100),
        ("nuclear-downloader-runtime-windows-x64.json.sig", 100),
        ("nuclear-downloader-runtime-2026.06.09-windows-x64.zip", 123),
        (
            "nuclear-downloader-runtime-2026.06.09-windows-x64.zip.sha256",
            100,
        ),
    ]);
    let (descriptor, signature) = select_runtime_descriptor_assets(&release).unwrap();
    assert!(descriptor.name.ends_with(".json"));
    assert!(signature.name.ends_with(".json.sig"));
    let selected = select_runtime_archive(
        &release,
        SignedRuntimeDescriptor {
            schema_version: 1,
            key_id: "key-1".into(),
            runtime_version: "2026.06.09".into(),
            platform: "windows-x64".into(),
            archive_name: "nuclear-downloader-runtime-2026.06.09-windows-x64.zip".into(),
            compressed_size: 123,
            sha256: "a".repeat(64),
            manifest_sha256: "b".repeat(64),
        },
        b"descriptor".to_vec(),
        b"signature".to_vec(),
    )
    .unwrap();
    assert_eq!(selected.version, "2026.06.09");
    assert!(selected.archive_name.ends_with(".zip"));
    assert_eq!(selected.archive_sha256, "a".repeat(64));
}

#[test]
fn runtime_descriptor_is_strict_and_binds_exact_archive_name() {
    let valid = br#"{"schemaVersion":1,"keyId":"release-key-1","runtimeVersion":"2026.06.09","platform":"windows-x64","archiveName":"nuclear-downloader-runtime-2026.06.09-windows-x64.zip","compressedSize":123,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","manifestSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#;
    let descriptor = parse_runtime_descriptor(valid).unwrap();
    assert_eq!(descriptor.runtime_version, "2026.06.09");

    let wrong_name = br#"{"schemaVersion":1,"keyId":"release-key-1","runtimeVersion":"2026.06.09","platform":"windows-x64","archiveName":"other.zip","compressedSize":123,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","manifestSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#;
    assert!(parse_runtime_descriptor(wrong_name).is_err());
    let unknown_field = br#"{"schemaVersion":1,"keyId":"release-key-1","runtimeVersion":"2026.06.09","platform":"windows-x64","archiveName":"nuclear-downloader-runtime-2026.06.09-windows-x64.zip","compressedSize":123,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","manifestSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","extra":true}"#;
    assert!(parse_runtime_descriptor(unknown_field).is_err());
}

#[test]
fn runtime_zip_rejects_excessive_depth() {
    let root = unique_test_root("runtime-zip-depth");
    fs::create_dir_all(&root).unwrap();
    let archive_path = root.join("runtime.zip");
    let file = File::create(&archive_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file(
        "one/two/three/four/five.exe",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(b"x").unwrap();
    zip.finish().unwrap();

    let error =
        extract_runtime_zip(&archive_path, &root.join("extracted"), &|| Ok(())).unwrap_err();
    assert!(error.to_string().contains("depth"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn runtime_zip_observes_typed_cancellation() {
    let root = unique_test_root("runtime-zip-cancelled");
    fs::create_dir_all(&root).unwrap();
    let archive_path = root.join("runtime.zip");
    let file = File::create(&archive_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file(
        "runtime-manifest.json",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(b"{}").unwrap();
    zip.finish().unwrap();

    let error = extract_runtime_zip(&archive_path, &root.join("extracted"), &|| {
        Err(UpdateRunError::Cancelled)
    })
    .unwrap_err();

    assert!(matches!(error, UpdateRunError::Cancelled));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn runtime_zip_rejects_extreme_compression_ratio() {
    let root = unique_test_root("runtime-zip-ratio");
    fs::create_dir_all(&root).unwrap();
    let archive_path = root.join("runtime.zip");
    let file = File::create(&archive_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file(
        "payload.exe",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(&vec![0_u8; 1024 * 1024]).unwrap();
    zip.finish().unwrap();

    let error =
        extract_runtime_zip(&archive_path, &root.join("extracted"), &|| Ok(())).unwrap_err();
    assert!(error.to_string().contains("compression-ratio"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn runtime_zip_rejects_case_collisions_and_file_directory_conflicts() {
    let root = unique_test_root("runtime-zip-collisions");
    fs::create_dir_all(&root).unwrap();

    let case_archive = root.join("case-collision.zip");
    let file = File::create(&case_archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for name in ["runtime-manifest.json", "TOOLS/tool.exe", "tools/TOOL.exe"] {
        zip.start_file(
            name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"x").unwrap();
    }
    zip.finish().unwrap();
    let error =
        extract_runtime_zip(&case_archive, &root.join("case-output"), &|| Ok(())).unwrap_err();
    assert!(error.to_string().contains("duplicate or case-colliding"));

    let conflict_archive = root.join("file-directory-conflict.zip");
    let file = File::create(&conflict_archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for name in ["runtime-manifest.json", "tools", "tools/tool.exe"] {
        zip.start_file(
            name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"x").unwrap();
    }
    zip.finish().unwrap();
    let error = extract_runtime_zip(&conflict_archive, &root.join("conflict-output"), &|| Ok(()))
        .unwrap_err();
    assert!(error.to_string().contains("file/directory path conflict"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn runtime_zip_requires_one_exact_root_manifest_and_new_staging() {
    let root = unique_test_root("runtime-zip-manifest");
    fs::create_dir_all(&root).unwrap();

    let nested_archive = root.join("nested-manifest.zip");
    let file = File::create(&nested_archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file(
        "nested/runtime-manifest.json",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(b"{}").unwrap();
    zip.finish().unwrap();
    let error =
        extract_runtime_zip(&nested_archive, &root.join("nested-output"), &|| Ok(())).unwrap_err();
    assert!(error
        .to_string()
        .contains("exactly one root runtime-manifest.json"));

    let extra_manifest_archive = root.join("extra-manifest.zip");
    let file = File::create(&extra_manifest_archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for name in ["runtime-manifest.json", "nested/runtime-manifest.json"] {
        zip.start_file(
            name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"{}").unwrap();
    }
    zip.finish().unwrap();
    let error = extract_runtime_zip(&extra_manifest_archive, &root.join("extra-output"), &|| {
        Ok(())
    })
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("exactly one root runtime-manifest.json"));

    let valid_archive = root.join("valid.zip");
    let file = File::create(&valid_archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file(
        "runtime-manifest.json",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(b"{}").unwrap();
    zip.finish().unwrap();
    let staging = root.join("existing-staging");
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("sentinel.txt"), b"keep").unwrap();
    let error = extract_runtime_zip(&valid_archive, &staging, &|| Ok(())).unwrap_err();
    assert!(error.to_string().contains("refusing to overwrite"));
    assert_eq!(fs::read(staging.join("sentinel.txt")).unwrap(), b"keep");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn runtime_paths_reject_windows_reserved_device_stems() {
    for reserved in [
        "CON",
        "con.txt",
        "PRN.exe",
        "AUX",
        "NUL.json",
        "COM1.exe",
        "com9.data",
        "LPT1",
        "lpt9.exe",
    ] {
        assert!(
            normalize_runtime_archive_path(Path::new(reserved)).is_err(),
            "accepted {reserved}"
        );
        assert!(
            validate_relative_manifest_path(reserved).is_err(),
            "accepted manifest path {reserved}"
        );
    }
    for allowed in ["console.exe", "com0.exe", "com10.exe", "lpt0.exe"] {
        assert!(normalize_runtime_archive_path(Path::new(allowed)).is_ok());
    }
}

#[tokio::test]
async fn abandoned_runtime_cleanup_requires_uuid_and_owner_marker() {
    let root = unique_test_root("runtime-cleanup");
    let updates = root.join(".updates");
    let owned = updates.join("550e8400-e29b-41d4-a716-446655440000");
    let unowned = updates.join("not-owned");
    fs::create_dir_all(&owned).unwrap();
    fs::create_dir_all(&unowned).unwrap();
    fs::write(
        owned.join(RUNTIME_UPDATE_OWNER_MARKER),
        b"schemaVersion=1\n",
    )
    .unwrap();
    fs::write(unowned.join("data.bin"), b"keep").unwrap();

    cleanup_abandoned_runtime_updates_at(&updates, &std::collections::HashSet::new())
        .await
        .unwrap();
    assert!(!owned.exists());
    assert!(unowned.exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn managed_runtime_integrity_is_verified_before_trust() {
    let root = unique_test_root("runtime-integrity");
    let runtime_dir = write_test_runtime_tree(&root, "2026.06.09", false, true);
    assert!(discover_managed_runtime_at(&root, true).unwrap().is_some());

    fs::write(runtime_dir.join("yt-dlp.exe"), b"tampered").unwrap();
    let error = discover_managed_runtime_at(&root, true).unwrap_err();
    assert!(error.contains("checksum mismatch"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn marker_owned_runtime_requires_signed_auth_contract() {
    let root = unique_test_root("runtime-auth-required");
    write_test_runtime_tree(&root, "2026.06.09", true, true);
    let error = discover_managed_runtime_at(&root, true).unwrap_err();
    assert!(error.contains("installed runtime descriptor"));
    let _ = fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn runtime_tool_lease_denies_mutation_and_rehashes_before_use() {
    let root = unique_test_root("runtime-tool-lease");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("yt-dlp.exe");
    let replacement = root.join("replacement.exe");
    fs::write(&path, b"trusted-runtime-tool").unwrap();
    fs::write(&replacement, b"malicious-runtime!!").unwrap();
    let hash = format!("{:x}", Sha256::digest(b"trusted-runtime-tool"));

    let lease = open_verified_tool_file(&path, Some(&hash)).unwrap();
    assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
    assert!(fs::rename(&replacement, &path).is_err());
    drop(lease);

    fs::write(&path, b"tampered-runtime-tool").unwrap();
    assert!(open_verified_tool_file(&path, Some(&hash)).is_err());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn marker_owned_runtime_requires_current_pointer() {
    let root = unique_test_root("runtime-pointer-required");
    write_test_runtime_tree(&root, "2026.06.09", true, false);
    let error = discover_managed_runtime_at(&root, true).unwrap_err();
    assert!(error.contains("current.json is missing"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unmarked_legacy_runtime_has_a_bounded_migration_path() {
    let root = unique_test_root("runtime-legacy-migration");
    write_test_runtime_tree(&root, "2026.06.09", false, false);
    let discovered = discover_managed_runtime_at(&root, true)
        .unwrap()
        .expect("legacy runtime");
    assert_eq!(discovered.1.runtime_version, "2026.06.09");
    let _ = fs::remove_dir_all(root);
}

fn unique_test_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "nuclear-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn write_test_runtime_tree(
    root: &Path,
    version: &str,
    write_marker: bool,
    write_pointer: bool,
) -> PathBuf {
    let runtime_dir = root.join(version);
    fs::create_dir_all(&runtime_dir).unwrap();
    let mut manifest_tools = Vec::new();
    for tool in ["yt-dlp", "ffmpeg", "ffprobe"] {
        let bytes = format!("{tool}-fixture");
        fs::write(runtime_dir.join(format!("{tool}.exe")), bytes.as_bytes()).unwrap();
        manifest_tools.push(format!(
            r#"{{"name":"{tool}","version":"1.0.0","path":"{tool}.exe","sha256":"{:x}"}}"#,
            Sha256::digest(bytes.as_bytes())
        ));
    }
    fs::write(
            runtime_dir.join("runtime-manifest.json"),
            format!(
                r#"{{"schemaVersion":1,"runtimeVersion":"{version}","platform":"windows-x64","tools":[{}]}}"#,
                manifest_tools.join(",")
            ),
        )
        .unwrap();
    if write_marker {
        write_runtime_install_marker(&runtime_dir, version).unwrap();
    }
    if write_pointer {
        fs::write(
            root.join(RUNTIME_CURRENT_POINTER),
            serde_json::to_vec(&RuntimeCurrentPointer {
                schema_version: 1,
                runtime_version: version.to_string(),
            })
            .unwrap(),
        )
        .unwrap();
    }
    runtime_dir
}

const PHASE4_FIXTURE_VERSION: &str = test_support::FIXTURE_VERSION;
const PHASE4_FIXTURE_DESCRIPTOR: &str = test_support::FIXTURE_DESCRIPTOR;
const PHASE4_FIXTURE_SIGNATURE: &str = test_support::FIXTURE_SIGNATURE;

fn verify_phase4_fixture_signature(
    key_id: &str,
    bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    test_support::verify_fixture_signature(key_id, bytes, signature_bytes)
}

fn write_phase4_signed_runtime_fixture(root: &Path) -> PathBuf {
    test_support::write_fixture(root).unwrap()
}

#[test]
fn validates_runtime_manifest_and_tool_hashes() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-runtime-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();

    let tools = ["yt-dlp", "ffmpeg", "ffprobe", "deno"];
    let mut manifest_tools = Vec::new();
    for tool in tools {
        let path = root.join(format!("{tool}.exe"));
        fs::write(&path, tool.as_bytes()).unwrap();
        let hash = format!("{:x}", Sha256::digest(tool.as_bytes()));
        manifest_tools.push(format!(
            r#"{{"name":"{tool}","version":"1.0.0","path":"{tool}.exe","sha256":"{hash}"}}"#
        ));
    }

    fs::write(
            root.join("runtime-manifest.json"),
            format!(
                r#"{{"schemaVersion":1,"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[{}]}}"#,
                manifest_tools.join(",")
            ),
        )
        .unwrap();

    let manifest = validate_manifest_at(&root, true).unwrap();
    assert_eq!(manifest.runtime_version, "2026.06.09");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn runtime_manifest_allows_optional_deno_to_be_absent() {
    let root = std::env::temp_dir().join(format!(
        "nuclear-runtime-no-deno-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();

    let mut manifest_tools = Vec::new();
    for tool in ["yt-dlp", "ffmpeg", "ffprobe"] {
        let path = root.join(format!("{tool}.exe"));
        fs::write(&path, tool.as_bytes()).unwrap();
        let hash = format!("{:x}", Sha256::digest(tool.as_bytes()));
        manifest_tools.push(format!(
            r#"{{"name":"{tool}","version":"1.0.0","path":"{tool}.exe","sha256":"{hash}"}}"#
        ));
    }
    fs::write(
            root.join("runtime-manifest.json"),
            format!(
                r#"{{"schemaVersion":1,"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[{}]}}"#,
                manifest_tools.join(",")
            ),
        )
        .unwrap();

    assert!(validate_manifest_at(&root, true).is_ok());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn runtime_manifest_rejects_missing_schema_version() {
    let root = unique_test_root("runtime-no-schema");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("runtime-manifest.json"),
        r#"{"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[]}"#,
    )
    .unwrap();
    let error = validate_manifest_at(&root, false).unwrap_err();
    assert!(error.contains("schema version 0"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn runtime_manifest_rejects_unknown_manifest_and_tool_fields() {
    let unknown_manifest = r#"{"schemaVersion":1,"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[],"unexpected":true}"#;
    assert!(serde_json::from_str::<RuntimeManifest>(unknown_manifest).is_err());

    let unknown_tool = r#"{"schemaVersion":1,"runtimeVersion":"2026.06.09","platform":"windows-x64","tools":[{"name":"yt-dlp","version":"1.0.0","path":"yt-dlp.exe","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","unexpected":true}]}"#;
    assert!(serde_json::from_str::<RuntimeManifest>(unknown_tool).is_err());
}

#[cfg(windows)]
#[test]
fn runtime_manifest_rejects_a_reparse_runtime_root() {
    use std::os::windows::fs::symlink_dir;

    let root = unique_test_root("runtime-reparse-root");
    let actual = root.join("actual");
    let linked = root.join("linked");
    fs::create_dir_all(&actual).unwrap();
    if symlink_dir(&actual, &linked).is_err() {
        let _ = fs::remove_dir_all(root);
        return;
    }

    let error = validate_manifest_at(&linked, false).unwrap_err();
    assert!(error.contains("reparse point"));
    fs::remove_dir(&linked).unwrap();
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn atomic_runtime_promotion_restores_existing_destination_on_publish_failure() {
    let root = unique_test_root("runtime-promote-failure");
    fs::create_dir_all(&root).unwrap();
    let mut transaction = RuntimeTransaction::new(
        uuid::Uuid::new_v4().to_string(),
        "2026.06.09".to_string(),
        true,
    )
    .unwrap();
    let paths = transaction.paths(&root);
    fs::create_dir_all(&paths.final_dir).unwrap();
    fs::write(paths.final_dir.join("marker.txt"), "original").unwrap();
    runtime_transaction::store(&root, &transaction).unwrap();

    let result = promote_runtime_atomically(&root, &mut transaction, &paths.candidate).await;

    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(paths.final_dir.join("marker.txt")).unwrap(),
        "original"
    );
    assert!(!paths.backup.exists());
    assert!(runtime_transaction::load(&root).unwrap().is_none());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn verified_candidate_publication_uses_the_transaction_and_refreshes_its_cache() {
    let root = unique_test_root("runtime-publisher-entry");
    let managed_root = root.join("managed");
    let update_id = uuid::Uuid::new_v4().to_string();
    let work_root = managed_root.join(".updates").join(&update_id);
    fs::create_dir_all(&work_root).unwrap();
    let fixture_dir = write_phase4_signed_runtime_fixture(&work_root);
    let candidate_dir = work_root.join("extracted");
    fs::rename(&fixture_dir, &candidate_dir).unwrap();
    fs::remove_file(work_root.join(RUNTIME_CURRENT_POINTER)).unwrap();
    let cache = RuntimeCache::new();
    let lifecycle = crate::lifecycle::LifecycleCoordinator::new(1, 1, 1);
    let (_tracked, context) = lifecycle
        .register_update(
            update_id.clone(),
            crate::models::OperationKind::RuntimeUpdate,
        )
        .unwrap();

    let outcome = publish_verified_candidate_with_cache(
        RuntimePublicationRequest {
            managed_root: managed_root.clone(),
            candidate_dir,
            update_id,
            runtime_version: PHASE4_FIXTURE_VERSION.to_string(),
            bundled_root: None,
            signature_verifier: verify_phase4_fixture_signature,
        },
        &context,
        &cache,
    )
    .await
    .unwrap();

    assert_eq!(outcome.installed_version, PHASE4_FIXTURE_VERSION);
    assert!(outcome.warnings.is_empty());
    assert!(runtime_transaction::load(&managed_root).unwrap().is_none());
    let cached = cache.get_initialized().unwrap().unwrap();
    assert_eq!(
        cached.runtime_version.as_deref(),
        Some(PHASE4_FIXTURE_VERSION)
    );
    drop(cached);
    cache.begin_mutation().await.publish(Ok(None));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancellation_while_waiting_for_cache_does_not_publish_a_transaction_record() {
    let root = unique_test_root("runtime-publisher-cancel-before-record");
    let managed_root = root.join("managed");
    fs::create_dir_all(&managed_root).unwrap();
    let update_id = uuid::Uuid::new_v4().to_string();
    let cache = RuntimeCache::new();
    let held = cache
        .get_or_initialize(|| async {
            Ok(Some(VerifiedRuntimeSnapshot {
                runtime_dir: None,
                runtime_version: None,
                source: "test".into(),
                tools: HashMap::new(),
            }))
        })
        .await
        .unwrap()
        .unwrap();
    let lifecycle = crate::lifecycle::LifecycleCoordinator::new(1, 1, 1);
    let (_tracked, context) = lifecycle
        .register_update(
            update_id.clone(),
            crate::models::OperationKind::RuntimeUpdate,
        )
        .unwrap();
    let publication = publish_verified_candidate_with_cache(
        RuntimePublicationRequest {
            managed_root: managed_root.clone(),
            candidate_dir: managed_root
                .join(".updates")
                .join(&update_id)
                .join("extracted"),
            update_id: update_id.clone(),
            runtime_version: PHASE4_FIXTURE_VERSION.to_string(),
            bundled_root: None,
            signature_verifier: verify_phase4_fixture_signature,
        },
        &context,
        &cache,
    );
    tokio::pin!(publication);

    assert!(
        tokio::time::timeout(Duration::from_millis(50), publication.as_mut())
            .await
            .is_err()
    );
    assert!(runtime_transaction::load(&managed_root).unwrap().is_none());
    assert_eq!(
        lifecycle.cancel_update(&update_id).unwrap(),
        crate::lifecycle::UpdateCancellation::Requested
    );
    assert!(matches!(publication.await, Err(UpdateRunError::Cancelled)));
    assert!(runtime_transaction::load(&managed_root).unwrap().is_none());

    drop(held);
    cache.begin_mutation().await.publish(Ok(None));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn corrupted_marker_owned_same_version_is_authorized_for_repair() {
    let root = unique_test_root("runtime-same-version-repair");
    let version = "2026.06.09";
    let runtime_dir = write_test_runtime_tree(&root, version, true, false);
    fs::write(runtime_dir.join("yt-dlp.exe"), b"corrupted").unwrap();

    assert!(validate_manifest_at(&runtime_dir, true).is_err());
    assert!(authorize_runtime_replacement(&runtime_dir, version).unwrap());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unowned_same_version_directory_is_preserved() {
    let root = unique_test_root("runtime-unowned-preserved");
    let runtime_dir = root.join("2026.06.09");
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::write(runtime_dir.join("sentinel.txt"), b"keep").unwrap();

    assert!(authorize_runtime_replacement(&runtime_dir, "2026.06.09").is_err());
    assert_eq!(fs::read(runtime_dir.join("sentinel.txt")).unwrap(), b"keep");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn runtime_publication_rollback_restores_previous_directory() {
    let root = unique_test_root("runtime-publication-rollback");
    let final_dir = root.join("2026.06.09");
    let candidate_dir = root.join("candidate");
    let backup_dir = root.join("backup");
    fs::create_dir_all(&final_dir).unwrap();
    fs::create_dir_all(&backup_dir).unwrap();
    fs::write(final_dir.join("payload.txt"), "new").unwrap();
    fs::write(backup_dir.join("payload.txt"), "previous").unwrap();

    rollback_runtime_promotion(&candidate_dir, &final_dir, &backup_dir, true)
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(final_dir.join("payload.txt")).unwrap(),
        "previous"
    );
    assert_eq!(
        fs::read_to_string(candidate_dir.join("payload.txt")).unwrap(),
        "new"
    );
    assert!(!backup_dir.exists());
    let _ = fs::remove_dir_all(root);
}

fn write_recovery_fixture(path: &Path, version: &str, payload: &str) {
    fs::create_dir_all(path).unwrap();
    fs::write(path.join("valid"), b"yes").unwrap();
    fs::write(path.join("payload.txt"), payload).unwrap();
    write_runtime_install_marker(path, version).unwrap();
}

fn recovery_fixture_is_valid(path: &Path, version: &str) -> bool {
    installed_runtime_is_owned(path, version).ok() == Some(true)
        && fs::read(path.join("valid")).ok().as_deref() == Some(b"yes")
}

#[tokio::test]
async fn recovery_is_idempotent_after_every_durable_checkpoint() {
    for checkpoint in [
        RuntimeTransactionCheckpoint::CandidateVerified,
        RuntimeTransactionCheckpoint::OldMoved,
        RuntimeTransactionCheckpoint::NewPublished,
        RuntimeTransactionCheckpoint::CurrentPointerCommitted,
        RuntimeTransactionCheckpoint::BackupCleaned,
    ] {
        let root = unique_test_root("runtime-checkpoint-recovery");
        fs::create_dir_all(&root).unwrap();
        let mut transaction = RuntimeTransaction::new(
            uuid::Uuid::new_v4().to_string(),
            "2026.06.09".to_string(),
            true,
        )
        .unwrap();
        transaction.set_checkpoint(checkpoint);
        let paths = transaction.paths(&root);
        fs::create_dir_all(&paths.work_root).unwrap();
        fs::write(
            paths.work_root.join(RUNTIME_UPDATE_OWNER_MARKER),
            b"schemaVersion=1\n",
        )
        .unwrap();
        match checkpoint {
            RuntimeTransactionCheckpoint::CandidateVerified => {
                write_recovery_fixture(&paths.final_dir, &transaction.runtime_version, "previous");
                write_recovery_fixture(&paths.candidate, &transaction.runtime_version, "candidate");
            }
            RuntimeTransactionCheckpoint::OldMoved => {
                write_recovery_fixture(&paths.backup, &transaction.runtime_version, "previous");
                write_recovery_fixture(&paths.candidate, &transaction.runtime_version, "candidate");
            }
            RuntimeTransactionCheckpoint::NewPublished
            | RuntimeTransactionCheckpoint::CurrentPointerCommitted => {
                write_recovery_fixture(&paths.backup, &transaction.runtime_version, "previous");
                write_recovery_fixture(&paths.final_dir, &transaction.runtime_version, "candidate");
            }
            RuntimeTransactionCheckpoint::BackupCleaned => {
                write_recovery_fixture(&paths.final_dir, &transaction.runtime_version, "candidate");
            }
        }
        if checkpoint == RuntimeTransactionCheckpoint::CurrentPointerCommitted
            || checkpoint == RuntimeTransactionCheckpoint::BackupCleaned
        {
            fs::write(
                root.join(RUNTIME_CURRENT_POINTER),
                serde_json::to_vec(&RuntimeCurrentPointer {
                    schema_version: 1,
                    runtime_version: transaction.runtime_version.clone(),
                })
                .unwrap(),
            )
            .unwrap();
        }
        runtime_transaction::store(&root, &transaction).unwrap();

        recover_runtime_update_transaction_at(&root, &recovery_fixture_is_valid)
            .await
            .unwrap();
        recover_runtime_update_transaction_at(&root, &recovery_fixture_is_valid)
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(paths.final_dir.join("payload.txt")).unwrap(),
            "candidate"
        );
        assert!(!paths.backup.exists());
        assert!(runtime_transaction::load(&root).unwrap().is_none());
        let pointer: RuntimeCurrentPointer =
            serde_json::from_slice(&fs::read(root.join(RUNTIME_CURRENT_POINTER)).unwrap()).unwrap();
        assert_eq!(pointer.runtime_version, transaction.runtime_version);
        let _ = fs::remove_dir_all(root);
    }
}

#[tokio::test]
async fn recovery_resumes_after_rollback_succeeds_but_journal_clear_fails() {
    let root = unique_test_root("runtime-rollback-journal-retained");
    fs::create_dir_all(&root).unwrap();
    let mut transaction = RuntimeTransaction::new(
        uuid::Uuid::new_v4().to_string(),
        "2026.06.09".to_string(),
        true,
    )
    .unwrap();
    transaction.set_checkpoint(RuntimeTransactionCheckpoint::OldMoved);
    let paths = transaction.paths(&root);
    fs::create_dir_all(&paths.work_root).unwrap();
    fs::write(
        paths.work_root.join(RUNTIME_UPDATE_OWNER_MARKER),
        b"schemaVersion=1\n",
    )
    .unwrap();
    write_recovery_fixture(&paths.final_dir, &transaction.runtime_version, "previous");
    write_recovery_fixture(&paths.candidate, &transaction.runtime_version, "candidate");
    runtime_transaction::store(&root, &transaction).unwrap();

    recover_runtime_update_transaction_at(&root, &recovery_fixture_is_valid)
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(paths.final_dir.join("payload.txt")).unwrap(),
        "candidate"
    );
    assert!(runtime_transaction::load(&root).unwrap().is_none());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn quarantined_transaction_remains_fail_closed_and_preserves_unowned_directory() {
    let root = unique_test_root("runtime-quarantine-preservation");
    fs::create_dir_all(&root).unwrap();
    let transaction = RuntimeTransaction::new(
        uuid::Uuid::new_v4().to_string(),
        "2026.06.09".to_string(),
        false,
    )
    .unwrap();
    let paths = transaction.paths(&root);
    fs::create_dir_all(&paths.work_root).unwrap();
    fs::write(
        paths.work_root.join(RUNTIME_UPDATE_OWNER_MARKER),
        b"schemaVersion=1\n",
    )
    .unwrap();
    write_recovery_fixture(&paths.candidate, &transaction.runtime_version, "candidate");
    fs::create_dir_all(&paths.final_dir).unwrap();
    fs::write(paths.final_dir.join("sentinel.txt"), b"keep").unwrap();
    runtime_transaction::store(&root, &transaction).unwrap();

    assert!(
        recover_runtime_update_transaction_at(&root, &recovery_fixture_is_valid)
            .await
            .is_err()
    );
    assert!(
        recover_runtime_update_transaction_at(&root, &recovery_fixture_is_valid)
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(paths.final_dir.join("sentinel.txt")).unwrap(),
        b"keep"
    );
    assert!(paths.candidate.exists());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn verified_runtime_cache_waits_for_leases_and_blocks_reacquisition() {
    let root = unique_test_root("runtime-cache-mutation");
    fs::create_dir_all(&root).unwrap();
    write_phase4_signed_runtime_fixture(&root);
    let cache = RuntimeCache::new();
    reset_runtime_hash_counters_for_root(&root);
    let health_snapshot =
        initialize_test_runtime_cache_at(&cache, root.clone(), verify_phase4_fixture_signature)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(runtime_hash_counters_for_root(&root).total_invocations, 5);
    let lease = resolve_tool_lease_from_cache("yt-dlp", &cache, false)
        .unwrap()
        .unwrap();

    let mutation = {
        let mutation = cache.begin_mutation();
        tokio::pin!(mutation);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut mutation)
                .await
                .is_err()
        );
        drop(health_snapshot);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut mutation)
                .await
                .is_err(),
            "a returned runtime lease must keep mutation blocked"
        );
        drop(lease);
        mutation.await
    };

    let blocked_error = resolve_tool_lease_from_cache("yt-dlp", &cache, false).unwrap_err();
    assert!(blocked_error.contains("temporarily unavailable"));

    let refreshed = build_verified_runtime_snapshot_async(
        root.clone(),
        None,
        verify_phase4_fixture_signature,
        CancellationToken::new(),
    )
    .await;
    mutation.publish(refreshed);
    let lease = resolve_tool_lease_from_cache("yt-dlp", &cache, false)
        .unwrap()
        .expect("resolution should resume after cache publication");
    assert_eq!(
        lease.runtime_version.as_deref(),
        Some(PHASE4_FIXTURE_VERSION)
    );
    drop(lease);
    assert_eq!(runtime_hash_counters_for_root(&root).total_invocations, 10);

    drop(cache);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn explicit_root_runtime_harness_reuses_leases_and_refreshes() {
    let root = unique_test_root("runtime-explicit-harness");
    let harness = test_support::VerifiedRuntimeHarness::create_at(root.clone()).unwrap();
    harness.reset_counts();
    harness.initialize().await.unwrap();
    let initialized = harness.counts();
    assert_eq!(initialized.hash_invocations, 5);
    assert!(initialized.hash_bytes > 0);
    assert_eq!(initialized.resolution_calls, 0);
    assert_eq!(initialized.successful_resolutions, 0);

    let lease = harness.resolve("yt-dlp").unwrap().unwrap();
    assert!(lease.path().starts_with(&root));
    let mutation = {
        let mutation = harness.begin_mutation();
        tokio::pin!(mutation);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut mutation)
                .await
                .is_err()
        );
        drop(lease);
        tokio::time::timeout(Duration::from_secs(1), mutation)
            .await
            .expect("explicit-root harness mutation should acquire after lease release")
    };
    mutation.refresh().await.unwrap();
    let refreshed = harness.counts();
    assert_eq!(refreshed.hash_invocations, 10);
    assert_eq!(refreshed.resolution_calls, 1);
    assert_eq!(refreshed.successful_resolutions, 1);

    let lease = harness.resolve("ffmpeg").unwrap().unwrap();
    assert!(lease.path().starts_with(&root));
    drop(lease);
    drop(harness);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn corrupt_managed_runtime_is_cached_as_repair_condition() {
    let root = unique_test_root("runtime-cache-corrupt");
    fs::create_dir_all(&root).unwrap();
    let runtime_dir = write_phase4_signed_runtime_fixture(&root);
    fs::write(runtime_dir.join("yt-dlp.exe"), b"corrupt").unwrap();
    let cache = RuntimeCache::new();
    reset_runtime_hash_counters_for_root(&root);

    let first = match initialize_test_runtime_cache_at(
        &cache,
        root.clone(),
        verify_phase4_fixture_signature,
    )
    .await
    {
        Ok(_) => panic!("corrupt runtime must not initialize the cache"),
        Err(error) => error,
    };
    assert!(first.contains("integrity validation"));
    let after_first = runtime_hash_counters_for_root(&root);
    assert!(after_first.total_invocations > 0);

    let second = match initialize_test_runtime_cache_at(
        &cache,
        root.clone(),
        verify_phase4_fixture_signature,
    )
    .await
    {
        Ok(_) => panic!("cached corruption must remain a repair condition"),
        Err(error) => error,
    };
    assert_eq!(second, first);
    assert_eq!(
        runtime_hash_counters_for_root(&root).total_invocations,
        after_first.total_invocations
    );

    drop(cache);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn verified_snapshot_allows_optional_deno_to_resolve_as_absent() {
    let root = unique_test_root("runtime-cache-optional-deno");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("yt-dlp.exe");
    let bytes = b"verified-required-tool";
    fs::write(&path, bytes).unwrap();
    let hash = format!("{:x}", Sha256::digest(bytes));
    let file = open_verified_tool_file(&path, Some(&hash)).unwrap();
    let snapshot = VerifiedRuntimeSnapshot {
        runtime_dir: Some(root.clone()),
        runtime_version: Some("2026.09.08".into()),
        source: "managed".into(),
        tools: HashMap::from([("yt-dlp".into(), VerifiedRuntimeTool { path, file })]),
    };
    let cache = RuntimeCache::new();
    drop(
        cache
            .get_or_initialize(move || async move { Ok(Some(snapshot)) })
            .await
            .unwrap(),
    );

    assert!(resolve_tool_lease_from_cache("deno", &cache, false)
        .unwrap()
        .is_none());
    let required = resolve_tool_lease_from_cache("yt-dlp", &cache, false)
        .unwrap()
        .expect("required tool should remain resolvable");
    drop(required);

    drop(cache);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn bundled_snapshot_verifies_pinned_sidecars_once() {
    let root = unique_test_root("runtime-cache-bundled");
    fs::create_dir_all(&root).unwrap();
    let sidecars = REQUIRED_TOOLS
        .iter()
        .map(|tool| {
            let bytes = format!("{}-bundled-fixture", tool.name);
            fs::write(root.join(tool_exe_name(tool.name)), bytes.as_bytes()).unwrap();
            serde_json::json!({
                "name": tool.name,
                "sourceUrl": format!("https://example.invalid/{}.exe", tool.name),
                "version": "1.0.0",
                "license": "MIT",
                "architecture": "x86_64-pc-windows-msvc",
                "filename": format!("{}-x86_64-pc-windows-msvc.exe", tool.name),
                "sha256": format!("{:x}", Sha256::digest(bytes.as_bytes())),
            })
        })
        .collect::<Vec<_>>();
    let lock = serde_json::json!({
        "schemaVersion": 1,
        "platform": "windows-x86_64",
        "sidecars": sidecars,
    });
    reset_runtime_hash_counters_for_root(&root);

    let snapshot = build_verified_bundled_snapshot_from_lock_at(
        Some(&root),
        &serde_json::to_string(&lock).unwrap(),
        &CancellationToken::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(snapshot.source, "bundled");
    assert_eq!(snapshot.runtime_version, None);
    assert_eq!(snapshot.tools.len(), REQUIRED_TOOLS.len());
    assert_eq!(
        runtime_hash_counters_for_root(&root).manifest_invocations,
        0
    );
    assert_eq!(
        runtime_hash_counters_for_root(&root).tool_invocations,
        REQUIRED_TOOLS.len() as u64
    );

    drop(snapshot);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn embedded_bundled_sidecar_lock_is_strictly_valid() {
    let lock: BundledSidecarLock = serde_json::from_str(BUNDLED_SIDECAR_LOCK).unwrap();
    validate_bundled_sidecar_lock(&lock).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn cached_runtime_tool_lease_pins_verified_file_identity() {
    let root = unique_test_root("runtime-cache-identity-lease");
    fs::create_dir_all(&root).unwrap();
    let runtime_dir = write_phase4_signed_runtime_fixture(&root);
    let cache = RuntimeCache::new();
    let snapshot =
        initialize_test_runtime_cache_at(&cache, root.clone(), verify_phase4_fixture_signature)
            .await
            .unwrap()
            .unwrap();
    let lease = resolve_tool_lease_from_cache("yt-dlp", &cache, false)
        .unwrap()
        .unwrap();
    drop(snapshot);

    let original = runtime_dir.join("yt-dlp.exe");
    let moved = runtime_dir.join("yt-dlp.moved.exe");
    let mutation = {
        let mutation = cache.begin_mutation();
        tokio::pin!(mutation);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut mutation)
                .await
                .is_err(),
            "a returned tool lease must keep cache mutation pending"
        );
        assert!(fs::rename(&original, &moved).is_err());
        drop(lease);
        tokio::time::timeout(Duration::from_secs(1), mutation)
            .await
            .expect("cache mutation should acquire after the tool lease is released")
    };
    fs::rename(&original, &moved).unwrap();
    fs::rename(&moved, &original).unwrap();
    let refreshed = build_verified_runtime_snapshot_async(
        root.clone(),
        None,
        verify_phase4_fixture_signature,
        CancellationToken::new(),
    )
    .await;
    mutation.publish(refreshed);

    drop(cache);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
#[ignore = "Phase 4 performance evidence; run explicitly with --ignored --exact"]
async fn phase4_hash_baseline() {
    const OPERATIONS: u64 = 100;
    let root = unique_test_root("phase4-runtime-hash");
    fs::create_dir_all(&root).unwrap();
    write_phase4_signed_runtime_fixture(&root);
    verify_phase4_fixture_signature(
        "phase4-fixture-key",
        PHASE4_FIXTURE_DESCRIPTOR.as_bytes(),
        PHASE4_FIXTURE_SIGNATURE.as_bytes(),
    )
    .unwrap();

    let label = std::env::var("NUCLEAR_PERF_LABEL").unwrap_or_else(|_| "baseline".into());
    assert!(matches!(label.as_str(), "baseline" | "after"));
    reset_runtime_hash_counters();

    if label == "baseline" {
        let health_runtime =
            discover_managed_runtime_at_with_verifier(&root, true, verify_phase4_fixture_signature)
                .unwrap()
                .expect("signed fixture runtime should be discoverable");
        assert_eq!(health_runtime.1.runtime_version, PHASE4_FIXTURE_VERSION);
        for _ in 0..2 {
            for tool in REQUIRED_TOOLS {
                let lease = resolve_tool_lease_uncached_at(
                    tool.name,
                    &root,
                    verify_phase4_fixture_signature,
                )
                .unwrap()
                .expect("health tool should resolve from the signed fixture");
                assert_eq!(lease.source, "managed");
                drop(lease);
            }
        }
        for _ in 0..OPERATIONS {
            for tool in REQUIRED_TOOLS {
                let lease = resolve_tool_lease_uncached_at(
                    tool.name,
                    &root,
                    verify_phase4_fixture_signature,
                )
                .unwrap()
                .expect("operation tool should resolve from the signed fixture");
                assert_eq!(
                    lease.runtime_version.as_deref(),
                    Some(PHASE4_FIXTURE_VERSION)
                );
                drop(lease);
            }
        }
    } else {
        let cache = RuntimeCache::new();
        let health_snapshot =
            initialize_test_runtime_cache_at(&cache, root.clone(), verify_phase4_fixture_signature)
                .await
                .unwrap()
                .expect("signed fixture runtime should initialize the verified cache");
        assert_eq!(
            health_snapshot.runtime_version.as_deref(),
            Some(PHASE4_FIXTURE_VERSION)
        );
        for _ in 0..2 {
            for tool in REQUIRED_TOOLS {
                let lease = resolve_tool_lease_from_cache(tool.name, &cache, false)
                    .unwrap()
                    .expect("health tool should resolve from the verified cache");
                assert_eq!(lease.source, "managed");
                drop(lease);
            }
        }
        for _ in 0..OPERATIONS {
            for tool in REQUIRED_TOOLS {
                let lease = resolve_tool_lease_from_cache(tool.name, &cache, false)
                    .unwrap()
                    .expect("operation tool should resolve from the verified cache");
                assert_eq!(
                    lease.runtime_version.as_deref(),
                    Some(PHASE4_FIXTURE_VERSION)
                );
                drop(lease);
            }
        }
        drop(health_snapshot);
    }

    let counters = runtime_hash_counters();
    let actual_profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let profile = std::env::var("NUCLEAR_PERF_PROFILE").unwrap_or_else(|_| actual_profile.into());
    assert_eq!(
        profile, actual_profile,
        "performance profile label must be exact"
    );
    assert_eq!(counters.resolution_calls, 408);
    assert_eq!(counters.successful_resolutions, 408);
    if label == "baseline" {
        assert_eq!(counters.manifest_invocations, 409);
        assert_eq!(counters.tool_invocations, 412);
        assert_eq!(counters.total_invocations, 821);
    } else {
        assert_eq!(counters.manifest_invocations, 1);
        assert_eq!(counters.tool_invocations, 4);
        assert_eq!(counters.total_invocations, 5);
    }

    let evidence = serde_json::json!({
        "schemaVersion": 1,
        "label": label,
        "profile": profile,
        "benchmark": "runtime_hash",
        "recordedAtUnixMs": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        "workload": {
            "healthValidations": 1,
            "operations": OPERATIONS,
            "toolNames": REQUIRED_TOOLS.iter().map(|tool| tool.name).collect::<Vec<_>>(),
        },
        "metrics": {
            "hashes": {
                "totalInvocations": counters.total_invocations,
                "totalBytes": counters.total_bytes,
                "manifestInvocations": counters.manifest_invocations,
                "manifestBytes": counters.manifest_bytes,
                "toolInvocations": counters.tool_invocations,
                "toolBytes": counters.tool_bytes,
            },
            "leases": {
                "resolutionCalls": counters.resolution_calls,
                "successfulResolutions": counters.successful_resolutions,
            },
        },
    });
    let evidence_line = serde_json::to_string(&evidence).unwrap();
    println!("PHASE4_PERF_JSON={evidence_line}");
    if let Some(path) = std::env::var_os("NUCLEAR_PERF_EVIDENCE_PATH") {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .unwrap();
        file.write_all(evidence_line.as_bytes()).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();
    }

    fs::remove_dir_all(root).unwrap();
}
