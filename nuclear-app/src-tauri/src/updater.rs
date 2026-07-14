use crate::models::{UpdateCheckResult, UpdateInstallProgress};
use futures_util::StreamExt;
use reqwest::header::ACCEPT;
use reqwest::Client;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use url::Url;

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/HoodedBandit/nuclear-downloader/releases/latest";
const UPDATE_PROGRESS_EVENT: &str = "update-install-progress";
const UPDATE_TEMP_DIR_NAME: &str = "nuclear-downloader-updater";
const NETWORK_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const NETWORK_READ_TIMEOUT: Duration = Duration::from_secs(30);
const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
static UPDATE_INSTALL_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

struct UpdateInstallGuard;

impl Drop for UpdateInstallGuard {
    fn drop(&mut self) {
        UPDATE_INSTALL_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Deserialize)]
struct GitHubReleaseAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    body: Option<String>,
    published_at: Option<String>,
    assets: Vec<GitHubReleaseAsset>,
}

pub async fn check_for_app_update(app: &AppHandle) -> Result<UpdateCheckResult, String> {
    let current_version = app.package_info().version.to_string();
    let current_semver = parse_semver(&current_version)?;
    let client = build_client(updater_user_agent(&current_version))?;
    let release = fetch_latest_release(&client).await?;
    let latest_semver = parse_semver(&release.tag_name)?;
    let has_update = latest_semver > current_semver;
    let installer_name = if has_update {
        Some(select_nsis_installer_asset(&release)?.name.clone())
    } else {
        None
    };

    Ok(UpdateCheckResult {
        current_version,
        has_update,
        latest_version: Some(latest_semver.to_string()),
        notes: normalize_optional_text(release.body),
        published_at: normalize_optional_text(release.published_at),
        installer_name,
    })
}

pub async fn install_app_update(app: &AppHandle, expected_version: String) -> Result<(), String> {
    if UPDATE_INSTALL_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("An app update is already in progress.".into());
    }
    let _guard = UpdateInstallGuard;

    install_app_update_inner(app, expected_version.clone())
        .await
        .inspect_err(|error| {
            emit_install_progress(
                app,
                UpdateInstallProgress {
                    status: "error".to_string(),
                    version: normalize_version_label(&expected_version),
                    downloaded_bytes: 0,
                    total_bytes: None,
                    message: Some(error.clone()),
                },
            );
        })
}

async fn install_app_update_inner(app: &AppHandle, expected_version: String) -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        let _ = expected_version;
        return Err("Automatic updates are only supported on Windows builds.".into());
    }

    #[cfg(target_os = "windows")]
    {
        let expected_semver = parse_semver(&expected_version)?;
        let current_semver = parse_semver(&app.package_info().version.to_string())?;
        let expected_version_string = expected_semver.to_string();
        let client = build_client(updater_user_agent(&current_semver.to_string()))?;
        let release = fetch_latest_release(&client).await?;
        let latest_semver = parse_semver(&release.tag_name)?;

        if latest_semver != expected_semver {
            return Err(format!(
                "The latest GitHub release changed from {} to {}. Please check for updates again.",
                expected_version_string, latest_semver
            ));
        }

        if latest_semver <= current_semver {
            return Err(format!(
                "No newer update is available. Current version is {}.",
                current_semver
            ));
        }

        let installer = select_nsis_installer_asset(&release)?;
        let checksum_asset = select_checksum_manifest_asset(&release, &latest_semver.to_string())?;
        let expected_checksum =
            download_installer_checksum(&client, checksum_asset, &installer.name).await?;
        let installer_path = download_installer(
            app,
            &client,
            &latest_semver.to_string(),
            installer,
            &expected_checksum,
        )
        .await?;

        emit_install_progress(
            app,
            UpdateInstallProgress {
                status: "launching".to_string(),
                version: latest_semver.to_string(),
                downloaded_bytes: installer.size,
                total_bytes: Some(installer.size),
                message: Some(format!(
                    "Launching {}. Nuclear Downloader will close and reopen after install.",
                    installer.name
                )),
            },
        );

        std::process::Command::new(&installer_path)
            .args(["/S", "/R"])
            .spawn()
            .map_err(|error| {
                format!(
                    "Failed to launch installer at {}: {}",
                    installer_path.display(),
                    error
                )
            })?;

        app.exit(0);
        Ok(())
    }
}

