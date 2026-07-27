use atelier_provider::{ModelKey, ProviderRegistry, RoleId, WireApi, WireApiSource};
use tempfile::tempdir;

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn generated_v2_tree_is_loadable_and_roles_are_separate() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 3

[providers.example]
display_name = "Example"
auth = { type = "bearer" }
base_url = "https://example.example/v1"
enabled = true

[providers.example.credential]
type = "none"

[providers.example.discovery]
type = "open_ai_models"
path = "models"
"#,
    );
    write(
        &home.path().join("roles.toml"),
        r#"schema_version = 1

[roles.planner]
provider = "example"
model = "deepseek-v4-flash"
effort = "high"
fast_mode = true
"#,
    );

    let registry = ProviderRegistry::load_or_create(home.path().join("providers.toml")).unwrap();
    assert_eq!(registry.providers().count(), 1);
    let planner = registry.role(RoleId::Planner).unwrap();
    assert_eq!(planner.provider, "example");
    assert_eq!(planner.model, "deepseek-v4-flash");
}

#[test]
fn providers_file_rejects_models_roles_and_other_non_connection_sections() {
    for forbidden in [
        "[models.\"gpt-5.4\"]\ncontext_window = 272000\n",
        "[roles.main]\nprovider = \"openai\"\nmodel = \"gpt-5.4\"\n",
        "[runtime]\nmodel = \"openai/gpt-5.4\"\n",
    ] {
        let home = tempdir().unwrap();
        write(
            &home.path().join("providers.toml"),
            &format!(
                r#"schema_version = 3

[providers.openai]
display_name = "OpenAI"
auth = {{ type = "bearer" }}
base_url = "https://api.openai.com/v1"
enabled = true

{forbidden}"#
            ),
        );

        let error = ProviderRegistry::load_or_create(home.path().join("providers.toml"))
            .expect_err("providers.toml must contain connection configuration only");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }
}

#[test]
fn providers_file_rejects_legacy_provider_protocol() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 3

[providers.openai]
display_name = "OpenAI"
base_url = "https://api.openai.com/v1"
auth = { type = "bearer" }
protocol = "open_ai_responses"
"#,
    );

    let error = ProviderRegistry::load_or_create(home.path().join("providers.toml"))
        .expect_err("Provider wire API must not be accepted in providers.toml");
    assert!(
        error.to_string().contains("unknown field `protocol`"),
        "{error}"
    );
}

#[test]
fn model_wire_api_is_provider_scoped_and_missing_pair_uses_chat_default() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 3

[providers.responses]
display_name = "Responses"
auth = { type = "bearer" }
base_url = "https://responses.example/v1"
enabled = true

[providers.chat]
display_name = "Chat"
auth = { type = "bearer" }
base_url = "https://chat.example/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/providers/responses/models.toml"),
        r#"schema_version = 1

[models."shared-model"]
wire_api = "messages"
context_window = 100000
"#,
    );
    write(
        &home.path().join("models/providers/chat/models.toml"),
        r#"schema_version = 1

[models."shared-model"]
context_window = 100000
"#,
    );

    let registry = ProviderRegistry::load_or_create(home.path().join("providers.toml")).unwrap();
    let responses_key = ModelKey::new("responses", "shared-model").unwrap();
    let chat_key = ModelKey::new("chat", "shared-model").unwrap();

    let responses = registry.resolve_wire_api(&responses_key).unwrap();
    assert_eq!(responses.wire_api, WireApi::Messages);
    assert_eq!(responses.source, WireApiSource::ProviderModelOverride);

    let chat = registry.resolve_wire_api(&chat_key).unwrap();
    assert_eq!(chat.wire_api, WireApi::ChatCompletions);
    assert_eq!(chat.source, WireApiSource::Default);
}

#[test]
fn exact_common_model_preset_supplies_context_effort_and_fast_mode_without_family_matching() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 3

[providers.example]
display_name = "Example"
auth = { type = "bearer" }
base_url = "https://example.example/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/default/deepseek.toml"),
        r#"schema_version = 2

