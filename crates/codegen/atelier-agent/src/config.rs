//! Agent definition types — parsed from `.atelier/agents/*.md` files.
#[cfg(test)]
use crate::error::AgentBuildError;
use crate::prompt::context::TemplateOverride;
use crate::prompt::user_message::UserMessageTemplate;
use atelier_tools::implementations::atelier_build;
use atelier_tools::implementations::atelier_build_concise;
use atelier_tools::implementations::codex;
use atelier_tools::implementations::memory;
use atelier_tools::implementations::opencode;
use atelier_tools::implementations::search_tool;
use atelier_tools::implementations::use_tool;
use atelier_tools::registry::types::{ToolConfig, ToolServerConfig};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use strum::{AsRefStr, Display, EnumIter, EnumString, IntoStaticStr};
/// Process-global registry of externally-provided toolset presets.
///
/// # Visibility
/// Each preset is registered as either **public** or **internal**:
/// - **Public** presets are product presets: they are enumerated by
///   [`preset_names`] / [`all_toolset_presets`] (so they appear in the
///   workspace manifest, preset sets, etc.) *and* resolvable via
///   [`toolset_for_preset`].
/// - **Internal** presets are resolved by name at runtime by the shell /
///   orchestrator spawn path via [`toolset_for_preset`], but are deliberately
///   NOT enumerated, so a harness-internal preset never leaks into public
///   preset enumeration.
///
/// # Ordering contract
/// [`register_toolset_preset`] / [`register_internal_toolset_preset`] MUST run
/// before the first preset resolution in the process. Presets registered later
/// are still visible to subsequent `toolset_for_preset` / `preset_names` /
/// `all_toolset_presets` calls, but any config resolved before registration
/// will not see them.
/// A toolset preset builder: a function producing a [`ToolServerConfig`].
pub type ToolsetPresetBuilder = fn() -> ToolServerConfig;
/// Whether a registered preset is enumerated publicly or resolved by name only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PresetVisibility {
    /// A product preset: enumerated by `preset_names()` / `all_toolset_presets()`.
    Public,
    /// A harness-internal preset: resolvable by name but never enumerated.
    Internal,
}
static TOOLSET_PRESETS: OnceLock<Mutex<HashMap<String, (ToolsetPresetBuilder, PresetVisibility)>>> =
    OnceLock::new();
fn toolset_preset_registry()
-> &'static Mutex<HashMap<String, (ToolsetPresetBuilder, PresetVisibility)>> {
    TOOLSET_PRESETS.get_or_init(|| Mutex::new(HashMap::new()))
}
/// Register an out-of-tree **public** (product) toolset preset by name. Public
/// presets are enumerated by [`preset_names`] / [`all_toolset_presets`] and
/// resolvable via [`toolset_for_preset`]. See [`TOOLSET_PRESETS`].
pub fn register_toolset_preset(name: &str, builder: ToolsetPresetBuilder) {
    toolset_preset_registry()
        .lock()
        .expect("toolset preset registry poisoned")
        .insert(name.to_string(), (builder, PresetVisibility::Public));
}
/// Register an out-of-tree **internal** toolset preset by name. Internal presets
/// are resolvable via [`toolset_for_preset`] (the shell / orchestrator spawn
/// path resolves them by name) but are deliberately NOT enumerated by
/// [`preset_names`] / [`all_toolset_presets`], so they never leak into public
/// preset enumeration (manifest generation, product preset sets, …). See
/// [`TOOLSET_PRESETS`].
pub fn register_internal_toolset_preset(name: &str, builder: ToolsetPresetBuilder) {
    toolset_preset_registry()
        .lock()
        .expect("toolset preset registry poisoned")
        .insert(name.to_string(), (builder, PresetVisibility::Internal));
}
/// Look up an externally-registered toolset preset by (already-normalized) name.
/// Resolves BOTH public and internal presets.
fn registered_toolset_preset(name: &str) -> Option<ToolServerConfig> {
    toolset_preset_registry()
        .lock()
        .expect("toolset preset registry poisoned")
        .get(name)
        .map(|(f, _)| f())
}
/// Names of externally-registered **public** presets only (internal presets are
/// intentionally excluded from enumeration).
fn registered_public_toolset_preset_names() -> Vec<String> {
    toolset_preset_registry()
        .lock()
        .expect("toolset preset registry poisoned")
        .iter()
        .filter(|(_, (_, visibility))| *visibility == PresetVisibility::Public)
        .map(|(name, _)| name.clone())
        .collect()
}
/// Orchestrator-specific prompt body appended to the standard AtelierBuild
/// system prompt (`prompt.md`). Instructs the GBL model to delegate
/// coding and exploration work to subagents.
const ORCHESTRATOR_PROMPT_BODY: &str = "\
## Orchestrator Mode

You are a technical lead orchestrating a team of senior-engineer subagents. Your subagents \
are highly capable \u{2014} treat them as expert peers, not junior helpers. Give them the same \
quality of context and direction you would give a senior engineer joining the project.

Your job is to think, plan, coordinate, and review. Their job is to explore, implement, \
and execute. Use them aggressively and liberally \u{2014} spawn subagents early and often.

### Your direct responsibilities:
- High-level planning and architecture decisions
- Reading files for quick context (${{ tools.by_kind.read }}, ${{ tools.by_kind.search }}, ${{ tools.by_kind.list }})
- Running quick terminal commands for orientation (${{ tools.by_kind.execute }})
- Invoking skills and MCP tools (${{ tools.by_kind.skill }}, ${{ tools.by_kind.search_tool }}, ${{ tools.by_kind.use_tool }})
- Web research (${{ tools.by_kind.web_search }}, ${{ tools.by_kind.web_fetch }})
- Asking the user questions (${{ tools.by_kind.ask_user }})
- Managing task lists and tracking progress (${{ tools.by_kind.plan }})
- Reviewing subagent results and synthesizing responses for the user

### ALWAYS delegate to subagents:
- **ALL file modifications** \u{2014} creating, editing, deleting files (`general-purpose`)
- **ALL builds, tests, and verification** \u{2014} running test suites, linters, compilers (`general-purpose`)
- **Deep codebase exploration** \u{2014} searching across many files, understanding patterns (`explore`)
- **Multi-step implementation** \u{2014} any task involving more than reading (`general-purpose`)
- **Any research requiring thoroughness** \u{2014} don\u{2019}t do shallow searches yourself, spawn an `explore` subagent

### How to talk to subagents:
Write prompts the way you would brief a senior engineer:
- Explain WHAT you need done and WHY (the context behind the task)
- Share what you already know \u{2014} file paths, function names, architectural decisions
- Describe the end state, not step-by-step commands \u{2014} trust their judgment on HOW
- If you have opinions on approach, share them as guidance, not rigid instructions
- Include acceptance criteria: what does \"done\" look like?

### Parallelism:
- Break independent tasks into separate subagents and run them in parallel
- Use `explore` subagents to investigate multiple areas simultaneously
- Launch implementation subagents for independent files/modules at the same time
- Do NOT wait for one subagent before spawning others that don\u{2019}t depend on it

### Anti-patterns to avoid:
- Do NOT do shallow 1-2 file reads yourself when an `explore` agent would be more thorough
- Do NOT implement code changes yourself \u{2014} you have no file editing tools
- Do NOT give subagents overly prescriptive step-by-step instructions \u{2014} trust their expertise
- Do NOT summarize or re-explain what the user said \u{2014} get to work immediately";
/// Bash tool with clearer model-facing names:
/// `run_terminal_cmd` → `run_terminal_command`, `is_background` → `background`.
fn bash_tool_config() -> ToolConfig {
    ToolConfig::from(&atelier_build::BashTool)
        .with_name("run_terminal_command")
        .with_param_rename("is_background", "background")
}
/// Task/subagent tool with clearer model-facing names:
/// `task` → `spawn_subagent`, `run_in_background` → `background`.
fn task_tool_config() -> ToolConfig {
    ToolConfig::from(&atelier_build::TaskTool)
        .with_name("spawn_subagent")
        .with_param_rename("run_in_background", "background")
}
/// Task output tool renamed for clarity:
/// `get_task_output` → `get_command_or_subagent_output`.
fn task_output_tool_config() -> ToolConfig {
    ToolConfig::from(&atelier_build::TaskOutputTool).with_name("get_command_or_subagent_output")
}
/// `wait_tasks` → `wait_commands_or_subagents`.
fn wait_tasks_tool_config() -> ToolConfig {
    ToolConfig::from(&atelier_build::WaitTasksTool).with_name("wait_commands_or_subagents")
}
/// `kill_task` → `kill_command_or_subagent`.
fn kill_task_tool_config() -> ToolConfig {
    ToolConfig::from(&atelier_build::KillTaskTool).with_name("kill_command_or_subagent")
}
/// Complete workspace-executable toolset for hub registration.
///
/// Extends `default_atelier_build_toolset()` with tools that are dynamically
/// injected by `AgentBuilder::build()` or only available in specific modes.
/// In proxy mode, the workspace server executes ALL tools — the shell has
/// zero local dispatch.
pub fn workspace_atelier_build_toolset() -> ToolServerConfig {
    let mut tools = default_atelier_build_toolset().tools;
    tools.push((&opencode::OpenCodeWriteTool).into());
    tools.push((&atelier_build::EnterPlanModeTool).into());
    tools.push((&atelier_build::ExitPlanModeTool).into());
    tools.push((&atelier_build::AskUserQuestionTool).into());
    tools.push((&atelier_build::WebSearchTool).into());
    tools.push((&atelier_build::WebFetchTool).into());
    tools.push((&memory::search_tool::MemorySearchImpl).into());
    tools.push((&memory::get_tool::MemoryGetImpl).into());
    tools.push((&atelier_build::LspTool).into());
    ToolServerConfig {
        tools,
        behavior_preset: None,
    }
}
/// Toolset for the `atelier-computer` (workspace/sandbox) preset.
fn atelier_computer_toolset() -> ToolServerConfig {
    #[allow(unused_mut)]
    let mut tools = vec![
        bash_tool_config(),
        (&atelier_build::ReadFileTool).into(),
        (&atelier_build::SearchReplaceTool).into(),
        (&opencode::OpenCodeWriteTool).into(),
        (&atelier_build::ListDirTool).into(),
        (&atelier_build::GrepTool).into(),
        (&atelier_build::KillTerminalCommandTool).into(),
        (&atelier_build::GetTerminalCommandOutputTool).into(),
    ];
    ToolServerConfig {
        tools,
        behavior_preset: None,
    }
}
/// Every named toolset preset, as `(normalized_name, config)` pairs.
///
/// Single source of truth: [`toolset_for_preset`] resolves through this
/// table, and the preset-coverage tests iterate it, so a new preset is
/// automatically covered the moment it becomes resolvable.
/// Native (in-crate) toolset presets.
fn native_toolset_presets() -> Vec<(&'static str, ToolServerConfig)> {
    vec![
        ("atelier-build", workspace_atelier_build_toolset()),
        ("atelier-build-concise", atelier_build_concise_toolset()),
        ("atelier-build-plan", atelier_build_plan_toolset()),
        ("codex", codex_toolset()),
        ("explore", explore_toolset()),
        ("plan", plan_toolset()),
        ("atelier-computer", atelier_computer_toolset()),
    ]
}
/// Every named **public** toolset preset (native + externally registered public
/// presets), as `(name, config)` pairs. Harness-internal registered presets are
/// intentionally excluded — resolve them by name via [`toolset_for_preset`].
fn all_toolset_presets() -> Vec<(String, ToolServerConfig)> {
    let mut out: Vec<(String, ToolServerConfig)> = native_toolset_presets()
        .into_iter()
        .map(|(name, cfg)| (name.to_string(), cfg))
        .collect();
    for name in registered_public_toolset_preset_names() {
        if !out.iter().any(|(n, _)| *n == name)
            && let Some(cfg) = registered_toolset_preset(&name)
        {
            out.push((name, cfg));
        }
    }
    out
}
/// Names of every named toolset preset (native first, then registered).
pub fn preset_names() -> Vec<String> {
    all_toolset_presets()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}
