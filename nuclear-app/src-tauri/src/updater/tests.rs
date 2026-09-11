use super::*;
use reqwest::Client;
use semver::Version;
use sha2::{Digest, Sha256};
use std::io::{Read as _, Write as _};
use std::path::PathBuf;

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

fn release_with_assets(tag_name: &str, assets: Vec<(&str, u64)>) -> GitHubRelease {
    GitHubRelease {
        tag_name: tag_name.to_string(),
        body: Some("notes".to_string()),
        published_at: Some("2026-08-17T00:00:00Z".to_string()),
        assets: assets
            .into_iter()
            .map(|(name, size)| GitHubReleaseAsset {
                name: name.to_string(),
                browser_download_url: format!("https://example.com/{name}"),
                size,
            })
            .collect(),
    }
}

#[test]
fn release_tags_are_exact_and_canonical() {
    assert_eq!(parse_release_tag("v0.6.0").unwrap(), Version::new(0, 6, 0));
    for invalid in [
        "0.6.0",
        "v00.6.0",
        "v0.6.0+build",
        "v0.6.0-beta",
        "v０.６.０",
    ] {
        assert!(parse_release_tag(invalid).is_err(), "accepted {invalid}");
    }
}

#[tokio::test]
async fn bounded_http_reader_rejects_streamed_overflow_without_content_length() {
    let url = start_http_fixture(vec![(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nabcde\r\n0\r\n\r\n"
                .to_vec(),
            Duration::ZERO,
        )]);
    let response = Client::new().get(url).send().await.unwrap();
    let error = read_bounded_response(response, 4, "fixture")
        .await
        .unwrap_err();
    assert!(error.contains("exceeds the 4-byte limit"));
}

#[tokio::test]
async fn bounded_http_reader_enforces_idle_read_timeout() {
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
    let error = read_bounded_response(response, 5, "fixture")
        .await
        .unwrap_err();
    assert!(error.contains("Failed while reading fixture"));
}

#[test]
fn signed_assets_are_exact_and_unambiguous() {
    let version = Version::new(0, 6, 0);
    let release = release_with_assets(
        "v0.6.0",
        vec![
            ("nuclear-downloader-v0.6.0-update.json", 100),
            ("nuclear-downloader-v0.6.0-update.json.sig", 100),
            ("Nuclear.Downloader_0.6.0_x64-setup.exe", 42),
            ("nuclear-downloader-v0.6.0-sha256.txt", 100),
        ],
    );
    let (_, _, installer) = select_signed_update_assets(&release, &version).unwrap();
    assert_eq!(installer.name, expected_installer_name(&version));

    let ambiguous = release_with_assets(
        "v0.6.0",
        vec![
            ("nuclear-downloader-v0.6.0-update.json", 100),
            ("nuclear-downloader-v0.6.0-update.json.sig", 100),
            ("Nuclear.Downloader_0.6.0_x64-setup.exe", 42),
            ("Nuclear.Downloader_0.5.9_x64-setup.exe", 42),
        ],
    );
    assert!(select_signed_update_assets(&ambiguous, &version).is_err());

    let extra_manifest = release_with_assets(
        "v0.6.0",
        vec![
            ("nuclear-downloader-v0.6.0-update.json", 100),
            ("nuclear-downloader-v0.6.0-update.json.sig", 100),
            ("nuclear-downloader-v0.5.9-update.json", 100),
            ("nuclear-downloader-v0.5.9-update.json.sig", 100),
            ("Nuclear.Downloader_0.6.0_x64-setup.exe", 42),
        ],
    );
    assert!(select_signed_update_assets(&extra_manifest, &version).is_err());
}

#[test]
fn manifest_rejects_metadata_mismatch_and_unknown_fields() {
    let version = Version::new(0, 6, 0);
    let installer = GitHubReleaseAsset {
        name: expected_installer_name(&version),
        browser_download_url: "https://example.com/setup.exe".into(),
        size: 43,
    };
    let bytes = br#"{"schemaVersion":1,"keyId":"x","version":"0.6.0","platform":"windows-x86_64","publishedAt":"2026-08-17T00:00:00Z","installer":{"fileName":"Nuclear.Downloader_0.6.0_x64-setup.exe","size":42,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"unexpected":true}"#;
    assert!(parse_and_validate_manifest(bytes, &version, &installer).is_err());
}

