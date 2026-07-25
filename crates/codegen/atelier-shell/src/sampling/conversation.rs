//! API-agnostic conversation representation.
//!
//! The canonical types now live in `atelier_sampling_types::conversation`.
//! This module re-exports the canonical conversation types.

// Re-export everything from the standalone crate.
pub use atelier_sampling_types::conversation::*;

// Tests for conversation types now live in atelier-sampling-types crate.