fn build_client(user_agent: String) -> Result<Client, String> {
    Client::builder()
        .user_agent(user_agent)
        .connect_timeout(NETWORK_CONNECT_TIMEOUT)
        .read_timeout(NETWORK_READ_TIMEOUT)
        .build()
        .map_err(|error| format!("Failed to prepare update client: {}", error))
}

async fn fetch_latest_release(client: &Client) -> Result<GitHubRelease, String> {
    let response = client
        .get(GITHUB_RELEASES_LATEST_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to reach GitHub Releases: {}", error))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read GitHub response: {}", error))?;

    if !status.is_success() {
        let detail = summarize_error_body(&body);
        return Err(if detail.is_empty() {
            format!("GitHub update check failed with HTTP {}.", status.as_u16())
        } else {
            format!(
                "GitHub update check failed with HTTP {}: {}",
                status.as_u16(),
                detail
            )
        });
    }

    serde_json::from_str::<GitHubRelease>(&body)
        .map_err(|error| format!("Failed to parse GitHub release metadata: {}", error))
}

async fn download_installer(
    app: &AppHandle,
    client: &Client,
    version: &str,
    installer: &GitHubReleaseAsset,
    expected_checksum: &str,
) -> Result<PathBuf, String> {
    validate_installer_download_url(&installer.browser_download_url)?;

    let target_dir = std::env::temp_dir().join(UPDATE_TEMP_DIR_NAME);
    fs::create_dir_all(&target_dir)
        .await
        .map_err(|error| format!("Failed to create update temp folder: {}", error))?;

    let file_name = sanitize_installer_name(&installer.name)?;
    cleanup_stale_update_files(&target_dir, &file_name).await;
    let final_path = target_dir.join(&file_name);
    let part_path = target_dir.join(format!("{}.part", file_name));

    if fs::try_exists(&part_path).await.unwrap_or(false) {
        let _ = fs::remove_file(&part_path).await;
    }

    let response = client
        .get(&installer.browser_download_url)
        .send()
        .await
        .map_err(|error| format!("Failed to download update installer: {}", error))?;

    let status = response.status();
    if !status.is_success() {
        let detail = response
            .text()
            .await
            .ok()
            .map(|body| summarize_error_body(&body))
            .unwrap_or_default();

        return Err(if detail.is_empty() {
            format!(
                "Failed to download update installer: GitHub returned HTTP {}.",
                status.as_u16()
            )
        } else {
            format!(
                "Failed to download update installer: GitHub returned HTTP {}: {}",
                status.as_u16(),
                detail
            )
        });
    }

    let total_bytes = response
        .content_length()
        .or_else(|| (installer.size > 0).then_some(installer.size));
    let mut stream = response.bytes_stream();
    let mut file = fs::File::create(&part_path)
        .await
        .map_err(|error| format!("Failed to create installer temp file: {}", error))?;
    let mut downloaded_bytes = 0u64;
    let mut hasher = Sha256::new();

    emit_install_progress(
        app,
        UpdateInstallProgress {
            status: "downloading".to_string(),
            version: version.to_string(),
            downloaded_bytes,
            total_bytes,
            message: Some(format!("Downloading {}...", installer.name)),
        },
    );

    while let Some(chunk_result) = stream.next().await {
        let chunk = match chunk_result {
            Ok(chunk) => chunk,
            Err(error) => {
                cleanup_file_if_exists(&part_path).await;
                return Err(format!(
                    "Failed while downloading update installer: {}",
                    error
                ));
            }
        };

        if let Err(error) = file.write_all(&chunk).await {
            cleanup_file_if_exists(&part_path).await;
            return Err(format!("Failed to write installer download: {}", error));
        }

        downloaded_bytes += chunk.len() as u64;
        hasher.update(&chunk);

        emit_install_progress(
            app,
            UpdateInstallProgress {
                status: "downloading".to_string(),
                version: version.to_string(),
                downloaded_bytes,
                total_bytes,
                message: Some(format!("Downloading {}...", installer.name)),
            },
        );
    }

    if let Err(error) = file.flush().await {
        cleanup_file_if_exists(&part_path).await;
        return Err(format!("Failed to finalize installer download: {}", error));
    }
    drop(file);

    if let Some(expected_bytes) = total_bytes {
        if downloaded_bytes != expected_bytes {
            cleanup_file_if_exists(&part_path).await;
            return Err(format!(
                "Downloaded installer size mismatch: expected {} bytes, got {} bytes.",
                expected_bytes, downloaded_bytes
            ));
        }
    }

    let actual_checksum = format!("{:x}", hasher.finalize());
    emit_install_progress(
        app,
        UpdateInstallProgress {
            status: "verifying".to_string(),
            version: version.to_string(),
            downloaded_bytes,
            total_bytes,
            message: Some(format!("Verifying {}...", installer.name)),
        },
    );
    if !actual_checksum.eq_ignore_ascii_case(expected_checksum) {
        cleanup_file_if_exists(&part_path).await;
        return Err(format!(
            "Downloaded installer checksum mismatch: expected {expected_checksum}, got {actual_checksum}."
        ));
    }

    if fs::try_exists(&final_path).await.unwrap_or(false) {
        fs::remove_file(&final_path)
            .await
            .map_err(|error| format!("Failed to replace previous installer download: {}", error))?;
    }

    fs::rename(&part_path, &final_path)
        .await
        .map_err(|error| format!("Failed to move installer into place: {}", error))?;

    Ok(final_path)
}

