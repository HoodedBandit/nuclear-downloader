use super::manifest::{validate_canonical_sha256, validate_runtime_version};
use futures_util::StreamExt;
use reqwest::header::ACCEPT;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use url::Url;

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/HoodedBandit/nuclear-downloader/releases/latest";
const NETWORK_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const NETWORK_READ_TIMEOUT: Duration = Duration::from_secs(30);
const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const RUNTIME_DESCRIPTOR_LIMIT: u64 = 64 * 1024;
const RUNTIME_SIGNATURE_LIMIT: u64 = 8 * 1024;
const RELEASE_METADATA_LIMIT: u64 = 1024 * 1024;
pub(super) const RUNTIME_ARCHIVE_LIMIT: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub(super) struct GitHubReleaseAsset {
    pub(super) name: String,
    pub(super) browser_download_url: String,
    pub(super) size: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct GitHubRelease {
    pub(super) assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SignedRuntimeDescriptor {
    pub(super) schema_version: u32,
    pub(super) key_id: String,
    pub(super) runtime_version: String,
    pub(super) platform: String,
    pub(super) archive_name: String,
    pub(super) compressed_size: u64,
    pub(super) sha256: String,
    pub(super) manifest_sha256: String,
}

#[derive(Debug, Clone)]
pub(super) struct RuntimeAssetSelection {
    pub(super) version: String,
    pub(super) archive_name: String,
    pub(super) archive_url: String,
    pub(super) archive_size: u64,
    pub(super) archive_sha256: String,
    pub(super) manifest_sha256: String,
    pub(super) descriptor_bytes: Vec<u8>,
    pub(super) signature_bytes: Vec<u8>,
}

pub(super) async fn fetch_latest_runtime_asset() -> Result<Option<RuntimeAssetSelection>, String> {
    let client = build_client()?;
    let release = fetch_latest_release(&client).await?;
    let (descriptor_asset, signature_asset) = select_runtime_descriptor_assets(&release)?;
    let descriptor_bytes = download_bounded_body(
        &client,
        &descriptor_asset.browser_download_url,
        RUNTIME_DESCRIPTOR_LIMIT,
        "runtime descriptor",
    )
    .await?;
    let signature_bytes = download_bounded_body(
        &client,
        &signature_asset.browser_download_url,
        RUNTIME_SIGNATURE_LIMIT,
        "runtime descriptor signature",
    )
    .await?;
    let descriptor = parse_runtime_descriptor(&descriptor_bytes)?;
    crate::updater::verify_release_signature_for_key(
        &descriptor.key_id,
        &descriptor_bytes,
        &signature_bytes,
    )?;
    Ok(Some(select_runtime_archive(
        &release,
        descriptor,
        descriptor_bytes,
        signature_bytes,
    )?))
}

pub(super) fn build_client() -> Result<Client, String> {
    Client::builder()
        .user_agent(
            "NuclearDownloaderRuntime/1 (+https://github.com/HoodedBandit/nuclear-downloader)",
        )
        .connect_timeout(NETWORK_CONNECT_TIMEOUT)
        .read_timeout(NETWORK_READ_TIMEOUT)
        .build()
        .map_err(|error| format!("Failed to prepare runtime update client: {error}"))
}

pub(super) async fn fetch_latest_release(client: &Client) -> Result<GitHubRelease, String> {
    let response = client
        .get(GITHUB_RELEASES_LATEST_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to reach GitHub Releases: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "GitHub runtime update check failed with HTTP {}.",
            status.as_u16()
        ));
    }
    let body =
        read_response_limited(response, RELEASE_METADATA_LIMIT, "GitHub release metadata").await?;
    serde_json::from_slice::<GitHubRelease>(&body)
        .map_err(|error| format!("Failed to parse GitHub release metadata: {error}"))
}

