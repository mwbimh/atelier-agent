//! Backend selection, runtime status, and fail-closed diagnostics.
//!
//! This module deliberately stops at the preview boundary. It reports whether
//! the platform can provide a native sandbox and refuses to authorize a native
//! launch when it cannot. It does not implement the Codex Windows token,
//! helper, ACL, or firewall machinery.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
#[cfg(target_os = "windows")]
use std::sync::OnceLock;

/// The sandbox backend selected for a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxBackendKind {
    /// Use the platform's native sandbox implementation.
    #[serde(alias = "windows-codex", alias = "codex")]
    Native,
    /// Deliberately allow an unsandboxed process. This is never selected implicitly.
    Unsafe,
}

impl Default for SandboxBackendKind {
    fn default() -> Self {
        Self::Native
    }
}

impl SandboxBackendKind {
    /// The stable serialized/configuration label for this backend.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Unsafe => "unsafe",
        }
    }

    /// Whether this selection explicitly permits unsandboxed execution.
    pub const fn is_unsafe(self) -> bool {
        matches!(self, Self::Unsafe)
    }
}

impl fmt::Display for SandboxBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned for an unknown backend configuration value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxBackendParseError {
    input: String,
}

impl fmt::Display for SandboxBackendParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown sandbox backend {:?}; expected `native` or explicit `unsafe`",
            self.input
        )
    }
}

impl std::error::Error for SandboxBackendParseError {}

impl FromStr for SandboxBackendKind {
    type Err = SandboxBackendParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "native" | "windows-codex" | "codex" => Ok(Self::Native),
            "unsafe" => Ok(Self::Unsafe),
            _ => Err(SandboxBackendParseError {
                input: value.to_string(),
            }),
        }
    }
}

/// Parse an optional backend configuration, defaulting to the native backend.
pub fn parse_backend(value: Option<&str>) -> Result<SandboxBackendKind, SandboxBackendParseError> {
    value.map_or(Ok(SandboxBackendKind::Native), str::parse)
}

/// Runtime state exposed to shell/RPC diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxStatus {
    /// The native sandbox was applied successfully.
    Active,
    /// The native backend is selected but this crate is still at the preview boundary.
    Preview,
    /// The requested native backend cannot be used, so execution must be refused.
    Unavailable,
    /// Unsandboxed execution was explicitly selected by the caller.
    Unsafe,
    /// The sandbox profile was explicitly disabled.
    Disabled,
}

impl SandboxStatus {
    /// Stable label for RPC/UI consumers.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Preview => "preview",
            Self::Unavailable => "unavailable",
            Self::Unsafe => "unsafe",
            Self::Disabled => "disabled",
        }
    }
}

/// Result of probing the platform's native sandbox capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeSandboxAvailability {
    /// A native implementation is available for the current build/platform.
    Available,
    /// Native sandboxing is unavailable, with a user-facing diagnostic reason.
    Unavailable { reason: String },
}

impl NativeSandboxAvailability {
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }
}

/// Error returned when native execution cannot be made safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// Native sandbox setup/support is missing; callers must not run bare.
    NativeUnavailable { reason: String },
    /// Native sandbox setup started but did not complete.
    NativeApplyFailed { reason: String },
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NativeUnavailable { reason } => {
                write!(
                    f,
                    "native sandbox unavailable; refusing unsandboxed execution: {reason}"
                )
            }
            Self::NativeApplyFailed { reason } => {
                write!(
                    f,
                    "native sandbox failed; refusing unsandboxed execution: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for SandboxError {}

/// Stable diagnostics returned to shell/RPC callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxDiagnostics {
    pub backend: SandboxBackendKind,
    pub profile: String,
    pub status: SandboxStatus,
    pub enforced: bool,
    pub reason: String,
    /// True while the crate is reporting the first-stage preview boundary.
    pub preview: bool,
    /// Whether the native backend probe succeeded.
    pub native_available: bool,
}

impl SandboxDiagnostics {
    pub fn new(
        backend: SandboxBackendKind,
        profile: impl Into<String>,
        native_available: bool,
        enforced: bool,
        reason: impl Into<String>,
    ) -> Self {
        let status = resolve_sandbox_status(backend, native_available, enforced);
        Self {
            backend,
            profile: profile.into(),
            status,
            enforced,
            reason: reason.into(),
            preview: !matches!(status, SandboxStatus::Active | SandboxStatus::Disabled),
            native_available,
        }
    }

    pub fn disabled(backend: SandboxBackendKind, profile: impl Into<String>) -> Self {
        Self {
            backend,
            profile: profile.into(),
            status: SandboxStatus::Disabled,
            enforced: false,
            reason: "sandbox profile is disabled".to_string(),
            preview: false,
            native_available: false,
        }
    }

    pub fn unavailable(
        backend: SandboxBackendKind,
        profile: impl Into<String>,
        native_available: bool,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            profile: profile.into(),
            status: SandboxStatus::Unavailable,
            enforced: false,
            reason: reason.into(),
            preview: true,
            native_available,
        }
    }

    pub fn unsafe_backend(profile: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            backend: SandboxBackendKind::Unsafe,
            profile: profile.into(),
            status: SandboxStatus::Unsafe,
            enforced: false,
            reason: reason.into(),
            preview: true,
            native_available: false,
        }
    }

    pub fn active(
        backend: SandboxBackendKind,
        profile: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            profile: profile.into(),
            status: SandboxStatus::Active,
            enforced: true,
            reason: reason.into(),
            preview: false,
            native_available: true,
        }
    }

    pub fn unconfigured(profile: impl Into<String>) -> Self {
        Self {
            backend: SandboxBackendKind::Native,
            profile: profile.into(),
            status: SandboxStatus::Unavailable,
            enforced: false,
            reason: "sandbox has not been applied".to_string(),
            preview: true,
            native_available: false,
        }
    }
}

