use super::command_args::build_download_args_with_runtime;
use super::errors::{
    classify_process_error, should_retry_with_twitter_syndication, simple_error, DownloadErrorInfo,
};
use super::naming::{
    build_final_output_path, build_staged_webm_output_path, build_webm_final_path, path_to_string,
};
use super::process::{
    wait_with_bounded_output, wait_with_streamed_stdout, DownloadJob, MAX_PROCESS_LINE_BYTES,
    MAX_STDERR_BYTES,
};
use super::progress::{
    parse_ffmpeg_progress_percent, DOWNLOAD_ETA_RE, DOWNLOAD_MERGE_RE, DOWNLOAD_PROGRESS_RE,
    DOWNLOAD_SPEED_RE,
};
use super::publication::{
    build_staging_dir, cleanup_staging_with_warning, publish_staged_output, reset_staging_dir,
    resolve_staged_output,
};
use super::validation::{validate_download_request, validate_output_directory};
use crate::lifecycle::DownloadManager;
use crate::models::{DownloadProgress, DownloadRequest};
use crate::notifications::DownloadNotifications;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

#[derive(Default)]
struct ProgressFields {
    speed: Option<String>,
    eta: Option<String>,
    error: Option<String>,
    error_code: Option<String>,
    error_detail: Option<String>,
    filename: Option<String>,
    phase: Option<&'static str>,
    download_progress: Option<f64>,
    conversion_progress: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DownloadOutcome {
    Completed {
        filename: Option<String>,
    },
    Cancelled,
    Failed {
        code: String,
        message: String,
        detail: String,
    },
}

impl From<DownloadErrorInfo> for DownloadOutcome {
    fn from(error: DownloadErrorInfo) -> Self {
        Self::Failed {
            code: error.code,
            message: error.message,
            detail: error.detail,
        }
    }
}

async fn emit_progress(
    notifications: &DownloadNotifications,
    download_id: &str,
    status: &str,
    progress: f64,
    fields: ProgressFields,
) {
    let event = DownloadProgress {
        download_id: download_id.to_string(),
        status: status.to_string(),
        progress,
        phase: fields.phase.map(str::to_string),
        download_progress: fields.download_progress,
        conversion_progress: fields.conversion_progress,
        speed: fields.speed,
        eta: fields.eta,
        error: fields.error,
        error_code: fields.error_code,
        error_detail: fields.error_detail,
        filename: fields.filename,
    };
    (notifications.progress)(event).await;
}

async fn probe_media_duration_seconds(path: &Path, job: &DownloadJob) -> Result<f64, String> {
    if job.is_cancelled() {
        return Err("Conversion was cancelled.".into());
    }

    let ffprobe = job.required_runtime_tool("ffprobe")?;
    let mut cmd = Command::new(ffprobe);
    cmd.kill_on_drop(true);
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
    ]);
    cmd.arg(path);

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = job.spawn(&mut cmd, "ffprobe", false).await?;

    let output = wait_with_bounded_output(
        child,
        job,
        MAX_PROCESS_LINE_BYTES,
        MAX_STDERR_BYTES,
        Duration::from_secs(30),
    )
    .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let duration = stdout
        .trim()
        .parse::<f64>()
        .map_err(|_| "ffprobe did not return a valid duration.".to_string())?;

    if duration.is_finite() && duration > 0.0 {
        Ok(duration)
    } else {
        Err("ffprobe could not determine media duration.".into())
    }
}

async fn publish_converted_output(
    staged_output: &Path,
    final_path: &Path,
    job: &DownloadJob,
) -> Result<PathBuf, String> {
    publish_staged_output(staged_output, final_path, Some(job)).await
}

enum DownloadAttemptResult {
    Completed(Option<String>),
    Cancelled,
    RetryWithTwitterSyndication,
    Error(DownloadErrorInfo),
}

fn terminal_outcome_from_attempt(result: DownloadAttemptResult) -> DownloadOutcome {
    match result {
        DownloadAttemptResult::Completed(filename) => DownloadOutcome::Completed { filename },
        DownloadAttemptResult::Cancelled => DownloadOutcome::Cancelled,
        DownloadAttemptResult::Error(error) => error.into(),
        DownloadAttemptResult::RetryWithTwitterSyndication => simple_error(
            "download_failed",
            "Download failed before retry could complete.",
        )
        .into(),
    }
}

