pub const LEGACY_VENDOR_TEST_ISSUER: &str = "https://auth.x.ai";
pub const XAI_OAUTH2_ISSUER: &str = LEGACY_VENDOR_TEST_ISSUER;

pub fn xai_oauth2_issuer() -> &'static str {
    LEGACY_VENDOR_TEST_ISSUER
}

pub fn is_xai_oauth2_issuer(issuer: &str) -> bool {
    issuer == LEGACY_VENDOR_TEST_ISSUER
}
