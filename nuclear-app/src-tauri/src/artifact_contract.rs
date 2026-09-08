// Shared by build-time packaging validation and runtime authentication so an
// accepted artifact contract cannot become unusable after installation.
pub(crate) fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::is_canonical_sha256;

    #[test]
    fn sidecar_digest_policy_rejects_noncanonical_case() {
        assert!(is_canonical_sha256(&"a".repeat(64)));
        assert!(!is_canonical_sha256(&"A".repeat(64)));
    }
}