async fn run_download_attempt(
    notifications: &DownloadNotifications,
    download_id: &str,
    request: &DownloadRequest,
    job: &DownloadJob,
    use_twitter_syndication: bool,
) -> DownloadAttemptResult {
    if job.is_cancelled() {
        return DownloadAttemptResult::Cancelled;
    }

    let runtime_config = match job.ytdlp_runtime_config() {
        Ok(config) => config,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("runtime_missing", error));
        }
    };
    let args = build_download_args_with_runtime(request, use_twitter_syndication, &runtime_config);

    let bin = match job.required_runtime_tool("yt-dlp") {
        Ok(path) => path,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("runtime_missing", error));
        }
    };
    let mut cmd = Command::new(&bin);
    cmd.kill_on_drop(true);
    cmd.args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = match job.spawn(&mut cmd, "yt-dlp", true).await {
        Ok(child) => child,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error(
                "runtime_missing",
                format!("Failed to start yt-dlp: {}", error),
            ));
        }
    };

    let output = match wait_with_streamed_stdout(child, job, "yt-dlp", |line| {
        let update = if let Some(caps) = DOWNLOAD_PROGRESS_RE.captures(&line) {
            let pct: f64 = caps[1].parse().unwrap_or(0.0);
            let speed = DOWNLOAD_SPEED_RE.captures(&line).map(|c| c[1].to_string());
            let eta = DOWNLOAD_ETA_RE.captures(&line).map(|c| c[1].to_string());
            Some((
                "downloading",
                pct,
                ProgressFields {
                    speed,
                    eta,
                    filename: None,
                    phase: Some("download"),
                    download_progress: Some(pct),
                    ..Default::default()
                },
            ))
        } else if DOWNLOAD_MERGE_RE.is_match(&line) {
            Some((
                "postprocessing",
                100.0,
                ProgressFields {
                    filename: None,
                    phase: Some("postprocess"),
                    download_progress: Some(100.0),
                    ..Default::default()
                },
            ))
        } else {
            None
        };

        async move {
            if let Some((status, progress, fields)) = update {
                emit_progress(notifications, download_id, status, progress, fields).await;
            }
            Ok(())
        }
    })
    .await
    {
        Ok(output) => output,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("process_output_failed", error));
        }
    };

    if output.cancelled || job.is_cancelled() {
        return DownloadAttemptResult::Cancelled;
    }

    match output.status {
        s if s.success() => DownloadAttemptResult::Completed(None),
        s => {
            if !use_twitter_syndication
                && should_retry_with_twitter_syndication(&request.url, &output.stderr)
            {
                DownloadAttemptResult::RetryWithTwitterSyndication
            } else {
                DownloadAttemptResult::Error(classify_process_error(
                    &output.stderr,
                    s.code(),
                    Some(&request.format),
                ))
            }
        }
    }
}

async fn run_webm_conversion(
    notifications: &DownloadNotifications,
    download_id: &str,
    input_path: &Path,
    staged_output: &Path,
    final_path: &Path,
    job: &DownloadJob,
) -> DownloadAttemptResult {
    let duration_seconds = match probe_media_duration_seconds(input_path, job).await {
        Ok(duration) => duration,
        Err(_) if job.is_cancelled() => return DownloadAttemptResult::Cancelled,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("postprocess_failed", error))
        }
    };

    emit_progress(
        notifications,
        download_id,
        "postprocessing",
        0.0,
        ProgressFields {
            filename: None,
            phase: Some("conversion"),
            download_progress: Some(100.0),
            conversion_progress: Some(0.0),
            ..Default::default()
        },
    )
    .await;

    let ffmpeg = match job.required_runtime_tool("ffmpeg") {
        Ok(path) => path,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("runtime_missing", error));
        }
    };
    let mut cmd = Command::new(ffmpeg);
    cmd.kill_on_drop(true);
    cmd.args([
        "-y",
        "-hide_banner",
        "-nostats",
        "-stats_period",
        "0.5",
        "-i",
    ]);
    cmd.arg(input_path);
    cmd.args([
        "-map",
        "0:v:0",
        "-map",
        "0:a?",
        "-c:v",
        "libvpx-vp9",
        "-row-mt",
        "1",
        "-cpu-used",
        "4",
        "-crf",
        "32",
        "-b:v",
        "0",
        "-c:a",
        "libopus",
        "-b:a",
        "128k",
        "-f",
        "webm",
        "-progress",
        "pipe:1",
    ]);
    cmd.arg(staged_output);
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = match job.spawn(&mut cmd, "ffmpeg", true).await {
        Ok(child) => child,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error(
                "runtime_missing",
                format!("Failed to start ffmpeg: {error}"),
            ));
        }
    };

    let mut last_progress: f64 = 0.0;
    let output = match wait_with_streamed_stdout(child, job, "FFmpeg", |line| {
        let progress = if line.trim() == "progress=end" {
            Some(100.0)
        } else {
            parse_ffmpeg_progress_percent(&line, duration_seconds)
        };
        let update = progress.map(|progress| {
            last_progress = last_progress.max(progress);
            last_progress
        });

        async move {
            if let Some(progress) = update {
                emit_progress(
                    notifications,
                    download_id,
                    "postprocessing",
                    progress,
                    ProgressFields {
                        filename: None,
                        phase: Some("conversion"),
                        download_progress: Some(100.0),
                        conversion_progress: Some(progress),
                        ..Default::default()
                    },
                )
                .await;
            }
            Ok(())
        }
    })
    .await
    {
        Ok(output) => output,
        Err(error) => {
            return DownloadAttemptResult::Error(simple_error("process_output_failed", error));
        }
    };

    if output.cancelled || job.is_cancelled() {
        return DownloadAttemptResult::Cancelled;
    }

    match output.status {
        status if status.success() => {
            match publish_converted_output(staged_output, final_path, job).await {
                Ok(published_path) => {
                    DownloadAttemptResult::Completed(Some(path_to_string(&published_path)))
                }
                Err(_) if job.is_cancelled() => DownloadAttemptResult::Cancelled,
                Err(error) => {
                    DownloadAttemptResult::Error(simple_error("postprocess_failed", error))
                }
            }
        }
        status => DownloadAttemptResult::Error(classify_process_error(
            &output.stderr,
            status.code(),
            Some("webm"),
        )),
    }
}

