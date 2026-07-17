use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("Atelier Windows sandbox requires at least one existing workspace root")]
    NoRoots,
    #[error("sandbox path does not exist: {0}")]
    MissingPath(PathBuf),
    #[error("sandbox path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("command cwd is outside every configured workspace root: {cwd}")]
    CwdOutsideRoots { cwd: PathBuf },
    #[error("sandbox command is empty")]
    EmptyCommand,
    #[error("Windows sandbox operation failed: {0}")]
    Operation(#[from] anyhow::Error),
}
