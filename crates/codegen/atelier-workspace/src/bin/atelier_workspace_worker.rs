//! Standalone local workspace worker.
//!
//! The process speaks the versioned NDJSON protocol implemented by
//! `atelier-workspace::worker` on stdin/stdout.  Diagnostics are written to
//! stderr so stdout remains a machine-readable transport.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = atelier_workspace::parse_worker_args(std::env::args_os().skip(1))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()
        .ok();
    atelier_workspace::run_worker(root)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}
