use super::{redact, redact_and_bound, Diagnostics, MAX_DIAGNOSTIC_MESSAGE_BYTES, MAX_LOG_BYTES};
use std::io::Write;

impl Diagnostics {
    fn fail_next_export_after_copy_for_test(&self) {
        self.inner
            .fail_next_export_after_copy
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

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
    let root = std::env::temp_dir().join(format!("nuclear-diagnostics-{}", uuid::Uuid::new_v4()));
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

#[test]
fn failed_export_removes_partial_destination_and_allows_retry() {
    let root = std::env::temp_dir().join(format!("nuclear-diagnostics-{}", uuid::Uuid::new_v4()));
    let diagnostics = Diagnostics::open(root.clone()).unwrap();
    std::fs::write(root.join("diagnostics.jsonl"), b"{\"message\":\"test\"}\n").unwrap();
    let destination = root.join("export.jsonl");

    diagnostics.fail_next_export_after_copy_for_test();
    assert!(diagnostics.export_to(&destination).is_err());
    let partial_exists = destination.exists();
    let retry = diagnostics.export_to(&destination);
    let _ = std::fs::remove_dir_all(&root);

    assert!(!partial_exists, "failed export left a partial destination");
    assert!(retry.is_ok(), "same-path retry remained blocked");
}

#[test]
fn export_preserves_a_preexisting_destination() {
    let root = std::env::temp_dir().join(format!("nuclear-diagnostics-{}", uuid::Uuid::new_v4()));
    let diagnostics = Diagnostics::open(root.clone()).unwrap();
    let destination = root.join("export.jsonl");
    std::fs::write(&destination, b"existing").unwrap();

    let error = diagnostics.export_to(&destination).unwrap_err();
    let retained = std::fs::read(&destination).unwrap();
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(error.code, "diagnostics_export_failed");
    assert_eq!(retained, b"existing");
}
