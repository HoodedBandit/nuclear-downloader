use futures_util::StreamExt;
use reqwest::header::ACCEPT;
use reqwest::{Client, Response};
use serde::Deserialize;
use std::time::Duration;
use url::Url;

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/HoodedBandit/nuclear-downloader/releases/latest";
const NETWORK_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const NETWORK_READ_TIMEOUT: Duration = Duration::from_secs(30);
const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const RELEASE_METADATA_LIMIT: u64 = 1024 * 1024;
const ERROR_BODY_LIMIT: u64 = 8 * 1024;

#[derive(Debug, Deserialize)]
pub(super) struct GitHubReleaseAsset {
    pub(super) name: String,
    pub(super) browser_download_url: String,
    pub(super) size: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct GitHubRelease {
    pub(super) tag_name: String,
    pub(super) body: Option<String>,
    pub(super) published_at: Option<String>,
    pub(super) assets: Vec<GitHubReleaseAsset>,
}

pub(super) fn build_client(user_agent: String) -> Result<Client, String> {
    Client::builder()
        .user_agent(user_agent)
        .connect_timeout(NETWORK_CONNECT_TIMEOUT)
        .read_timeout(NETWORK_READ_TIMEOUT)
        .build()
        .map_err(|error| format!("Failed to prepare update client: {error}"))
}

pub(super) async fn fetch_latest_release(client: &Client) -> Result<GitHubRelease, String> {
    let response = client
        .get(GITHUB_RELEASES_LATEST_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to reach GitHub Releases: {error}"))?;
    let bytes =
        read_bounded_response(response, RELEASE_METADATA_LIMIT, "GitHub release metadata").await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("Failed to parse GitHub release metadata: {error}"))
}

pub(super) async fn download_bounded_success_body(
    client: &Client,
    raw_url: &str,
    limit: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    validate_download_url(raw_url)?;
    let response = client
        .get(raw_url)
        .timeout(METADATA_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Failed to download {label}: {error}"))?;
    read_bounded_response(response, limit, label).await
}

pub(super) async fn read_bounded_response(
    response: Response,
    limit: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    if !response.status().is_success() {
        return Err(read_http_error(response, label).await);
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(format!("{label} exceeds the {limit}-byte limit."));
    }
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Failed while reading {label}: {error}"))?;
        let next_len = output
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| format!("{label} length overflowed."))?;
        if next_len as u64 > limit {
            return Err(format!("{label} exceeds the {limit}-byte limit."));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

pub(super) async fn read_http_error(response: Response, label: &str) -> String {
    let status = response.status();
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(Ok(chunk)) = stream.next().await {
        let remaining = ERROR_BODY_LIMIT.saturating_sub(output.len() as u64) as usize;
        if remaining == 0 {
            break;
        }
        output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    let detail = summarize_error_body(&String::from_utf8_lossy(&output));
    if detail.is_empty() {
        format!("Failed to download {label}: HTTP {}.", status.as_u16())
    } else {
        format!(
            "Failed to download {label}: HTTP {}: {detail}",
            status.as_u16()
        )
    }
}

pub(super) fn validate_download_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|error| format!("Invalid release asset URL: {error}"))?;
    if url.scheme() != "https" {
        return Err("Release asset URLs must use HTTPS.".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Release asset URLs must not contain credentials.".into());
    }
    Ok(())
}

fn summarize_error_body(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(240).collect())
        .unwrap_or_default()
}

pub(super) fn updater_user_agent(version: &str) -> String {
    format!("NuclearDownloader/{version} (+https://github.com/HoodedBandit/nuclear-downloader)")
}
