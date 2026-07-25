use atelier_config::runtime_defaults::{
    ContextPrompt, install_runtime_defaults_at, load_context_prompt_at,
    load_context_role_prompt_at, load_logo_at, merge_role_prompts, resolve_runtime_defaults_at,
    runtime_context_prompt, runtime_logo, update_default_model_at,
};

#[test]
fn selected_context_preset_loads_each_prompt_kind() {
    let home = tempfile::tempdir().unwrap();
    atelier_config::defaults::ensure_user_defaults(home.path(), "1.2.3").unwrap();
    std::fs::create_dir_all(home.path().join("contexts/custom/goal")).unwrap();
    std::fs::create_dir_all(home.path().join("contexts/custom/compaction")).unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "model = \"proxy/custom\"\ncontext = \"custom\"\nrequest_agent = \"pi\"\n",
    )
    .unwrap();

    for (kind, relative) in [
        (ContextPrompt::Main, "main.md"),
        (ContextPrompt::Subagent, "subagent.md"),
        (ContextPrompt::ApplyPatch, "apply_patch.md"),
        (ContextPrompt::GoalPlanner, "goal/planner.md"),
        (ContextPrompt::GoalStrategist, "goal/strategist.md"),
        (ContextPrompt::GoalSkeptic, "goal/skeptic.md"),
        (ContextPrompt::GoalSummary, "goal/summary.md"),
        (
            ContextPrompt::CompactionDeveloper,
            "compaction/developer.md",
        ),
        (ContextPrompt::CompactionUser, "compaction/user.md"),
    ] {
        let path = home.path().join("contexts/custom").join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let expected = format!("custom:{relative}\n");
        std::fs::write(path, &expected).unwrap();
        assert_eq!(
            load_context_prompt_at(home.path(), "custom", kind).unwrap(),
            expected
        );
    }

    let resolved = resolve_runtime_defaults_at(home.path()).unwrap();
    assert_eq!(resolved.model.as_deref(), Some("proxy/custom"));
    assert_eq!(resolved.context, "custom");
    assert_eq!(resolved.request_agent.id, "pi");
    assert_eq!(resolved.request_agent.name, "pi");
    assert_eq!(resolved.request_agent.version.as_deref(), Some("0.82.1"));
    assert!(
        resolved
            .request_agent
            .user_agent_value()
            .starts_with("pi/0.82.1 (")
    );
}

#[test]
fn context_role_prompt_is_optional_safe_and_merged_after_the_generic_subagent_prompt() {
    let home = tempfile::tempdir().unwrap();
    atelier_config::defaults::ensure_user_defaults(home.path(), "1.0.0").unwrap();

    assert_eq!(
        load_context_role_prompt_at(home.path(), "default", "review")
            .unwrap()
            .as_deref(),
        Some(
            "Review the assigned change for correctness, regressions, security issues, and missing verification. Report concrete findings first.\n"
        )
    );
    assert_eq!(
        load_context_role_prompt_at(home.path(), "default", "custom").unwrap(),
        None
    );
    let merged = merge_role_prompts(
        Some("Context review instructions.\n"),
        Some("Workspace-specific review instructions.\n"),
    )
    .unwrap();
    assert_eq!(
        merged,
        "Context review instructions.\n\nWorkspace-specific review instructions.\n"
    );

    let error = load_context_role_prompt_at(home.path(), "default", "../outside")
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid context role"), "{error}");
}

#[test]
fn context_name_cannot_escape_the_contexts_directory() {
    let home = tempfile::tempdir().unwrap();
    atelier_config::defaults::ensure_user_defaults(home.path(), "1.0.0").unwrap();
    let error = load_context_prompt_at(home.path(), "../outside", ContextPrompt::Main)
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid context preset"), "{error}");
}

#[test]
fn unknown_request_agent_is_an_explicit_configuration_error() {
    let home = tempfile::tempdir().unwrap();
    atelier_config::defaults::ensure_user_defaults(home.path(), "1.0.0").unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "model = \"proxy/custom\"\ncontext = \"default\"\nrequest_agent = \"missing\"\n",
    )
    .unwrap();

    let error = resolve_runtime_defaults_at(home.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("missing"), "{error}");
    assert!(error.contains("request-agents.toml"), "{error}");
}

