//! Fixed built-in Agent lookup.
//!
//! Runtime Roles are fixed and user-defined Agent preset files are not
//! discovered. The functions in this module retain the stable lookup surface
//! used by the shell, but resolve only compiled built-ins.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use atelier_tools::types::config_source::ConfigSource;

use crate::config::{AgentDefinition, BuiltinAgentName};

/// Custom project Agent directories are intentionally not recognized.
pub fn project_agent_dirs(cwd: Option<&Path>) -> (Vec<PathBuf>, Option<PathBuf>) {
    let git_root = cwd.and_then(|cwd| crate::repo::RepoDirChain::resolve(cwd).git_root);
    (Vec::new(), git_root)
}

/// Custom project Agent directories are intentionally not recognized.
pub fn project_agent_dirs_in(_chain_dirs: &[PathBuf]) -> Vec<PathBuf> {
    Vec::new()
}

#[derive(Debug, Clone)]
pub struct SubagentEntry {
    pub name: String,
    pub description: String,
    pub source: SubagentSource,
    pub shadows_builtin: Option<BuiltinAgentName>,
    pub config_source: ConfigSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentSource {
    Builtin(BuiltinAgentName),
}

fn fixed_subagents(toggle: &HashMap<String, bool>) -> Vec<SubagentEntry> {
    BuiltinAgentName::subagent_variants()
        .iter()
        .filter_map(|builtin| {
            let definition = builtin.definition();
            toggle
                .get(&definition.name)
                .copied()
                .unwrap_or(true)
                .then_some(SubagentEntry {
                    name: definition.name,
                    description: definition.description,
                    source: SubagentSource::Builtin(*builtin),
                    shadows_builtin: None,
                    config_source: ConfigSource::Builtin,
                })
        })
        .collect()
}

pub fn all_subagents(_cwd: &Path, toggle: &HashMap<String, bool>) -> Vec<SubagentEntry> {
    fixed_subagents(toggle)
}

/// Filesystem Agent preset discovery is disabled.
pub fn discover(_cwd: &Path) -> Vec<AgentDefinition> {
    Vec::new()
}

pub fn by_name(name: &str) -> Option<AgentDefinition> {
    BuiltinAgentName::from_str(name)
        .ok()
        .map(BuiltinAgentName::definition)
}

pub fn by_name_in_cwd(name: &str, _cwd: &Path) -> Option<AgentDefinition> {
    by_name(name)
}

pub fn builtin_subagents() -> Vec<AgentDefinition> {
    BuiltinAgentName::subagent_variants()
        .iter()
        .map(|name| name.definition())
        .collect()
}

pub fn all_builtin_agent_definitions() -> Vec<AgentDefinition> {
    use strum::IntoEnumIterator;
    BuiltinAgentName::iter()
        .map(BuiltinAgentName::definition)
        .collect()
}

pub fn all_subagents_with_plugins(
    _cwd: &Path,
    toggle: &HashMap<String, bool>,
    _plugins: Option<&crate::plugins::PluginRegistry>,
) -> Vec<SubagentEntry> {
    fixed_subagents(toggle)
}

pub fn by_name_in_cwd_with_plugins(
    name: &str,
    _cwd: &Path,
    _plugins: Option<&crate::plugins::PluginRegistry>,
) -> Option<AgentDefinition> {
    by_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_returns_only_fixed_builtins() {
        let cwd = Path::new(".");
        assert!(discover(cwd).is_empty());
        assert!(project_agent_dirs(Some(cwd)).0.is_empty());
        assert!(by_name_in_cwd("custom", cwd).is_none());

        let names = all_subagents(cwd, &HashMap::new())
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["general-purpose", "explore", "plan"]);
    }

    #[test]
    fn plugin_agents_are_not_runtime_agent_presets() {
        let entries = all_subagents_with_plugins(Path::new("."), &HashMap::new(), None);
        assert!(
            entries
                .iter()
                .all(|entry| matches!(entry.source, SubagentSource::Builtin(_)))
        );
        assert!(by_name_in_cwd_with_plugins("plugin:custom", Path::new("."), None).is_none());
    }

    #[test]
    fn toggles_only_filter_the_fixed_set() {
        let toggle = HashMap::from([("explore".to_owned(), false)]);
        let names = all_subagents(Path::new("."), &toggle)
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["general-purpose", "plan"]);
    }
}
