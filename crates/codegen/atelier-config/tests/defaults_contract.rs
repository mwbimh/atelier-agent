use atelier_config::defaults::{ensure_user_defaults, reset_user_defaults};
use tempfile::tempdir;

#[test]
fn first_run_writes_the_split_atelier_config_tree() {
    let home = tempdir().unwrap();
    ensure_user_defaults(home.path(), "0.1.220-alpha.4").unwrap();

    for path in [
        "config.toml",
        "providers.toml",
        "roles.toml",
        "models/default/common.toml",
        "contexts/default/main.md",
        "contexts/default/subagent.md",
        "contexts/default/apply_patch.md",
        "contexts/default/goal/planner.md",
        "contexts/default/goal/strategist.md",
        "contexts/default/goal/skeptic.md",
        "contexts/default/goal/summary.md",
        "contexts/default/compaction/developer.md",
        "contexts/default/compaction/user.md",
        "branding/logo.txt",
    ] {
        assert!(home.path().join(path).is_file(), "missing generated {path}");
    }
    assert!(home.path().join("models/providers").is_dir());
    assert!(home.path().join("credentials/oauth/providers").is_dir());
    assert!(home.path().join("credentials/oauth/mcp").is_dir());
    assert!(home.path().join("cache/providers").is_dir());

    let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(config.contains("request_agent = \"atelier\""));
    assert!(config.contains("value = \"Atelier/0.1.220-alpha.4\""));
    assert!(config.contains("value = \"pi 1.0\""));

    let models = std::fs::read_to_string(home.path().join("models/default/common.toml")).unwrap();
    assert!(models.contains("context_window"));
    assert!(models.contains("reasoning_efforts"));
    assert!(models.contains("fast_mode"));
    assert!(models.contains("wire_api"));
}

#[test]
fn ensure_defaults_never_overwrites_user_edits() {
    let home = tempdir().unwrap();
    ensure_user_defaults(home.path(), "1.0.0").unwrap();
    let config = home.path().join("config.toml");
    std::fs::write(&config, "request_agent = \"pi\"\n").unwrap();

    ensure_user_defaults(home.path(), "2.0.0").unwrap();

    assert_eq!(
        std::fs::read_to_string(config).unwrap(),
        "request_agent = \"pi\"\n"
    );
}

#[test]
fn reset_restores_owned_defaults_but_preserves_provider_overrides_and_credentials() {
    let home = tempdir().unwrap();
    ensure_user_defaults(home.path(), "1.0.0").unwrap();
    std::fs::write(home.path().join("branding/logo.txt"), "custom logo\n").unwrap();
    let provider_override = home
        .path()
        .join("models/providers/allm/deepseek-v4-flash.toml");
    std::fs::create_dir_all(provider_override.parent().unwrap()).unwrap();
    std::fs::write(&provider_override, "fast_mode = false\n").unwrap();
    let credential = home.path().join("credentials/oauth/providers/allm.json");
    std::fs::write(&credential, "encrypted-user-data").unwrap();

    reset_user_defaults(home.path(), "2.0.0").unwrap();

    let logo = std::fs::read_to_string(home.path().join("branding/logo.txt")).unwrap();
    assert_ne!(logo, "custom logo\n");
    assert!(logo.contains("ATE"));
    assert_eq!(
        std::fs::read_to_string(provider_override).unwrap(),
        "fast_mode = false\n"
    );
    assert_eq!(
        std::fs::read_to_string(credential).unwrap(),
        "encrypted-user-data"
    );
}