/// Resolve a named toolset preset to its [`ToolServerConfig`], or `None` if unknown.
pub fn toolset_for_preset(preset: &str) -> Option<ToolServerConfig> {
    let normalized = preset.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    native_toolset_presets()
        .into_iter()
        .find(|(name, _)| *name == normalized)
        .map(|(_, toolset)| toolset)
        .or_else(|| registered_toolset_preset(&normalized))
}
fn default_atelier_build_toolset() -> ToolServerConfig {
    ToolServerConfig {
        tools: vec![
            bash_tool_config(),
            (&atelier_build::ReadFileTool).into(),
            (&atelier_build::SearchReplaceTool).into(),
            (&atelier_build::ListDirTool).into(),
            (&atelier_build::GrepTool).into(),
            kill_task_tool_config(),
            (&atelier_build::TodoWriteTool).into(),
            task_output_tool_config(),
            wait_tasks_tool_config(),
            task_tool_config(),
            (&atelier_build::SchedulerCreateTool).into(),
            (&atelier_build::SchedulerDeleteTool).into(),
            (&atelier_build::SchedulerListTool).into(),
            (&atelier_build::MonitorTool).into(),
            (&search_tool::SearchTool).into(),
            (&use_tool::UseTool).into(),
            (&atelier_build::UpdateGoalTool).into(),
        ],
        behavior_preset: None,
    }
}
fn atelier_build_concise_toolset() -> ToolServerConfig {
    ToolServerConfig {
        tools: vec![
            (&atelier_build_concise::BashConciseTool).into(),
            (&atelier_build_concise::ReadFileConciseTool).into(),
            (&atelier_build_concise::SearchReplaceConciseTool).into(),
            (&atelier_build::ListDirTool).into(),
            (&atelier_build::GrepTool).into(),
            kill_task_tool_config(),
            (&atelier_build::TodoWriteTool).into(),
            task_output_tool_config(),
            (&atelier_build::SchedulerCreateTool).into(),
            (&atelier_build::SchedulerDeleteTool).into(),
            (&atelier_build::SchedulerListTool).into(),
            (&atelier_build::MonitorTool).into(),
            (&atelier_build::UpdateGoalTool).into(),
        ],
        behavior_preset: None,
    }
}
/// Hashline toolset: anchor-based read/edit/search + standard utilities.
///
/// `hashline_tools` should be the 3 hashline `ToolConfig` entries produced by
/// `FileToolset::Hashline.tool_configs(&hashline_config)` — they carry the
/// scheme parameters as tool params.
pub fn atelier_build_hashline_toolset(
    hashline_tools: Vec<atelier_tools::registry::types::ToolConfig>,
) -> ToolServerConfig {
    let mut tools: Vec<atelier_tools::registry::types::ToolConfig> = vec![bash_tool_config()];
    tools.extend(hashline_tools);
    tools.extend([
        (&atelier_build::ListDirTool).into(),
        kill_task_tool_config(),
        (&atelier_build::TodoWriteTool).into(),
        task_output_tool_config(),
        wait_tasks_tool_config(),
        task_tool_config(),
        (&atelier_build::WebSearchTool).into(),
        (&atelier_build::SchedulerCreateTool).into(),
        (&atelier_build::SchedulerDeleteTool).into(),
        (&atelier_build::SchedulerListTool).into(),
        (&atelier_build::MonitorTool).into(),
        (&search_tool::SearchTool).into(),
        (&use_tool::UseTool).into(),
        (&atelier_build::UpdateGoalTool).into(),
    ]);
    ToolServerConfig {
        tools,
        behavior_preset: None,
    }
}
fn codex_toolset() -> ToolServerConfig {
    ToolServerConfig {
        tools: vec![
            bash_tool_config(),
            (&codex::CodexReadFileTool).into(),
            (&codex::ApplyPatchTool).into(),
            (&codex::CodexListDirTool).into(),
            (&codex::CodexGrepFilesTool).into(),
            kill_task_tool_config(),
            (&atelier_build::TodoWriteTool).into(),
            task_output_tool_config(),
            (&search_tool::SearchTool).into(),
            (&use_tool::UseTool).into(),
        ],
        behavior_preset: None,
    }
}
/// Read-only toolset for the **explore** subagent.
///
/// Genuinely read-only: `read_file` (Read), `list_dir` (Glob), `grep` (Grep).
/// `run_terminal_command` (Bash) is intentionally omitted so exploration cannot
/// mutate the workspace — the read-only guarantee is enforced by the toolset,
/// not merely by the prompt. With no `BashTool`, the background-task helpers
/// (`KillTaskTool`/`TaskOutputTool`) are unnecessary and also omitted.
fn explore_toolset() -> ToolServerConfig {
    ToolServerConfig {
        tools: vec![
            (&atelier_build::ReadFileTool).into(),
            (&atelier_build::ListDirTool).into(),
            (&atelier_build::GrepTool).into(),
        ],
        behavior_preset: None,
    }
}
/// Plan-mode toolset — read-only inspection tools, no shell, no file-editing.
///
/// Enforces read-only at the toolset: the agent may inspect the repo and keep
/// a todo list, but `search_replace` (file edits) and `run_terminal_command`
/// (shell) are both omitted so it cannot mutate the workspace.
fn plan_toolset() -> ToolServerConfig {
    ToolServerConfig {
        tools: vec![
            (&atelier_build::ReadFileTool).into(),
            (&atelier_build::ListDirTool).into(),
            (&atelier_build::GrepTool).into(),
            (&atelier_build::TodoWriteTool).into(),
        ],
        behavior_preset: None,
    }
}
/// Atelier + plan mode toolset.
///
/// Extends the default `atelier-build` toolset with plan mode tools:
/// `enter_plan_mode`, `exit_plan_mode`, and `ask_user_question`.
/// This allows the agent to enter a structured planning phase before
/// writing code, with user-approved plans.
fn atelier_build_plan_toolset() -> ToolServerConfig {
    ToolServerConfig {
        tools: vec![
            bash_tool_config(),
            (&atelier_build::ReadFileTool).into(),
            (&atelier_build::SearchReplaceTool).into(),
            (&atelier_build::ListDirTool).into(),
            (&atelier_build::GrepTool).into(),
            kill_task_tool_config(),
            (&atelier_build::TodoWriteTool).into(),
            task_output_tool_config(),
            task_tool_config(),
            (&atelier_build::SchedulerCreateTool).into(),
            (&atelier_build::SchedulerDeleteTool).into(),
            (&atelier_build::SchedulerListTool).into(),
            (&atelier_build::MonitorTool).into(),
            (&search_tool::SearchTool).into(),
            (&use_tool::UseTool).into(),
            (&atelier_build::UpdateGoalTool).into(),
            (&atelier_build::EnterPlanModeTool).into(),
            (&atelier_build::ExitPlanModeTool).into(),
            (&atelier_build::AskUserQuestionTool).into(),
        ],
        behavior_preset: None,
    }
}
/// Orchestrator toolset: read/search/orchestration tools only.
///
/// No terminal execution, no file editing. The orchestrator delegates
/// all execution and file modification to subagents. Retains read_file,
/// grep, list_dir for research, plus the full subagent/skill/MCP/plan
/// stack for orchestration.
fn orchestrator_toolset() -> ToolServerConfig {
    ToolServerConfig {
        tools: vec![
            bash_tool_config(),
            (&atelier_build::ReadFileTool).into(),
            (&atelier_build::ListDirTool).into(),
            (&atelier_build::GrepTool).into(),
            task_tool_config(),
            task_output_tool_config(),
            wait_tasks_tool_config(),
            kill_task_tool_config(),
            (&search_tool::SearchTool).into(),
            (&use_tool::UseTool).into(),
            (&atelier_build::TodoWriteTool).into(),
            (&atelier_build::EnterPlanModeTool).into(),
            (&atelier_build::ExitPlanModeTool).into(),
            (&atelier_build::AskUserQuestionTool).into(),
            (&atelier_build::UpdateGoalTool).into(),
            (&atelier_build::SchedulerCreateTool).into(),
            (&atelier_build::SchedulerDeleteTool).into(),
            (&atelier_build::SchedulerListTool).into(),
            (&atelier_build::MonitorTool).into(),
            (&atelier_build::WebSearchTool).into(),
            (&atelier_build::WebFetchTool).into(),
            (&memory::MemorySearchImpl).into(),
            (&memory::MemoryGetImpl).into(),
        ],
        behavior_preset: None,
    }
}
/// Atelier + plan mode toolset WITHOUT subagent tools.
///
/// Same as `atelier_build_plan_toolset` but excludes `TaskTool`,
/// `TaskOutputTool`, and `KillTaskTool`. Use this when the shell
/// does not have subagent infrastructure wired up.
fn atelier_build_plan_no_subagents_toolset() -> ToolServerConfig {
    ToolServerConfig {
        tools: vec![
            bash_tool_config(),
            (&atelier_build::ReadFileTool).into(),
            (&atelier_build::SearchReplaceTool).into(),
            (&atelier_build::ListDirTool).into(),
            (&atelier_build::GrepTool).into(),
            kill_task_tool_config(),
            (&atelier_build::TodoWriteTool).into(),
            task_output_tool_config(),
            (&atelier_build::SchedulerCreateTool).into(),
            (&atelier_build::SchedulerDeleteTool).into(),
            (&atelier_build::SchedulerListTool).into(),
            (&atelier_build::MonitorTool).into(),
            (&search_tool::SearchTool).into(),
            (&use_tool::UseTool).into(),
            (&atelier_build::UpdateGoalTool).into(),
            (&atelier_build::EnterPlanModeTool).into(),
            (&atelier_build::ExitPlanModeTool).into(),
            (&atelier_build::AskUserQuestionTool).into(),
        ],
        behavior_preset: None,
    }
}
/// Default Atelier toolset + `ask_user_question`.
///
/// Same as `default_atelier_build_toolset` with the `AskUserQuestionTool` added,
/// allowing the agent to ask structured questions without full plan mode.
fn atelier_build_ask_user_toolset() -> ToolServerConfig {
    ToolServerConfig {
        tools: vec![
            bash_tool_config(),
            (&atelier_build::ReadFileTool).into(),
            (&atelier_build::SearchReplaceTool).into(),
            (&atelier_build::ListDirTool).into(),
            (&atelier_build::GrepTool).into(),
            kill_task_tool_config(),
            (&atelier_build::TodoWriteTool).into(),
            task_output_tool_config(),
            wait_tasks_tool_config(),
            task_tool_config(),
            (&atelier_build::SchedulerCreateTool).into(),
            (&atelier_build::SchedulerDeleteTool).into(),
            (&atelier_build::SchedulerListTool).into(),
            (&atelier_build::MonitorTool).into(),
            (&search_tool::SearchTool).into(),
            (&use_tool::UseTool).into(),
            (&atelier_build::UpdateGoalTool).into(),
            (&atelier_build::AskUserQuestionTool).into(),
        ],
        behavior_preset: None,
    }
}
fn opencode_toolset() -> ToolServerConfig {
    ToolServerConfig {
        tools: vec![
            (&opencode::OpenCodeBashTool).into(),
            (&opencode::OpenCodeReadTool).into(),
            (&opencode::OpenCodeEditTool).into(),
            (&opencode::OpenCodeWriteTool).into(),
            (&opencode::OpenCodeGrepTool).into(),
            (&opencode::OpenCodeGlobTool).into(),
            (&opencode::OpenCodeTodoWriteTool).into(),
            (&opencode::OpenCodeSkillTool).into(),
            kill_task_tool_config(),
            task_output_tool_config(),
        ],
        behavior_preset: None,
    }
}
/// Model override for an agent definition.
///
/// Two states:
/// - `Inherit` — use the parent session's model (default).
/// - `Override(String)` — use a specific model ID, resolved against
///   available models at subagent spawn time.
///
/// In YAML frontmatter / JSON:
/// - `model: inherit` or omitted → `Inherit`
/// - `model: atelier-3-fast` → `Override("atelier-3-fast")`
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModelOverride {
    /// Use the parent session's model.
    #[default]
    Inherit,
    /// Use a specific model ID.
    Override(String),
}
impl std::fmt::Display for ModelOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inherit => f.write_str("inherit"),
            Self::Override(id) => f.write_str(id),
        }
    }
}
impl<'de> Deserialize<'de> for ModelOverride {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let opt = Option::<String>::deserialize(deserializer)?;
        match opt {
            None => Ok(Self::Inherit),
            Some(s) if s.is_empty() || s.eq_ignore_ascii_case("inherit") => Ok(Self::Inherit),
            Some(s) => Ok(Self::Override(s)),
        }
    }
}
impl serde::Serialize for ModelOverride {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Inherit => serializer.serialize_str("inherit"),
            Self::Override(id) => serializer.serialize_str(id),
        }
    }
}
const AGENT_TASK_KEYWORDS: &str = "Agent|Task";
/// Splits `"Agent(a, b), read_file"` → `["Agent(a, b)", "read_file"]`.
pub static AGENT_TASK_TOKENIZER_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!(r"(?i:{AGENT_TASK_KEYWORDS})\([^)]*\)|[^,]+"))
            .expect("valid regex")
    });
