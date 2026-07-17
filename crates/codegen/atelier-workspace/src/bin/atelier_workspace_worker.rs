//! Standalone local workspace worker.
//!
//! The process speaks the versioned NDJSON protocol implemented by
//! `atelier-workspace::worker` on stdin/stdout.  Diagnostics are written to
//! stderr so stdout remains a machine-readable transport.

use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "atelier-workspace-worker")]
#[command(about = "Confined local workspace RPC worker")]
struct Args {
    /// Workspace root. The client repeats this value in the hello frame and
    /// the worker rejects a mismatch after canonicalization.
    #[arg(long)]
    root: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()
        .ok();
    atelier_workspace::run_worker(args.root)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}
