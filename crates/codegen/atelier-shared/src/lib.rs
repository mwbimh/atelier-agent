//! Shared utilities used by both `atelier-shell` and its downstream clients
//! (e.g. `atelier-pager-render`). This crate sits upstream of `atelier-shell`
//! so it must never depend on it.

pub mod clipboard;
pub mod placeholder_images;
pub mod session;
pub mod stderr;
pub mod ui_config;