/// Matches `"Agent(a, b)"` and captures `"a, b"` in group 1. `None` for bare `Agent`.
pub static AGENT_TASK_CLASSIFIER_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!(r"^(?i:{AGENT_TASK_KEYWORDS})(?:\(([^)]*)\))?$"))
            .expect("valid regex")
    });
/// Accepts `"a, b, c"` or `["a", "b"]`. Trims whitespace.
fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;
    struct StringOrVec;
    impl<'de> de::Visitor<'de> for StringOrVec {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a comma-separated string or an array of strings")
        }
        fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
            Ok(AGENT_TASK_TOKENIZER_RE
                .find_iter(s)
                .map(|m| m.as_str().trim().to_string())
                .filter(|s| !s.is_empty())
                .collect())
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut v = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                v.push(item);
            }
            Ok(v)
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }
    deserializer.deserialize_any(StringOrVec)
}
/// Accepts a positive u32 or null/absent. Rejects 0.
fn deserialize_nonzero_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<u32>::deserialize(deserializer)?;
    if let Some(0) = opt {
        return Err(serde::de::Error::custom("maxTurns must be greater than 0"));
    }
    Ok(opt)
}
/// All built-in agent names as a typed enum.
///
/// Eliminates string matching in discovery and ensures built-in names
/// are defined in exactly one place. The enum covers all built-in
/// agents for centralized name management and `by_name()` dispatch.
///
/// `subagent_variants()` returns only the 3 that are exposed to the LLM
/// via the `TaskTool` description. The remaining 6 are top-level agent
/// profiles resolvable by name but not advertised as subagent types.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, EnumIter, AsRefStr, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum BuiltinAgentName {
    AtelierBuild,
    AtelierBuildConcise,
    AtelierBuildPlan,
    AtelierBuildPlanNoSubagents,
    AtelierBuildAskUser,
    Codex,
    Opencode,
    GeneralPurpose,
    Explore,
    Plan,
    BrowserUse,
    #[strum(serialize = "atelier-build-orchestrator")]
    AtelierBuildOrchestrator,
}
/// Strict-harness predicate by name. Resolves via `BuiltinAgentName` and
/// delegates to [`AgentDefinition::is_strict_harness`]; unknown names
/// return `false` (conservative — never enforce a harness we can't verify).
/// Callers that already hold an `AgentDefinition` should call that method
/// directly so project-level shadowing is honored.
pub fn is_strict_harness_agent_type(name: &str) -> bool {
    use std::str::FromStr;
    BuiltinAgentName::from_str(name)
        .map(|b| b.definition().is_strict_harness())
        .unwrap_or(false)
}
impl BuiltinAgentName {
    /// Build the `AgentDefinition` for this built-in agent.
    pub fn definition(self) -> AgentDefinition {
        match self {
            Self::AtelierBuild => AgentDefinition::default_atelier_build(),
            Self::AtelierBuildConcise => AgentDefinition::atelier_build_concise(),
            Self::AtelierBuildPlan => AgentDefinition::atelier_build_plan(),
            Self::AtelierBuildPlanNoSubagents => AgentDefinition::atelier_build_plan_no_subagents(),
            Self::AtelierBuildAskUser => AgentDefinition::atelier_build_ask_user(),
            Self::Codex => AgentDefinition::codex(),
            Self::Opencode => AgentDefinition::opencode(),
            Self::GeneralPurpose => AgentDefinition::general_purpose(),
            Self::Explore => AgentDefinition::explore(),
            Self::Plan => AgentDefinition::plan(),
            Self::BrowserUse => AgentDefinition::browser_use(),
            Self::AtelierBuildOrchestrator => AgentDefinition::atelier_build_orchestrator(),
        }
    }
    /// Built-in agents available as subagents via the Task tool.
    pub fn subagent_variants() -> &'static [Self] {
        &[Self::GeneralPurpose, Self::Explore, Self::Plan]
    }
}
/// Compile-time Agent harness definition used by primary and Subagent sessions.
///
/// Session-level policies such as compaction and system reminders are applied
/// by `AgentBuilder` at build time.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    #[serde(default = "default_prompt_mode")]
    pub prompt_mode: PromptMode,
    #[serde(default = "default_atelier_build_toolset")]
    pub tool_config: ToolServerConfig,
    /// Runtime capability mode that constrains which tool kinds the agent
    /// can use. Applied during subagent spawn in `handle_subagent_request`
    /// by filtering the definition's `tool_config` before session creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_mode: Option<atelier_tool_types::SubagentCapabilityMode>,
    #[serde(default)]
    pub permission_mode: PermissionMode,
    #[serde(default)]
    pub skills: Vec<String>,
    /// When true (the default), the AgentBuilder discovers skills from CWD
    /// at build time and seeds mid-session skill discovery. When false,
    /// skill discovery is suppressed and the agent gets an empty skill
    /// list with no CWD-based runtime discovery.
    #[serde(default = "default_true")]
    pub discover_skills: bool,
    /// Whether to inherit the parent session's discovered skills when
    /// spawned as a subagent. Ignored for primary sessions.
    #[serde(default = "default_true")]
    pub inherit_skills: bool,
    #[serde(default = "default_true")]
    pub agents_md: bool,
    /// When true (the default), the AgentBuilder layers session-level optional
    /// tools on top of the agent's declared `tool_config`: memory_search/get,
    /// web_search, web_fetch, lsp, image_gen, video_gen, OpenCode write
    /// fallback, and the plan-mode tools.
    ///
    /// Set this to `false` for harnesses that need an exact, minimal toolset
    /// (e.g. the compat harness, where every advertised tool must match the
    /// model's trained schema). The agent's `tool_config` is then used
    /// verbatim with only the subagent strip applied.
    #[serde(default = "default_true")]
    pub inject_default_tools: bool,
    /// Tool allowlist. Empty = inherit all. Also carries `Agent(type)` directives.
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub tools: Vec<String>,
    /// Tool denylist. `Agent(type)` entries strip spawn permissions.
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub disallowed_tools: Vec<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default, deserialize_with = "deserialize_nonzero_u32")]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub isolation: Option<IsolationMode>,
    #[serde(default)]
    pub background: Option<bool>,
    #[serde(default)]
    pub color: Option<AgentColor>,
    #[serde(default)]
    pub initial_prompt: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerRef>,
    #[serde(default)]
    pub mcp_inheritance: McpInheritance,
    #[serde(default, deserialize_with = "deserialize_hooks_map")]
    pub hooks: Option<HooksConfig>,
    #[serde(default)]
    pub memory: Option<MemoryScope>,
    #[serde(default)]
    pub model: ModelOverride,
    /// Completion requirement — declares that this agent must call a
    /// specific tool before the turn ends.
    #[serde(default)]
    pub completion_requirement: Option<CompletionRequirement>,
    /// Subagent types this agent can spawn (derived by builder from `tools`).
    /// `None` = unrestricted, `Some([t1])` = restricted, `Some([])` = blocked.
    #[serde(skip)]
    pub allowed_subagent_types: Option<Vec<String>>,
    /// Session-operator tool restrictions (`--tools` / `--disallowed-tools`),
    /// distinct from the agent author's own `tools`/`disallowed_tools`. The
    /// builder applies them as a final clamp over the fully-assembled toolset
    /// (function + hosted), so they bind regardless of later `tool_config`
    /// mutations and compose with the agent's own filters by intersection.
    /// `None` = no session restriction.
    #[serde(skip)]
    pub session_tools_allowlist: Option<Vec<String>>,
    #[serde(skip)]
    pub session_tools_denylist: Option<Vec<String>>,
    #[serde(skip)]
    pub prompt_body: Option<String>,
    #[serde(skip)]
    pub system_prompt: TemplateOverride,
    /// First-user-message template selector. `Default` (the default) lets
    /// the shell layer build the legacy `<user_info>` + `<git_status>`
    /// prefix; `Custom` uses a caller-supplied template string.
    #[serde(default)]
    pub user_message_template: UserMessageTemplate,
}
/// Declares that the agent must call a specific tool before the turn ends.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionRequirement {
    /// Canonical tool name that must be called.
    pub tool: String,
    /// Reminder text injected when the tool hasn't been called.
    pub reminder: String,
    /// Suggested recovery policy for the harness.
    #[serde(default)]
    pub recovery: Option<RecoveryPolicy>,
}
/// Suggested turn-level recovery policy.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryPolicy {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}
/// Per-tool execution config.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecConfig {
    /// Retry config for this tool. None = no retry (execute once).
    #[serde(default)]
    pub retry: Option<ToolRetryConfig>,
}
/// Retry configuration for a single tool.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRetryConfig {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}
/// How the Markdown body interacts with the base prompt template.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptMode {
    /// Body is appended to the base template (tool conventions,
    /// formatting rules, user_info). Default.
    #[default]
    Extend,
    /// Body IS the complete system prompt. No base template.
    Full,
}
fn default_prompt_mode() -> PromptMode {
    PromptMode::Extend
}
/// Controls which parent MCP servers a subagent inherits.
///
/// Deserializes from:
/// - `"all"` / `"none"` (string, case-insensitive)
/// - `{ "named": ["slack", "github"] }` / `{ "except": ["internal"] }` (map)
///
/// The custom `Deserialize` is needed because `serde_yaml` 0.9 uses YAML
/// tags (`!named`) for externally-tagged enum data variants, but agent
/// definition frontmatter uses the mapping style that JSON also expects.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum McpInheritance {
    #[default]
    All,
    None,
    Named(Vec<String>),
    Except(Vec<String>),
}
impl<'de> Deserialize<'de> for McpInheritance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;
        struct McpInheritanceVisitor;
        impl<'de> de::Visitor<'de> for McpInheritanceVisitor {
            type Value = McpInheritance;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(r#""all", "none", {"named": [...]}, or {"except": [...]}"#)
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
                match s.to_ascii_lowercase().as_str() {
                    "all" => Ok(McpInheritance::All),
                    "none" => Ok(McpInheritance::None),
                    other => Err(de::Error::unknown_variant(other, &["all", "none"])),
                }
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let key: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::invalid_length(0, &"a single-key map"))?;
                let value: Vec<String> = map.next_value()?;
                if map.next_key::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::custom(
                        "mcpInheritance map must have exactly one key",
                    ));
                }
                match key.as_str() {
                    "named" => Ok(McpInheritance::Named(value)),
                    "except" => Ok(McpInheritance::Except(value)),
                    other => Err(de::Error::unknown_variant(other, &["named", "except"])),
                }
            }
        }
        deserializer.deserialize_any(McpInheritanceVisitor)
    }
}
/// Permission mode. Only `BypassPermissions` is wired at spawn; others are forward-compat.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, serde::Serialize, strum::EnumCount)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    Default,
    AcceptEdits,
    /// Background classifier reviews tool calls.
    Auto,
    /// Silently deny non-pre-approved tools.
    DontAsk,
    BypassPermissions,
    Plan,
}
impl PermissionMode {
    pub const VALID_VALUES: &[&str] = &[
        "default",
        "acceptEdits",
        "auto",
        "dontAsk",
        "bypassPermissions",
        "plan",
    ];
}
const _: () =
    assert!(PermissionMode::VALID_VALUES.len() == <PermissionMode as strum::EnumCount>::COUNT);
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    serde::Serialize,
    IntoStaticStr,
    strum::EnumCount,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    #[strum(serialize = "xhigh")]
    XHigh,
    Max,
}
impl Effort {
    pub const VALID_VALUES: &[&str] = &["low", "medium", "high", "xhigh", "max"];
}
const _: () = assert!(Effort::VALID_VALUES.len() == <Effort as strum::EnumCount>::COUNT);
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    serde::Serialize,
    IntoStaticStr,
    strum::EnumCount,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum IsolationMode {
    None,
    Worktree,
}
impl IsolationMode {
    pub const VALID_VALUES: &[&str] = &["none", "worktree"];
}
const _: () =
    assert!(IsolationMode::VALID_VALUES.len() == <IsolationMode as strum::EnumCount>::COUNT);
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    serde::Serialize,
    IntoStaticStr,
    strum::EnumCount,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum AgentColor {
    Red,
    Blue,
    Green,
    Yellow,
    Purple,
    Orange,
    Pink,
    Cyan,
}
impl AgentColor {
    pub const VALID_VALUES: &[&str] = &[
        "red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan",
    ];
}
const _: () = assert!(AgentColor::VALID_VALUES.len() == <AgentColor as strum::EnumCount>::COUNT);
/// Agent memory scope. Distinct from `storage::MemoryScope` (global-vs-workspace write target).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    serde::Serialize,
    IntoStaticStr,
    strum::EnumCount,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum MemoryScope {
    /// `~/.atelier/agent-memory/<name>/`
    User,
    /// `<project>/.atelier/agent-memory/<name>/`
    Project,
    /// `<project>/.atelier/agent-memory-local/<name>/`
    Local,
}
impl MemoryScope {
    pub const VALID_VALUES: &[&str] = &["user", "project", "local"];
}
const _: () = assert!(MemoryScope::VALID_VALUES.len() == <MemoryScope as strum::EnumCount>::COUNT);
#[derive(Debug)]
pub struct ResolvedMemoryDir {
    pub path: std::path::PathBuf,
    /// No workspace hash needed (already project-scoped).
    pub is_project_scoped: bool,
}
impl MemoryScope {
    pub fn resolve_dir(self, agent_name: &str, project_cwd: &std::path::Path) -> ResolvedMemoryDir {
        match self {
            Self::User => ResolvedMemoryDir {
                path: atelier_config::atelier_home()
                    .join("agent-memory")
                    .join(agent_name),
                is_project_scoped: false,
            },
            Self::Project => ResolvedMemoryDir {
                path: project_cwd.join(".atelier/agent-memory").join(agent_name),
                is_project_scoped: true,
            },
            Self::Local => ResolvedMemoryDir {
                path: project_cwd
                    .join(".atelier/agent-memory-local")
                    .join(agent_name),
                is_project_scoped: true,
            },
        }
    }
}
/// Hooks config validated as an object at parse time. Semantic parsing deferred to spawn.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HooksConfig(pub serde_json::Map<String, serde_json::Value>);
impl HooksConfig {
    pub fn as_value(&self) -> serde_json::Value {
        serde_json::Value::Object(self.0.clone())
    }
}
fn deserialize_hooks_map<'de, D>(deserializer: D) -> Result<Option<HooksConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<serde_json::Value>::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(serde_json::Value::Object(map)) => Ok(Some(HooksConfig(map))),
        Some(_) => Err(serde::de::Error::custom("hooks must be an object")),
    }
}
/// MCP server reference — typed to catch config errors at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerRef {
    Named(String),
    /// Opaque JSON config resolved to `McpServer` at spawn time.
    Inline {
        name: String,
        config: serde_json::Value,
    },
}
impl<'de> Deserialize<'de> for McpServerRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let val = serde_json::Value::deserialize(deserializer)?;
        match val {
            serde_json::Value::String(s) => Ok(McpServerRef::Named(s)),
            serde_json::Value::Object(obj) if obj.len() == 1 => {
                let (name, config) = obj.into_iter().next().unwrap();
                if !config.is_object() {
                    return Err(serde::de::Error::custom(format!(
                        "mcpServers inline config for '{name}' must be an object"
                    )));
                }
                Ok(McpServerRef::Inline { name, config })
            }
            serde_json::Value::Object(obj) => {
                if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
                    Ok(McpServerRef::Inline {
                        name: name.to_string(),
                        config: serde_json::Value::Object(obj),
                    })
                } else {
                    Err(serde::de::Error::custom(
                        "mcpServers entry must be a string, a {name: config} map, \
                         or an object with a 'name' field",
                    ))
                }
            }
            _ => Err(serde::de::Error::custom(
                "mcpServers entry must be a string or object",
            )),
        }
    }
}
impl serde::Serialize for McpServerRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            McpServerRef::Named(s) => serializer.serialize_str(s),
            McpServerRef::Inline { name, config } => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(name, config)?;
                map.end()
            }
        }
    }
}
/// Bash tool config overrides (agent-definition layer).
///
/// NOTE: Uses `camelCase` for YAML frontmatter. The `AgentBuilder` maps
/// these into `atelier_tools::registry::types::ToolsetConfig.bash`
/// which uses the tools crate's `BashToolConfig` type.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashConfig {
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: f64,
    #[serde(default = "default_output_byte_limit")]
    pub output_byte_limit: usize,
    #[serde(default)]
    pub cmd_prefix: Option<String>,
}
impl Default for BashConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_timeout_secs(),
            output_byte_limit: default_output_byte_limit(),
            cmd_prefix: None,
        }
    }
}
fn default_timeout_secs() -> f64 {
    120.0
}
fn default_output_byte_limit() -> usize {
    200_000
}
fn default_true() -> bool {
    true
}
/// Strip a tool id's `Namespace:` prefix, yielding its short name.
pub(crate) fn short_tool_name(id: &str) -> &str {
    id.rsplit(':').next().unwrap_or(id)
}
/// Whether an allow/deny `entry` refers to tool `id` (by full id or short name).
pub(crate) fn tool_id_eq(entry: &str, id: &str) -> bool {
    entry == id || entry == short_tool_name(id)
}
/// Whether any `list` entry refers to tool `id`.
pub(crate) fn tool_id_matches(list: &[String], id: &str) -> bool {
    list.iter().any(|e| tool_id_eq(e, id))
}
impl AgentDefinition {
    /// Parse frontmatter in unit tests for the built-in definition schema.
    #[cfg(test)]
    fn parse(content: &str) -> Result<Self, AgentBuildError> {
        let trimmed = content.trim_start();
        if !trimmed.starts_with("---") {
            return Err(AgentBuildError::ParseError(
                "missing frontmatter delimiters".to_string(),
            ));
        }
        let after_opening = &trimmed[3..];
        let closing_idx = after_opening.find("\n---").ok_or_else(|| {
            AgentBuildError::ParseError("missing closing frontmatter delimiter".to_string())
        })?;
        let yaml_content = &after_opening[..closing_idx];
        let after_closing = &after_opening[closing_idx + 4..];
        let body_start = after_closing.find('\n').map(|i| i + 1).unwrap_or(0);
        let body = after_closing[body_start..].trim();
        let prompt_body = if body.is_empty() {
            None
        } else {
            Some(body.to_string())
        };
        let mut def: AgentDefinition = serde_yaml::from_str(yaml_content)
            .map_err(|e| AgentBuildError::ParseError(e.to_string()))?;
        def.prompt_body = prompt_body;
        Ok(def)
    }
}
impl AgentDefinition {
    /// Whether `id` passes the session-operator clamp: denylist wins, then an
    /// unset allowlist allows all.
    pub(crate) fn session_tools_allowed(&self, id: &str) -> bool {
        if self
            .session_tools_denylist
            .as_deref()
            .is_some_and(|d| tool_id_matches(d, id))
        {
            return false;
        }
        self.session_tools_allowlist
            .as_deref()
            .is_none_or(|a| tool_id_matches(a, id))
    }
    /// Whether a hosted/server-side tool `id` survives the agent's own
    /// `disallowed_tools`/`tools` and the session clamp. Hosted tools aren't in
    /// `tool_config`, so they're gated by name here — and strictly (no
    /// compat-name mapping or unresolved-entry fallback like the function path).
    pub(crate) fn hosted_tool_allowed(&self, id: &str) -> bool {
        if tool_id_matches(&self.disallowed_tools, id) {
            return false;
        }
        if id == "x_search" && !tool_id_matches(&self.tools, id) {
            return false;
        }
        if !self.tools.is_empty() && !tool_id_matches(&self.tools, id) {
            return false;
        }
        self.session_tools_allowed(id)
    }
    /// Replace the file-operation tools (read/edit/search) in the tool config
    /// with the given set. Used by the shell layer to swap from standard to
    /// hashline toolset based on `config.toml` / remote settings.
    /// True iff the active system prompt template for `audience` carries
    /// the `<task_completion_discipline>` block.
    ///
    /// Used by the runtime turn-end TodoGate to gate firing on sessions
    /// whose prompt actually references the rules the gate's reminder
    /// text invokes. The block has been removed from every built-in
    /// template, so this returns `false` unconditionally. Kept as a
    /// helper so the gate's call-site stays stable in case the block
    /// is reintroduced behind a future flag.
    pub fn carries_task_completion_discipline(
        &self,
        _audience: crate::prompt::context::PromptAudience,
    ) -> bool {
        false
    }
    /// True iff this agent's wire format is non-interchangeable with the
    /// stock harness, so a client-supplied `_meta.agentProfile` must NOT
    /// override it. Strict iff any of: bespoke `system_prompt` template,
    /// bespoke `user_message_template`, or curated toolset
    /// (`!inject_default_tools`). Stock `atelier-build*` agents leave all
    /// three at defaults and are non-strict.
    pub fn is_strict_harness(&self) -> bool {
        use crate::prompt::context::TemplateOverride;
        use crate::prompt::user_message::UserMessageTemplate;
        let prompt_is_custom = !matches!(self.system_prompt, TemplateOverride::None);
        let user_template_is_custom =
            !matches!(self.user_message_template, UserMessageTemplate::Default);
        let toolset_is_curated = !self.inject_default_tools;
        prompt_is_custom || user_template_is_custom || toolset_is_curated
    }
    /// Swap the definition's file tools for the equivalents in `file_tools`
    /// (hashline vs standard), slot by slot — never granting a slot the
    /// definition doesn't already have (read-only toolsets stay read-only).
    pub fn override_file_tools(
        &mut self,
        file_tools: Vec<atelier_tools::registry::types::ToolConfig>,
    ) {
        const FILE_TOOL_SLOTS: &[[&str; 2]] = &[
            [
                "AtelierBuild:read_file",
                "AtelierBuildHashline:hashline_read",
            ],
            [
                "AtelierBuild:search_replace",
                "AtelierBuildHashline:hashline_edit",
            ],
            ["AtelierBuild:grep", "AtelierBuildHashline:hashline_grep"],
        ];
        for tool in self.tool_config.tools.iter_mut() {
            let Some(slot) = FILE_TOOL_SLOTS
                .iter()
                .find(|slot| slot.contains(&tool.id.as_str()))
            else {
                continue;
            };
            if let Some(replacement) = file_tools.iter().find(|ft| slot.contains(&ft.id.as_str())) {
                *tool = replacement.clone();
            }
        }
    }
    /// Shared defaults for built-in constructors.
    fn base(name: BuiltinAgentName, description: &str) -> Self {
        Self::builtin_defaults(name.as_ref(), description)
    }
    /// Shared defaults for out-of-tree built-in agent registrations.
    pub fn builtin_defaults(name: &str, description: &str) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_string(),
            prompt_mode: PromptMode::Extend,
            tool_config: default_atelier_build_toolset(),
            capability_mode: None,
            permission_mode: PermissionMode::Default,
            skills: vec![],
            agents_md: true,
            discover_skills: true,
            inherit_skills: true,
            inject_default_tools: true,
            disallowed_tools: vec![],
            tools: vec![],
            effort: None,
            max_turns: None,
            isolation: None,
            background: None,
            color: None,
            initial_prompt: None,
            mcp_servers: vec![],
            mcp_inheritance: McpInheritance::All,
            hooks: None,
            memory: None,
            allowed_subagent_types: None,
            session_tools_allowlist: None,
            session_tools_denylist: None,
            model: ModelOverride::Inherit,
            completion_requirement: None,
            prompt_body: None,
            system_prompt: TemplateOverride::None,
            user_message_template: UserMessageTemplate::Default,
        }
    }
    pub fn default_atelier_build() -> Self {
        Self::base(
            BuiltinAgentName::AtelierBuild,
            "Atelier agent for software engineering tasks.",
        )
    }
    /// Atelier Concise agent definition — concise output format for SFT/RL.
    pub fn atelier_build_concise() -> Self {
        Self {
            tool_config: atelier_build_concise_toolset(),
            agents_md: false,
            ..Self::base(
                BuiltinAgentName::AtelierBuildConcise,
                "Atelier agent with concise output format.",
            )
        }
    }
    /// Atelier agent with plan mode tools.
    pub fn atelier_build_plan() -> Self {
        Self {
            tool_config: atelier_build_plan_toolset(),
            ..Self::base(
                BuiltinAgentName::AtelierBuildPlan,
                "Atelier agent with plan mode support.",
            )
        }
    }
    /// Atelier + plan mode WITHOUT subagent tools.
    pub fn atelier_build_plan_no_subagents() -> Self {
        Self {
            tool_config: atelier_build_plan_no_subagents_toolset(),
            ..Self::base(
                BuiltinAgentName::AtelierBuildPlanNoSubagents,
                "Atelier agent with plan mode (no subagents).",
            )
        }
    }
    /// Default Atelier agent with the `ask_user_question` tool.
    pub fn atelier_build_ask_user() -> Self {
        Self {
            tool_config: atelier_build_ask_user_toolset(),
            ..Self::base(
                BuiltinAgentName::AtelierBuildAskUser,
                "Atelier agent with ask-user-question tool.",
            )
        }
    }
    pub fn codex() -> Self {
        Self {
            tool_config: codex_toolset(),
            system_prompt: TemplateOverride::Codex,
            ..Self::base(BuiltinAgentName::Codex, "Codex toolset and prompt")
        }
    }
    pub fn opencode() -> Self {
        Self {
            tool_config: opencode_toolset(),
            ..Self::base(
                BuiltinAgentName::Opencode,
                "OpenCode toolset — opencode-style tools and parameter conventions",
            )
        }
    }
    /// General-purpose subagent definition.
    pub fn general_purpose() -> Self {
        use crate::prompt::subagent_prompts;
        Self {
            description: atelier_tool_types::GENERAL_PURPOSE_SUBAGENT
                .description
                .to_string(),
            prompt_body: Some(subagent_prompts::GENERAL_PURPOSE_PROMPT.to_string()),
            ..Self::base(BuiltinAgentName::GeneralPurpose, "")
        }
    }
    /// Explore subagent — fast, read-only codebase exploration.
    pub fn explore() -> Self {
        use crate::prompt::subagent_prompts;
        Self {
            description: atelier_tool_types::EXPLORE_SUBAGENT.description.to_string(),
            tool_config: explore_toolset(),
            permission_mode: PermissionMode::Plan,
            prompt_body: Some(subagent_prompts::EXPLORE_PROMPT.to_string()),
            inherit_skills: false,
            ..Self::base(BuiltinAgentName::Explore, "")
        }
    }
    /// Plan subagent — read-only architect for implementation plans.
    pub fn plan() -> Self {
        use crate::prompt::subagent_prompts;
        Self {
            description: atelier_tool_types::PLAN_SUBAGENT.description.to_string(),
            tool_config: plan_toolset(),
            permission_mode: PermissionMode::Plan,
            prompt_body: Some(subagent_prompts::PLAN_PROMPT.to_string()),
            inherit_skills: false,
            ..Self::base(BuiltinAgentName::Plan, "")
        }
    }
    /// Browser Use agent definition.
    pub fn browser_use() -> Self {
        Self {
            prompt_mode: PromptMode::Full,
            agents_md: false,
            prompt_body: Some(
                "You are a web browsing agent. You can navigate, interact with, and \
                 extract information from web pages. Use the available browsing tools \
                 to complete the user's request."
                    .to_string(),
            ),
            ..Self::base(
                BuiltinAgentName::BrowserUse,
                "Web browsing and interaction agent.",
            )
        }
    }
    /// Atelier Orchestrator — GBL model with full AtelierBuild tools
    /// (skills, MCPs, plan mode) that delegates coding/exploration to
    /// subagents.
    ///
    /// Subagent overrides are applied in `handle_subagent_request`:
    /// general-purpose children get `implementer_toolset()` and explore
    /// children get `explorer_toolset()`, both with the subagent model.
    pub fn atelier_build_orchestrator() -> Self {
        Self {
            tool_config: orchestrator_toolset(),
            inject_default_tools: false,
            prompt_body: Some(ORCHESTRATOR_PROMPT_BODY.to_string()),
            ..Self::base(
                BuiltinAgentName::AtelierBuildOrchestrator,
                "AtelierBuild orchestrator that delegates coding to specialized subagents",
            )
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Native presets only.
    #[test]
    fn toolset_for_preset_resolves_known_names() {
        for name in [
            "atelier-build",
            "atelier_build",
            "atelier-build-concise",
            "atelier-build-plan",
            "codex",
            "explore",
            "plan",
            "atelier-computer",
            "atelier_computer",
        ] {
            assert!(
                toolset_for_preset(name).is_some(),
                "preset `{name}` should resolve"
            );
        }
        assert!(toolset_for_preset("does-not-exist").is_none());
    }
    #[test]
    fn presets_select_distinct_toolsets_by_size() {
        let gb = toolset_for_preset("atelier-build").unwrap();
        let plan = toolset_for_preset("plan").unwrap();
        let explore = toolset_for_preset("explore").unwrap();
        assert!(explore.tools.len() < plan.tools.len());
        assert!(plan.tools.len() < gb.tools.len());
    }
    fn atelier_computer_exclusive_ids() -> Vec<String> {
        #[allow(unused_mut)]
        let mut ids: Vec<String> = vec![
            ToolConfig::from(&atelier_build::GetTerminalCommandOutputTool).id,
            ToolConfig::from(&atelier_build::KillTerminalCommandTool).id,
        ];
        ids
    }
    #[test]
    fn atelier_computer_preset_is_curated_atelier_build_subset() {
        let gc = toolset_for_preset("atelier-computer").unwrap();
        let gb = toolset_for_preset("atelier-build").unwrap();
        let gb_ids: std::collections::HashSet<&str> =
            gb.tools.iter().map(|t| t.id.as_str()).collect();
        let exclusive_ids = atelier_computer_exclusive_ids();
        assert!(!gc.tools.is_empty());
        for t in &gc.tools {
            if exclusive_ids.contains(&t.id) {
                continue;
            }
            assert!(
                gb_ids.contains(t.id.as_str()),
                "atelier-computer tool `{}` must also ship in the atelier-build preset",
                t.id
            );
        }
        assert!(
            gc.tools.len() < gb.tools.len(),
            "atelier-computer should be a curated subset of atelier-build"
        );
    }
    #[test]
    fn atelier_computer_uses_subagent_free_background_task_tools() {
        let gc = toolset_for_preset("atelier-computer").unwrap();
        let ids: std::collections::HashSet<&str> = gc.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(
            ids.contains(
                ToolConfig::from(&atelier_build::GetTerminalCommandOutputTool)
                    .id
                    .as_str()
            )
        );
        assert!(
            ids.contains(
                ToolConfig::from(&atelier_build::KillTerminalCommandTool)
                    .id
                    .as_str()
            )
        );
        assert!(!ids.contains(ToolConfig::from(&atelier_build::TaskOutputTool).id.as_str()));
        assert!(!ids.contains(ToolConfig::from(&atelier_build::KillTaskTool).id.as_str()));
        assert!(!ids.contains(ToolConfig::from(&atelier_build::TaskTool).id.as_str()));
        for t in &gc.tools {
            if t.id == ToolConfig::from(&atelier_build::GetTerminalCommandOutputTool).id
                || t.id == ToolConfig::from(&atelier_build::KillTerminalCommandTool).id
            {
                assert!(t.name_override.is_none(), "tool `{}` must not rename", t.id);
            }
        }
    }
    /// The atelier-computer preset must ship a full-file write tool (legacy
    /// `write_file` parity) — the same OpenCode `write` tool the atelier-build
    /// preset uses. Guards against `search_replace` being the only
    /// file-mutation path, which has no single-tool full-rewrite when the
    /// empty-old_string overwrite guard is enabled.
    #[test]
    fn atelier_computer_preset_includes_write_tool() {
        let gc = toolset_for_preset("atelier-computer").unwrap();
        let write_id = ToolConfig::from(&opencode::OpenCodeWriteTool).id;
        assert!(
            gc.tools.iter().any(|t| t.id == write_id),
            "atelier-computer preset must include the `{write_id}` tool"
        );
    }
    #[test]
    fn atelier_computer_preset_excludes_plan_and_lsp() {
        let gc = toolset_for_preset("atelier-computer").unwrap();
        let gc_ids: std::collections::HashSet<&str> =
            gc.tools.iter().map(|t| t.id.as_str()).collect();
        for excluded in [
            ToolConfig::from(&atelier_build::LspTool).id,
            ToolConfig::from(&atelier_build::EnterPlanModeTool).id,
            ToolConfig::from(&atelier_build::ExitPlanModeTool).id,
        ] {
            assert!(
                !gc_ids.contains(excluded.as_str()),
                "atelier-computer preset must not advertise `{excluded}`"
            );
        }
        let full = workspace_atelier_build_toolset();
        let full_ids: std::collections::HashSet<&str> =
            full.tools.iter().map(|t| t.id.as_str()).collect();
        for present in [
            ToolConfig::from(&atelier_build::LspTool).id,
            ToolConfig::from(&atelier_build::EnterPlanModeTool).id,
            ToolConfig::from(&atelier_build::ExitPlanModeTool).id,
        ] {
            assert!(
                full_ids.contains(present.as_str()),
                "workspace_atelier_build_toolset must ship `{present}`"
            );
        }
    }
    /// Exhaustive match → adding a new `BuiltinAgentName` won't compile
    /// until classified.
    fn expected_strict_harness(name: BuiltinAgentName) -> bool {
        match name {
            BuiltinAgentName::Codex | BuiltinAgentName::AtelierBuildOrchestrator => true,
            BuiltinAgentName::AtelierBuild
            | BuiltinAgentName::AtelierBuildConcise
            | BuiltinAgentName::AtelierBuildPlan
            | BuiltinAgentName::AtelierBuildPlanNoSubagents
            | BuiltinAgentName::AtelierBuildAskUser
            | BuiltinAgentName::GeneralPurpose
            | BuiltinAgentName::Explore
            | BuiltinAgentName::Plan
            | BuiltinAgentName::Opencode
            | BuiltinAgentName::BrowserUse => false,
        }
    }
    /// Invariant: structural `is_strict_harness()` must match the
    /// hand-classified expectation for every built-in variant.
    #[test]
    fn is_strict_harness_matches_structural_classification_for_all_builtins() {
        use strum::IntoEnumIterator;
        for variant in BuiltinAgentName::iter() {
            let structural = variant.definition().is_strict_harness();
            let expected = expected_strict_harness(variant);
            assert_eq!(
                structural, expected,
                "BuiltinAgentName::{variant:?}: structural={structural} but \
                 expected={expected}. Update `expected_strict_harness` if the \
                 change is intentional.",
            );
        }
    }
    #[test]
    fn is_strict_harness_agent_type_classifies_by_name() {
        for strict in ["codex", "atelier-build-orchestrator"] {
            assert!(
                is_strict_harness_agent_type(strict),
                "{strict} should be strict"
            );
        }
        for non_strict in [
            "atelier-build",
            "atelier-build-plan",
            "atelier-build-concise",
            "atelier-build-ask-user",
            "opencode",
            "browser-use",
            "custom-user-agent",
            "",
            "atelier-build-totally-made-up",
        ] {
            assert!(
                !is_strict_harness_agent_type(non_strict),
                "{non_strict} should be non-strict",
            );
        }
    }
    #[test]
    fn test_parse_valid_full_definition() {
        let content = r#"---
name: test-agent
description: A test agent
promptMode: full
tools:
  - read_file
  - grep
permissionMode: plan
agentsMd: false
---

You are a test agent.
"#;
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(def.name, "test-agent");
        assert_eq!(def.description, "A test agent");
        assert_eq!(def.prompt_mode, PromptMode::Full);
        assert_eq!(def.permission_mode, PermissionMode::Plan);
        assert!(!def.agents_md);
        assert_eq!(def.prompt_body.as_deref(), Some("You are a test agent."));
        assert_eq!(
            def.tools,
            vec!["read_file".to_string(), "grep".to_string()],
            "tools allowlist must be parsed from YAML frontmatter"
        );
    }
    #[test]
    fn test_parse_tools_and_disallowed_together() {
        let content = r#"---
name: mixed-tools
description: Both tools and disallowedTools
tools:
  - read_file
  - grep
  - search_replace
  - task
disallowedTools:
  - search_replace
---

Mixed agent.
"#;
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(def.tools.len(), 4, "tools allowlist should have 4 entries");
        assert_eq!(
            def.disallowed_tools,
            vec!["search_replace".to_string()],
            "disallowedTools should have 1 entry"
        );
    }
    #[test]
    fn test_parse_tools_with_agent_parens() {
        let content = r#"---
name: coordinator
description: test
tools: Agent(worker, researcher), Read, Bash
---

Agent.
"#;
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(
            def.tools,
            vec!["Agent(worker, researcher)", "Read", "Bash"],
            "Agent(a, b) must be kept as a single token"
        );
    }
    #[test]
    fn test_parse_tools_comma_separated() {
        let content = r#"---
name: comma-tools
description: Comma-separated tools
tools: read_file, grep, list_dir
disallowedTools: search_replace, write
---

Agent.
"#;
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(
            def.tools,
            vec!["read_file", "grep", "list_dir"],
            "comma-separated tools must parse correctly"
        );
        assert_eq!(
            def.disallowed_tools,
            vec!["search_replace", "write"],
            "comma-separated disallowedTools must parse correctly"
        );
    }
    #[test]
    fn test_parse_tools_case_insensitive_agent() {
        let content = r#"---
name: ci-test
description: test
tools: agent(worker, researcher), Read
---

Agent.
"#;
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(
            def.tools,
            vec!["agent(worker, researcher)", "Read"],
            "lowercase agent(a, b) must be kept as a single token"
        );
    }
    #[test]
    fn test_parse_max_turns_zero_rejected() {
        let content = "---\nname: test\ndescription: Test\nmaxTurns: 0\n---\n";
        let result = AgentDefinition::parse(content);
        assert!(
            result.is_err(),
            "maxTurns: 0 should be rejected at parse time"
        );
    }
    #[test]
    fn test_parse_model_empty_is_inherit() {
        let content = "---\nname: test\ndescription: Test\nmodel: \"\"\n---\n";
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(def.model, ModelOverride::Inherit);
    }
    #[test]
    fn test_parse_model_null_is_inherit() {
        let content = "---\nname: test\ndescription: Test\nmodel: ~\n---\n";
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(def.model, ModelOverride::Inherit);
    }
    #[test]
    fn test_parse_minimal_defaults_none_fields() {
        let content = "---\nname: minimal\ndescription: Test\n---\n";
        let def = AgentDefinition::parse(content).unwrap();
        assert!(def.effort.is_none());
        assert!(def.max_turns.is_none());
        assert!(def.isolation.is_none());
        assert!(def.background.is_none());
        assert!(def.color.is_none());
        assert!(def.initial_prompt.is_none());
        assert!(def.memory.is_none());
        assert!(def.hooks.is_none());
    }
    #[test]
    fn test_model_override_display_shows_id() {
        assert_eq!(ModelOverride::Inherit.to_string(), "inherit");
        assert_eq!(
            ModelOverride::Override("atelier-3-fast".to_string()).to_string(),
            "atelier-3-fast"
        );
    }
    #[test]
    fn test_parse_new_fields() {
        let content = r#"---
name: full-fields
description: All new fields
effort: high
maxTurns: 10
isolation: worktree
background: true
color: blue
initialPrompt: "hello world"
model: atelier-3
---

Agent body.
"#;
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(def.effort, Some(Effort::High));
        assert_eq!(def.max_turns, Some(10));
        assert_eq!(def.isolation, Some(IsolationMode::Worktree));
        assert_eq!(def.background, Some(true));
        assert_eq!(def.color, Some(AgentColor::Blue));
        assert_eq!(def.initial_prompt.as_deref(), Some("hello world"));
        assert_eq!(def.model, ModelOverride::Override("atelier-3".to_string()));
    }
    #[test]
    fn test_parse_minimal_definition() {
        let content = r#"---
name: minimal
description: Minimal agent
---
"#;
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(def.name, "minimal");
        assert_eq!(def.description, "Minimal agent");
        assert_eq!(def.prompt_mode, PromptMode::Extend);
        assert!(def.agents_md);
        assert!(def.prompt_body.is_none());
    }
    #[test]
    fn mcp_server_ref_parse_and_reject() {
        let v: McpServerRef = serde_json::from_value(serde_json::json!("slack")).unwrap();
        assert_eq!(v, McpServerRef::Named("slack".to_string()));
        let v: McpServerRef =
            serde_json::from_value(serde_json::json!({ "s" : { "type" : "stdio" } })).unwrap();
        assert!(matches!(v, McpServerRef::Inline { ref name, .. } if name == "s"));
        let v: McpServerRef =
            serde_json::from_value(serde_json::json!({ "name" : "s", "type" : "stdio" })).unwrap();
        assert!(matches!(v, McpServerRef::Inline { ref name, .. } if name == "s"));
        assert!(
            serde_json::from_value::<McpServerRef>(serde_json::json!({ "type" :
            "stdio" }))
            .is_err()
        );
        assert!(serde_json::from_value::<McpServerRef>(serde_json::json!(42)).is_err());
        assert!(
            serde_json::from_value::<McpServerRef>(serde_json::json!({ "s" : "bad" })).is_err()
        );
    }
    #[test]
    fn memory_scope_resolve_dir() {
        let cwd = std::path::Path::new("/project");
        let user = MemoryScope::User.resolve_dir("a", cwd);
        assert!(user.path.ends_with("agent-memory/a"));
        assert!(!user.is_project_scoped);
        let proj = MemoryScope::Project.resolve_dir("a", cwd);
        assert_eq!(
            proj.path,
            std::path::PathBuf::from("/project/.atelier/agent-memory/a")
        );
        assert!(proj.is_project_scoped);
        let local = MemoryScope::Local.resolve_dir("a", cwd);
        assert_eq!(
            local.path,
            std::path::PathBuf::from("/project/.atelier/agent-memory-local/a")
        );
        assert!(local.is_project_scoped);
    }
    #[test]
    fn all_new_enum_variants_parse() {
        for effort in Effort::VALID_VALUES {
            let c = format!("---\nname: t\ndescription: t\neffort: {effort}\n---\n");
            assert!(
                AgentDefinition::parse(&c).unwrap().effort.is_some(),
                "effort: {effort}"
            );
        }
        for iso in IsolationMode::VALID_VALUES {
            let c = format!("---\nname: t\ndescription: t\nisolation: {iso}\n---\n");
            assert!(
                AgentDefinition::parse(&c).unwrap().isolation.is_some(),
                "isolation: {iso}"
            );
        }
        for color in AgentColor::VALID_VALUES {
            let c = format!("---\nname: t\ndescription: t\ncolor: {color}\n---\n");
            assert!(
                AgentDefinition::parse(&c).unwrap().color.is_some(),
                "color: {color}"
            );
        }
        for memory in MemoryScope::VALID_VALUES {
            let c = format!("---\nname: t\ndescription: t\nmemory: {memory}\n---\n");
            assert!(
                AgentDefinition::parse(&c).unwrap().memory.is_some(),
                "memory: {memory}"
            );
        }
    }
    #[test]
    fn test_parse_missing_name() {
        let content = r#"---
description: No name
---
"#;
        let result = AgentDefinition::parse(content);
        assert!(result.is_err());
        match result.unwrap_err() {
            AgentBuildError::ParseError(msg) => {
                assert!(msg.contains("name"), "Error should mention 'name': {}", msg);
            }
            e => panic!("Expected ParseError, got: {:?}", e),
        }
    }
    #[test]
    fn test_parse_missing_delimiters() {
        let content = "Just some text without frontmatter";
        let result = AgentDefinition::parse(content);
        assert!(result.is_err());
        match result.unwrap_err() {
            AgentBuildError::ParseError(msg) => {
                assert!(msg.contains("delimiter") || msg.contains("frontmatter"));
            }
            e => panic!("Expected ParseError, got: {:?}", e),
        }
    }
    #[test]
    fn test_parse_unknown_fields_ignored() {
        let content = r#"---
name: test
description: Test
hooks:
  PreToolUse:
    - matcher: Bash
      hooks:
        - type: command
          command: echo hi
memory: user
unknownField: value
---
"#;
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(def.name, "test");
    }
    #[test]
    fn test_parse_completion_requirement() {
        let content = r#"---
name: completion-test
description: Test completion requirement parsing
completionRequirement:
  tool: my_agent__complete_task
  reminder: You must call complete_task
  recovery:
    maxRetries: 5
    baseDelayMs: 5000
    maxDelayMs: 60000
---
"#;
        let def = AgentDefinition::parse(content).unwrap();
        let req = def.completion_requirement.unwrap();
        assert_eq!(req.tool, "my_agent__complete_task");
        assert_eq!(req.reminder, "You must call complete_task");
        let recovery = req.recovery.unwrap();
        assert_eq!(recovery.max_retries, 5);
        assert_eq!(recovery.base_delay_ms, 5000);
        assert_eq!(recovery.max_delay_ms, 60000);
    }
    #[test]
    fn test_builtin_browser_use() {
        let def = AgentDefinition::browser_use();
        assert_eq!(def.name, "browser-use");
        assert_eq!(def.prompt_mode, PromptMode::Full);
        assert!(!def.agents_md);
    }
    #[test]
    fn test_completion_requirement_round_trips() {
        let content = r#"---
name: roundtrip
description: Test round-trip
completionRequirement:
  tool: my__complete
  reminder: Please complete
  recovery:
    maxRetries: 3
    baseDelayMs: 1000
    maxDelayMs: 10000
---
"#;
        let def = AgentDefinition::parse(content).unwrap();
        let req = def.completion_requirement.as_ref().unwrap();
        assert_eq!(req.tool, "my__complete");
        assert_eq!(req.reminder, "Please complete");
        let rec = req.recovery.as_ref().unwrap();
        assert_eq!(rec.max_retries, 3);
        assert_eq!(rec.base_delay_ms, 1000);
        assert_eq!(rec.max_delay_ms, 10000);
    }
    #[test]
    fn test_default_tool_config_has_atelier_build_tools() {
        let content = r#"---
name: default-tools
description: Test default tool config
---
"#;
        let def = AgentDefinition::parse(content).unwrap();
        assert!(
            !def.tool_config.tools.is_empty(),
            "default tool_config should have atelier_build tools"
        );
    }
    #[test]
    fn test_permission_mode_round_trips() {
        for v in PermissionMode::VALID_VALUES {
            let content = format!("---\nname: test\ndescription: Test\npermissionMode: {v}\n---\n");
            AgentDefinition::parse(&content)
                .unwrap_or_else(|e| panic!("PermissionMode '{v}' failed parse: {e}"));
        }
    }
    #[test]
    fn test_prompt_mode_round_trips() {
        for (yaml_val, expected) in [("extend", PromptMode::Extend), ("full", PromptMode::Full)] {
            let content =
                format!("---\nname: test\ndescription: Test\npromptMode: {yaml_val}\n---\n");
            let def = AgentDefinition::parse(&content).unwrap();
            assert_eq!(def.prompt_mode, expected, "Failed for: {yaml_val}");
        }
    }
    #[test]
    fn test_model_override_default_is_inherit() {
        assert_eq!(ModelOverride::default(), ModelOverride::Inherit);
    }
    #[test]
    fn test_model_override_serde_inherit() {
        let yaml = "\"inherit\"";
        let m: ModelOverride = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m, ModelOverride::Inherit);
    }
    #[test]
    fn test_model_override_serde_inherit_case_insensitive() {
        let yaml = "\"Inherit\"";
        let m: ModelOverride = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m, ModelOverride::Inherit);
    }
    #[test]
    fn test_model_override_serde_explicit_model_id() {
        let yaml = "\"atelier-3-fast\"";
        let m: ModelOverride = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m, ModelOverride::Override("atelier-3-fast".to_string()));
    }
    #[test]
    fn test_model_override_serialize_inherit() {
        let m = ModelOverride::Inherit;
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(s, "\"inherit\"");
    }
    #[test]
    fn test_model_override_serialize_override() {
        let m = ModelOverride::Override("atelier-3-fast".to_string());
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(s, "\"atelier-3-fast\"");
    }
    #[test]
    fn test_model_override_in_frontmatter() {
        let content = "---\nname: test\ndescription: Test\nmodel: atelier-3-fast\n---\n";
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(
            def.model,
            ModelOverride::Override("atelier-3-fast".to_string())
        );
    }
    #[test]
    fn test_model_override_in_frontmatter_inherit() {
        let content = "---\nname: test\ndescription: Test\nmodel: inherit\n---\n";
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(def.model, ModelOverride::Inherit);
    }
    #[test]
    fn test_model_override_omitted_defaults_to_inherit() {
        let content = "---\nname: test\ndescription: Test\n---\n";
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(def.model, ModelOverride::Inherit);
    }
    #[test]
    fn test_builtin_agent_name_strum_round_trip() {
        use std::str::FromStr;
        for (s, expected) in [
            ("atelier-build", BuiltinAgentName::AtelierBuild),
            (
                "atelier-build-concise",
                BuiltinAgentName::AtelierBuildConcise,
            ),
            (
                "atelier-build-ask-user",
                BuiltinAgentName::AtelierBuildAskUser,
            ),
            ("codex", BuiltinAgentName::Codex),
            ("opencode", BuiltinAgentName::Opencode),
            ("general-purpose", BuiltinAgentName::GeneralPurpose),
            ("explore", BuiltinAgentName::Explore),
            ("plan", BuiltinAgentName::Plan),
            ("browser-use", BuiltinAgentName::BrowserUse),
        ] {
            let parsed = BuiltinAgentName::from_str(s).unwrap();
            assert_eq!(parsed, expected, "from_str failed for: {s}");
            assert_eq!(parsed.as_ref(), s, "as_ref failed for: {s}");
        }
    }
    #[test]
    fn test_builtin_agent_name_unknown_returns_err() {
        use std::str::FromStr;
        assert!(BuiltinAgentName::from_str("nonexistent").is_err());
        assert!(BuiltinAgentName::from_str("not-a-builtin-agent").is_err());
    }
    #[test]
    fn test_builtin_agent_name_definition_names_match() {
        use strum::IntoEnumIterator;
        for variant in BuiltinAgentName::iter() {
            let def = variant.definition();
            assert_eq!(
                def.name,
                variant.as_ref(),
                "definition().name doesn't match as_ref() for {:?}",
                variant
            );
        }
    }
    #[test]
    fn test_builtin_agent_name_subagent_variants() {
        let variants = BuiltinAgentName::subagent_variants();
        assert_eq!(variants.len(), 3);
        assert!(variants.contains(&BuiltinAgentName::GeneralPurpose));
        assert!(variants.contains(&BuiltinAgentName::Explore));
        assert!(variants.contains(&BuiltinAgentName::Plan));
    }
    #[test]
    fn test_all_builtins_have_inherit_model() {
        use strum::IntoEnumIterator;
        for variant in BuiltinAgentName::iter() {
            let def = variant.definition();
            assert_eq!(
                def.model,
                ModelOverride::Inherit,
                "Built-in {:?} should default to Inherit",
                variant
            );
        }
    }
    #[test]
    fn mcp_inheritance_default_when_omitted() {
        let def = AgentDefinition::parse("---\nname: t\ndescription: t\n---\n").unwrap();
        assert_eq!(def.mcp_inheritance, McpInheritance::All);
    }
    #[test]
    fn mcp_inheritance_all_parses() {
        let def =
            AgentDefinition::parse("---\nname: t\ndescription: t\nmcpInheritance: all\n---\n")
                .unwrap();
        assert_eq!(def.mcp_inheritance, McpInheritance::All);
    }
    #[test]
    fn mcp_inheritance_none_parses() {
        let def =
            AgentDefinition::parse("---\nname: t\ndescription: t\nmcpInheritance: none\n---\n")
                .unwrap();
        assert_eq!(def.mcp_inheritance, McpInheritance::None);
    }
    #[test]
    fn mcp_inheritance_named_parses() {
        let content = "---\nname: t\ndescription: t\nmcpInheritance:\n  named:\n    - slack\n    - github\n---\n";
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(
            def.mcp_inheritance,
            McpInheritance::Named(vec!["slack".into(), "github".into()])
        );
    }
    #[test]
    fn mcp_inheritance_except_parses() {
        let content =
            "---\nname: t\ndescription: t\nmcpInheritance:\n  except:\n    - internal\n---\n";
        let def = AgentDefinition::parse(content).unwrap();
        assert_eq!(
            def.mcp_inheritance,
            McpInheritance::Except(vec!["internal".into()])
        );
    }
    fn def_with_template(tpl: crate::prompt::context::TemplateOverride) -> AgentDefinition {
        let mut def = AgentDefinition::default_atelier_build();
        def.system_prompt = tpl;
        def
    }
    #[test]
    fn carries_discipline_false_for_every_template_and_audience() {
        for tpl in [
            crate::prompt::context::TemplateOverride::None,
            crate::prompt::context::TemplateOverride::Codex,
            crate::prompt::context::TemplateOverride::Custom("fake".to_string()),
        ] {
            let def = def_with_template(tpl.clone());
            for audience in [
                crate::prompt::context::PromptAudience::Primary,
                crate::prompt::context::PromptAudience::Subagent,
            ] {
                assert!(
                    !def.carries_task_completion_discipline(audience),
                    "discipline block was removed; helper must return false \
                     (template: {tpl:?}, audience: {audience:?})"
                );
            }
        }
    }
}
