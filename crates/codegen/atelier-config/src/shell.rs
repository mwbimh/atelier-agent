//! Native shell resolution for terminal command execution.
//!
//! Windows uses an absolute-path, startup-only cascade:
//! Atelier-managed PowerShell 7 → machine-wide PowerShell 7 → Windows
//! PowerShell 5.1 → unavailable. WindowsApps aliases, Git Bash, cmd.exe, WSL,
//! and bare PATH-dependent shell names are never selected automatically.

/// Detected Windows shell and how to invoke it.
#[cfg(not(unix))]
#[derive(Clone, Debug)]
pub enum WindowsShell {
    PowerShell7(String),
    WindowsPowerShell51(String),
    Unavailable(String),
}

/// Command-language capability profile for the startup-resolved Windows shell.
#[cfg(not(unix))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsShellCapability {
    PowerShell7,
    PowerShell51Compatibility,
    Unavailable,
}

/// Detect the startup shell once. The returned program is always an absolute,
/// ordinary filesystem path or `Unavailable`.
#[cfg(not(unix))]
pub fn detect_windows_shell() -> &'static WindowsShell {
    use std::sync::OnceLock;
    static CACHED: OnceLock<WindowsShell> = OnceLock::new();
    CACHED.get_or_init(resolve_windows_shell)
}

#[cfg(not(unix))]
fn resolve_windows_shell() -> WindowsShell {
    if let Ok(override_path) = std::env::var("ATELIER_SHELL") {
        let path = std::path::PathBuf::from(override_path.trim());
        if path.is_absolute() && supported_shell_path(&path) {
            let shell = classify_windows_shell(path);
            if probe_windows_shell(&shell) {
                return shell;
            }
            return WindowsShell::Unavailable(
                "ATELIER_SHELL did not pass the PowerShell startup probe".to_owned(),
            );
        }
        return WindowsShell::Unavailable(
            "ATELIER_SHELL must be an absolute, non-WindowsApps PowerShell path".to_owned(),
        );
    }

    if let Some(path) = managed_powershell_path().filter(|path| supported_pwsh7_path(path)) {
        let shell = WindowsShell::PowerShell7(path.to_string_lossy().into_owned());
        if probe_windows_shell(&shell) {
            tracing::info!(shell = %path.display(), "Windows shell: managed PowerShell 7");
            return shell;
        }
        tracing::warn!(shell = %path.display(), "managed PowerShell 7 failed its startup probe");
    }

    for path in machine_powershell7_candidates() {
        if supported_pwsh7_path(&path) {
            let shell = WindowsShell::PowerShell7(path.to_string_lossy().into_owned());
            if probe_windows_shell(&shell) {
                tracing::info!(shell = %path.display(), "Windows shell: machine-wide PowerShell 7");
                return shell;
            }
            tracing::warn!(shell = %path.display(), "machine-wide PowerShell 7 failed its startup probe");
        }
    }

    let system_root = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
    let ps51 = system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    if supported_shell_path(&ps51) {
        let shell = WindowsShell::WindowsPowerShell51(ps51.to_string_lossy().into_owned());
        if probe_windows_shell(&shell) {
            tracing::warn!(shell = %ps51.display(), "Windows shell: PowerShell 5.1 compatibility fallback");
            return shell;
        }
        tracing::warn!(shell = %ps51.display(), "PowerShell 5.1 failed its startup probe");
    }

    WindowsShell::Unavailable("no sandbox-compatible PowerShell runtime is installed".to_owned())
}

#[cfg(not(unix))]
fn managed_powershell_path() -> Option<std::path::PathBuf> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        schema_version: u32,
        path: String,
    }
    let program_data = std::env::var_os("ProgramData")?;
    let runtime_root = std::path::PathBuf::from(program_data).join("Atelier/runtimes/powershell");
    let manifest_path = runtime_root.join("active.json");
    let manifest: Manifest =
        serde_json::from_str(&std::fs::read_to_string(manifest_path).ok()?).ok()?;
    let path = std::path::PathBuf::from(manifest.path);
    (manifest.schema_version == 1 && is_managed_pwsh_layout(&runtime_root, &path)).then_some(path)
}

