// Per-test-case module for the `pty_e2e_smoke` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// `/provider list` renders only a user-facing Provider summary. The internal
/// RPC name, connection JSON, credentials, and model catalog stay hidden.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn provider_list_hides_internal_json_and_models() {
    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    inject_keys_paced(&mut harness, b"/provider list");
    harness.inject_keys(b"\r").expect("submit Provider list");
    harness
        .wait_for_text("PTY mock (mock) | None | enabled", Duration::from_secs(10))
        .expect("Provider summary");

    let screen = harness.screen_contents();
    assert!(screen.contains("Providers"), "{screen}");
    assert!(!screen.contains("_atelier/provider/list"), "{screen}");
    assert!(!screen.contains("base_url"), "{screen}");
    assert!(!screen.contains("credential"), "{screen}");
    assert!(!screen.contains("context_window"), "{screen}");
    assert!(!screen.contains("chat_completions"), "{screen}");
    assert!(!screen.contains("\"models\""), "{screen}");

    harness.quit().expect("clean quit");
}

/// The real slash composer walks Provider → model, then persists the exact
/// composite key without requiring a live Session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn model_picker_persists_provider_scoped_model() {
    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    inject_keys_paced(&mut harness, b"/model ");
    harness
        .wait_for_text("mock", Duration::from_secs(10))
        .expect("Provider phase");
    harness.inject_keys(b"\t").expect("accept Provider");
    harness
        .wait_for_text("mock/test-model", Duration::from_secs(10))
        .expect("model phase");
    inject_keys_paced(&mut harness, b"test-model");
    tokio::time::sleep(Duration::from_millis(250)).await;
    inject_keys_paced(&mut harness, b"\r");
    harness.update(Duration::from_millis(500));

    let config_path = content.home().join(".atelier").join("config.toml");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let config = loop {
        let source = std::fs::read_to_string(&config_path).expect("read config.toml");
        if source.contains("model = \"mock/test-model\"") {
            break source;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "model selection was not persisted:\n{source}\nraw output:\n{}",
            String::from_utf8_lossy(harness.raw_output())
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(config.contains("model = \"mock/test-model\""));

    harness.quit().expect("clean quit");
}

/// Submitting the exact `/provider add` command opens the connection wizard.
/// This covers the real composer → slash dispatch → modal path rather than
/// invoking `ProviderCommand::run` directly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn provider_add_opens_wizard() {
    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness.set_respond_to_queries(true);

    if let Err(error) = harness.wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT) {
        panic!(
            "welcome text: {error}\nraw output:\n{}",
            String::from_utf8_lossy(harness.raw_output())
        );
    }
    inject_keys_paced(&mut harness, b"/provider add");
    harness.inject_keys(b"\r").expect("submit Provider add");

    harness
        .wait_for_text("Add Provider", Duration::from_secs(10))
        .expect("Provider wizard title");
    harness
        .wait_for_text("Select Provider", Duration::from_secs(10))
        .expect("known Provider picker");
    let screen = harness.screen_contents();
    assert!(!screen.contains("Usage: /provider add"), "{screen}");
    assert!(
        !screen.to_ascii_lowercase().contains("protocol"),
        "{screen}"
    );
    assert!(screen.contains("OpenAI"), "{screen}");
    assert!(screen.contains("Custom endpoint"), "{screen}");
    let private_provider_name = ["all", "m"].concat();
    assert!(
        !screen.to_ascii_lowercase().contains(&private_provider_name),
        "{screen}"
    );

    harness
        .inject_keys(b"\x1b[B")
        .expect("select OpenAI after Custom endpoint");
    harness
        .inject_keys(b"\r")
        .expect("select known OpenAI Provider");
    harness
        .wait_for_text("Authentication method", Duration::from_secs(10))
        .expect("known Provider authentication step");
    let known_screen = harness.screen_contents();
    assert!(known_screen.contains("ChatGPT Plus/Pro"), "{known_screen}");
    assert!(known_screen.contains("OAuth"), "{known_screen}");
    assert!(known_screen.contains("OpenAI API key"), "{known_screen}");
    assert!(
        !known_screen.contains("Credential command"),
        "{known_screen}"
    );
    assert!(!known_screen.contains("Base URL"), "{known_screen}");
    assert!(!known_screen.contains("x-api-key"), "{known_screen}");
    harness.inject_keys(b"\r").expect("select ChatGPT OAuth");
    harness
        .wait_for_text("Review", Duration::from_secs(10))
        .expect("OAuth review step");
    let oauth_review = harness.screen_contents();
    assert!(
        oauth_review.contains("Provider-managed OAuth"),
        "{oauth_review}"
    );
    assert!(
        !oauth_review.contains("Credential command"),
        "{oauth_review}"
    );
    assert!(!oauth_review.contains("OAuth client ID"), "{oauth_review}");
    harness
        .inject_keys(b"\x1b[Z")
        .expect("return to login method selection");
    harness
        .wait_for_text("Authentication method", Duration::from_secs(10))
        .expect("login method selection after Back");
    harness
        .inject_keys(b"\x1b[Z")
        .expect("return to Provider selection");
    harness
        .wait_for_text("Select Provider", Duration::from_secs(10))
        .expect("Provider selection after Back");

    harness
        .inject_keys(b"\x1b[A")
        .expect("select Custom endpoint first");
    harness
        .inject_keys(b"\r")
        .expect("open custom Provider flow");
    harness
        .wait_for_text("Provider ID", Duration::from_secs(10))
        .expect("custom Provider ID step");
    harness
        .inject_keys(b"\r")
        .expect("submit empty Provider ID");
    harness
        .wait_for_text("Provider ID is required", Duration::from_secs(10))
        .expect("inline validation error");

    inject_keys_paced(&mut harness, b"mock");
    harness
        .inject_keys(b"\r")
        .expect("submit existing Provider ID");
    harness
        .wait_for_text("Provider already exists", Duration::from_secs(10))
        .expect("replacement confirmation");
    harness
        .wait_for_text("Choose another Provider", Duration::from_secs(10))
        .expect("safe replacement default");
    harness
        .wait_for_text("Replace existing Provider", Duration::from_secs(10))
        .expect("explicit replacement choice");

    harness
        .inject_keys(b"\x1b")
        .expect("cancel Provider wizard");
    tokio::time::sleep(Duration::from_millis(250)).await;
    let one_line = format!("/provider add ptyline {} none none", content.url());
    inject_keys_paced(&mut harness, one_line.as_bytes());
    harness
        .inject_keys(b"\r")
        .expect("submit advanced one-line Provider command");

    let providers_path = content.home().join(".atelier").join("providers.toml");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let providers = loop {
        let source = std::fs::read_to_string(&providers_path).expect("read providers.toml");
        if source.contains("[providers.ptyline]") {
            break source;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "one-line Provider command was not persisted:\n{source}\nraw output:\n{}",
            String::from_utf8_lossy(harness.raw_output())
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(!providers.contains("protocol"));
    let registry = atelier_provider::ProviderRegistry::load_or_create(&providers_path)
        .expect("reload one-line Provider config");
    assert!(matches!(
        registry.provider("ptyline").map(|provider| &provider.auth),
        Some(atelier_provider::ProviderAuth::None)
    ));

    harness.quit().expect("clean quit");
}
