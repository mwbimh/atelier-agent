//! Inert compatibility configuration for the removed vendor-hosted video tools.
//!
//! No video client or tool implementation is present.  The retained config
//! types only allow older config/session structs to deserialize while all
//! capability queries remain fail-closed.

use serde::Deserialize;

const DEFAULT_KEY_PREFIX: &str = "atelier-videos/";
const DEFAULT_EXPIRES_SECS: u64 = 900;

#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct S3AccessCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl S3AccessCredentials {
    fn is_valid(&self) -> bool {
        !self.access_key_id.trim().is_empty() && !self.secret_access_key.trim().is_empty()
    }
}

impl std::fmt::Debug for S3AccessCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3AccessCredentials")
            .field("access_key_id", &"[redacted]")
            .field("secret_access_key", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct ZdrVideoOutputS3Config {
    pub bucket: String,
    pub endpoint: String,
    pub region: String,
    #[serde(default = "default_key_prefix")]
    pub key_prefix: String,
    #[serde(default = "default_expires_secs")]
    pub expires_secs: u64,
    pub read_write: S3AccessCredentials,
    #[serde(default)]
    pub read_only: Option<S3AccessCredentials>,
}

fn default_key_prefix() -> String {
    DEFAULT_KEY_PREFIX.to_owned()
}

fn default_expires_secs() -> u64 {
    DEFAULT_EXPIRES_SECS
}

impl ZdrVideoOutputS3Config {
    pub fn is_valid(&self) -> bool {
        !self.bucket.trim().is_empty()
            && !self.endpoint.trim().is_empty()
            && !self.region.trim().is_empty()
            && self.read_write.is_valid()
    }
}

impl std::fmt::Debug for ZdrVideoOutputS3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZdrVideoOutputS3Config")
            .field("bucket", &self.bucket)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("key_prefix", &self.key_prefix)
            .field("expires_secs", &self.expires_secs)
            .field("read_write", &self.read_write)
            .field("read_only", &self.read_only.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub enum VideoGenConfig {
    #[default]
    Disabled,
    Enabled {
        api_key: String,
        base_url: String,
        extra_headers: indexmap::IndexMap<String, String>,
        zdr_video_output_s3: Option<Box<ZdrVideoOutputS3Config>>,
        tier_restricted: bool,
    },
}

impl VideoGenConfig {
    pub fn is_enabled(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::VideoGenConfig;

    #[test]
    fn compatibility_config_is_always_fail_closed() {
        let configured = VideoGenConfig::Enabled {
            api_key: "unused".into(),
            base_url: "https://example.invalid".into(),
            extra_headers: Default::default(),
            zdr_video_output_s3: None,
            tier_restricted: false,
        };
        assert!(!configured.is_enabled());
    }
}