async fn run_webm_download(
    notifications: &DownloadNotifications,
    download_id: &str,
    request: &DownloadRequest,
    manager: &DownloadManager,
    job: &DownloadJob,
) -> DownloadAttemptResult {
    let output_dir = Path::new(&request.output_dir);
    let staging_dir = build_staging_dir(output_dir, download_id);
    let mut use_twitter_syndication = false;

    loop {
        if let Err(error) = reset_staging_dir(&staging_dir, output_dir, download_id) {
            return DownloadAttemptResult::Error(simple_error("staging_failed", error));
        }

        let mut staged_request = request.clone();
        staged_request.output_dir = path_to_string(&staging_dir);

        match run_download_attempt(
            notifications,
            download_id,
            &staged_request,
            job,
            use_twitter_syndication,
        )
        .await
        {
            DownloadAttemptResult::Completed(_) => {
                let intermediate_path = match resolve_staged_output(&staging_dir) {
                    Ok(path) => path,
                    Err(error) => {
                        cleanup_staging_with_warning(
                            notifications,
                            &staging_dir,
                            output_dir,
                            download_id,
                        );
                        return DownloadAttemptResult::Error(simple_error(
                            error.code,
                            error.message,
                        ));
                    }
                };

                let final_path = build_webm_final_path(request, &intermediate_path);
                let staged_output = build_staged_webm_output_path(&staging_dir, &final_path);
                emit_progress(
                    notifications,
                    download_id,
                    "postprocessing",
                    0.0,
                    ProgressFields {
                        filename: None,
                        phase: Some("waiting_conversion"),
                        download_progress: Some(100.0),
                        conversion_progress: Some(0.0),
                        ..Default::default()
                    },
                )
                .await;
                let _conversion_permit = match manager.acquire_conversion(job).await {
                    Ok(Some(permit)) => permit,
                    Ok(None) => {
                        cleanup_staging_with_warning(
                            notifications,
                            &staging_dir,
                            output_dir,
                            download_id,
                        );
                        return DownloadAttemptResult::Cancelled;
                    }
                    Err(error) => {
                        cleanup_staging_with_warning(
                            notifications,
                            &staging_dir,
                            output_dir,
                            download_id,
                        );
                        return DownloadAttemptResult::Error(simple_error(
                            "conversion_scheduler_failed",
                            error,
                        ));
                    }
                };
                let result = run_webm_conversion(
                    notifications,
                    download_id,
                    &intermediate_path,
                    &staged_output,
                    &final_path,
                    job,
                )
                .await;

                cleanup_staging_with_warning(notifications, &staging_dir, output_dir, download_id);
                return result;
            }
            DownloadAttemptResult::RetryWithTwitterSyndication => {
                use_twitter_syndication = true;
            }
            other => {
                cleanup_staging_with_warning(notifications, &staging_dir, output_dir, download_id);
                return other;
            }
        }
    }
}

