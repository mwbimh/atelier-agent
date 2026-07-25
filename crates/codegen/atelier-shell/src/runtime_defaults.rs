//! Runtime defaults that do not depend on a Provider or remote service.

/// Fallback context window used when a local model definition omits one.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 256_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_context_window_is_non_zero() {
        assert!(DEFAULT_CONTEXT_WINDOW > 0);
    }
}
