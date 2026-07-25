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
        r#"schema_version = 2

[providers.allm]
display_name = "AllM"
protocol = "open_ai_chat_completions"
base_url = "https://allm.example/v1"
enabled = true

[providers.allm.credential]
type = "none"

[providers.allm.discovery]
type = "open_ai_models"
path = "models"
"#,
    );
    write(
        &home.path().join("roles.toml"),
        r#"schema_version = 1

[roles.planner]
provider = "allm"
model = "deepseek-v4-flash"
effort = "high"
fast_mode = true
"#,
    );

    let registry = ProviderRegistry::load_or_create(home.path().join("providers.toml")).unwrap();
    assert_eq!(registry.providers().count(), 1);
    let planner = registry.role(RoleId::Planner).unwrap();
    assert_eq!(planner.provider, "allm");
    assert_eq!(planner.model, "deepseek-v4-flash");
}

#[test]
fn model_wire_api_is_provider_scoped_and_falls_back_to_provider_protocol() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 2

[providers.responses]
display_name = "Responses"
protocol = "open_ai_responses"
base_url = "https://responses.example/v1"
enabled = true

[providers.chat]
display_name = "Chat"
protocol = "open_ai_chat_completions"
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
    assert_eq!(chat.source, WireApiSource::ProviderDefault);
}

#[test]
fn common_model_preset_supplies_context_effort_and_fast_mode() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 2

[providers.allm]
display_name = "AllM"
protocol = "open_ai_chat_completions"
base_url = "https://allm.example/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/default/deepseek.toml"),
        r#"schema_version = 1

[[models]]
match = "deepseek-*"
wire_api = "chat_completions"
context_window = 131072
reasoning_efforts = ["low", "medium", "high"]
default_effort = "medium"
fast_mode = true
"#,
    );
    write(
        &home.path().join("models/providers/allm/models.toml"),
        r#"schema_version = 1

[models."deepseek-v4-flash"]
"#,
    );

    let registry = ProviderRegistry::load_or_create(home.path().join("providers.toml")).unwrap();
    let model = registry
        .model(&ModelKey::new("allm", "deepseek-v4-flash").unwrap())
        .unwrap();
    assert_eq!(model.context_window, Some(131072));
    assert_eq!(model.reasoning_efforts, ["low", "medium", "high"]);
    assert_eq!(model.default_effort.as_deref(), Some("medium"));
    assert!(model.fast_mode);
}

#[test]
fn provider_model_profile_exposes_experimental_endpoints() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 2

[providers.openai]
display_name = "OpenAI"
protocol = "open_ai_responses"
base_url = "https://api.openai.com/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/providers/openai/models.toml"),
        r#"schema_version = 1

[models."gpt-5.4"]
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
        r#"schema_version = 2

[providers.alpha]
display_name = "Alpha"
protocol = "open_ai_responses"
base_url = "https://alpha.example.test/v1"
enabled = true

[providers.beta]
display_name = "Beta"
protocol = "open_ai_chat_completions"
base_url = "https://beta.example.test/v1"
enabled = true

[providers.messages]
display_name = "Messages"
protocol = "anthropic_messages"
base_url = "https://messages.example.test/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/providers/alpha/models.toml"),
        r#"schema_version = 1

[models."shared"]
context_window = 128000

[models."shared".experimental.image_generation]
enabled = true
endpoint = "images/generations"

[models."disabled"]
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
context_window = 128000
"#,
    );
    write(
        &home.path().join("models/providers/messages/models.toml"),
        r#"schema_version = 1

[models."shared"]
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
        r#"schema_version = 2

[providers.responses]
display_name = "Responses"
protocol = "open_ai_responses"
base_url = "https://responses.example.test/v1"
enabled = true

[providers.chat]
display_name = "Chat"
protocol = "open_ai_chat_completions"
base_url = "https://chat.example.test/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/providers/responses/models.toml"),
        r#"schema_version = 1

[models."shared"]
context_window = 128000

[models."shared".experimental.remote_compaction]
enabled = true
endpoint = "responses/compact"

[models."disabled"]
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
            r#"schema_version = 2

[providers.openai]
display_name = "OpenAI"
protocol = "open_ai_responses"
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
fn common_defaults_cannot_enable_experimental_provider_endpoints() {
    let home = tempdir().unwrap();
    write(
        &home.path().join("providers.toml"),
        r#"schema_version = 2

[providers.openai]
display_name = "OpenAI"
protocol = "open_ai_responses"
base_url = "https://api.openai.com/v1"
enabled = true
"#,
    );
    write(
        &home.path().join("models/default/openai.toml"),
        r#"schema_version = 1

[[models]]
match = "gpt-*"

[models.experimental.remote_compaction]
enabled = true
endpoint = "responses/compact"
"#,
    );

    let error = ProviderRegistry::load_or_create(home.path().join("providers.toml"))
        .expect_err("common defaults must not activate experimental endpoints");
    assert!(error.to_string().contains("provider-specific"));
}
