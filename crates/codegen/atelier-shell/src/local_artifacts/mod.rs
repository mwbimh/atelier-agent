//! Local, session-scoped diagnostic artifacts.
//!
//! This module deliberately contains no remote storage client, upload queue,
//! cloud credential, or network behavior. Artifacts are written below the
//! session persistence directory.

pub(crate) mod artifacts;
pub(crate) mod manifest;
pub(crate) mod turn;