#[cfg(not(unix))]
fn is_managed_pwsh_layout(runtime_root: &std::path::Path, path: &std::path::Path) -> bool {
    path.is_absolute()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("pwsh.exe"))
        && path
            .parent()
            .and_then(std::path::Path::parent)
            .is_some_and(|parent| parent == runtime_root)
        && path
            .parent()
            .and_then(std::path::Path::file_name)
            .is_some_and(|version| !version.is_empty())
}

#[cfg(not(unix))]
fn machine_powershell7_candidates() -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();
    for variable in ["ProgramW6432", "ProgramFiles"] {
        if let Some(root) = std::env::var_os(variable) {
            let candidate = std::path::PathBuf::from(root).join("PowerShell/7/pwsh.exe");
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

#[cfg(not(unix))]
fn classify_windows_shell(path: std::path::PathBuf) -> WindowsShell {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if file_name.eq_ignore_ascii_case("pwsh.exe") {
        WindowsShell::PowerShell7(path.to_string_lossy().into_owned())
    } else if file_name.eq_ignore_ascii_case("powershell.exe") {
        WindowsShell::WindowsPowerShell51(path.to_string_lossy().into_owned())
    } else {
        WindowsShell::Unavailable("ATELIER_SHELL must select pwsh.exe or powershell.exe".to_owned())
    }
}

#[cfg(not(unix))]
fn probe_windows_shell(shell: &WindowsShell) -> bool {
    let (program, minimum_major) = match shell {
        WindowsShell::PowerShell7(path) => (path, 7),
        WindowsShell::WindowsPowerShell51(path) => (path, 5),
        WindowsShell::Unavailable(_) => return false,
    };
    let mut command = std::process::Command::new(program);
    atelier_tty_utils::detach_std_command(&mut command);
    let probe = format!("if ($PSVersionTable.PSVersion.Major -lt {minimum_major}) {{ exit 1 }}");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", &probe])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(not(unix))]
fn supported_pwsh7_path(path: &std::path::Path) -> bool {
    supported_shell_path(path)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("pwsh.exe"))
}

#[cfg(not(unix))]
fn supported_shell_path(path: &std::path::Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    if !path.is_absolute() || is_windowsapps_path(path) {
        return false;
    }
    std::fs::metadata(path).is_ok_and(|metadata| {
        metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    })
}

#[cfg(not(unix))]
fn is_windowsapps_path(path: &std::path::Path) -> bool {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
        .contains("\\windowsapps\\")
}

#[cfg(not(unix))]
impl WindowsShell {
    /// Short executable-family name used by shell-aware tool dispatch.
    pub fn name(&self) -> &'static str {
        match self {
            Self::PowerShell7(_) => "pwsh",
            Self::WindowsPowerShell51(_) => "powershell",
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// Explicit command-language capability profile. Callers must not treat
    /// Windows PowerShell 5.1 as equivalent to PowerShell 7.
    pub fn capability(&self) -> WindowsShellCapability {
        match self {
            Self::PowerShell7(_) => WindowsShellCapability::PowerShell7,
            Self::WindowsPowerShell51(_) => WindowsShellCapability::PowerShell51Compatibility,
            Self::Unavailable(_) => WindowsShellCapability::Unavailable,
        }
    }

    /// Human-readable shell description for prompts and diagnostics.
    pub fn prompt_description(&self) -> &'static str {
        match self.capability() {
            WindowsShellCapability::PowerShell7 => "PowerShell 7 (pwsh)",
            WindowsShellCapability::PowerShell51Compatibility => {
                "Windows PowerShell 5.1 compatibility mode (no && pipeline chaining)"
            }
            WindowsShellCapability::Unavailable => "unavailable",
        }
    }

    /// User-facing startup warning for the reduced PS5.1 fallback profile.
    pub fn compatibility_notice(&self) -> Option<&'static str> {
        matches!(
            self.capability(),
            WindowsShellCapability::PowerShell51Compatibility
        )
        .then_some(
            "Warning: Atelier is using Windows PowerShell 5.1 compatibility mode. Install or repair Atelier-managed PowerShell 7 for full shell and PTY support.",
        )
    }

    /// Whether this shell supports the `&&` pipeline chain operator for
    /// error-propagating command chaining.
    ///
    /// - `pwsh` (PowerShell 7+): `&&` added in PS 7.0.
    /// - Git Bash: standard bash `&&`.
    /// - `powershell.exe` (5.1): no `&&` support; use `;`.
    /// - `cmd.exe`: `&&` works but is inconsistent with the `-Command`
    ///   invocation style used elsewhere; use `;` for uniformity.
    pub fn supports_chain_operator(&self) -> bool {
        matches!(self, Self::PowerShell7(_))
    }

    /// Whether `grep`, `head`, `tail`, `sed`, `awk`, `find` are usable
    /// from this shell. True for Git Bash (MSYS2 bundles them inside the
    /// bash subprocess); false for PowerShell and `cmd.exe`.
    pub fn has_unix_utilities(&self) -> bool {
        false
    }

    /// How this shell interprets a bare `&` token. Drives the `run_terminal_cmd`
    /// background-operator validation, which must differ per shell.
    pub fn ampersand_semantics(&self) -> AmpersandSemantics {
        match self {
            Self::PowerShell7(_) => AmpersandSemantics::PowerShellCore,
            Self::WindowsPowerShell51(_) => AmpersandSemantics::WindowsPowerShell,
            Self::Unavailable(_) => AmpersandSemantics::WindowsPowerShell,
        }
    }
}

/// Returns the appropriate command chaining separator for the current
/// platform and detected shell.
///
/// - Unix: always `"&&"` (bash/zsh).
/// - Windows with pwsh or Git Bash: `"&&"` (both support pipeline chain
///   operators).
/// - Windows with powershell.exe (5.1) or cmd.exe: `";"`.
pub fn chain_separator() -> &'static str {
    #[cfg(unix)]
    {
        "&&"
    }
    #[cfg(not(unix))]
    {
        if detect_windows_shell().supports_chain_operator() {
            "&&"
        } else {
            ";"
        }
    }
}

