// Shared by build-time packaging validation and runtime authentication so an
// accepted artifact contract cannot become unusable after installation.
use minisign_verify::PublicKey;

#[cfg(test)]
pub(crate) const TEST_MINISIGN_PUBLIC_KEY: &[u8] = b"untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
#[cfg(test)]
pub(crate) const TEST_TAURI_UPDATE_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXkgRTc2MjBGMTg0MkI0RTgxRgpSV1FmNkxSQ0dBOWk1M21sWWVjTzRJelQ1MVRHUHB2V3VjTlNDaDFDQk0wUVRhTG43M1k3R0ZPMw==";

pub(crate) fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn is_canonical_update_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub(crate) fn parse_tauri_update_public_key(input: &str) -> Result<PublicKey, String> {
    let decoded_key = decode_tauri_base64_wrapper(input.as_bytes(), 8 * 1024)
        .map_err(|_| "The embedded Tauri update public key wrapper is invalid.".to_string())?;
    let public_key_text = std::str::from_utf8(&decoded_key)
        .map_err(|_| "The embedded update public key is not valid UTF-8.".to_string())?;
    PublicKey::decode(public_key_text)
        .map_err(|_| "The embedded update public key is invalid.".to_string())
}

pub(crate) fn decode_tauri_base64_wrapper(
    input: &[u8],
    decoded_limit: usize,
) -> Result<Vec<u8>, ()> {
    let input = input
        .strip_suffix(b"\r\n")
        .or_else(|| input.strip_suffix(b"\n"))
        .unwrap_or(input);
    if input.is_empty()
        || !input.len().is_multiple_of(4)
        || input.iter().any(u8::is_ascii_whitespace)
    {
        return Err(());
    }
    let padding = input.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 || input[..input.len() - padding].contains(&b'=') {
        return Err(());
    }
    let decoded_length = input
        .len()
        .checked_div(4)
        .and_then(|groups| groups.checked_mul(3))
        .and_then(|length| length.checked_sub(padding))
        .ok_or(())?;
    if decoded_length > decoded_limit {
        return Err(());
    }
    let mut output = Vec::with_capacity(decoded_length);
    for (group_index, group) in input.chunks_exact(4).enumerate() {
        let last = group_index + 1 == input.len() / 4;
        let a = decode_base64_digit(group[0]).ok_or(())? as u32;
        let b = decode_base64_digit(group[1]).ok_or(())? as u32;
        let c = if group[2] == b'=' {
            if !last || group[3] != b'=' {
                return Err(());
            }
            if b & 0x0f != 0 {
                return Err(());
            }
            0
        } else {
            decode_base64_digit(group[2]).ok_or(())? as u32
        };
        let d = if group[3] == b'=' {
            if !last {
                return Err(());
            }
            if c & 0x03 != 0 {
                return Err(());
            }
            0
        } else {
            decode_base64_digit(group[3]).ok_or(())? as u32
        };
        let value = (a << 18) | (b << 12) | (c << 6) | d;
        output.push((value >> 16) as u8);
        if group[2] != b'=' {
            output.push((value >> 8) as u8);
        }
        if group[3] != b'=' {
            output.push(value as u8);
        }
    }
    (output.len() == decoded_length).then_some(output).ok_or(())
}

fn decode_base64_digit(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
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
