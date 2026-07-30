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
        "models/default/openai.toml",
        "models/default/anthropic.toml",
        "models/default/google.toml",
        "models/default/deepseek.toml",
        "models/default/xai.toml",
        "contexts/default/main.md",
        "contexts/default/subagent.md",
        "contexts/default/apply_patch.md",
        "contexts/default/goal/planner.md",
        "contexts/default/goal/strategist.md",
        "contexts/default/goal/skeptic.md",
        "contexts/default/goal/summary.md",
        "contexts/default/roles/general.md",
        "contexts/default/roles/explore.md",
        "contexts/default/roles/implement.md",
        "contexts/default/roles/review.md",
        "contexts/default/roles/test.md",
        "contexts/default/roles/compact.md",
        "contexts/default/roles/summary.md",
        "contexts/default/roles/title.md",
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
        "context = \"default\"\nrequest_agent = \"atelier\"\n"
    );

    let providers = std::fs::read_to_string(home.path().join("providers.toml")).unwrap();
    assert_eq!(providers, "schema_version = 3\n\n[providers]\n");

    let roles = std::fs::read_to_string(home.path().join("roles.toml")).unwrap();
    assert_eq!(roles, "schema_version = 1\n\n[roles]\n");
    assert!(!roles.contains("deepseek-v4-flash"));

    let agents = std::fs::read_to_string(home.path().join("request-agents.toml")).unwrap();
    assert!(agents.contains("schema_version = 1"));
    assert!(agents.contains("[agents.atelier]"));
    assert!(agents.contains("name = \"Atelier\""));
    assert!(agents.contains("version = \"0.1.220-alpha.4\""));
    assert!(agents.contains("[agents.pi]"));
    assert!(agents.contains("[agents.codex]"));
    assert!(agents.contains("[agents.opencode]"));
    assert!(agents.contains("version = \"0.82.1\""));
    assert!(agents.contains("version = \"0.145.0\""));
    assert!(agents.contains("version = \"1.18.5\""));
    assert!(agents.contains("user_agent = \"pi/0.82.1 ("));
    assert!(agents.contains("user_agent = \"codex_cli_rs/0.145.0 ("));
    #[cfg(target_os = "windows")]
    assert!(!agents.contains("Windows unknown"), "{agents}");
    assert!(agents.contains("user_agent = \"opencode/1.18.5\""));

    for vendor in ["openai", "anthropic", "google", "deepseek", "xai"] {
        let source =
            std::fs::read_to_string(home.path().join(format!("models/default/{vendor}.toml")))
                .unwrap();
        assert!(
            source.starts_with("# Verified"),
            "missing source note: {vendor}"
        );
        assert!(source.contains("schema_version = 2"));
        assert!(!source.contains('*'), "wildcard model preset in {vendor}");
    }

    let openai = std::fs::read_to_string(home.path().join("models/default/openai.toml")).unwrap();
    for model in ["o3", "o3-deep-research", "o3-mini", "o3-pro"] {
        assert!(
            openai.contains(&format!("[models.\"{model}\"]")),
            "missing exact preset for {model}"
        );
    }
    let openai: toml::Value = toml::from_str(&openai).unwrap();
    assert_eq!(
        openai["models"]["gpt-5"]["reasoning_efforts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>(),
        ["minimal", "low", "medium", "high"]
    );
    assert_eq!(
        openai["models"]["gpt-5.4"]["reasoning_efforts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>(),
        ["none", "low", "medium", "high", "xhigh"]
    );
    assert_eq!(
        openai["models"]["gpt-5.6-sol"]["reasoning_efforts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>(),
        ["none", "low", "medium", "high", "xhigh", "max"]
    );
    for model in ["gpt-5", "gpt-5.4", "gpt-5.6-sol"] {
        assert_eq!(
            openai["models"][model]["fast_mode"].as_bool(),
            Some(true),
            "{model} must expose OpenAI priority service tier"
        );
    }
    assert_eq!(openai["models"]["o3"]["fast_mode"].as_bool(), Some(false));
    assert!(atelier_config::defaults::built_in_model_supports_fast_mode(
        "gpt-5.6-sol"
    ));
    assert!(!atelier_config::defaults::built_in_model_supports_fast_mode("o3"));
    assert!(!atelier_config::defaults::built_in_model_supports_fast_mode("gpt-unknown"));

    let anthropic =
        std::fs::read_to_string(home.path().join("models/default/anthropic.toml")).unwrap();
    let anthropic: toml::Value = toml::from_str(&anthropic).unwrap();
    assert_eq!(
        anthropic["models"]["claude-opus-4-7"]["reasoning_efforts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>(),
        ["low", "medium", "high", "xhigh", "max"]
    );

    for relative in [
        "contexts/default/main.md",
        "contexts/default/subagent.md",
        "contexts/default/goal/planner.md",
        "contexts/default/compaction/developer.md",
    ] {
        let prompt = std::fs::read_to_string(home.path().join(relative)).unwrap();
        let lowercase = prompt.to_ascii_lowercase();
        assert!(!lowercase.contains("xai"), "xAI branding in {relative}");
        assert!(!lowercase.contains("x.ai"), "x.ai branding in {relative}");
        assert!(
            !lowercase.contains("grok build"),
            "Grok branding in {relative}"
        );
    }
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
fn reset_restores_only_built_in_model_and_context_defaults() {
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
        home.path().join("models/default/openai.toml"),
        "user default models\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("contexts/default/main.md"),
        "user default context\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("models/default/stale-user-file.toml"),
        "stale default preset\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("models/providers/example")).unwrap();
    std::fs::write(
        home.path().join("models/providers/example/models.toml"),
        "user provider models\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("credentials/oauth/providers/example")).unwrap();
    std::fs::write(
        home.path()
            .join("credentials/oauth/providers/example/credential.json"),
        "user credential\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("cache/providers/example")).unwrap();
    std::fs::write(
        home.path().join("cache/providers/example/models.json"),
        "user cache\n",
    )
    .unwrap();

    reset_user_defaults(home.path()).unwrap();

    assert_eq!(
        std::fs::read_to_string(home.path().join("config.toml")).unwrap(),
        "model = \"proxy/custom\"\ncontext = \"custom\"\nrequest_agent = \"pi\"\n"
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("request-agents.toml")).unwrap(),
        "custom\n"
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("branding/logo.txt")).unwrap(),
        "custom logo\n"
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("providers.toml")).unwrap(),
        "user providers\n"
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("roles.toml")).unwrap(),
        "user roles\n"
    );
    let default_models =
        std::fs::read_to_string(home.path().join("models/default/openai.toml")).unwrap();
    assert!(default_models.contains("[models.\"gpt-5.4\"]"));
    assert!(default_models.contains("context_window"));
    assert_ne!(default_models, "user default models\n");
    assert!(
        !home
            .path()
            .join("models/default/stale-user-file.toml")
            .exists(),
        "reset must replace the owned default model preset directory"
    );
    let default_context =
        std::fs::read_to_string(home.path().join("contexts/default/main.md")).unwrap();
    assert_ne!(default_context, "user default context\n");
    assert!(!default_context.trim().is_empty());
    assert_eq!(
        std::fs::read_to_string(home.path().join("models/providers/example/models.toml")).unwrap(),
        "user provider models\n"
    );
    assert_eq!(
        std::fs::read_to_string(
            home.path()
                .join("credentials/oauth/providers/example/credential.json")
        )
        .unwrap(),
        "user credential\n"
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("cache/providers/example/models.json")).unwrap(),
        "user cache\n"
    );
}

#[test]
fn reset_preflights_all_owned_directories_before_removing_anything() {
    let home = tempfile::tempdir().unwrap();
    ensure_user_defaults(home.path(), "1.0.0").unwrap();
    let marker = home.path().join("models/default/keep-on-error.toml");
    std::fs::write(&marker, "keep\n").unwrap();
    std::fs::remove_dir_all(home.path().join("contexts/default")).unwrap();
    std::fs::write(home.path().join("contexts/default"), "not a directory\n").unwrap();

    let error = reset_user_defaults(home.path()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        marker.exists(),
        "preflight failure must not partially reset models"
    );
}

#[test]
fn reset_does_not_modify_main_config_or_other_settings() {
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

    reset_user_defaults(home.path()).unwrap();

    let config: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.path().join("config.toml")).unwrap()).unwrap();
    assert_eq!(config["model"].as_str(), Some("proxy/custom"));
    assert_eq!(config["context"].as_str(), Some("custom"));
    assert_eq!(config["request_agent"].as_str(), Some("pi"));
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