/// Resolve a runtime status from explicit backend selection and pure state.
pub const fn resolve_sandbox_status(
    backend: SandboxBackendKind,
    native_available: bool,
    enforced: bool,
) -> SandboxStatus {
    match backend {
        SandboxBackendKind::Unsafe => SandboxStatus::Unsafe,
        SandboxBackendKind::Native if enforced => SandboxStatus::Active,
        SandboxBackendKind::Native if native_available => SandboxStatus::Preview,
        SandboxBackendKind::Native => SandboxStatus::Unavailable,
    }
}

/// Pure fail-closed gate used before native execution is authorized.
pub fn ensure_backend_available(
    backend: SandboxBackendKind,
    native_available: bool,
) -> Result<(), SandboxError> {
    match backend {
        SandboxBackendKind::Unsafe => Ok(()),
        SandboxBackendKind::Native if native_available => Ok(()),
        SandboxBackendKind::Native => Err(SandboxError::NativeUnavailable {
            reason: "native sandbox availability probe failed".to_string(),
        }),
    }
}

/// Probe native support without applying an irreversible sandbox.
pub fn native_sandbox_availability() -> NativeSandboxAvailability {
    #[cfg(all(feature = "enforce", unix))]
    {
        let support = nono::Sandbox::support_info();
        if support.is_supported {
            NativeSandboxAvailability::Available
        } else {
            NativeSandboxAvailability::Unavailable {
                reason: support.details.to_string(),
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        static AVAILABILITY: OnceLock<NativeSandboxAvailability> = OnceLock::new();
        AVAILABILITY
            .get_or_init(|| {
                let helper = atelier_windows_sandbox::command_runner_path()
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                if helper.is_err() {
                    return windows_native_availability_from_probes(helper, Ok(()));
                }
                let launch = atelier_windows_sandbox::probe_ready_launch_chain()
                    .map_err(|error| error.to_string());
                windows_native_availability_from_probes(helper, launch)
            })
            .clone()
    }

    #[cfg(all(not(target_os = "windows"), not(all(feature = "enforce", unix))))]
    {
        NativeSandboxAvailability::Unavailable {
            reason: "native sandbox enforcement is not compiled in this build".to_string(),
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_native_availability_from_probes(
    helper_probe: Result<(), String>,
    launch_probe: Result<(), String>,
) -> NativeSandboxAvailability {
    if let Err(error) = helper_probe {
        return NativeSandboxAvailability::Unavailable {
            reason: format!(
                "Windows child sandbox helper unavailable: {error}. Reinstall or rebuild `ate`, then run `ate sandbox status --json`."
            ),
        };
    }
    if let Err(error) = launch_probe {
        return NativeSandboxAvailability::Unavailable {
            reason: format!(
                "Windows persistent sandbox setup or launch-chain probe failed: {error} Run `ate sandbox status --json`. If setup is incomplete or a ready setup cannot launch, run `ate sandbox reset --yes`, then `ate sandbox setup` and retry."
            ),
        };
    }
    NativeSandboxAvailability::Available
}

/// Refuse a native launch when the platform probe failed, while preserving an
/// explicit unsafe escape hatch for callers that intentionally selected it.
pub fn require_backend(backend: SandboxBackendKind) -> Result<(), SandboxError> {
    let availability = native_sandbox_availability();
    if backend.is_unsafe() {
        return Ok(());
    }
    match availability {
        NativeSandboxAvailability::Available => Ok(()),
        NativeSandboxAvailability::Unavailable { reason } => {
            Err(SandboxError::NativeUnavailable { reason })
        }
    }
}

/// Validate the profiles supported by the first-stage Windows child sandbox.
///
/// The restricted-token backend can enforce workspace/read-only write scopes,
/// but it cannot yet enforce the allow-list and deny-read semantics required by
/// `strict` or arbitrary custom profiles. Refuse those profiles at startup
/// instead of presenting a weaker policy as equivalent enforcement.
#[cfg(windows)]
pub fn validate_windows_preview_profile(profile: &crate::ProfileName) -> Result<(), String> {
    match profile {
        crate::ProfileName::Workspace
        | crate::ProfileName::Devbox
        | crate::ProfileName::ReadOnly => Ok(()),
        crate::ProfileName::Strict => Err(
            "Windows sandbox preview cannot enforce the strict read allow-list yet; use workspace, read-only, or explicit unsafe"
                .to_owned(),
        ),
        crate::ProfileName::Custom(name) => Err(format!(
            "Windows sandbox preview cannot enforce custom profile `{name}` yet; use workspace, read-only, or explicit unsafe"
        )),
        crate::ProfileName::Off => Ok(()),
    }
}

/// Return the Windows child-runner mode for a successfully configured preview.
///
/// `None` means either the sandbox is not configured, the explicit unsafe
/// backend is active, or the profile is intentionally disabled.
#[cfg(windows)]
pub fn windows_child_sandbox_mode() -> Option<&'static str> {
    let diagnostics = crate::diagnostics();
    if diagnostics.backend != SandboxBackendKind::Native
        || !matches!(
            diagnostics.status,
            SandboxStatus::Preview | SandboxStatus::Active
        )
    {
        return None;
    }
    match crate::configured_profile_name()? {
        "read-only" | "readonly" => Some("read-only"),
        "workspace" | "devbox" => Some("workspace-write"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_and_explicit_unsafe_backends() {
        assert_eq!(
            parse_backend(None).expect("default backend"),
            SandboxBackendKind::Native
        );
        assert_eq!(
            "native"
                .parse::<SandboxBackendKind>()
                .expect("native backend"),
            SandboxBackendKind::Native
        );
        assert_eq!(
            "windows-codex"
                .parse::<SandboxBackendKind>()
                .expect("Codex backend alias"),
            SandboxBackendKind::Native
        );
        assert_eq!(
            "unsafe"
                .parse::<SandboxBackendKind>()
                .expect("unsafe backend"),
            SandboxBackendKind::Unsafe
        );
        assert!("unknown".parse::<SandboxBackendKind>().is_err());
    }

    #[test]
    fn resolves_status_without_platform_or_io() {
        assert_eq!(
            resolve_sandbox_status(SandboxBackendKind::Native, true, false),
            SandboxStatus::Preview
        );
        assert_eq!(
            resolve_sandbox_status(SandboxBackendKind::Native, true, true),
            SandboxStatus::Active
        );
        assert_eq!(
            resolve_sandbox_status(SandboxBackendKind::Native, false, false),
            SandboxStatus::Unavailable
        );
        assert_eq!(
            resolve_sandbox_status(SandboxBackendKind::Unsafe, false, false),
            SandboxStatus::Unsafe
        );
    }

    #[test]
    fn native_backend_fails_closed_but_explicit_unsafe_does_not() {
        assert!(ensure_backend_available(SandboxBackendKind::Native, false).is_err());
        assert!(ensure_backend_available(SandboxBackendKind::Native, true).is_ok());
        assert!(ensure_backend_available(SandboxBackendKind::Unsafe, false).is_ok());
    }

    #[test]
    fn diagnostics_are_serializable_and_labelled() {
        let diagnostics = SandboxDiagnostics::new(
            SandboxBackendKind::Unsafe,
            "workspace",
            false,
            false,
            "explicit unsafe backend selected",
        );
        assert_eq!(diagnostics.status, SandboxStatus::Unsafe);
        assert_eq!(diagnostics.status.as_str(), "unsafe");
        assert_eq!(
            serde_json::to_value(&diagnostics).expect("diagnostics serialization")["backend"],
            "unsafe"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_preview_rejects_profiles_it_cannot_enforce() {
        assert!(validate_windows_preview_profile(&crate::ProfileName::Workspace).is_ok());
        assert!(validate_windows_preview_profile(&crate::ProfileName::ReadOnly).is_ok());
        assert!(validate_windows_preview_profile(&crate::ProfileName::Strict).is_err());
        assert!(validate_windows_preview_profile(&crate::ProfileName::Custom("x".into())).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_availability_requires_ready_persistent_setup() {
        let availability = windows_native_availability_from_probes(
            Ok(()),
            Err(
                "Windows sandbox is not set up (state: not_setup). Run `ate sandbox setup`."
                    .to_owned(),
            ),
        );

        let NativeSandboxAvailability::Unavailable { reason } = availability else {
            panic!("an unconfigured persistent sandbox must fail closed");
        };
        assert!(reason.contains("not set up"), "{reason}");
        assert!(reason.contains("ate sandbox setup"), "{reason}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_availability_fails_closed_when_persistent_launch_chain_is_broken() {
        let availability = windows_native_availability_from_probes(
            Ok(()),
            Err("CreateProcessAsUserW failed: 5 (Access is denied)".to_owned()),
        );

        let NativeSandboxAvailability::Unavailable { reason } = availability else {
            panic!("a broken persistent launch chain must fail closed");
        };
        assert!(
            reason.contains("CreateProcessAsUserW failed: 5"),
            "{reason}"
        );
        assert!(reason.contains("ate sandbox status --json"), "{reason}");
        assert!(reason.contains("ate sandbox reset --yes"), "{reason}");
        assert!(reason.contains("ate sandbox setup"), "{reason}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_availability_requires_helper_and_full_launch_probe_to_succeed() {
        assert_eq!(
            windows_native_availability_from_probes(Ok(()), Ok(())),
            NativeSandboxAvailability::Available
        );

        let availability = windows_native_availability_from_probes(
            Err("embedded helper is missing".to_owned()),
            Ok(()),
        );
        let NativeSandboxAvailability::Unavailable { reason } = availability else {
            panic!("a missing helper must fail closed");
        };
        assert!(reason.contains("embedded helper is missing"), "{reason}");
    }

    #[test]
    fn manager_keeps_explicit_unsafe_state_visible() {
        let workspace = std::path::Path::new(".");
        let mut manager = crate::SandboxManager::new_with_backend(
            crate::ProfileName::Workspace,
            workspace,
            SandboxBackendKind::Unsafe,
        );

        manager.apply(workspace).expect("explicit unsafe backend");

        let diagnostics = manager.diagnostics();
        assert_eq!(diagnostics.backend, SandboxBackendKind::Unsafe);
        assert_eq!(diagnostics.status, SandboxStatus::Unsafe);
        assert!(!diagnostics.enforced);
        assert!(diagnostics.reason.contains("explicit unsafe"));
    }

    #[cfg(all(not(unix), not(windows)))]
    #[test]
    fn manager_rejects_native_backend_when_native_support_is_unavailable() {
        let workspace = std::path::Path::new(".");
        let mut manager = crate::SandboxManager::new(crate::ProfileName::Workspace, workspace);

        let error = manager
            .apply(workspace)
            .expect_err("native backend must fail closed");

        assert!(error.to_string().contains("refusing unsandboxed execution"));
        assert_eq!(manager.diagnostics().status, SandboxStatus::Unavailable);
        assert!(!manager.diagnostics().enforced);
    }

    #[cfg(windows)]
    #[test]
    fn manager_is_preview_or_fails_closed_when_helper_is_missing() {
        // A Rust test-harness executable does not implement Atelier's hidden
        // `--internal-windows-sandbox-runner` mode. Probing it would recursively
        // launch the test harness under the sandbox account instead of testing
        // the production runner. The real `ate.exe` launch chain is covered by
        // the Windows sandbox contract/E2E tests.
        let current_exe = std::env::current_exe().expect("current test executable");
        if current_exe
            .file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("atelier_sandbox-"))
        {
            return;
        }

        let workspace = std::path::Path::new(".");
        let mut manager = crate::SandboxManager::new(crate::ProfileName::Workspace, workspace);
        match native_sandbox_availability() {
            NativeSandboxAvailability::Available => {
                manager
                    .apply(workspace)
                    .expect("preview helper is available");
                assert_eq!(manager.diagnostics().status, SandboxStatus::Preview);
                assert!(!manager.diagnostics().enforced);
            }
            NativeSandboxAvailability::Unavailable { .. } => {
                let error = manager
                    .apply(workspace)
                    .expect_err("missing helper must fail closed");
                assert!(error.to_string().contains("refusing unsandboxed execution"));
                assert_eq!(manager.diagnostics().status, SandboxStatus::Unavailable);
                assert!(!manager.diagnostics().enforced);
            }
        }
    }
}
