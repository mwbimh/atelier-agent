//! Local-only crash lifecycle compatibility API.
//!
//! Atelier never configures a remote crash reporter. The guard remains so the
//! binary shutdown flow does not need a vendor-specific branch.

#[derive(Debug, Clone, Copy, Default)]
pub struct Config {
    pub client: &'static str,
    pub client_version: &'static str,
    pub release: &'static str,
    pub disabled: bool,
}
#[derive(Debug, Default)]
pub struct ClientInitGuard;

pub fn init(config: Config) -> ClientInitGuard {
    let _ = config;
    ClientInitGuard
}

pub fn flush_on_shutdown() {}
