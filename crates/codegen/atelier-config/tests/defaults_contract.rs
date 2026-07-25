use atelier_config::defaults::{ensure_user_defaults, reset_user_defaults};

#[test]
fn first_run_writes_the_split_atelier_config_tree() {
    let home = tempfile::tempdir().unwrap();
    ensure_user_defaults(home.path(), "0.1.220-alpha.4").unwrap();

    for relative in [
        "config.toml",
        "providers.toml",
        "roles.toml",
        "request-agents.toml",
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
        assert!(
            home.path().join(relative).is_file(),
            "missing generated default: {relative}"
        );
    }

    let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert_eq!(
        config,
        "model = \"allm/deepseek-v4-flash\"\ncontext = \"default\"\nrequest_agent = \"atelier\"\n"
    );

    let agents = std::fs::read_to_string(home.path().join("request-agents.toml")).unwrap();
    assert!(agents.contains("schema_version = 1"));
    assert!(agents.contains("[agents.atelier]"));
    assert!(agents.contains("name = \"Atelier\""));
    assert!(agents.contains("version = \"0.1.220-alpha.4\""));
    assert!(agents.contains("[agents.pi]"));
    assert!(agents.contains("[agents.codex]"));
    assert!(agents.contains("[agents.opencode]"));
}

#[test]
fn ensure_preserves_user_owned_files() {
    let home = tempfile::tempdir().unwrap();
    ensure_user_defaults(home.path(), "1.0.0").unwrap();

    let config = home.path().join("config.toml");
    let agents = home.path().join("request-agents.toml");
    let logo = home.path().join("branding/logo.txt");
    let prompt = home.path().join("contexts/default/main.md");
    std::fs::write(
        &config,
        "model = \"proxy/custom\"\ncontext = \"custom\"\nrequest_agent = \"pi\"\n",
    )
    .unwrap();
    std::fs::write(
        &agents,
        "schema_version = 1\n[agents.custom]\nname = \"custom\"\n",
    )
    .unwrap();
    std::fs::write(&logo, "custom logo\n").unwrap();
    std::fs::write(&prompt, "custom prompt\n").unwrap();

    ensure_user_defaults(home.path(), "2.0.0").unwrap();

    assert_eq!(
        std::fs::read_to_string(config).unwrap(),
        "model = \"proxy/custom\"\ncontext = \"custom\"\nrequest_agent = \"pi\"\n"
    );
    assert!(
        std::fs::read_to_string(agents)
            .unwrap()
            .contains("agents.custom")
    );
    assert_eq!(std::fs::read_to_string(logo).unwrap(), "custom logo\n");
    assert_eq!(std::fs::read_to_string(prompt).unwrap(), "custom prompt\n");
}

