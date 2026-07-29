//! Public API types for subagent resolution.

use std::path::PathBuf;

use crate::resume::ResumeValidationError;

/// How the child session's initial context was bootstrapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextSource {
    /// Fresh session with no inherited history.
    New,
    /// Resumed from a previously completed peer subagent. The child inherits
    /// the source's raw transcript, tool state, and model. System prompt and
    /// prompt context are freshly rendered.
    Resumed,
}

/// Resolved explicit runtime configuration for a child agent.
#[derive(Debug, Clone, Default)]
pub struct EffectiveRuntimeConfig {
    /// Explicit fixed Atelier runtime Role selected by an internal harness.
    pub fixed_role: Option<String>,
    /// Resolved model ID override (if any).
    pub model: Option<String>,
    /// Resolved reasoning effort (e.g. "low", "medium", "high").
    // TODO(phase2): consider a typed `ReasoningEffort` enum to prevent typos.
    // Currently stringly-typed for compatibility with the shell's existing API.
    pub reasoning_effort: Option<String>,
    /// Resolved capability mode controlling tool access.
    pub capability_mode: Option<atelier_tool_types::SubagentCapabilityMode>,
    /// Fixed Role Context prompt loaded by the shell.
    pub context_prompt: Option<String>,
    /// Isolation mode for the child execution environment.
    pub isolation: atelier_tool_types::SubagentIsolationMode,
}

/// Data about a completed source subagent, needed for resume validation
/// and downstream spawn orchestration.
#[derive(Debug, Clone)]
pub struct ResumeSourceData {
    /// Source subagent ID.
    pub subagent_id: String,
    /// Source subagent type (e.g. "general-purpose", "explore").
    /// Used by `validate_resume_identity` to check type match.
    pub subagent_type: String,
    /// Effective model ID used by the source child session.
    /// Used by the shell for resume model pinning (model overrides on
    /// resume are soft-ignored, not identity-gated).
    pub model_id: Option<String>,
    /// Effective cwd the source child used. Consumed by the shell's
    /// spawn orchestration to reconstruct `SessionInfo` for raw
    /// transcript continuation and worktree reuse.
    pub child_cwd: String,
    /// Worktree path if the source used `isolation=worktree`. Consumed
    /// by the shell to reuse the source's isolated workspace directory
    /// when resuming a worktree-isolated child.
    pub worktree_path: Option<PathBuf>,
    /// Durable git ref holding a snapshot of the source worktree's working
    /// state, set when the worktree was snapshotted at completion. Consumed
    /// by the shell to rehydrate a deleted worktree directory on resume.
    pub snapshot_ref: Option<String>,
    /// The child session ID of the source subagent. Consumed by the
    /// shell to locate the source's session directory for raw transcript
    /// copying (`copy_session_data_sync`).
    pub child_session_id: String,
}

/// Errors that can occur during subagent resolution.
#[derive(Debug, thiserror::Error)]
pub enum ResolutionError {
    /// Resume identity validation failed.
    #[error("resume validation failed: {0}")]
    ResumeValidation(#[from] ResumeValidationError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelier_tool_types::SubagentIsolationMode;

    #[test]
    fn effective_runtime_config_default_values() {
        let config = EffectiveRuntimeConfig::default();
        assert!(config.fixed_role.is_none());
        assert!(config.model.is_none());
        assert!(config.reasoning_effort.is_none());
        assert!(config.capability_mode.is_none());
        assert!(config.context_prompt.is_none());
        assert_eq!(config.isolation, SubagentIsolationMode::None);
    }

    #[test]
    fn resolution_error_resume_from_typed_error() {
        let typed = ResumeValidationError::TypeMismatch {
            requested: "explore".into(),
            source_value: "general-purpose".into(),
        };
        let err = ResolutionError::from(typed);
        let msg = err.to_string();
        assert!(msg.contains("resume validation failed"));
        assert!(msg.contains("explore"));
        assert!(msg.contains("general-purpose"));
    }

    #[test]
    fn context_source_equality() {
        assert_eq!(ContextSource::New, ContextSource::New);
        assert_ne!(ContextSource::New, ContextSource::Resumed);
    }
}