/// Whether `grep`, `head`, `tail`, `sed`, `awk`, `find` are usable from
/// the active shell. True on Unix and Windows + Git Bash; false on
/// Windows + PowerShell or `cmd.exe`.
///
/// Tool descriptions branch on this to swap Unix-centric guidance for
/// shell-aware guidance and avoid `'grep' is not recognized` failures.
pub fn has_unix_utilities() -> bool {
    #[cfg(unix)]
    {
        true
    }
    #[cfg(not(unix))]
    {
        detect_windows_shell().has_unix_utilities()
    }
}

/// Whether `name` resolves to an executable on the current `$PATH`.
///
/// Used by the truncated-MCP steer to name only tools that are actually present
/// on the tool server's `$PATH` (no "if available" hedge). `which` handles the
/// platform details (PATHEXT and App Execution Aliases on Windows).
///
/// Probes the base environment (tool server is co-located with the shell tool
/// in production). Per-session `export PATH` mutations inside the persistent
/// shell are not reflected (uncommon for `jq`/`python`/`sed`/`cut`).
pub fn is_command_available(name: &str) -> bool {
    which::which(name).is_ok()
}

/// How a shell interprets a bare `&` token. Drives `run_terminal_cmd`
/// background-operator detection and remediation, which must differ per shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmpersandSemantics {
    /// Bash/POSIX: a bare `&` backgrounds the command (Unix shells, Git Bash).
    PosixBackground,
    /// PowerShell 7+ (`pwsh`): a *leading* `&` is the call/invocation operator;
    /// a *trailing* `&` starts a background job.
    PowerShellCore,
    /// Windows PowerShell 5.1 (`powershell.exe`): a *leading* `&` is the call
    /// operator; a *trailing* `&` is a parse error.
    WindowsPowerShell,
    /// `cmd.exe`: `&` is an unconditional sequential command separator.
    CmdSeparator,
}