pub(super) async fn read_response_limited(
    response: reqwest::Response,
    limit: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|size| size > limit) {
        return Err(format!("{label} exceeds the {limit}-byte limit."));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Failed while reading {label}: {error}"))?;
        if body.len().saturating_add(chunk.len()) as u64 > limit {
            return Err(format!("{label} exceeds the {limit}-byte limit."));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(super) fn select_runtime_descriptor_assets(
    release: &GitHubRelease,
) -> Result<(&GitHubReleaseAsset, &GitHubReleaseAsset), String> {
    let descriptor =
        select_exact_runtime_asset(release, "nuclear-downloader-runtime-windows-x64.json")?;
    let signature =
        select_exact_runtime_asset(release, "nuclear-downloader-runtime-windows-x64.json.sig")?;
    Ok((descriptor, signature))
}

pub(super) fn select_exact_runtime_asset<'a>(
    release: &'a GitHubRelease,
    expected: &str,
) -> Result<&'a GitHubReleaseAsset, String> {
    let mut matches = release.assets.iter().filter(|asset| asset.name == expected);
    let selected = matches
        .next()
        .ok_or_else(|| format!("Required runtime release asset {expected} was not found."))?;
    if matches.next().is_some() {
        return Err(format!("Runtime release asset {expected} is ambiguous."));
    }
    Ok(selected)
}

pub(super) fn parse_runtime_descriptor(bytes: &[u8]) -> Result<SignedRuntimeDescriptor, String> {
    let descriptor: SignedRuntimeDescriptor = serde_json::from_slice(bytes)
        .map_err(|error| format!("Failed to parse signed runtime descriptor: {error}"))?;
    if descriptor.schema_version != 1 {
        return Err(format!(
            "Unsupported runtime descriptor schema version {}.",
            descriptor.schema_version
        ));
    }
    if !crate::updater::is_canonical_update_key_id(&descriptor.key_id) {
        return Err("Runtime descriptor key ID is not in the canonical release-key format.".into());
    }
    validate_runtime_version(&descriptor.runtime_version)?;
    if descriptor.platform != "windows-x64" {
        return Err("Runtime descriptor platform must be exactly windows-x64.".into());
    }
    let expected_name = format!(
        "nuclear-downloader-runtime-{}-windows-x64.zip",
        descriptor.runtime_version
    );
    if descriptor.archive_name != expected_name {
        return Err("Runtime descriptor archive name does not match its version.".into());
    }
    if descriptor.compressed_size == 0 || descriptor.compressed_size > RUNTIME_ARCHIVE_LIMIT {
        return Err("Runtime descriptor compressed size is outside the allowed range.".into());
    }
    validate_canonical_sha256(&descriptor.sha256)?;
    validate_canonical_sha256(&descriptor.manifest_sha256)?;
    Ok(descriptor)
}

pub(super) fn select_runtime_archive(
    release: &GitHubRelease,
    descriptor: SignedRuntimeDescriptor,
    descriptor_bytes: Vec<u8>,
    signature_bytes: Vec<u8>,
) -> Result<RuntimeAssetSelection, String> {
    let archive = select_exact_runtime_asset(release, &descriptor.archive_name)?;
    let candidates = release
        .assets
        .iter()
        .filter(|asset| {
            asset.name.starts_with("nuclear-downloader-runtime-")
                && asset.name.ends_with("-windows-x64.zip")
        })
        .count();
    if candidates != 1 {
        return Err("The release contains ambiguous or extra runtime archives.".into());
    }
    if archive.size != descriptor.compressed_size {
        return Err("GitHub runtime archive size does not match the signed descriptor.".into());
    }
    Ok(RuntimeAssetSelection {
        version: descriptor.runtime_version,
        archive_name: descriptor.archive_name,
        archive_url: archive.browser_download_url.clone(),
        archive_size: descriptor.compressed_size,
        archive_sha256: descriptor.sha256,
        manifest_sha256: descriptor.manifest_sha256,
        descriptor_bytes,
        signature_bytes,
    })
}

pub(super) async fn download_bounded_body(
    client: &Client,
    url: &str,
    limit: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    validate_https_url(url)?;
    let response = client
        .get(url)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to download {label}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to download {label}: HTTP {}.",
            response.status().as_u16()
        ));
    }
    if response.content_length().is_some_and(|size| size > limit) {
        return Err(format!("{label} exceeds the {limit}-byte limit."));
    }
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Failed while reading {label}: {error}"))?;
        if output.len().saturating_add(chunk.len()) as u64 > limit {
            return Err(format!("{label} exceeds the {limit}-byte limit."));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

pub(super) fn validate_https_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|error| format!("Invalid runtime asset URL: {error}"))?;
    if url.scheme() == "https" {
        Ok(())
    } else {
        Err("Runtime asset URL must use HTTPS.".into())
    }
}
