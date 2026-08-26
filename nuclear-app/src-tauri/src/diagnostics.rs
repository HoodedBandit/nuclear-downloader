use crate::app_error::AppError;
use crate::journal::now_ms;
use regex::Regex;
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime};

const MAX_LOG_FILES: usize = 5;
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 64 * 1024;
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const LOG_PREFIX: &str = "diagnostics";

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)https?://[^\s"']+"#).expect("valid URL regex"));
static WINDOWS_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)(?:[a-z]:\\|\\\\)[^\r\n\t"']+"#).expect("valid path regex"));
static HEADER_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)\b(authorization|proxy-authorization|cookie|set-cookie)\s*[:=]\s*[^\r\n]+")
        .expect("valid header secret regex")
});
static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(token|api[_-]?key)\s*[:=]\s*[^\s,;]+").expect("valid token regex")
});

#[derive(Clone)]
pub struct Diagnostics {
    inner: Arc<DiagnosticsInner>,
}

struct DiagnosticsInner {
    directory: PathBuf,
    lock: Mutex<()>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticRecord<'a> {
    schema_version: u32,
    timestamp_ms: u64,
    level: &'a str,
    event: &'a str,
    correlation_id: &'a str,
    message: String,
}

impl Diagnostics {
    pub fn open_default() -> Result<Self, AppError> {
        let directory = dirs::data_local_dir()
            .ok_or_else(|| AppError::internal("Could not locate per-user application data."))?
            .join("Nuclear Downloader")
            .join("diagnostics");
        Self::open(directory)
    }

    pub fn open(directory: PathBuf) -> Result<Self, AppError> {
        fs::create_dir_all(&directory).map_err(|error| {
            AppError::internal("Could not create the diagnostics folder.")
                .with_detail(error.kind().to_string())
        })?;
        let diagnostics = Self {
            inner: Arc::new(DiagnosticsInner {
                directory,
                lock: Mutex::new(()),
            }),
        };
        diagnostics.cleanup_expired();
        Ok(diagnostics)
    }

    pub fn log(&self, level: &str, event: &str, correlation_id: &str, message: &str) {
        let Ok(_guard) = self.inner.lock.lock() else {
            return;
        };
        let record = DiagnosticRecord {
            schema_version: 1,
            timestamp_ms: now_ms(),
            level,
            event,
            correlation_id,
            message: redact_and_bound(message),
        };
        let Ok(mut line) = serde_json::to_vec(&record) else {
            return;
        };
        line.push(b'\n');
        let _ = self.rotate_if_needed(line.len());
        if let Ok(mut output) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path(0))
        {
            let _ = output.write_all(&line);
        }
    }

    pub fn export_to(&self, destination: &Path) -> Result<(), AppError> {
        let _guard = self
            .inner
            .lock
            .lock()
            .map_err(|_| AppError::internal("The diagnostics writer is unavailable."))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|error| {
                AppError::new(
                    "diagnostics_export_failed",
                    "Could not create the diagnostics export file.",
                )
                .with_detail(error.kind().to_string())
            })?;

        for index in (0..MAX_LOG_FILES).rev() {
            let source = self.log_path(index);
            if !source.is_file() {
                continue;
            }
            let mut input = fs::File::open(&source).map_err(|error| {
                AppError::internal("Could not read a diagnostics log.")
                    .with_detail(error.kind().to_string())
            })?;
            std::io::copy(&mut input, &mut output).map_err(|error| {
                AppError::internal("Could not write the diagnostics export.")
                    .with_detail(error.kind().to_string())
            })?;
        }
        output.sync_all().map_err(|error| {
            AppError::internal("Could not flush the diagnostics export.")
                .with_detail(error.kind().to_string())
        })
    }

    pub fn clear(&self) -> Result<(), AppError> {
        let _guard = self
            .inner
            .lock
            .lock()
            .map_err(|_| AppError::internal("The diagnostics writer is unavailable."))?;
        for index in 0..MAX_LOG_FILES {
            let path = self.log_path(index);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(AppError::new(
                        "diagnostics_clear_failed",
                        "Could not clear every diagnostics log.",
                    )
                    .with_detail(error.kind().to_string()))
                }
            }
        }
        Ok(())
    }

    fn cleanup_expired(&self) {
        let Ok(_guard) = self.inner.lock.lock() else {
            return;
        };
        let now = SystemTime::now();
        for index in 0..MAX_LOG_FILES {
            let path = self.log_path(index);
            let expired = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > RETENTION);
            if expired {
                let _ = fs::remove_file(path);
            }
        }
    }

    fn rotate_if_needed(&self, incoming_bytes: usize) -> std::io::Result<()> {
        if fs::metadata(self.log_path(0))
            .map(|metadata| metadata.len().saturating_add(incoming_bytes as u64) <= MAX_LOG_BYTES)
            .unwrap_or(true)
        {
            return Ok(());
        }

        let _ = fs::remove_file(self.log_path(MAX_LOG_FILES - 1));
        for index in (0..(MAX_LOG_FILES - 1)).rev() {
            let source = self.log_path(index);
            if source.exists() {
                fs::rename(source, self.log_path(index + 1))?;
            }
        }
        Ok(())
    }

    fn log_path(&self, index: usize) -> PathBuf {
        if index == 0 {
            self.inner.directory.join(format!("{LOG_PREFIX}.jsonl"))
        } else {
            self.inner
                .directory
                .join(format!("{LOG_PREFIX}.{index}.jsonl"))
        }
    }
}

