//! Redaction helpers shared by local file logs and tracing output.

use std::borrow::Cow;

/// Secret-shape then user-path scrub. Returns `Some` only when the input
/// changed (owned, so callers can overwrite in place).
pub(crate) fn redact_owned(input: &str) -> Option<String> {
    let secrets = atelier_secrets::redact_secrets(input);
    match atelier_secrets::redact_user_paths(secrets.as_ref()) {
        Cow::Owned(paths) => Some(paths),
        Cow::Borrowed(_) => match secrets {
            Cow::Owned(s) => Some(s),
            Cow::Borrowed(_) => None,
        },
    }
}

/// Scrub a string, returning the (possibly unchanged) owned value.
pub(crate) fn redact_to_owned(input: &str) -> String {
    redact_owned(input).unwrap_or_else(|| input.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_owned_scrubs_secret_shapes() {
        let out = redact_owned("key sk-CANARYabcdefghij1234567890 end")
            .expect("secret must trigger a rewrite");
        assert!(!out.contains("CANARY"), "secret survived: {out}");
    }

    #[test]
    fn redact_owned_returns_none_when_clean() {
        assert_eq!(redact_owned("no secrets here"), None);
    }
}