[models."deepseek-v4-flash"]
wire_api = "chat_completions"
context_window = 1000000
reasoning_efforts = ["high", "max"]
default_effort = "high"
fast_mode = false
"#,
    );
    write(
        &home.path().join("models/providers/example/models.toml"),
        r#"schema_version = 1

[models."deepseek-v4-flash"]
"#,
    );

    let registry = ProviderRegistry::load_or_create(home.path().join("providers.toml")).unwrap();
    let model = registry
        .model(&ModelKey::new("example", "deepseek-v4-flash").unwrap())
        .unwrap();
    assert_eq!(model.context_window, Some(1_000_000));
    assert_eq!(model.reasoning_efforts, ["high", "max"]);
    assert_eq!(model.default_effort.as_deref(), Some("high"));
    assert!(!model.fast_mode);

    write(
        &home.path().join("models/providers/example/models.toml"),
        r#"schema_version = 1

[models."deepseek-v4-pro"]
"#,
    );
    let registry = ProviderRegistry::load_or_create(home.path().join("providers.toml")).unwrap();
    let unmatched = registry
        .model(&ModelKey::new("example", "deepseek-v4-pro").unwrap())
        .unwrap();
    assert_eq!(unmatched.context_window, None);
    assert!(unmatched.reasoning_efforts.is_empty());
}

#[test]
fn explicit_remote_metadata_takes_precedence_over_common_model_defaults() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 3

[providers.openai]
display_name = "OpenAI"
auth = { type = "bearer" }
base_url = "https://api.openai.com/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/default/openai.toml"),
        r#"schema_version = 2

[models."gpt-5.4"]
wire_api = "responses"
context_window = 272000

[models."gpt-5.4".capabilities]
image_input = true
tool_calls = true
"#,
    );
    write(
        &home.path().join("cache/providers/openai/models.json"),
        r#"{
  "schema_version": 1,
  "provider_id": "openai",
  "models": [{
    "key": {"provider_id": "openai", "model_id": "gpt-5.4"},
    "display_name": "Remote GPT-5.4",
    "wire_api": "messages",
    "context_window": 128000,
    "capabilities": {
      "text_input": true,
      "image_input": false,
      "tool_calls": false,
      "parallel_tool_calls": false,
      "reasoning_effort": false,
      "web_search": true,
      "image_generation": false,
      "server_compaction": false
    },
    "source": "remote",
    "enabled": true
  }]
}"#,
    );

    let registry = ProviderRegistry::load_or_create(home.path().join("providers.toml")).unwrap();
    let model = registry
        .model(&ModelKey::new("openai", "gpt-5.4").unwrap())
        .unwrap();
    assert_eq!(model.wire_api, Some(WireApi::Messages));
    assert_eq!(model.context_window, Some(128_000));
    assert!(!model.capabilities.image_input);
    assert!(!model.capabilities.tool_calls);
    assert!(model.capabilities.web_search);
    let key = ModelKey::new("openai", "gpt-5.4").unwrap();
    assert_eq!(
        registry.resolve_wire_api(&key).unwrap().wire_api,
        WireApi::Messages
    );

    write(
        &home.path().join("models/providers/openai/models.toml"),
        r#"schema_version = 1

[models."gpt-5.4"]
wire_api = "chat_completions"
"#,
    );
    let registry = ProviderRegistry::load_or_create(home.path().join("providers.toml")).unwrap();
    let resolved = registry.resolve_wire_api(&key).unwrap();
    assert_eq!(resolved.wire_api, WireApi::ChatCompletions);
    assert_eq!(resolved.source, WireApiSource::ProviderModelOverride);
}

#[test]
fn generic_remote_discovery_is_enriched_by_exact_common_capabilities() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 3

[providers.example]
display_name = "Example"
auth = { type = "bearer" }
base_url = "https://example.example/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/default/deepseek.toml"),
        r#"schema_version = 2

[models."deepseek-v4-flash".capabilities]
text_input = true
tool_calls = true
parallel_tool_calls = true
reasoning_effort = true
"#,
    );
    write(
        &home.path().join("cache/providers/example/models.json"),
        r#"{
  "schema_version": 1,
  "provider_id": "example",
  "models": [{
    "key": {"provider_id": "example", "model_id": "deepseek-v4-flash"},
    "display_name": "deepseek-v4-flash",
    "capabilities": {
      "text_input": true,
      "image_input": false,
      "tool_calls": false,
      "parallel_tool_calls": false,
      "reasoning_effort": false,
      "web_search": false,
      "image_generation": false,
      "server_compaction": false
    },
    "source": "remote",
    "enabled": true
  }]
}"#,
    );

    let registry = ProviderRegistry::load_or_create(home.path().join("providers.toml")).unwrap();
    let model = registry
        .model(&ModelKey::new("example", "deepseek-v4-flash").unwrap())
        .unwrap();
    assert!(model.capabilities.tool_calls);
    assert!(model.capabilities.parallel_tool_calls);
    assert!(model.capabilities.reasoning_effort);
}

#[test]
fn provider_model_profile_exposes_experimental_endpoints() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 3