#[test]
fn logo_is_loaded_from_the_user_branding_file() {
    let home = tempfile::tempdir().unwrap();
    atelier_config::defaults::ensure_user_defaults(home.path(), "1.0.0").unwrap();
    std::fs::write(home.path().join("branding/logo.txt"), "CUSTOM ATE\n").unwrap();
    assert_eq!(load_logo_at(home.path()).unwrap(), "CUSTOM ATE\n");
}

#[test]
fn missing_model_keeps_first_run_unconfigured() {
    let home = tempfile::tempdir().unwrap();
    atelier_config::defaults::ensure_user_defaults(home.path(), "1.0.0").unwrap();

    let resolved = resolve_runtime_defaults_at(home.path()).unwrap();

    assert_eq!(resolved.model, None);
    assert_eq!(resolved.context, "default");
    assert_eq!(resolved.request_agent.id, "atelier");
}

#[test]
fn model_must_be_a_provider_model_composite_key() {
    let home = tempfile::tempdir().unwrap();
    atelier_config::defaults::ensure_user_defaults(home.path(), "1.0.0").unwrap();
    for invalid in ["", "model-only", "/model", "provider/", "a/b/c"] {
        std::fs::write(
            home.path().join("config.toml"),
            format!("model = {invalid:?}\ncontext = \"default\"\nrequest_agent = \"atelier\"\n"),
        )
        .unwrap();
        let error = resolve_runtime_defaults_at(home.path())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("provider/model"),
            "invalid={invalid:?}: {error}"
        );
    }
}

#[test]
fn updating_default_model_is_atomic_and_preserves_other_config() {
    let home = tempfile::tempdir().unwrap();
    atelier_config::defaults::ensure_user_defaults(home.path(), "1.0.0").unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "model = \"old/model\"\ncontext = \"default\"\nrequest_agent = \"atelier\"\n\n[ui]\ntheme = \"ateliernight\"\n",
    )
    .unwrap();
    let resolved = update_default_model_at(home.path(), "new/model").unwrap();
    assert_eq!(resolved.model.as_deref(), Some("new/model"));
    let parsed: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.path().join("config.toml")).unwrap()).unwrap();
    assert_eq!(parsed["model"].as_str(), Some("new/model"));
    assert_eq!(parsed["context"].as_str(), Some("default"));
    assert_eq!(parsed["request_agent"].as_str(), Some("atelier"));
    assert_eq!(parsed["ui"]["theme"].as_str(), Some("ateliernight"));
}

#[test]
fn installed_runtime_uses_the_selected_context_logo_and_request_agent() {
    let home = tempfile::tempdir().unwrap();
    atelier_config::defaults::ensure_user_defaults(home.path(), "1.2.3").unwrap();

    for kind in ContextPrompt::ALL {
        let source = home
            .path()
            .join("contexts/default")
            .join(kind.relative_path());
        let target = home
            .path()
            .join("contexts/custom")
            .join(kind.relative_path());
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(source, target).unwrap();
    }
    std::fs::write(
        home.path().join("contexts/custom/main.md"),
        "CUSTOM RUNTIME MAIN\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("branding/logo.txt"),
        "CUSTOM RUNTIME LOGO\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "model = \"proxy/custom\"\ncontext = \"custom\"\nrequest_agent = \"pi\"\n",
    )
    .unwrap();

    let installed = install_runtime_defaults_at(home.path()).unwrap();
    assert_eq!(installed.context, "custom");
    assert!(
        installed
            .request_agent
            .user_agent_value()
            .starts_with("pi/0.82.1 (")
    );
    assert_eq!(
        runtime_context_prompt(ContextPrompt::Main, "embedded"),
        "CUSTOM RUNTIME MAIN\n"
    );
    assert_eq!(runtime_logo("embedded"), "CUSTOM RUNTIME LOGO\n");
}