pub fn redact(message: &str) -> String {
    let no_urls = URL_RE.replace_all(message, "[url]");
    let no_paths = WINDOWS_PATH_RE.replace_all(&no_urls, "[path]");
    let no_headers = HEADER_SECRET_RE.replace_all(&no_paths, "$1=[redacted]");
    TOKEN_RE
        .replace_all(&no_headers, "$1=[redacted]")
        .into_owned()
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    const SUFFIX: &str = "...[truncated]";
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let available = max_bytes.saturating_sub(SUFFIX.len());
    let mut end = available.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{SUFFIX}", &value[..end])
}

fn redact_and_bound(message: &str) -> String {
    truncate_utf8(&redact(message), MAX_DIAGNOSTIC_MESSAGE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::{
        redact, redact_and_bound, Diagnostics, MAX_DIAGNOSTIC_MESSAGE_BYTES, MAX_LOG_BYTES,
    };
    use std::io::Write;

    #[test]
    fn redaction_removes_urls_paths_and_secret_values() {
        let value = redact("open https://example.com/watch?v=secret at C:\\Users\\Alice\\cookies.txt\nAuthorization: Bearer secret-token\nCookie: session=abc; preference=dark");
        assert!(!value.contains("example.com"));
        assert!(!value.contains("Alice"));
        assert!(!value.contains("abc"));
        assert!(!value.contains("secret-token"));
        assert!(!value.contains("preference"));
        assert!(value.contains("[url]"));
        assert!(value.contains("[path]"));
    }

    #[test]
    fn rotation_keeps_a_bounded_number_of_files() {
        let root =
            std::env::temp_dir().join(format!("nuclear-diagnostics-{}", uuid::Uuid::new_v4()));
        let diagnostics = Diagnostics::open(root.clone()).unwrap();
        let mut file = std::fs::File::create(root.join("diagnostics.jsonl")).unwrap();
        file.write_all(&vec![b'x'; MAX_LOG_BYTES as usize]).unwrap();
        diagnostics.log("info", "test", "correlation", "message");
        assert!(root.join("diagnostics.1.jsonl").is_file());
        assert!(root.join("diagnostics.jsonl").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn diagnostic_messages_are_unicode_safely_bounded() {
        let message = format!("{} secret-token", "🙂".repeat(MAX_DIAGNOSTIC_MESSAGE_BYTES));
        let bounded = redact_and_bound(&message);

        assert!(bounded.len() <= MAX_DIAGNOSTIC_MESSAGE_BYTES);
        assert!(bounded.ends_with("...[truncated]"));
        assert!(std::str::from_utf8(bounded.as_bytes()).is_ok());
    }
}