#[test]
fn installer_hash_requires_canonical_lowercase_hex() {
    assert!(validate_sha256(&"a".repeat(64)).is_ok());
    assert!(validate_sha256(&"A".repeat(64)).is_err());
    assert!(validate_sha256(&"a".repeat(63)).is_err());
}

#[test]
fn publication_timestamp_requires_a_real_canonical_utc_time() {
    assert!(validate_timestamp("2026-08-17T23:59:59Z").is_ok());
    assert!(validate_timestamp("2026-02-30T00:00:00Z").is_err());
    assert!(validate_timestamp("2026-08-17T24:00:00Z").is_err());
    assert!(validate_timestamp("2026-08-17t23:59:59z").is_err());
}

#[test]
fn download_urls_require_https_without_credentials() {
    assert!(validate_download_url("https://github.com/example/setup.exe").is_ok());
    assert!(validate_download_url("http://github.com/example/setup.exe").is_err());
    assert!(validate_download_url("https://user:pass@example.com/setup.exe").is_err());
}

#[test]
fn minisign_verification_covers_exact_manifest_bytes() {
    let public_key = crate::artifact_contract::TEST_MINISIGN_PUBLIC_KEY;
    let signature = b"untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";
    let wrapped_key = select_public_key(
        "current",
        Some("current"),
        Some(crate::artifact_contract::TEST_TAURI_UPDATE_PUBLIC_KEY),
        Some(""),
        Some(""),
    )
    .unwrap();
    let wrapped_signature = base64_encode(signature);
    assert!(verify_with_public_key(wrapped_key, b"test", wrapped_signature.as_bytes()).is_ok());
    assert!(verify_with_public_key(wrapped_key, b"Test", wrapped_signature.as_bytes()).is_err());
    assert!(verify_with_public_key(
        std::str::from_utf8(public_key).unwrap(),
        b"test",
        wrapped_signature.as_bytes()
    )
    .is_err());
}

#[test]
fn key_rotation_selects_by_id_and_rejects_bad_configuration() {
    assert_eq!(
        select_public_key(
            "next",
            Some("current"),
            Some("current-key"),
            Some("next"),
            Some("next-key")
        )
        .unwrap(),
        "next-key"
    );
    assert!(
        select_public_key("current", Some("same"), Some("a"), Some("same"), Some("b")).is_err()
    );
    assert!(select_public_key("current", Some("current"), None, None, None).is_err());
    assert!(select_public_key("unknown", Some("current"), Some("a"), None, None).is_err());
}

#[test]
fn key_rotation_empty_environment_slots_match_build_validation() {
    let public_key = crate::artifact_contract::TEST_TAURI_UPDATE_PUBLIC_KEY;
    for next_id in [None, Some("")] {
        for next_key in [None, Some("")] {
            crate::build_config::validate_update_key_configuration(
                "release",
                "current",
                public_key,
                next_id.unwrap_or_default(),
                next_key.unwrap_or_default(),
            )
            .unwrap();
            assert_eq!(
                select_public_key(
                    "current",
                    Some("current"),
                    Some(public_key),
                    next_id,
                    next_key,
                )
                .unwrap(),
                public_key
            );
            assert!(select_public_key(
                "unknown",
                Some("current"),
                Some(public_key),
                next_id,
                next_key,
            )
            .is_err());
        }
    }
}

#[test]
fn key_rotation_partial_or_malformed_environment_slots_fail_closed() {
    let public_key = crate::artifact_contract::TEST_TAURI_UPDATE_PUBLIC_KEY;
    for (next_id, next_key) in [
        (Some("next"), None),
        (Some("next"), Some("")),
        (None, Some(public_key)),
        (Some(""), Some(public_key)),
        (Some(" "), Some(public_key)),
        (Some(""), Some(" ")),
        (Some("current"), Some(public_key)),
    ] {
        assert!(crate::build_config::validate_update_key_configuration(
            "release",
            "current",
            public_key,
            next_id.unwrap_or_default(),
            next_key.unwrap_or_default(),
        )
        .is_err());
        assert!(select_public_key(
            "current",
            Some("current"),
            Some(public_key),
            next_id,
            next_key,
        )
        .is_err());
    }
}

