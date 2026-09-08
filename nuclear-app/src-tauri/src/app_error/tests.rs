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
