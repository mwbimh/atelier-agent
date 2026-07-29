//! Fixed Subagent runtime override resolution.
//!
//! Runtime Roles are resolved by `atelier-provider`; this crate only carries
//! explicit spawn-time restrictions. Custom Role and Persona layers are not
//! accepted or discovered.

use atelier_tool_types::SubagentIsolationMode;
use atelier_tools::implementations::atelier_build::task::types::SubagentRuntimeOverrides;

use crate::types::EffectiveRuntimeConfig;

/// Resolve explicit spawn-time restrictions for one fixed Runtime Role.
///
/// Provider/model Role inheritance is applied later by the shell against the
/// fixed Role registry. This function deliberately has no custom Role,
/// Persona, prompt-file, or filesystem-discovery inputs.
pub fn resolve_effective_overrides(overrides: &SubagentRuntimeOverrides) -> EffectiveRuntimeConfig {
    EffectiveRuntimeConfig {
        fixed_role: overrides.fixed_role.clone(),
        model: overrides.model.clone(),
        reasoning_effort: overrides.reasoning_effort.clone(),
        capability_mode: overrides.capability_mode,
        isolation: overrides.isolation.unwrap_or(SubagentIsolationMode::None),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelier_tool_types::{SubagentCapabilityMode, SubagentIsolationMode};

    #[test]
    fn explicit_fixed_runtime_overrides_are_preserved() {
        let overrides = SubagentRuntimeOverrides {
            fixed_role: Some("review".into()),
            model: Some("provider/model".into()),
            reasoning_effort: Some("high".into()),
            capability_mode: Some(SubagentCapabilityMode::ReadOnly),
            isolation: Some(SubagentIsolationMode::Worktree),
            ..Default::default()
        };

        let resolved = resolve_effective_overrides(&overrides);

        assert_eq!(resolved.fixed_role.as_deref(), Some("review"));
        assert_eq!(resolved.model.as_deref(), Some("provider/model"));
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(
            resolved.capability_mode,
            Some(SubagentCapabilityMode::ReadOnly)
        );
        assert_eq!(resolved.isolation, SubagentIsolationMode::Worktree);
    }

    #[test]
    fn missing_isolation_defaults_to_none() {
        let resolved = resolve_effective_overrides(&SubagentRuntimeOverrides::default());
        assert_eq!(resolved.isolation, SubagentIsolationMode::None);
    }
}
