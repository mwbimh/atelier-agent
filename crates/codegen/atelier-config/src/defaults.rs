//! First-run Atelier configuration tree and resettable built-in presets.

use std::io;
use std::path::{Path, PathBuf};

const PROVIDERS_TOML: &str = "schema_version = 2\n\n[providers]\n";
const ROLES_TOML: &str = r#"schema_version = 1

[roles.main]
provider = "allm"
model = "deepseek-v4-flash"
[roles.explore]
provider = "allm"
model = "deepseek-v4-flash"
[roles.implement]
provider = "allm"
model = "deepseek-v4-flash"
[roles.review]
provider = "allm"
model = "deepseek-v4-flash"
[roles.test]
provider = "allm"
model = "deepseek-v4-flash"
[roles.compact]
provider = "allm"
model = "deepseek-v4-flash"
[roles.summary]
provider = "allm"
model = "deepseek-v4-flash"
[roles.title]
provider = "allm"
model = "deepseek-v4-flash"
[roles.planner]
provider = "allm"
model = "deepseek-v4-flash"
[roles.strategist]
provider = "allm"
model = "deepseek-v4-flash"
[roles.skeptic]
provider = "allm"
model = "deepseek-v4-flash"
"#;
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

const RESETTABLE_DEFAULT_FILES: &[DefaultFile] = &[
    DefaultFile {
        relative: "providers.toml",
        content: PROVIDERS_TOML,
    },
    DefaultFile {
        relative: "roles.toml",
        content: ROLES_TOML,
    },
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
    DefaultFile {
        relative: "branding/logo.txt",
        content: LOGO,
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
    for file in RESETTABLE_DEFAULT_FILES {
        write_if_missing(home.join(file.relative), file.content)?;
    }
    Ok(())
}

pub fn reset_user_defaults(home: &Path, version: &str) -> io::Result<()> {
    std::fs::create_dir_all(home)?;
    for directory in USER_OWNED_DIRECTORIES {
        std::fs::create_dir_all(home.join(directory))?;
    }
    reset_main_config(home.join("config.toml"))?;
    write_owned(
        home.join("request-agents.toml"),
        &default_request_agents(version),
    )?;
    for file in RESETTABLE_DEFAULT_FILES {
        write_owned(home.join(file.relative), file.content)?;
    }
    Ok(())
}

fn reset_main_config(path: PathBuf) -> io::Result<()> {
    let mut document = match std::fs::read_to_string(&path) {
        Ok(source) => source.parse::<toml_edit::DocumentMut>().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to parse {}: {error}", path.display()),
            )
        })?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(error) => return Err(error),
    };

    document["model"] = toml_edit::value(super::runtime_defaults::DEFAULT_NEW_SESSION_MODEL);
    document["context"] = toml_edit::value("default");
    document["request_agent"] = toml_edit::value("atelier");
    write_owned(path, &document.to_string())
}

fn default_config(_version: &str) -> String {
    format!(
        "model = {:?}\ncontext = \"default\"\nrequest_agent = \"atelier\"\n",
        super::runtime_defaults::DEFAULT_NEW_SESSION_MODEL
    )
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
