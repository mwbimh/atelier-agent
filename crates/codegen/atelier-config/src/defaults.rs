//! First-run Atelier configuration tree and resettable built-in presets.

use std::io;
use std::path::{Path, PathBuf};

const PROVIDERS_TOML: &str = "schema_version = 3\n\n[providers]\n";
const ROLES_TOML: &str = "schema_version = 1\n\n[roles]\n";
const OPENAI_MODELS_TOML: &str = include_str!("../defaults/models/openai.toml");
const ANTHROPIC_MODELS_TOML: &str = include_str!("../defaults/models/anthropic.toml");
const GOOGLE_MODELS_TOML: &str = include_str!("../defaults/models/google.toml");
const DEEPSEEK_MODELS_TOML: &str = include_str!("../defaults/models/deepseek.toml");
const XAI_MODELS_TOML: &str = include_str!("../defaults/models/xai.toml");
const LOGO: &str = r#"    ___  ____________
   /   |/_  __/ ____/
  / /| | / / / __/
 / ___ |/ / / /___
/_/  |_/_/ /_____/
     A T E L I E R
"#;

struct DefaultFile {
    relative: &'static str,
    content: &'static str,
}

const FIRST_RUN_ONLY_DEFAULT_FILES: &[DefaultFile] = &[
    DefaultFile {
        relative: "providers.toml",
        content: PROVIDERS_TOML,
    },
    DefaultFile {
        relative: "roles.toml",
        content: ROLES_TOML,
    },
    DefaultFile {
        relative: "branding/logo.txt",
        content: LOGO,
    },
];

const RESETTABLE_DEFAULT_FILES: &[DefaultFile] = &[
    DefaultFile {
        relative: "models/default/openai.toml",
        content: OPENAI_MODELS_TOML,
    },
    DefaultFile {
        relative: "models/default/anthropic.toml",
        content: ANTHROPIC_MODELS_TOML,
    },
    DefaultFile {
        relative: "models/default/google.toml",
        content: GOOGLE_MODELS_TOML,
    },
    DefaultFile {
        relative: "models/default/deepseek.toml",
        content: DEEPSEEK_MODELS_TOML,
    },
    DefaultFile {
        relative: "models/default/xai.toml",
        content: XAI_MODELS_TOML,
    },
    DefaultFile {
        relative: "contexts/default/main.md",
        content: include_str!("../../atelier-agent/templates/prompt.md"),
    },
    DefaultFile {
        relative: "contexts/default/subagent.md",
        content: include_str!("../../atelier-agent/templates/subagent_prompt.md"),
    },
    DefaultFile {
        relative: "contexts/default/apply_patch.md",
        content: include_str!("../../atelier-agent/templates/apply_patch_prompt.md"),
    },
    DefaultFile {
        relative: "contexts/default/goal/planner.md",
        content: include_str!("../../atelier-shell/src/session/templates/goal_planner_prompt.md"),
    },
    DefaultFile {
        relative: "contexts/default/goal/strategist.md",
        content: include_str!(
            "../../atelier-shell/src/session/templates/goal_strategist_prompt.md"
        ),
    },
    DefaultFile {
        relative: "contexts/default/goal/skeptic.md",
        content: include_str!("../../atelier-shell/src/session/templates/goal_verifier_prompt.md"),
    },
    DefaultFile {
        relative: "contexts/default/goal/summary.md",
        content: include_str!(
            "../../atelier-shell/src/session/templates/goal_summarizer_prompt.md"
        ),
    },
    DefaultFile {
        relative: "contexts/default/roles/main.md",
        content: include_str!("../defaults/contexts/roles/main.md"),
    },
    DefaultFile {
        relative: "contexts/default/roles/explore.md",
        content: include_str!("../defaults/contexts/roles/explore.md"),
    },
    DefaultFile {
        relative: "contexts/default/roles/implement.md",
        content: include_str!("../defaults/contexts/roles/implement.md"),
    },
    DefaultFile {
        relative: "contexts/default/roles/review.md",
        content: include_str!("../defaults/contexts/roles/review.md"),
    },
    DefaultFile {
        relative: "contexts/default/roles/test.md",
        content: include_str!("../defaults/contexts/roles/test.md"),
    },
    DefaultFile {
        relative: "contexts/default/roles/compact.md",
        content: include_str!("../defaults/contexts/roles/compact.md"),
    },
    DefaultFile {
        relative: "contexts/default/roles/summary.md",
        content: include_str!("../defaults/contexts/roles/summary.md"),
    },
    DefaultFile {
        relative: "contexts/default/roles/title.md",
        content: include_str!("../defaults/contexts/roles/title.md"),
    },
    DefaultFile {
        relative: "contexts/default/roles/planner.md",
        content: include_str!("../defaults/contexts/roles/planner.md"),
    },
    DefaultFile {
        relative: "contexts/default/roles/strategist.md",
        content: include_str!("../defaults/contexts/roles/strategist.md"),
    },
    DefaultFile {
        relative: "contexts/default/roles/skeptic.md",
        content: include_str!("../defaults/contexts/roles/skeptic.md"),
    },
    DefaultFile {
        relative: "contexts/default/compaction/developer.md",
        content: include_str!(
            "../../../common/atelier-compaction/src/templates/compaction_developer_prompt.txt"
        ),
    },
    DefaultFile {
        relative: "contexts/default/compaction/user.md",
        content: include_str!(
            "../../../common/atelier-compaction/src/templates/compaction_user_prompt.txt"
        ),
    },
];
const USER_OWNED_DIRECTORIES: &[&str] = &[
    "models/providers",
    "credentials/oauth/providers",
    "credentials/oauth/mcp",
    "cache/providers",
];

