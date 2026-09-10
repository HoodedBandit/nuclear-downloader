use super::command_args::{
    append_cookie_args, append_twitter_syndication_args, append_ytdlp_runtime_args,
};
use super::errors::{error_for_fetch, should_retry_with_twitter_syndication};
use super::process::{wait_with_bounded_output, DownloadJob, ProcessSpawnError, MAX_STDERR_BYTES};
use super::validation::validate_fetch_request;
use crate::models::{CookieConfig, MediaSelection, UrlInspection};
use std::future::Future;
use std::time::Duration;
use tokio::process::Command;

mod metadata;

const MAX_INSPECTION_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const INSPECTION_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PLAYLIST_ENTRIES: usize = 1_000;

fn append_inspection_args(args: &mut Vec<String>, selection: Option<&MediaSelection>) {
    args.extend(["--dump-single-json".into(), "--no-download".into()]);
    if let Some(selection) = selection {
        args.extend([
            "--yes-playlist".into(),
            "--no-flat-playlist".into(),
            "--playlist-items".into(),
            selection.playlist_index.to_string(),
        ]);
    } else {
        // Discover the bounded list without first extracting its first video.
        // A single-video URL still receives full format metadata with this flag.
        args.extend([
            "--flat-playlist".into(),
            "--lazy-playlist".into(),
            "--playlist-end".into(),
            (MAX_PLAYLIST_ENTRIES + 1).to_string(),
        ]);
    }
}

async fn run_fetch_info_command(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
    selection: Option<&MediaSelection>,
    use_twitter_syndication: bool,
    job: &DownloadJob,
) -> Result<std::process::Output, String> {
    let bin = job.required_runtime_tool("yt-dlp")?;
    let runtime_config = job.ytdlp_runtime_config()?;
    let mut args = Vec::new();
    append_ytdlp_runtime_args(&mut args, &runtime_config, compat_config_path);
    append_inspection_args(&mut args, selection);
    append_twitter_syndication_args(&mut args, url, use_twitter_syndication);
    if let Some(config) = cookie_config {
        append_cookie_args(&mut args, config);
    }
    args.push(url.to_string());

    let mut cmd = Command::new(&bin);
    cmd.kill_on_drop(true);
    cmd.args(&args);
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = job
        .spawn(&mut cmd, "yt-dlp", false)
        .await
        .map_err(ProcessSpawnError::into_message)?;
    wait_with_bounded_output(
        child,
        job,
        MAX_INSPECTION_OUTPUT_BYTES,
        MAX_STDERR_BYTES,
        INSPECTION_TIMEOUT,
    )
    .await
}

async fn with_inspection_timeout<T, F>(
    job: &DownloadJob,
    timeout: Duration,
    operation: F,
) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    match tokio::time::timeout(timeout, operation).await {
        Ok(result) => result,
        Err(_) => {
            job.terminate_processes();
            Err("process_timeout: inspection timed out".to_string())
        }
    }
}

pub(crate) async fn inspect_url(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
    selection: Option<&MediaSelection>,
    job: &DownloadJob,
) -> Result<UrlInspection, String> {
    validate_fetch_request(url, cookie_config, compat_config_path)?;
    if let Some(selection) = selection {
        selection.validate()?;
    }
    with_inspection_timeout(
        job,
        INSPECTION_TIMEOUT,
        inspect_url_inner(url, cookie_config, compat_config_path, selection, job),
    )
    .await
}

async fn inspect_url_inner(
    url: &str,
    cookie_config: Option<&CookieConfig>,
    compat_config_path: Option<&str>,
    selection: Option<&MediaSelection>,
    job: &DownloadJob,
) -> Result<UrlInspection, String> {
    let mut output = run_fetch_info_command(
        url,
        cookie_config,
        compat_config_path,
        selection,
        false,
        job,
    )
    .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if should_retry_with_twitter_syndication(url, &stderr) {
            output = run_fetch_info_command(
                url,
                cookie_config,
                compat_config_path,
                selection,
                true,
                job,
            )
            .await?;
        }
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(error_for_fetch(&stderr, output.status.code()));
    }
    metadata::parse_inspection(url, &output.stdout, selection)
}

#[cfg(test)]
mod tests;
