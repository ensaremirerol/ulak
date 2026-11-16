//! The trust root: the release public key and what a build is allowed to talk to.
//! Production builds embed `release-key.pub` and speak https only. Only a build with the cargo
//! feature `test-key` reads `ULAKD_TEST_RELEASE_KEY` / `ULAK_BUILD_VERSION` at compile time and
//! allows http; there is no runtime switch.
use crate::errors::UpdateError;
use ed25519_dalek::VerifyingKey;

const EMBEDDED_KEY: &str = include_str!("../../release-key.pub");

/// Plain http (fake servers in tests) is only possible in `test-key` builds.
pub const ALLOW_HTTP: bool = cfg!(feature = "test-key");

/// `over` wins only when `enabled`.
pub fn pick<'a>(embedded: &'a str, over: Option<&'a str>, enabled: bool) -> &'a str {
    match over {
        Some(o) if enabled => o,
        _ => embedded,
    }
}

fn key_text() -> &'static str {
    pick(
        EMBEDDED_KEY,
        option_env!("ULAKD_TEST_RELEASE_KEY"),
        cfg!(feature = "test-key"),
    )
}

/// This build's version: the crate version (`test-key` builds may override with `ULAK_BUILD_VERSION`).
pub fn build_version() -> &'static str {
    pick(
        env!("CARGO_PKG_VERSION"),
        option_env!("ULAK_BUILD_VERSION"),
        cfg!(feature = "test-key"),
    )
}

/// Exactly 64 lowercase hex chars (surrounding whitespace ignored) that form a valid ed25519 key.
pub fn parse_key(text: &str) -> Result<VerifyingKey, UpdateError> {
    let t = text.trim();
    if t.len() != 64 || !t.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(UpdateError::NoKey);
    }
    let mut raw = [0u8; 32];
    for (i, b) in raw.iter_mut().enumerate() {
        *b = u8::from_str_radix(&t[2 * i..2 * i + 2], 16).map_err(|_| UpdateError::NoKey)?;
    }
    VerifyingKey::from_bytes(&raw).map_err(|_| UpdateError::NoKey)
}

/// The key this binary trusts, or `NoKey` (fail closed).
pub fn release_key() -> Result<VerifyingKey, UpdateError> {
    parse_key(key_text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_and_malformed_keys_fail_closed() {
        for bad in [
            include_str!("../../release-key.pub"), // placeholder until gen-release-key.sh ran
            "",
            "abc",
            &"A".repeat(64),
            &"g".repeat(64),
            &"0".repeat(63),
            &"0".repeat(65),
        ] {
            if bad == include_str!("../../release-key.pub") && parse_key(bad).is_ok() {
                continue; // a real key was committed since
            }
            assert!(matches!(parse_key(bad), Err(UpdateError::NoKey)), "{bad:?}");
        }
    }

    #[test]
    fn accepts_a_valid_key_with_newline() {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let hex: String = sk
            .verifying_key()
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(parse_key(&format!("{hex}\n")).is_ok());
    }

    #[test]
    fn override_only_applies_when_enabled() {
        assert_eq!(pick("prod", Some("test"), false), "prod");
        assert_eq!(pick("prod", Some("test"), true), "test");
        assert_eq!(pick("prod", None, true), "prod");
    }
}
