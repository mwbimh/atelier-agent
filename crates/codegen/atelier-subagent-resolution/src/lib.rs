//! Fixed Subagent runtime resolution.
//!
//! Runtime Role execution settings are owned by `atelier-provider`. This crate
//! only normalizes forked context, explicit spawn-time restrictions, and
//! resume identity for the fixed Subagent types. It has no custom Role,
//! Persona, prompt-file, or filesystem-discovery layer.

pub mod context;
pub mod overrides;
pub mod resume;
pub mod types;

pub use overrides::resolve_effective_overrides;
pub use resume::{ResumeValidationError, validate_resume_identity};
pub use types::{ContextSource, EffectiveRuntimeConfig, ResolutionError, ResumeSourceData};
