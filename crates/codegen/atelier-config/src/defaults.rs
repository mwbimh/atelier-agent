//! First-run Atelier configuration tree and resettable built-in presets.

use std::io;
use std::path::{Path, PathBuf};

const PROVIDERS_TOML: &str = "schema_version = 2\n\n[providers]\n";
const ROLES_TOML: &str = "schema_version = 1\n\n[roles]\n";
const COMMON_MODELS_TOML: &str = r#"schema_version = 1

[[models]]
match = "gpt-5*"
wire_api = "responses"
default_effort = "medium"
reasoning_efforts = ["none", "minimal", "low", "medium", "high", "xhigh"]
fast_mode = true
context_window = 400000

[[models]]
match = "o3*"
wire_api = "responses"
default_effort = "medium"
reasoning_efforts = ["low", "medium", "high"]
fast_mode = false
context_window = 200000

[[models]]
match = "claude-*"
wire_api = "messages"
default_effort = "high"
reasoning_efforts = ["low", "medium", "high"]
fast_mode = false
context_window = 200000

[[models]]
match = "gemini-*"
wire_api = "chat_completions"
default_effort = "medium"
reasoning_efforts = ["low", "medium", "high"]
fast_mode = false
context_window = 1000000

[[models]]
match = "deepseek-*"
wire_api = "chat_completions"
default_effort = "medium"
reasoning_efforts = ["low", "medium", "high"]
fast_mode = true
context_window = 131072

[[models]]
match = "grok-*"
wire_api = "chat_completions"
default_effort = "medium"
reasoning_efforts = ["low", "medium", "high"]
fast_mode = true
context_window = 256000
"#;
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
        relative: "models/default/common.toml",
        content: COMMON_MODELS_TOML,
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
    for file in RESETTABLE_DEFAULT_FILES {
        write_owned(home.join(file.relative), file.content)?;
    }
    Ok(())
}

fn default_config(_version: &str) -> String {
    "context = \"default\"\nrequest_agent = \"atelier\"\n".to_owned()
}

fn default_request_agents(version: &str) -> String {
    format!(
        r#"schema_version = 1

[agents.atelier]
name = "Atelier"
version = "{version}"

[agents.pi]
name = "pi"
version = "1.0"

[agents.codex]
name = "codex"
version = "1.0"

[agents.opencode]
name = "opencode"
version = "1.0"
"#
    )
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
