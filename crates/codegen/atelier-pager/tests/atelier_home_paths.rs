//! `ATELIER_HOME` override tests in an isolated binary so `atelier_home()`'s
//! process-wide `OnceLock` initializes from the overridden env var.

use std::path::PathBuf;

#[test]
fn atelier_home_override_path_helpers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let atelier_home = tmp.path().to_path_buf();
    unsafe {
        std::env::set_var("ATELIER_HOME", &atelier_home);
    }

    assert_eq!(
        atelier_pager::util::pager_toml_path(),
        atelier_home.join("pager.toml")
    );
    assert_eq!(
        atelier_pager::util::display_atelier_home_prefix(),
        "$ATELIER_HOME"
    );
    assert_eq!(
        atelier_pager::util::display_user_atelier_path("config.toml"),
        "$ATELIER_HOME/config.toml"
    );

    let memory_path = atelier_home.join("memory/MEMORY.md");
    assert_eq!(
        atelier_pager::util::abbreviate_path(&memory_path.display().to_string()),
        "$ATELIER_HOME/memory/MEMORY.md"
    );

    assert!(atelier_pager::util::is_under_user_atelier_home(
        &memory_path
    ));
    assert!(!atelier_pager::util::is_under_user_atelier_home(
        PathBuf::from("/tmp/other").as_path()
    ));
}
