use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;
use ts_rs::TS;

static URL_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"(?i)https?://[^\s"']+"#).expect("valid URL regex"));
static WINDOWS_PATH_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?i)(?:[a-z]:\\|\\\\)[^\r\n\t"']+"#).expect("valid path regex")
});
static HEADER_SECRET_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?im)\b(authorization|proxy-authorization|cookie|set-cookie)\s*[:=]\s*[^\r\n]+",
    )
    .expect("valid header secret regex")
});
static TOKEN_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(token|api[_-]?key)\s*[:=]\s*[^\s,;]+").expect("valid token regex")
});

const MAX_ERROR_SUMMARY_BYTES: usize = 4 * 1024;
const MAX_ERROR_DETAIL_BYTES: usize = 64 * 1024;

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    const SUFFIX: &str = "...[truncated]";
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.saturating_sub(SUFFIX.len()).min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{SUFFIX}", &value[..end])
}

fn safe_text(value: impl Into<String>, max_bytes: usize) -> String {
    let value = value.into();
    let value = URL_RE.replace_all(&value, "[url]");
    let value = WINDOWS_PATH_RE.replace_all(&value, "[path]");
    let value = HEADER_SECRET_RE.replace_all(&value, "$1=[redacted]");
    let value = TOKEN_RE.replace_all(&value, "$1=[redacted]");
    truncate_utf8(&value, max_bytes)
}

/// Stable, renderer-safe failure contract for every fallible IPC command.
///
/// `detail` is optional and must already be redacted by the producer. Raw URLs,
/// cookie material, and full local paths must never be placed in this value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/bindings/")]
pub struct AppError {
    pub code: String,
    pub summary: String,
    pub detail: Option<String>,
    pub retryable: bool,
    pub correlation_id: String,
}

impl AppError {
    pub fn new(code: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            summary: safe_text(summary, MAX_ERROR_SUMMARY_BYTES),
            detail: None,
            retryable: false,
            correlation_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(safe_text(detail, MAX_ERROR_DETAIL_BYTES));
        self
    }

    pub fn invalid(summary: impl Into<String>) -> Self {
        Self::new("invalid_request", summary)
    }

    pub fn not_found(kind: &str) -> Self {
        Self::new("not_found", format!("The requested {kind} was not found."))
    }

    pub fn busy(summary: impl Into<String>) -> Self {
        Self::new("busy", summary).retryable(true)
    }

    pub fn internal(summary: impl Into<String>) -> Self {
        Self::new("internal_error", summary).retryable(true)
    }
}

impl Display for AppError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.summary)
    }
}

impl std::error::Error for AppError {}

impl From<String> for AppError {
    fn from(summary: String) -> Self {
        Self::internal(summary)
    }
}

impl From<&str> for AppError {
    fn from(summary: &str) -> Self {
        Self::internal(summary)
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        Self::internal("A local I/O operation failed.").with_detail(error.kind().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{AppError, MAX_ERROR_DETAIL_BYTES, MAX_ERROR_SUMMARY_BYTES};

    #[test]
    fn serialized_contract_uses_stable_camel_case_fields() {
        let error = AppError::new("not_found", "Missing")
            .retryable(true)
            .with_detail("safe detail");
        let value = serde_json::to_value(error).unwrap();

        assert_eq!(value["code"], "not_found");
        assert_eq!(value["retryable"], true);
        assert!(value["correlationId"].as_str().is_some());
        assert!(value.get("correlation_id").is_none());

        let without_detail = serde_json::to_value(AppError::new("failed", "Failed")).unwrap();
        assert!(without_detail
            .get("detail")
            .is_some_and(serde_json::Value::is_null));
    }

    #[test]
    fn boundary_redacts_urls_paths_and_secret_assignments() {
        let error = AppError::new(
            "failed",
            "https://example.com/private C:\\Users\\Alice\\file token=secret\nAuthorization: Bearer bearer-secret",
        )
        .with_detail("cookie=private-value at \\\\server\\share\\item");
        let rendered = serde_json::to_string(&error).unwrap();
        assert!(!rendered.contains("example.com"));
        assert!(!rendered.contains("Alice"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("server"));
        assert!(!rendered.contains("private-value"));
        assert!(!rendered.contains("bearer-secret"));
    }

    #[test]
    fn boundary_unicode_safely_caps_summary_and_detail() {
        let error = AppError::new("failed", "🙂".repeat(MAX_ERROR_SUMMARY_BYTES))
            .with_detail("🙂".repeat(MAX_ERROR_DETAIL_BYTES));

        assert!(error.summary.len() <= MAX_ERROR_SUMMARY_BYTES);
        assert!(error.detail.as_ref().unwrap().len() <= MAX_ERROR_DETAIL_BYTES);
        assert!(error.summary.ends_with("...[truncated]"));
        assert!(error.detail.unwrap().ends_with("...[truncated]"));
    }
}
