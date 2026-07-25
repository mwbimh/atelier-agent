use std::path::PathBuf;
use std::sync::OnceLock;

static ATELIER_HOME: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn atelier_home() -> PathBuf {
    ATELIER_HOME
        .get_or_init(|| {
            let path = std::env::var_os("ATELIER_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    #[allow(deprecated)]
                    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
                    home.join(".atelier")
                });
            let _ = std::fs::create_dir_all(&path);
            path
        })
        .clone()
}