#[test]
fn reset_restores_owned_defaults() {
    let home = tempfile::tempdir().unwrap();
    ensure_user_defaults(home.path(), "1.0.0").unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "model = \"proxy/custom\"\ncontext = \"custom\"\nrequest_agent = \"pi\"\n",
    )
    .unwrap();
    std::fs::write(home.path().join("request-agents.toml"), "custom\n").unwrap();
    std::fs::write(home.path().join("branding/logo.txt"), "custom logo\n").unwrap();
    std::fs::write(home.path().join("providers.toml"), "user providers\n").unwrap();
    std::fs::write(home.path().join("roles.toml"), "user roles\n").unwrap();
    std::fs::write(
        home.path().join("models/default/common.toml"),
        "user default models\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("contexts/default/main.md"),
        "user default context\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("models/providers/allm")).unwrap();
    std::fs::write(
        home.path().join("models/providers/allm/models.toml"),
        "user provider models\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("credentials/oauth/providers/allm")).unwrap();
    std::fs::write(
        home.path()
            .join("credentials/oauth/providers/allm/credential.json"),
        "user credential\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("cache/providers/allm")).unwrap();
    std::fs::write(
        home.path().join("cache/providers/allm/models.json"),
        "user cache\n",
    )
    .unwrap();

    reset_user_defaults(home.path(), "2.0.0").unwrap();

    assert_eq!(
        std::fs::read_to_string(home.path().join("config.toml")).unwrap(),
        "model = \"allm/deepseek-v4-flash\"\ncontext = \"default\"\nrequest_agent = \"atelier\"\n"
    );
    let agents = std::fs::read_to_string(home.path().join("request-agents.toml")).unwrap();
    assert!(agents.contains("version = \"2.0.0\""));
    let logo = std::fs::read_to_string(home.path().join("branding/logo.txt")).unwrap();
    assert!(logo.contains("A T E L I E R"));
    assert_eq!(
        std::fs::read_to_string(home.path().join("providers.toml")).unwrap(),
        "schema_version = 2\n\n[providers]\n"
    );
    let roles = std::fs::read_to_string(home.path().join("roles.toml")).unwrap();
    for role in [
        "main",
        "explore",
        "implement",
        "review",
        "test",
        "compact",
        "summary",
        "title",
        "planner",
        "strategist",
        "skeptic",
    ] {
        assert!(
            roles.contains(&format!("[roles.{role}]")),
            "reset roles.toml is missing {role}"
        );
    }
    let default_models =
        std::fs::read_to_string(home.path().join("models/default/common.toml")).unwrap();
    assert!(default_models.contains("[[models]]"));
    assert!(default_models.contains("context_window"));
    assert_ne!(default_models, "user default models\n");
    let default_context =
        std::fs::read_to_string(home.path().join("contexts/default/main.md")).unwrap();
    assert_ne!(default_context, "user default context\n");
    assert!(!default_context.trim().is_empty());
    assert_eq!(
        std::fs::read_to_string(home.path().join("models/providers/allm/models.toml")).unwrap(),
        "user provider models\n"
    );
    assert_eq!(
        std::fs::read_to_string(
            home.path()
                .join("credentials/oauth/providers/allm/credential.json")
        )
        .unwrap(),
        "user credential\n"
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("cache/providers/allm/models.json")).unwrap(),
        "user cache\n"
    );
}

#[test]
fn reset_only_replaces_atelier_managed_root_keys_in_main_config() {
    let home = tempfile::tempdir().unwrap();
    ensure_user_defaults(home.path(), "1.0.0").unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"model = "proxy/custom"
context = "custom"
request_agent = "pi"
user_root_key = "keep-me"

[ui]
theme = "ateliernight"
animations = false

[sandbox]
mode = "workspace-write"
network = false

[mcp.servers.local]
command = "local-mcp"
args = ["--stdio"]

[future_feature]
enabled = true
payload = { answer = 42 }
"#,
    )
    .unwrap();

    reset_user_defaults(home.path(), "2.0.0").unwrap();

    let config: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.path().join("config.toml")).unwrap()).unwrap();
    assert_eq!(config["model"].as_str(), Some("allm/deepseek-v4-flash"));
    assert_eq!(config["context"].as_str(), Some("default"));
    assert_eq!(config["request_agent"].as_str(), Some("atelier"));
    assert_eq!(config["user_root_key"].as_str(), Some("keep-me"));
    assert_eq!(config["ui"]["theme"].as_str(), Some("ateliernight"));
    assert_eq!(config["ui"]["animations"].as_bool(), Some(false));
    assert_eq!(config["sandbox"]["mode"].as_str(), Some("workspace-write"));
    assert_eq!(config["sandbox"]["network"].as_bool(), Some(false));
    assert_eq!(
        config["mcp"]["servers"]["local"]["command"].as_str(),
        Some("local-mcp")
    );
    assert_eq!(
        config["mcp"]["servers"]["local"]["args"][0].as_str(),
        Some("--stdio")
    );
    assert_eq!(config["future_feature"]["enabled"].as_bool(), Some(true));
    assert_eq!(
        config["future_feature"]["payload"]["answer"].as_integer(),
        Some(42)
    );
}
