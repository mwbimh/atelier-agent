pub const PAGER_CLIENT_TYPE: &str = "atelier";
pub const HEADLESS_CLIENT_TYPE: &str = "atelier-shell";
pub const PAGER_CLIENT_VERSION: &str = atelier_version::VERSION;

pub fn client_user_agent() -> String {
    format!(
        "{}/{} ({}; {})",
        HEADLESS_CLIENT_TYPE,
        PAGER_CLIENT_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_user_agent_has_expected_shape() {
        assert_eq!(
            client_user_agent(),
            format!(
                "atelier-shell/{} ({}; {})",
                PAGER_CLIENT_VERSION,
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        );
    }
}