/// How the active shell interprets a bare `&`. Unix shells are always
/// [`AmpersandSemantics::PosixBackground`]; on Windows it depends on the
/// detected shell (Git Bash vs. PowerShell vs. `cmd.exe`).
pub fn ampersand_semantics() -> AmpersandSemantics {
    #[cfg(unix)]
    {
        AmpersandSemantics::PosixBackground
    }
    #[cfg(not(unix))]
    {
        detect_windows_shell().ampersand_semantics()
    }
}

/// How to invoke a command in the detected Windows shell.
#[cfg(not(unix))]
#[derive(Debug)]
pub struct ShellInvocation {
    pub program: String,
    pub args: Vec<String>,
    /// Env vars that must be set on the child process (e.g. `MSYS_NO_PATHCONV`
    /// for Git Bash to prevent POSIX-to-Windows path translation of `/flags`).
    pub env: Vec<(&'static str, &'static str)>,
}

/// Build `(program, args, env)` for running `command` in the detected shell.
#[cfg(not(unix))]
pub fn shell_command_argv(command: &str) -> std::io::Result<ShellInvocation> {
    invocation_for(detect_windows_shell(), command)
}

/// Pure builder split out of `shell_command_argv` so tests can exercise every
/// `WindowsShell` variant, not just the one installed on the test host.
#[cfg(not(unix))]
fn invocation_for(shell: &WindowsShell, command: &str) -> std::io::Result<ShellInvocation> {
    // Force UTF-8 for descendant tools. Windows' legacy ANSI codepage (cp1252)
    // makes locale-sensitive children mis-decode UTF-8 subprocess output — e.g.
    // Python's text-mode `subprocess` raised `UnicodeDecodeError` on `gh` output.
    // `PYTHONUTF8=1` is the fix (forces `locale.getpreferredencoding` to utf-8);
    // `PYTHONIOENCODING` covers the interpreter's own stdio, `surrogateescape`
    // matching UTF-8 Mode's leniency. Applied before the per-request env, so an
    // explicit caller value still overrides these defaults.
    let utf8_env = [
        ("PYTHONUTF8", "1"),
        ("PYTHONIOENCODING", "utf-8:surrogateescape"),
    ];
    match shell {
        WindowsShell::PowerShell7(path) => Ok(ShellInvocation {
            program: path.clone(),
            args: vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                command.to_string(),
            ],
            env: utf8_env.to_vec(),
        }),
        WindowsShell::WindowsPowerShell51(path) => Ok(ShellInvocation {
            program: path.clone(),
            args: vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-Command".to_string(),
                format!(
                    "[Console]::InputEncoding = [Text.UTF8Encoding]::new($false); \
                     [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); \
                     $OutputEncoding = [Text.UTF8Encoding]::new($false); {command}"
                ),
            ],
            env: utf8_env.to_vec(),
        }),
        WindowsShell::Unavailable(reason) => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Windows shell unavailable: {reason}"),
        )),
    }
}

// =============================================================================
// Unix shell resolution
// =============================================================================
//
// Locates an absolute path to a bash/zsh binary on Unix:
//
//   1. `$ATELIER_SHELL` override, if it names the requested kind and is runnable.
//   2. `$SHELL`, if it names the requested kind and is runnable.
//      Covers most NixOS / Homebrew / `nix-darwin` setups where the user's
//      login shell already lives at the resolved path (e.g.
//      `/run/current-system/sw/bin/bash`, `/opt/homebrew/bin/bash`).
//   3. `which::which(name)` — walks `$PATH`. Catches NixOS profile shells in
//      `/nix/store/...` or `/etc/profiles/per-user/<u>/bin/` when `/bin/bash`
//      is absent.
//   4. A fixed candidate list: `{/bin, /usr/bin, /usr/local/bin,
//      /opt/homebrew/bin} × {bash,zsh}`.
//   5. Hardcoded `/bin/<name>` — historical behavior, only reached when every
//      earlier step has failed.
//
// The result is cached per kind in a process-wide `OnceLock`, so the cascade
// is run at most once per shell kind per process.

