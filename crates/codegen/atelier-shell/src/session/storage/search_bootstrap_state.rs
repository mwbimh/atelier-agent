//! Local session-search bootstrap metadata.

use std::io;
use std::path::Path;

use super::search_fts::SessionSearchIndex;

const META_KEY_LAST_BOOTSTRAP: &str = "last_bootstrap_at";

pub fn read_last_bootstrap_at(db_path: &Path) -> Option<i64> {
    try_read_last_bootstrap_at(db_path).ok().flatten()
}

pub fn try_read_last_bootstrap_at(db_path: &Path) -> Result<Option<i64>, String> {
    if !db_path.exists() {
        return Ok(None);
    }
    let index = SessionSearchIndex::open_or_create(db_path).map_err(|error| error.to_string())?;
    let value = index
        .get_meta(META_KEY_LAST_BOOTSTRAP)
        .map_err(|error| error.to_string())?;
    Ok(value.and_then(|value| value.parse::<i64>().ok()))
}

pub fn write_last_bootstrap_at(db_path: &Path) -> io::Result<()> {
    let index = SessionSearchIndex::open_or_create(db_path)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let now = chrono::Utc::now().timestamp();
    index
        .set_meta(META_KEY_LAST_BOOTSTRAP, &now.to_string())
        .map_err(|error| io::Error::other(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_last_bootstrap_at() {
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = temp.path().join("session_search.sqlite");

        assert_eq!(read_last_bootstrap_at(&db_path), None);
        write_last_bootstrap_at(&db_path).unwrap();

        let timestamp = read_last_bootstrap_at(&db_path).unwrap();
        let now = chrono::Utc::now().timestamp();
        assert!((now - timestamp).abs() < 5);
    }
}