[providers.openai]
display_name = "OpenAI"
auth = { type = "bearer" }
base_url = "https://api.openai.com/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/providers/openai/models.toml"),
        r#"schema_version = 1

[models."gpt-5.4"]
wire_api = "responses"
context_window = 400000

[models."gpt-5.4".experimental.remote_compaction]
enabled = true
endpoint = "responses/compact"

[models."gpt-5.4".experimental.image_generation]
enabled = true
endpoint = "images/generations"
"#,
    );

    let registry = ProviderRegistry::load_or_create(home.path().join("providers.toml")).unwrap();
    let key = ModelKey::new("openai", "gpt-5.4").unwrap();
    let features = registry.experimental_model_features(&key).unwrap();
    assert_eq!(
        features.remote_compaction.unwrap().endpoint,
        "responses/compact"
    );
    assert_eq!(
        features.image_generation.unwrap().endpoint,
        "images/generations"
    );

    let snapshot = registry.snapshot();
    assert_eq!(
        snapshot.experimental_model_features.get(&key.to_string()),
        Some(
            &registry
                .experimental_model_features(&key)
                .expect("exact Provider profile")
        )
    );
    assert_eq!(
        snapshot
            .resolve_remote_compaction_endpoint(&key)
            .expect("valid exact endpoint")
            .as_deref(),
        Some("responses/compact")
    );
    assert_eq!(
        snapshot
            .resolve_image_generation_endpoint(&key)
            .expect("valid exact image endpoint")
            .as_deref(),
        Some("images/generations")
    );
}

#[test]
fn image_generation_activation_is_exact_enabled_and_openai_compatible() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 3

[providers.alpha]
display_name = "Alpha"
auth = { type = "bearer" }
base_url = "https://alpha.example.test/v1"
enabled = true

[providers.beta]
display_name = "Beta"
auth = { type = "bearer" }
base_url = "https://beta.example.test/v1"
enabled = true

[providers.messages]
display_name = "Messages"
auth = { type = "header", name = "x-api-key" }
base_url = "https://messages.example.test/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/providers/alpha/models.toml"),
        r#"schema_version = 1

[models."shared"]
wire_api = "chat_completions"
context_window = 128000

[models."shared".experimental.image_generation]
enabled = true
endpoint = "images/generations"

[models."disabled"]
wire_api = "chat_completions"
context_window = 128000

[models."disabled".experimental.image_generation]
enabled = false
endpoint = "images/generations"
"#,
    );
    write(
        &home.path().join("models/providers/beta/models.toml"),
        r#"schema_version = 1

[models."shared"]
wire_api = "chat_completions"
context_window = 128000
"#,
    );
    write(
        &home.path().join("models/providers/messages/models.toml"),
        r#"schema_version = 1

[models."shared"]
wire_api = "messages"
context_window = 128000

[models."shared".experimental.image_generation]
enabled = true
endpoint = "images/generations"
"#,
    );

    let snapshot = ProviderRegistry::load_or_create(home.path().join("providers.toml"))
        .unwrap()
        .snapshot();
    let alpha = ModelKey::new("alpha", "shared").unwrap();
    let disabled = ModelKey::new("alpha", "disabled").unwrap();
    let beta = ModelKey::new("beta", "shared").unwrap();
    let messages = ModelKey::new("messages", "shared").unwrap();

    assert_eq!(
        snapshot
            .resolve_image_generation_endpoint(&alpha)
            .unwrap()
            .as_deref(),
        Some("images/generations")
    );
    assert_eq!(
        snapshot
            .resolve_image_generation_endpoint(&disabled)
            .unwrap(),
        None
    );
    assert_eq!(
        snapshot.resolve_image_generation_endpoint(&beta).unwrap(),
        None,
        "same model id under another Provider must not inherit the endpoint"
    );
    assert_eq!(
        snapshot
            .resolve_image_generation_endpoint(&messages)
            .unwrap(),
        None,
        "Anthropic Messages is not OpenAI Images-compatible"
    );
}

#[test]
fn remote_compaction_activation_is_exact_enabled_and_responses_only() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 3

[providers.responses]
display_name = "Responses"
auth = { type = "bearer" }
base_url = "https://responses.example.test/v1"
enabled = true

[providers.chat]
display_name = "Chat"
auth = { type = "bearer" }
base_url = "https://chat.example.test/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/providers/responses/models.toml"),
        r#"schema_version = 1

[models."shared"]
wire_api = "responses"
context_window = 128000

[models."shared".experimental.remote_compaction]
enabled = true
endpoint = "responses/compact"