/// Which Unix shell we're asking about. Bash and zsh are the only kinds
/// supported by the persistent shell-state backend (the dump scripts are
/// bash/zsh-specific). Fish / dash / ksh users fall through to bash.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnixShellKind {
    Bash,
    Zsh,
}

#[cfg(unix)]
impl UnixShellKind {
    /// Binary file name (`"bash"` / `"zsh"`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }

    /// Hardcoded historical default. Only used as the last-resort fallback.
    fn hardcoded_default(self) -> &'static str {
        match self {
            Self::Bash => "/bin/bash",
            Self::Zsh => "/bin/zsh",
        }
    }
}

/// Detect the user's preferred Unix shell kind from `$SHELL`. Defaults to
/// `Bash` when `$SHELL` is unset or unrecognized. Cheap; not cached.
#[cfg(unix)]
pub fn detect_unix_shell_kind() -> UnixShellKind {
    match std::env::var("SHELL") {
        Ok(s) if s.contains("zsh") => UnixShellKind::Zsh,
        _ => UnixShellKind::Bash,
    }
}

/// Absolute path to the requested Unix shell binary, computed via the
/// cascade above. Cached for the process lifetime.
#[cfg(unix)]
pub fn unix_shell_path(kind: UnixShellKind) -> &'static str {
    use std::sync::OnceLock;
    static BASH: OnceLock<String> = OnceLock::new();
    static ZSH: OnceLock<String> = OnceLock::new();
    let cache = match kind {
        UnixShellKind::Bash => &BASH,
        UnixShellKind::Zsh => &ZSH,
    };
    cache.get_or_init(|| {
        let path = resolve_unix_shell_path(kind);
        tracing::debug!(kind = ?kind, resolved = %path, "resolved Unix shell path");
        path
    })
}

#[cfg(unix)]
fn resolve_unix_shell_path(kind: UnixShellKind) -> String {
    let name = kind.name();
    let matches_kind = |p: &std::path::Path| p.file_name().and_then(|n| n.to_str()) == Some(name);

    // 1) Explicit override via $ATELIER_SHELL.
    if let Ok(s) = std::env::var("ATELIER_SHELL") {
        let p = std::path::PathBuf::from(&s);
        if matches_kind(&p) && is_executable(&p) {
            return s;
        }
    }

    // 2) $SHELL, when it matches the requested kind.
    if let Ok(s) = std::env::var("SHELL") {
        let p = std::path::PathBuf::from(&s);
        if matches_kind(&p) && is_executable(&p) {
            return s;
        }
    }

    // 3) `which` walks $PATH (handles NixOS, Homebrew, custom profiles).
    if let Ok(p) = which::which(name)
        && is_executable(&p)
    {
        return p.to_string_lossy().into_owned();
    }

    // 4) Common install dirs.
    for dir in ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
        let p = std::path::PathBuf::from(dir).join(name);
        if is_executable(&p) {
            return p.to_string_lossy().into_owned();
        }
    }

    // 5) Hardcoded fallback — same as historical behavior. Spawn will fail at
    //    runtime on a pure NixOS host with no bash, but that's no worse than
    //    before this resolver existed.
    kind.hardcoded_default().to_string()
}