pub async fn start_download(
    notifications: DownloadNotifications,
    download_id: String,
    mut request: DownloadRequest,
    manager: DownloadManager,
    job: DownloadJob,
) -> DownloadOutcome {
    if job.is_cancelled() {
        return DownloadOutcome::Cancelled;
    }

    if let Err(error) = validate_download_request(&request) {
        return simple_error("invalid_request", error).into();
    }

    match validate_output_directory(&request.output_dir) {
        Ok(output_dir) => request.output_dir = output_dir,
        Err(error) => {
            return DownloadOutcome::Failed {
                code: error.code,
                message: error.summary,
                detail: error.detail.unwrap_or_default(),
            };
        }
    }

    emit_progress(
        &notifications,
        &download_id,
        "downloading",
        0.0,
        ProgressFields {
            phase: Some("download"),
            download_progress: Some(0.0),
            ..Default::default()
        },
    )
    .await;

    if request.format == "webm" {
        return terminal_outcome_from_attempt(
            run_webm_download(&notifications, &download_id, &request, &manager, &job).await,
        );
    }

    let output_dir = Path::new(&request.output_dir);
    let staging_dir = build_staging_dir(output_dir, &download_id);
    let mut use_twitter_syndication = false;

    loop {
        if let Err(error) = reset_staging_dir(&staging_dir, output_dir, &download_id) {
            return simple_error("staging_failed", error).into();
        }

        let mut staged_request = request.clone();
        staged_request.output_dir = path_to_string(&staging_dir);

        match run_download_attempt(
            &notifications,
            &download_id,
            &staged_request,
            &job,
            use_twitter_syndication,
        )
        .await
        {
            DownloadAttemptResult::Completed(_) => {
                let staged_path = match resolve_staged_output(&staging_dir) {
                    Ok(path) => path,
                    Err(error) => {
                        cleanup_staging_with_warning(
                            &notifications,
                            &staging_dir,
                            output_dir,
                            &download_id,
                        );
                        return simple_error(error.code, error.message).into();
                    }
                };

                let desired_path = match build_final_output_path(&request, &staged_path) {
                    Ok(path) => path,
                    Err(error) => {
                        cleanup_staging_with_warning(
                            &notifications,
                            &staging_dir,
                            output_dir,
                            &download_id,
                        );
                        return simple_error("invalid_filename", error).into();
                    }
                };

                emit_progress(
                    &notifications,
                    &download_id,
                    "postprocessing",
                    100.0,
                    ProgressFields {
                        filename: None,
                        phase: Some("postprocess"),
                        download_progress: Some(100.0),
                        ..Default::default()
                    },
                )
                .await;

                let outcome =
                    match publish_staged_output(&staged_path, &desired_path, Some(&job)).await {
                        Ok(published_path) => DownloadOutcome::Completed {
                            filename: Some(path_to_string(&published_path)),
                        },
                        Err(_) if job.is_cancelled() => DownloadOutcome::Cancelled,
                        Err(error) => simple_error("publish_failed", error).into(),
                    };
                cleanup_staging_with_warning(
                    &notifications,
                    &staging_dir,
                    output_dir,
                    &download_id,
                );
                return outcome;
            }
            DownloadAttemptResult::Cancelled => {
                cleanup_staging_with_warning(
                    &notifications,
                    &staging_dir,
                    output_dir,
                    &download_id,
                );
                return DownloadOutcome::Cancelled;
            }
            DownloadAttemptResult::RetryWithTwitterSyndication => {
                cleanup_staging_with_warning(
                    &notifications,
                    &staging_dir,
                    output_dir,
                    &download_id,
                );
                use_twitter_syndication = true;
            }
            DownloadAttemptResult::Error(error) => {
                cleanup_staging_with_warning(
                    &notifications,
                    &staging_dir,
                    output_dir,
                    &download_id,
                );
                return error.into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DownloadAttemptResult, DownloadOutcome};
    use crate::downloader::errors::DownloadErrorInfo;

    #[test]
    fn terminal_outcome_preserves_published_filename() {
        let filename = r"C:\Users\Mr.W\Downloads\clip (2).mp4".to_string();

        let outcome = super::terminal_outcome_from_attempt(DownloadAttemptResult::Completed(Some(
            filename.clone(),
        )));

        assert_eq!(
            outcome,
            DownloadOutcome::Completed {
                filename: Some(filename)
            }
        );
    }

    #[test]
    fn terminal_outcome_preserves_structured_failure() {
        let error = DownloadErrorInfo {
            code: "publish_failed".to_string(),
            message: "Could not publish the download.".to_string(),
            detail: "The destination volume rejected the atomic move.".to_string(),
        };

        let outcome = super::terminal_outcome_from_attempt(DownloadAttemptResult::Error(error));

        assert_eq!(
            outcome,
            DownloadOutcome::Failed {
                code: "publish_failed".to_string(),
                message: "Could not publish the download.".to_string(),
                detail: "The destination volume rejected the atomic move.".to_string(),
            }
        );
    }
}
