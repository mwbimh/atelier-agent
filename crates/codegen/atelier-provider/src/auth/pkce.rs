use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore as _;
use sha2::{Digest as _, Sha256};

pub(super) struct Pkce {
    pub(super) verifier: String,
    pub(super) challenge: String,
}

impl Pkce {
    pub(super) fn generate() -> Self {
        let mut random = [0_u8; 32];
        rand::rng().fill_bytes(&mut random);
        let verifier = URL_SAFE_NO_PAD.encode(random);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

pub(super) fn generate_state() -> String {
    let mut random = [0_u8; 32];
    rand::rng().fill_bytes(&mut random);
    URL_SAFE_NO_PAD.encode(random)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_pkce_uses_s256_and_unpadded_base64url() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.verifier.len(), 43);
        assert!(!pkce.verifier.contains('='));
        assert!(!pkce.challenge.contains('='));
        assert_eq!(
            pkce.challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()))
        );
    }
}
