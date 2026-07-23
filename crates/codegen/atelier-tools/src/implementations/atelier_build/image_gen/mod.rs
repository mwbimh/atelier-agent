//! Inert compatibility configuration for the removed vendor-hosted image tools.
//!
//! The runtime no longer constructs an image client or registers image
//! generation/editing tools.  The enum remains temporarily so session/config
//! structs shared with older code can be rebuilt without carrying a network
//! implementation.  Every capability query is fail-closed.

#[derive(Debug, Clone, Default)]
pub enum ImageGenConfig {
    #[default]
    Disabled,
    Enabled {
        api_key: String,
        base_url: String,
        extra_headers: indexmap::IndexMap<String, String>,
        image_gen_enabled: bool,
        image_edit_enabled: bool,
        model_override: Option<String>,
        tier_restricted: bool,
    },
}

impl ImageGenConfig {
    pub fn has_credentials(&self) -> bool {
        false
    }

    pub fn image_gen_enabled(&self) -> bool {
        false
    }

    pub fn image_edit_enabled(&self) -> bool {
        false
    }

    pub fn model_override(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::ImageGenConfig;

    #[test]
    fn compatibility_config_is_always_fail_closed() {
        let configured = ImageGenConfig::Enabled {
            api_key: "unused".into(),
            base_url: "https://example.invalid".into(),
            extra_headers: Default::default(),
            image_gen_enabled: true,
            image_edit_enabled: true,
            model_override: Some("unused".into()),
            tier_restricted: false,
        };
        assert!(!configured.has_credentials());
        assert!(!configured.image_gen_enabled());
        assert!(!configured.image_edit_enabled());
        assert_eq!(configured.model_override(), None);
    }
}