async fn download_installer_checksum(
    client: &Client,
    checksum_asset: &GitHubReleaseAsset,
    installer_name: &str,
) -> Result<String, String> {
    validate_installer_download_url(&checksum_asset.browser_download_url)?;
    if checksum_asset.size > 1024 * 1024 {
        return Err("Update checksum manifest is unexpectedly large.".into());
    }

    let response = client
        .get(&checksum_asset.browser_download_url)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to download update checksum manifest: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read update checksum manifest: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "Failed to download update checksum manifest: HTTP {}.",
            status.as_u16()
        ));
    }

    parse_checksum_for_asset(&body, installer_name).ok_or_else(|| {
        format!(
            "Update checksum manifest did not contain a valid SHA-256 entry for {installer_name}."
        )
    })
}

async fn cleanup_stale_update_files(target_dir: &std::path::Path, keep_name: &str) {
    let Ok(mut entries) = fs::read_dir(target_dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".part")
            || (lower.ends_with("-setup.exe") && !name.eq_ignore_ascii_case(keep_name))
        {
            let _ = fs::remove_file(entry.path()).await;
        }
    }
}

fn validate_installer_download_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|error| {
        format!(
            "GitHub release returned an invalid installer URL: {}",
            error
        )
    })?;

    if url.scheme() != "https" {
        return Err("GitHub release returned a non-HTTPS installer URL.".into());
    }

    Ok(())
}

async fn cleanup_file_if_exists(path: &std::path::Path) {
    if fs::try_exists(path).await.unwrap_or(false) {
        let _ = fs::remove_file(path).await;
    }
}

fn select_nsis_installer_asset(release: &GitHubRelease) -> Result<&GitHubReleaseAsset, String> {
    release
        .assets
        .iter()
        .filter_map(|asset| installer_asset_score(&asset.name).map(|score| (score, asset)))
        .max_by_key(|(score, _asset)| *score)
        .map(|(_score, asset)| asset)
        .ok_or_else(|| "Update package not found in the latest GitHub release.".to_string())
}

fn select_checksum_manifest_asset<'a>(
    release: &'a GitHubRelease,
    version: &str,
) -> Result<&'a GitHubReleaseAsset, String> {
    let expected = format!("nuclear-downloader-v{version}-sha256.txt");
    release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(&expected))
        .ok_or_else(|| format!("Update checksum manifest {expected} was not found."))
}

fn parse_checksum_for_asset(body: &str, asset_name: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let checksum = parts.next()?;
        let filename = parts.next()?.trim_start_matches('*');
        if checksum.len() == 64
            && checksum.chars().all(|ch| ch.is_ascii_hexdigit())
            && filename.eq_ignore_ascii_case(asset_name)
        {
            Some(checksum.to_ascii_lowercase())
        } else {
            None
        }
    })
}

fn installer_asset_score(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with("-setup.exe") || !lower.contains("x64") {
        return None;
    }

    let normalized = lower.replace([' ', '_'], ".");
    if normalized.contains("nuclear.downloader") {
        Some(2)
    } else if normalized.contains("nuclear") && normalized.contains("downloader") {
        Some(1)
    } else {
        None
    }
}

fn parse_semver(raw: &str) -> Result<Version, String> {
    let trimmed = raw.trim();
    let normalized = trimmed.strip_prefix('v').unwrap_or(trimmed);
    Version::parse(normalized)
        .map_err(|error| format!("Invalid release version '{}': {}", raw, error))
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        (!trimmed.is_empty()).then_some(trimmed.to_string())
    })
}

