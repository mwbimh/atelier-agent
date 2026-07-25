//! Runtime selection for editable context, branding, and request-agent presets.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

static RUNTIME_HOME: OnceLock<PathBuf> = OnceLock::new();
static RUNTIME_CONTEXT_DIR: OnceLock<PathBuf> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextPrompt {
    Main,
    Subagent,
    ApplyPatch,
    GoalPlanner,
    GoalStrategist,
    GoalSkeptic,
    GoalSummary,
    CompactionDeveloper,
    CompactionUser,
}

impl ContextPrompt {
    pub const ALL: [Self; 9] = [
        Self::Main,
        Self::Subagent,
        Self::ApplyPatch,
        Self::GoalPlanner,
        Self::GoalStrategist,
        Self::GoalSkeptic,
        Self::GoalSummary,
        Self::CompactionDeveloper,
        Self::CompactionUser,
    ];

    pub const fn relative_path(self) -> &'static str {
        match self {
            Self::Main => "main.md",
            Self::Subagent => "subagent.md",
            Self::ApplyPatch => "apply_patch.md",
            Self::GoalPlanner => "goal/planner.md",
            Self::GoalStrategist => "goal/strategist.md",
            Self::GoalSkeptic => "goal/skeptic.md",
            Self::GoalSummary => "goal/summary.md",
            Self::CompactionDeveloper => "compaction/developer.md",
            Self::CompactionUser => "compaction/user.md",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RequestAgentIdentity {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
}

impl RequestAgentIdentity {
    pub fn user_agent_value(&self) -> String {
        match self.version.as_deref() {
            Some(version) => format!("{}/{}", self.name, version),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeDefaults {
    pub model: Option<String>,
    pub context: String,
    pub context_dir: PathBuf,
    pub request_agent: RequestAgentIdentity,
}

#[derive(Deserialize)]
struct MainConfig {
    #[serde(default)]
    model: Option<String>,
    context: String,
    request_agent: String,
}

#[derive(Deserialize)]
struct RequestAgentsFile {
    schema_version: u32,
    agents: BTreeMap<String, RequestAgentEntry>,
}

#[derive(Deserialize)]
struct RequestAgentEntry {
    name: String,
    #[serde(default)]
    version: Option<String>,
}

pub fn resolve_runtime_defaults_at(home: &Path) -> io::Result<RuntimeDefaults> {
    let main: MainConfig = parse_toml(&home.join("config.toml"))?;
    let model = main.model;
    if let Some(model) = model.as_deref() {
        validate_model_key(model)?;
    }
    validate_component("context preset", &main.context)?;
    let context_dir = home.join("contexts").join(&main.context);
    if !context_dir.is_dir() {
        return Err(invalid_data(format!(
            "context preset '{}' does not exist at {}",
            main.context,
            context_dir.display()
        )));
    }

    let request_agents_path = home.join("request-agents.toml");
    let request_agents: RequestAgentsFile = parse_toml(&request_agents_path)?;
    if request_agents.schema_version != 1 {
        return Err(invalid_data(format!(
            "unsupported request-agents.toml schema_version {}",
            request_agents.schema_version
        )));
    }
    let selected = request_agents
        .agents
        .get(&main.request_agent)
        .ok_or_else(|| {
            invalid_data(format!(
                "request agent '{}' is not defined in {}",
                main.request_agent,
                request_agents_path.display()
            ))
        })?;
    if selected.name.trim().is_empty() {
        return Err(invalid_data(format!(
            "request agent '{}' has an empty name in {}",
            main.request_agent,
            request_agents_path.display()
        )));
    }

    Ok(RuntimeDefaults {
        model,
        context: main.context,
        context_dir,
        request_agent: RequestAgentIdentity {
            id: main.request_agent,
            name: selected.name.clone(),
            version: selected.version.clone(),
        },
    })
}

pub fn update_default_model_at(home: &Path, model: &str) -> io::Result<RuntimeDefaults> {
    validate_model_key(model)?;
    let path = home.join("config.toml");
    let source = std::fs::read_to_string(&path)?;
    let mut document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| invalid_data(format!("failed to parse {}: {error}", path.display())))?;
    document["model"] = toml_edit::value(model);
    super::fs_atomic::write_atomically(&path, &document.to_string(), None)?;
    resolve_runtime_defaults_at(home)
}

pub fn install_runtime_defaults_at(home: &Path) -> io::Result<RuntimeDefaults> {
    let resolved = resolve_runtime_defaults_at(home)?;
    for kind in ContextPrompt::ALL {
        load_context_prompt_at(home, &resolved.context, kind)?;
    }
    install_once(&RUNTIME_HOME, home.to_path_buf(), "Atelier home")?;
    install_once(
        &RUNTIME_CONTEXT_DIR,
        resolved.context_dir.clone(),
        "context preset",
    )?;
    Ok(resolved)
}

pub fn load_context_prompt_at(
    home: &Path,
    preset: &str,
    kind: ContextPrompt,
) -> io::Result<String> {
    validate_component("context preset", preset)?;
    std::fs::read_to_string(
        home.join("contexts")
            .join(preset)
            .join(kind.relative_path()),
    )
}

pub fn runtime_context_prompt(kind: ContextPrompt, embedded_default: &str) -> String {
    let Some(directory) = RUNTIME_CONTEXT_DIR.get() else {
        return embedded_default.to_owned();
    };
    let path = directory.join(kind.relative_path());
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to load context prompt {}: {error}", path.display()))
}

pub fn load_logo_at(home: &Path) -> io::Result<String> {
    std::fs::read_to_string(home.join("branding/logo.txt"))
}

pub fn runtime_logo(embedded_default: &str) -> String {
    let Some(home) = RUNTIME_HOME.get() else {
        return embedded_default.to_owned();
    };
    let path = home.join("branding/logo.txt");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to load branding logo {}: {error}", path.display()))
}

fn parse_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<T> {
    let content = std::fs::read_to_string(path)?;
    toml::from_str(&content)
        .map_err(|error| invalid_data(format!("failed to parse {}: {error}", path.display())))
}

fn validate_component(label: &str, value: &str) -> io::Result<()> {
    let path = Path::new(value);
    let valid = !value.is_empty()
        && path.components().count() == 1
        && matches!(path.components().next(), Some(Component::Normal(_)));
    if valid {
        Ok(())
    } else {
        Err(invalid_data(format!("invalid {label}: {value:?}")))
    }
}

fn validate_model_key(value: &str) -> io::Result<()> {
    let Some((provider, model)) = value.split_once('/') else {
        return Err(invalid_data(format!(
            "model must use provider/model format, got {value:?}"
        )));
    };
    if provider.is_empty() || model.is_empty() || model.contains('/') {
        return Err(invalid_data(format!(
            "model must use provider/model format, got {value:?}"
        )));
    }
    validate_component("model provider", provider)
}

fn install_once(lock: &OnceLock<PathBuf>, value: PathBuf, label: &str) -> io::Result<()> {
    if let Some(existing) = lock.get() {
        if existing == &value {
            return Ok(());
        }
        return Err(invalid_data(format!(
            "runtime {label} was already initialized as {}",
            existing.display()
        )));
    }
    lock.set(value)
        .map_err(|_| invalid_data(format!("runtime {label} initialization raced")))
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