pub fn ensure_user_defaults(home: &Path, version: &str) -> io::Result<()> {
    std::fs::create_dir_all(home)?;
    for directory in USER_OWNED_DIRECTORIES {
        std::fs::create_dir_all(home.join(directory))?;
    }
    write_if_missing(home.join("config.toml"), &default_config(version))?;
    write_if_missing(
        home.join("request-agents.toml"),
        &default_request_agents(version),
    )?;
    for file in FIRST_RUN_ONLY_DEFAULT_FILES {
        write_if_missing(home.join(file.relative), file.content)?;
    }
    for file in RESETTABLE_DEFAULT_FILES {
        write_if_missing(home.join(file.relative), file.content)?;
    }
    Ok(())
}

/// Restore only the built-in model presets and the `default` context preset.
///
/// All user selections and other configuration files remain untouched.
pub fn reset_user_defaults(home: &Path) -> io::Result<()> {
    std::fs::create_dir_all(home)?;
    let owned_directories =
        ["models/default", "contexts/default"].map(|relative| home.join(relative));
    for path in &owned_directories {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "refusing to replace non-directory preset path {}",
                        path.display()
                    ),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    for path in owned_directories {
        if path.exists() {
            std::fs::remove_dir_all(path)?;
        }
    }
    for file in RESETTABLE_DEFAULT_FILES {
        write_owned(home.join(file.relative), file.content)?;
    }
    Ok(())
}

fn default_config(_version: &str) -> String {
    "context = \"default\"\nrequest_agent = \"atelier\"\n".to_owned()
}

fn default_request_agents(version: &str) -> String {
    let platform = request_agent_platform();
    let atelier_user_agent = format!(
        "Atelier/{version} ({}; {})",
        platform.generic_os, platform.rust_arch
    );
    let pi_user_agent = format!(
        "pi/0.82.1 ({}; {}; {})",
        platform.pi_os,
        detected_node_runtime(),
        platform.pi_arch
    );
    let codex_user_agent = format!(
        "codex_cli_rs/0.145.0 ({} {}; {}) {}",
        platform.codex_os,
        detected_os_version(),
        platform.rust_arch,
        detected_terminal_token()
    );

    format!(
        r#"# Version and User-Agent snapshot verified 2026-07-25 against the released clients.
schema_version = 1

[agents.atelier]
name = "Atelier"
version = "{version}"
user_agent = {atelier_user_agent:?}

[agents.pi]
name = "pi"
version = "0.82.1"
user_agent = {pi_user_agent:?}

[agents.codex]
name = "codex_cli_rs"
version = "0.145.0"
user_agent = {codex_user_agent:?}

[agents.opencode]
name = "opencode"
version = "1.18.5"
user_agent = "opencode/1.18.5"
"#
    )
}

struct RequestAgentPlatform {
    generic_os: &'static str,
    rust_arch: &'static str,
    pi_os: &'static str,
    pi_arch: &'static str,
    codex_os: &'static str,
}

fn request_agent_platform() -> RequestAgentPlatform {
    let generic_os = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        other => other,
    };
    let rust_arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => other,
    };
    let pi_os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let pi_arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    let codex_os = match std::env::consts::OS {
        "macos" => "Mac OS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    RequestAgentPlatform {
        generic_os,
        rust_arch,
        pi_os,
        pi_arch,
        codex_os,
    }
}

fn detected_node_runtime() -> String {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|version| version.trim().trim_start_matches('v').to_owned())
        .filter(|version| !version.is_empty())
        .map(|version| format!("node/v{version}"))
        .unwrap_or_else(|| "node/unknown".to_owned())
}

fn detected_os_version() -> String {
    #[cfg(target_os = "windows")]
    {
        return std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Environment]::OSVersion.Version.ToString()",
            ])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|version| !version.is_empty())
            .unwrap_or_else(|| "unknown".to_owned());
    }
    #[cfg(target_os = "linux")]
    {
        return std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|version| version.trim().to_owned())
            .filter(|version| !version.is_empty())
            .unwrap_or_else(|| "unknown".to_owned());
    }
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|version| version.trim().to_owned())
            .filter(|version| !version.is_empty())
            .unwrap_or_else(|| "unknown".to_owned());
    }
    #[allow(unreachable_code)]
    "unknown".to_owned()
}

fn detected_terminal_token() -> String {
    let term_program = std::env::var("TERM_PROGRAM")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let value = match term_program {
        Some(program) => std::env::var("TERM_PROGRAM_VERSION")
            .ok()
            .filter(|version| !version.trim().is_empty())
            .map_or(program.clone(), |version| format!("{program}/{version}")),
        None if std::env::var_os("WT_SESSION").is_some() => "WindowsTerminal".to_owned(),
        None => std::env::var("TERM")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
    };
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '/') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn write_if_missing(path: PathBuf, content: &str) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    write_owned(path, content)
}

fn write_owned(path: PathBuf, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}