fn sanitize_installer_name(name: &str) -> Result<String, String> {
    if name.trim().is_empty() || name.contains('/') || name.contains('\\') {
        return Err("GitHub release returned an invalid installer filename.".into());
    }

    Ok(name.to_string())
}

fn summarize_error_body(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            if line.len() > 240 {
                format!("{}...", &line[..240])
            } else {
                line.to_string()
            }
        })
        .unwrap_or_default()
}

fn normalize_version_label(raw: &str) -> String {
    parse_semver(raw)
        .map(|version| version.to_string())
        .unwrap_or_else(|_| raw.trim().trim_start_matches('v').to_string())
}

fn updater_user_agent(version: &str) -> String {
    format!(
        "NuclearDownloader/{} (+https://github.com/HoodedBandit/nuclear-downloader)",
        version
    )
}

fn emit_install_progress(app: &AppHandle, payload: UpdateInstallProgress) {
    let _ = app.emit(UPDATE_PROGRESS_EVENT, payload);
}

#[cfg(test)]
mod tests {
    use super::{
        installer_asset_score, parse_checksum_for_asset, parse_semver,
        select_checksum_manifest_asset, select_nsis_installer_asset,
        validate_installer_download_url, GitHubRelease, GitHubReleaseAsset,
    };

    fn release_with_assets(tag_name: &str, assets: Vec<&str>) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag_name.to_string(),
            body: Some("notes".to_string()),
            published_at: Some("2026-04-18T00:00:00Z".to_string()),
            assets: assets
                .into_iter()
                .map(|name| GitHubReleaseAsset {
                    name: name.to_string(),
                    browser_download_url: format!("https://example.com/{name}"),
                    size: 123,
                })
                .collect(),
        }
    }

    #[test]
    fn parse_semver_accepts_leading_v() {
        assert_eq!(parse_semver("v0.4.3").unwrap().to_string(), "0.4.3");
    }

    #[test]
    fn parse_semver_rejects_invalid_tags() {
        assert!(parse_semver("release-next").is_err());
    }

    #[test]
    fn installer_asset_score_accepts_space_and_dot_variants() {
        assert_eq!(
            installer_asset_score("Nuclear.Downloader_0.4.2_x64-setup.exe"),
            Some(2)
        );
        assert_eq!(
            installer_asset_score("Nuclear Downloader_0.4.2_x64-setup.exe"),
            Some(2)
        );
    }

    #[test]
    fn select_nsis_installer_prefers_setup_exe_over_other_assets() {
        let release = release_with_assets(
            "v0.4.3",
            vec![
                "nuclear.exe",
                "Nuclear.Downloader_0.4.3_x64.msi",
                "Nuclear.Downloader_0.4.3_x64-setup.exe",
            ],
        );

        let asset = select_nsis_installer_asset(&release).unwrap();
        assert_eq!(asset.name, "Nuclear.Downloader_0.4.3_x64-setup.exe");
    }

    #[test]
    fn select_nsis_installer_errors_when_setup_asset_is_missing() {
        let release = release_with_assets(
            "v0.4.3",
            vec!["nuclear.exe", "Nuclear.Downloader_0.4.3_x64.msi"],
        );

        assert!(select_nsis_installer_asset(&release).is_err());
    }

    #[test]
    fn validate_installer_download_url_requires_https() {
        assert!(validate_installer_download_url("https://github.com/example/setup.exe").is_ok());
        assert!(validate_installer_download_url("http://github.com/example/setup.exe").is_err());
    }

    #[test]
    fn selects_only_the_exact_versioned_checksum_manifest() {
        let release = release_with_assets(
            "v0.5.3",
            vec![
                "nuclear-downloader-v0.5.2-sha256.txt",
                "nuclear-downloader-v0.5.3-sha256.txt",
            ],
        );

        assert_eq!(
            select_checksum_manifest_asset(&release, "0.5.3")
                .unwrap()
                .name,
            "nuclear-downloader-v0.5.3-sha256.txt"
        );
        assert!(select_checksum_manifest_asset(&release, "0.5.4").is_err());
    }

    #[test]
    fn checksum_parser_requires_an_exact_installer_filename() {
        let checksum = "A".repeat(64);
        let body =
            format!("{checksum}  Nuclear.Downloader_0.5.3_x64-setup.exe\n{checksum} *nuclear.exe");

        assert_eq!(
            parse_checksum_for_asset(&body, "Nuclear.Downloader_0.5.3_x64-setup.exe"),
            Some(checksum.to_ascii_lowercase())
        );
        assert!(parse_checksum_for_asset(&body, "other.exe").is_none());
    }
}