#[tokio::test]
async fn existing_installer_must_match_signed_size_and_hash() {
    let root = unique_test_root("updater-existing");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
    std::fs::write(&path, b"verified bytes").unwrap();
    let hash = format!("{:x}", Sha256::digest(b"verified bytes"));
    assert!(open_verified_installer(&path, 14, &hash)
        .await
        .unwrap()
        .is_some());
    assert!(open_verified_installer(&path, 13, &hash)
        .await
        .unwrap()
        .is_none());
    assert!(open_verified_installer(&path, 14, &"a".repeat(64))
        .await
        .unwrap()
        .is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn invalid_cached_installer_is_quarantined_before_fresh_verification() {
    let root = unique_test_root("updater-invalid-cache");
    let directory_lock = UpdateDirectoryLock::acquire(&root).unwrap();
    let path = root.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
    let unrelated = root.join("keep-me.exe");
    std::fs::write(&path, b"invalid-bytes!").unwrap();
    std::fs::write(&unrelated, b"unrelated").unwrap();
    let fresh_bytes = b"verified bytes";
    let fresh_hash = format!("{:x}", Sha256::digest(fresh_bytes));
    write_owner_record(&path, fresh_bytes.len() as u64, &fresh_hash)
        .await
        .unwrap();

    assert!(matches!(
        open_or_quarantine_cached_installer(&root, &path, fresh_bytes.len() as u64, &fresh_hash,)
            .await
            .unwrap(),
        CachedInstaller::Vacant
    ));
    assert!(!path.exists());
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");
    let quarantined = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".quarantine"))
        .collect::<Vec<_>>();
    assert_eq!(quarantined.len(), 1);

    std::fs::write(&path, fresh_bytes).unwrap();
    let installer =
        open_or_quarantine_cached_installer(&root, &path, fresh_bytes.len() as u64, &fresh_hash)
            .await
            .unwrap();
    assert!(matches!(installer, CachedInstaller::Verified(_)));
    drop(directory_lock);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn unowned_canonical_collision_is_preserved_while_prepared_path_is_verified() {
    let root = unique_test_root("updater-unowned-collision");
    let directory_lock = UpdateDirectoryLock::acquire(&root).unwrap();
    let canonical = root.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
    std::fs::write(&canonical, b"unrelated legacy bytes").unwrap();
    let fresh = b"verified bytes";
    let hash = format!("{:x}", Sha256::digest(fresh));

    assert!(matches!(
        open_or_quarantine_cached_installer(&root, &canonical, fresh.len() as u64, &hash)
            .await
            .unwrap(),
        CachedInstaller::UnownedCollision
    ));
    assert_eq!(
        std::fs::read(&canonical).unwrap(),
        b"unrelated legacy bytes"
    );

    let prepared = create_prepared_directory(&root, fresh.len() as u64, &hash)
        .await
        .unwrap();
    let prepared_installer = prepared.join(canonical.file_name().unwrap());
    std::fs::write(&prepared_installer, fresh).unwrap();
    write_owner_record(&prepared_installer, fresh.len() as u64, &hash)
        .await
        .unwrap();
    let verified = open_verified_installer(&prepared_installer, fresh.len() as u64, &hash)
        .await
        .unwrap();
    assert!(verified.is_some());
    assert_eq!(
        std::fs::read(&canonical).unwrap(),
        b"unrelated legacy bytes"
    );
    drop(verified);
    drop(directory_lock);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn prepared_cleanup_requires_complete_owned_non_reparse_contents() {
    let root = unique_test_root("updater-prepared-cleanup");
    std::fs::create_dir_all(&root).unwrap();
    let bytes = b"verified bytes";
    let hash = format!("{:x}", Sha256::digest(bytes));

    let owned = create_prepared_directory(&root, bytes.len() as u64, &hash)
        .await
        .unwrap();
    let owned_installer = owned.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
    std::fs::write(&owned_installer, bytes).unwrap();
    write_owner_record(&owned_installer, bytes.len() as u64, &hash)
        .await
        .unwrap();
    let owned_record = owner_record_path(&owned).unwrap();

    let unowned = root.join("prepared-550e8400-e29b-41d4-a716-446655440000");
    std::fs::create_dir(&unowned).unwrap();
    std::fs::write(unowned.join("keep.txt"), b"keep").unwrap();

    let ambiguous = create_prepared_directory(&root, bytes.len() as u64, &hash)
        .await
        .unwrap();
    std::fs::write(ambiguous.join("unrelated.txt"), b"keep").unwrap();

    cleanup_owned_prepared_directories(&root).await.unwrap();
    assert!(!owned.exists());
    assert!(!owned_record.exists());
    assert_eq!(std::fs::read(unowned.join("keep.txt")).unwrap(), b"keep");
    assert_eq!(
        std::fs::read(ambiguous.join("unrelated.txt")).unwrap(),
        b"keep"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[tokio::test]
async fn verified_installer_lease_denies_mutation_and_replacement() {
    let root = unique_test_root("updater-installer-lease");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
    let replacement = root.join("replacement.exe");
    std::fs::write(&path, b"verified bytes").unwrap();
    std::fs::write(&replacement, b"replacement!!!").unwrap();
    let hash = format!("{:x}", Sha256::digest(b"verified bytes"));
    let installer = open_verified_installer(&path, 14, &hash)
        .await
        .unwrap()
        .unwrap();

    assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
    assert!(std::fs::rename(&replacement, &path).is_err());
    assert_eq!(installer.path(), path);

    drop(installer);
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[tokio::test]
async fn installer_handoff_retains_verification_and_directory_leases() {
    let root = unique_test_root("updater-handoff-lease");
    let directory_lock = UpdateDirectoryLock::acquire(&root).unwrap();
    let path = root.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
    std::fs::write(&path, b"verified bytes").unwrap();
    let hash = format!("{:x}", Sha256::digest(b"verified bytes"));
    let installer = open_verified_installer(&path, 14, &hash)
        .await
        .unwrap()
        .unwrap();
    let handoff = InstallerHandoff {
        expected_version: "0.6.0".into(),
        installer_name: "Nuclear.Downloader_0.6.0_x64-setup.exe".into(),
        installer_size: 14,
        installer,
        _directory_lock: directory_lock,
    };

    assert_eq!(handoff.expected_version(), "0.6.0");
    assert_eq!(
        handoff.installer_name(),
        "Nuclear.Downloader_0.6.0_x64-setup.exe"
    );
    assert_eq!(handoff.installer_size(), 14);
    assert_eq!(handoff.installer_path(), path);
    assert!(UpdateDirectoryLock::acquire(&root).is_err());
    assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());

    drop(handoff);
    let reacquired = UpdateDirectoryLock::acquire(&root).unwrap();
    drop(reacquired);
    std::fs::remove_file(path).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn partial_cleanup_removes_only_owned_regular_files() {
    let root = unique_test_root("updater-partials");
    std::fs::create_dir_all(&root).unwrap();
    let owned = root
        .join("Nuclear.Downloader_0.6.0_x64-setup.exe.550e8400-e29b-41d4-a716-446655440000.part");
    let unrelated = root.join("someone-elses-download.part");
    let unowned = root
        .join("Nuclear.Downloader_0.6.0_x64-setup.exe.650e8400-e29b-41d4-a716-446655440000.part");
    std::fs::write(&owned, b"partial").unwrap();
    std::fs::write(&unrelated, b"keep").unwrap();
    std::fs::write(&unowned, b"unowned").unwrap();
    write_owner_record(&owned, 42, &"a".repeat(64))
        .await
        .unwrap();
    cleanup_owned_partial_installers(&root).await.unwrap();
    assert!(!owned.exists());
    assert!(unrelated.exists());
    assert!(unowned.exists());
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[tokio::test]
async fn failed_partial_removal_retains_ownership_for_startup_retry() {
    let root = unique_test_root("updater-partial-cleanup-retry");
    let directory_lock = UpdateDirectoryLock::acquire(&root).unwrap();
    let part = root
        .join("Nuclear.Downloader_0.6.0_x64-setup.exe.550e8400-e29b-41d4-a716-446655440000.part");
    let bytes = b"verified fixture bytes";
    let hash = format!("{:x}", Sha256::digest(bytes));
    std::fs::write(&part, bytes).unwrap();
    let owner = write_owner_record(&part, bytes.len() as u64, &hash)
        .await
        .unwrap();
    let lease = open_verified_installer(&part, bytes.len() as u64, &hash)
        .await
        .unwrap()
        .unwrap();

    artifact::cleanup_current_artifact(&part).await;
    assert!(
        part.exists(),
        "the fixture lease must prevent artifact deletion"
    );
    assert!(
        owner.exists(),
        "failed deletion must retain proof for retry"
    );

    drop(lease);
    cleanup_owned_partial_installers(&root).await.unwrap();
    assert!(!part.exists());
    assert!(!owner.exists());
    drop(directory_lock);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ownership_record_growth_is_bounded_and_unrelated_files_are_preserved() {
    let root = unique_test_root("updater-owner-record-growth");
    std::fs::create_dir_all(&root).unwrap();
    let artifact = root
        .join("Nuclear.Downloader_0.6.0_x64-setup.exe.550e8400-e29b-41d4-a716-446655440000.part");
    let unrelated = root.join("unrelated.txt");
    std::fs::write(&artifact, b"partial").unwrap();
    std::fs::write(&unrelated, b"keep").unwrap();
    let record_path = write_owner_record(&artifact, 7, &"a".repeat(64))
        .await
        .unwrap();
    let stale_small_metadata = std::fs::symlink_metadata(&record_path).unwrap();
    let mut oversized = std::fs::read(&record_path).unwrap();
    oversized.resize(4 * 1024 + 1, b' ');
    std::fs::write(&record_path, &oversized).unwrap();

    let error = artifact::read_owner_record_bytes(&record_path, &stale_small_metadata)
        .await
        .unwrap_err();

    assert_eq!(
        error,
        "The updater ownership data is not a regular bounded file."
    );
    assert_eq!(std::fs::read(&record_path).unwrap(), oversized);
    assert_eq!(std::fs::read(&artifact).unwrap(), b"partial");
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn old_installer_cleanup_keeps_current_and_unrelated_files() {
    let root = unique_test_root("updater-old-installers");
    std::fs::create_dir_all(&root).unwrap();
    let old = root.join("Nuclear.Downloader_0.6.0_x64-setup.exe");
    let current = root.join("Nuclear.Downloader_0.6.1_x64-setup.exe");
    let noncanonical = root.join("Nuclear.Downloader_00.6.0_x64-setup.exe");
    let unrelated = root.join("keep.exe");
    let unowned_old = root.join("Nuclear.Downloader_0.5.9_x64-setup.exe");
    for path in [&old, &current, &noncanonical, &unrelated, &unowned_old] {
        std::fs::write(path, b"data").unwrap();
    }
    write_owner_record(&old, 4, &format!("{:x}", Sha256::digest(b"data")))
        .await
        .unwrap();

    cleanup_owned_old_installers(&root, current.file_name().unwrap().to_str().unwrap())
        .await
        .unwrap();
    assert!(!old.exists());
    assert!(current.exists());
    assert!(noncanonical.exists());
    assert!(unrelated.exists());
    assert!(unowned_old.exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn updater_lock_is_retained_and_reused_after_release() {
    let root = unique_test_root("updater-lock");
    {
        let _lock = UpdateDirectoryLock::acquire(&root).unwrap();
    }
    assert!(root.join(UPDATE_LOCK_FILE_NAME).is_file());
    {
        let _lock = UpdateDirectoryLock::acquire(&root).unwrap();
    }
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[tokio::test]
async fn updater_rejects_reparse_root_and_preserves_reparse_partial() {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    let root = unique_test_root("updater-reparse");
    let actual = root.join("actual");
    let linked = root.join("linked");
    std::fs::create_dir_all(&actual).unwrap();
    if symlink_dir(&actual, &linked).is_ok() {
        assert!(UpdateDirectoryLock::acquire(&linked).is_err());
        std::fs::remove_dir(&linked).unwrap();
    }

    let target = root.join("target.txt");
    std::fs::write(&target, b"keep").unwrap();
    let partial = actual
        .join("Nuclear.Downloader_0.6.0_x64-setup.exe.550e8400-e29b-41d4-a716-446655440000.part");
    if symlink_file(&target, &partial).is_ok() {
        cleanup_owned_partial_installers(&actual).await.unwrap();
        assert!(partial.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        std::fs::remove_file(&partial).unwrap();
    }
    let _ = std::fs::remove_dir_all(root);
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

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let value = (a << 16) | (b << 8) | c;
        output.push(ALPHABET[((value >> 18) & 0x3f) as usize] as char);
        output.push(ALPHABET[((value >> 12) & 0x3f) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(value & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}