[models."disabled"]
wire_api = "responses"
context_window = 128000

[models."disabled".experimental.remote_compaction]
enabled = false
endpoint = "responses/compact"
"#,
    );
    write(
        &home.path().join("models/providers/chat/models.toml"),
        r#"schema_version = 1

[models."shared"]
wire_api = "chat_completions"
context_window = 128000

[models."shared".experimental.remote_compaction]
enabled = true
endpoint = "responses/compact"
"#,
    );

    let snapshot = ProviderRegistry::load_or_create(home.path().join("providers.toml"))
        .unwrap()
        .snapshot();
    let responses = ModelKey::new("responses", "shared").unwrap();
    let disabled = ModelKey::new("responses", "disabled").unwrap();
    let chat = ModelKey::new("chat", "shared").unwrap();

    assert_eq!(
        snapshot
            .resolve_remote_compaction_endpoint(&responses)
            .unwrap()
            .as_deref(),
        Some("responses/compact")
    );
    assert_eq!(
        snapshot
            .resolve_remote_compaction_endpoint(&disabled)
            .unwrap(),
        None
    );
    assert_eq!(
        snapshot.resolve_remote_compaction_endpoint(&chat).unwrap(),
        None
    );
    assert!(
        !snapshot
            .experimental_model_features
            .contains_key("chat/missing"),
        "a same-named model under another Provider must not inherit an endpoint"
    );
}

#[test]
fn experimental_endpoint_rejects_origin_and_path_escape_syntax() {
    for endpoint in [
        "https://evil.example/compact",
        "/responses/compact",
        "../responses/compact",
        "responses/../compact",
        r"responses\compact",
        "responses/%2e%2e/compact",
        "responses/compact?target=evil",
        "responses/compact#fragment",
    ] {
        let home = tempdir().unwrap();
        write(
            &home.path().join("providers.toml"),
            r#"schema_version = 3

[providers.openai]
display_name = "OpenAI"
auth = { type = "bearer" }
base_url = "https://api.openai.com/v1"
enabled = true
"#,
        );
        write(
            &home.path().join("models/providers/openai/models.toml"),
            &format!(
                r#"schema_version = 1

[models."gpt-5.4"]
context_window = 400000

[models."gpt-5.4".experimental.remote_compaction]
enabled = true
endpoint = {endpoint:?}
"#
            ),
        );

        let error = ProviderRegistry::load_or_create(home.path().join("providers.toml"))
            .expect_err("unsafe endpoint must fail closed");
        assert!(
            error.to_string().contains("Provider-relative"),
            "unexpected error for {endpoint:?}: {error}"
        );
    }
}

#[test]
fn common_defaults_reject_wildcards_and_inconsistent_effort_menus() {
    for models in [
        r#"[models."gpt-5*"]
context_window = 400000
"#,
        r#"[models."gpt-5.4"]
reasoning_efforts = ["low", "high"]
default_effort = "medium"
"#,
        r#"[models."gpt-5.4"]
reasoning_efforts = ["high", "high"]
default_effort = "high"
"#,
    ] {
        let home = tempdir().unwrap();
        write(
            &home.path().join("providers.toml"),
            r#"schema_version = 3

[providers.openai]
display_name = "OpenAI"
auth = { type = "bearer" }
base_url = "https://api.openai.com/v1"
enabled = true
"#,
        );
        write(
            &home.path().join("models/default/openai.toml"),
            &format!("schema_version = 2\n\n{models}"),
        );
        write(
            &home.path().join("models/providers/openai/models.toml"),
            "schema_version = 1\n\n[models.\"gpt-5.4\"]\n",
        );

        let error = ProviderRegistry::load_or_create(home.path().join("providers.toml"))
            .expect_err("invalid exact model preset must fail closed");
        assert!(
            error.to_string().contains("default model") || error.to_string().contains("reasoning"),
            "{error}"
        );
    }
}

#[test]
fn common_defaults_cannot_enable_experimental_provider_endpoints() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 3

[providers.openai]
display_name = "OpenAI"
auth = { type = "bearer" }
base_url = "https://api.openai.com/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/default/openai.toml"),
        r#"schema_version = 2

[models."gpt-5.4"]

[models."gpt-5.4".experimental.remote_compaction]
enabled = true
endpoint = "responses/compact"
"#,
    );

    let error = ProviderRegistry::load_or_create(home.path().join("providers.toml"))
        .expect_err("common defaults must not activate experimental endpoints");
    assert!(error.to_string().contains("provider-specific"));
}