/// Whether `path` is an executable file.
///
/// First tries the file's mode bits (any-x). If that's inconclusive, falls
/// back to actually invoking `<path> --version`. The `--version` fallback
/// exists for Nix and other environments where the `X_OK` mode-bit check can
/// be misleading: some Nix overlay filesystems expose binaries whose
/// owner/group/world mode bits don't reflect their real executability.
///
/// The probe is spawned via `atelier_tty_utils::detach_std_command` so that
/// the child does NOT inherit the parent's controlling TTY. The resolver
/// runs lazily inside `unix_shell_path`'s `OnceLock::get_or_init` which
/// can fire during interactive TUI/pager startup; without detach, a
/// misbehaving shell binary that emits mouse-tracking escapes or asks
/// for a controlling tty during `--version` would spew garbage onto the
/// pager screen. `stdin`, `stdout`, and `stderr` are pinned to `null`
/// to drop any output the binary does emit. See `codegen-conventions`
/// SKILL.md for the workspace-wide subprocess rule.
#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path)
        && meta.is_file()
        && meta.permissions().mode() & 0o111 != 0
    {
        return true;
    }

    // Nix fallback. Detach from the controlling TTY via atelier_tty_utils so
    // the probe (which the resolver may run during interactive TUI/pager
    // startup) cannot leak escapes onto the parent's terminal.
    let mut cmd = std::process::Command::new(path);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    atelier_tty_utils::detach_std_command(&mut cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn has_unix_utilities_is_true_on_unix() {
        assert!(has_unix_utilities());
    }

    #[cfg(unix)]
    #[test]
    fn chain_separator_is_ampersand_on_unix() {
        assert_eq!(chain_separator(), "&&");
    }

    #[test]
    fn is_command_available_detects_present_and_absent() {
        // A shell present on every host of this OS resolves; a bogus name never
        // does. `cmd` resolves via PATHEXT on Windows, `sh` lives on $PATH on Unix.
        #[cfg(windows)]
        let present = "cmd";
        #[cfg(not(windows))]
        let present = "sh";
        assert!(is_command_available(present));
        assert!(!is_command_available(
            "xai-definitely-not-a-real-command-xyz"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn ampersand_semantics_is_posix_background_on_unix() {
        assert_eq!(ampersand_semantics(), AmpersandSemantics::PosixBackground);
    }

    #[cfg(unix)]
    #[test]
    fn unix_shell_path_returns_a_bash() {
        // Whatever it returns, it must end in "bash" (the resolver guarantees
        // the result's file_name matches the requested kind, even for the
        // hardcoded `/bin/bash` fallback).
        let p = unix_shell_path(UnixShellKind::Bash);
        assert!(
            std::path::Path::new(p).file_name().and_then(|n| n.to_str()) == Some("bash"),
            "expected a path ending in 'bash', got {p}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_shell_path_is_cached() {
        // Two calls return the same `&'static str` (pointer equality).
        let a = unix_shell_path(UnixShellKind::Bash);
        let b = unix_shell_path(UnixShellKind::Bash);
        assert!(
            std::ptr::eq(a.as_ptr(), b.as_ptr()),
            "result should be cached"
        );
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_recognizes_bin_sh() {
        // /bin/sh is the one path POSIX promises across every Unix variant
        // we care about; on macOS and Linux distros it's always executable.
        // (Pure NixOS images may lack it, in which case this test is
        // skipped — same approach as the existing `/bin/bash` gated tests.)
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }
        assert!(is_executable(std::path::Path::new("/bin/sh")));
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_rejects_non_executable() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Mode bits explicitly cleared — not executable.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(tmp.path()));
    }

    #[cfg(unix)]
    #[test]
    fn detect_unix_shell_kind_falls_back_to_bash() {
        // We can't safely mutate $SHELL in a multithreaded test runner, so just
        // sanity-check the function returns *something* and doesn't panic.
        let _ = detect_unix_shell_kind();
    }

    #[cfg(not(unix))]
    #[test]
    fn windows_shell_capability_profiles_are_explicit() {
        let ps7 = WindowsShell::PowerShell7(r"C:\PowerShell\pwsh.exe".into());
        assert_eq!(ps7.capability(), WindowsShellCapability::PowerShell7);
        assert_eq!(ps7.compatibility_notice(), None);
        assert!(ps7.prompt_description().contains("PowerShell 7"));

        let ps51 = WindowsShell::WindowsPowerShell51(
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into(),
        );
        assert_eq!(
            ps51.capability(),
            WindowsShellCapability::PowerShell51Compatibility
        );
        assert!(ps51.compatibility_notice().is_some());
        assert!(ps51.prompt_description().contains("compatibility mode"));
    }

    #[cfg(not(unix))]
    #[test]
    fn windows_shells_never_claim_unix_utilities() {
        assert!(!WindowsShell::PowerShell7(r"C:\PowerShell\pwsh.exe".into()).has_unix_utilities());
        assert!(
            !WindowsShell::WindowsPowerShell51(
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into()
            )
            .has_unix_utilities()
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn windowsapps_paths_are_rejected() {
        assert!(is_windowsapps_path(std::path::Path::new(
            r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\pwsh.exe"
        )));
        assert!(is_windowsapps_path(std::path::Path::new(
            r"C:\Program Files\WindowsApps\Microsoft.PowerShell_7\pwsh.exe"
        )));
    }

    #[cfg(not(unix))]
    #[test]
    fn managed_manifest_path_must_name_a_direct_versioned_runtime() {
        let root = std::path::Path::new(r"C:\ProgramData\Atelier\runtimes\powershell");
        assert!(is_managed_pwsh_layout(
            root,
            std::path::Path::new(r"C:\ProgramData\Atelier\runtimes\powershell\7.6.4\pwsh.exe")
        ));
        assert!(!is_managed_pwsh_layout(
            root,
            std::path::Path::new(r"C:\Program Files\PowerShell\7\pwsh.exe")
        ));
        assert!(!is_managed_pwsh_layout(
            root,
            std::path::Path::new(
                r"C:\ProgramData\Atelier\runtimes\powershell\active\nested\pwsh.exe"
            )
        ));
    }

    #[cfg(not(unix))]
    #[test]
    fn powershell_invocations_use_absolute_paths_and_utf8_envelopes() {
        let ps7 = invocation_for(
            &WindowsShell::PowerShell7(r"C:\Program Files\PowerShell\7\pwsh.exe".into()),
            "Write-Output ok",
        )
        .unwrap();
        assert!(std::path::Path::new(&ps7.program).is_absolute());
        assert!(ps7.env.contains(&("PYTHONUTF8", "1")));

        let ps51 = invocation_for(
            &WindowsShell::WindowsPowerShell51(
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into(),
            ),
            "Write-Output ok",
        )
        .unwrap();
        assert!(std::path::Path::new(&ps51.program).is_absolute());
        assert!(ps51.args.last().unwrap().contains("OutputEncoding"));
    }

    #[cfg(not(unix))]
    #[test]
    fn detected_windows_shell_uses_an_absolute_non_windowsapps_path() {
        let selected = match detect_windows_shell() {
            WindowsShell::PowerShell7(path) | WindowsShell::WindowsPowerShell51(path) => {
                assert!(std::path::Path::new(path).is_absolute());
                assert!(!is_windowsapps_path(std::path::Path::new(path)));
                path
            }
            WindowsShell::Unavailable(reason) => panic!("test host has no PowerShell: {reason}"),
        };

        // On an installed host, the managed manifest is authoritative ahead
        // of machine-wide PowerShell 7 and the PS5.1 compatibility fallback.
        if std::env::var_os("ATELIER_SHELL").is_none()
            && let Some(program_data) = std::env::var_os("ProgramData")
        {
            let manifest_path = std::path::PathBuf::from(program_data)
                .join("Atelier/runtimes/powershell/active.json");
            if let Ok(source) = std::fs::read_to_string(manifest_path) {
                let manifest: serde_json::Value =
                    serde_json::from_str(&source).expect("parse managed PowerShell manifest");
                let expected = manifest["path"]
                    .as_str()
                    .expect("managed PowerShell manifest path");
                assert!(selected.eq_ignore_ascii_case(expected));
            }
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn unavailable_shell_fails_closed() {
        let error =
            invocation_for(&WindowsShell::Unavailable("missing".into()), "echo hi").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
