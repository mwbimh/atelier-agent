#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
//! Shared test utilities for atelier-build crates: mock inference server, SSE
//! generators, ACP stdio client, headless runner, env sandbox.
//!
//! Provides:
//! - [`MockInferenceServer`] — Mock /v1/chat/completions + /v1/responses with request logging
//! - [`AtelierStdioClient`] — ACP client that drives `atelier agent stdio` as a subprocess
//! - [`RawStdioClient`] — raw-wire ACP driver for bytes the typed client can't
//!   produce (Foundation `\/` methods, string UUID ids)
//! - [`leader::LeaderStdioClient`] — ACP client that drives `atelier agent --leader stdio` (unix)
//! - [`run_headless`] — Run `atelier -p` against the mock server and capture output
//! - [`git_workdir`] — Create a temp directory with git repo (forces libgit2 init)
//! - [`atelier_binary`] — Resolve the atelier binary path (ATELIER_BINARY env or cargo_bin)
//! - [`spawn_counting_server`] — Connection-counting HTTP/1.1 server for wire/pooling tests
//! - [`uds_proxy::UdsProxy`] — Frame-aware fault-injection proxy for leader IPC sockets (unix)
pub mod acp_client;
pub mod counting_server;
pub mod env;
pub mod headless;
#[cfg(unix)]
pub mod leader;
pub mod mock_server;
mod process;
pub mod scripted;
pub mod sse;
#[cfg(unix)]
pub mod uds_proxy;
pub use acp_client::{AtelierStdioClient, RawStdioClient};
pub use counting_server::spawn_counting_server;
pub use env::{EnvGuard, atelier_binary, git_workdir};
pub use headless::{
    HeadlessResult, assert_headless_success, assert_no_crashes, run_headless,
    run_headless_with_cmd, stderr_tail,
};
pub use mock_server::{MockInferenceServer, MockModelEntry, ScriptedResponse, SseEvent};
