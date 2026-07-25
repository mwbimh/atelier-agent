//! Runtime control-plane E2E tests.
//!
//! These tests use only `MockInferenceServer`. They are ignored by default
//! because the stdio harness requires a pre-built Atelier binary.

use std::collections::BTreeMap;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use atelier_provider::{
    CredentialRef, ModelCapabilities, ModelDescriptor, ModelKey, ModelSource, ProviderConfig,
    ProviderDiscovery, ProviderProtocol, ProviderRegistry, RoleConfig, RoleId, WireApi,
};
use atelier_test_support::{AtelierStdioClient, MockInferenceServer};
use tempfile::TempDir;
use url::Url;

fn configure_mock_runtime(home: &TempDir, server: &MockInferenceServer) {
    let atelier_home = home.path().join(".atelier");
    std::fs::create_dir_all(&atelier_home).unwrap();
    let path = atelier_home.join("providers.toml");
    let mut registry = ProviderRegistry::load_or_create(&path).unwrap();
    registry
        .upsert_provider(ProviderConfig {
            id: "mock".to_owned(),
            display_name: "Mock inference".to_owned(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: Url::parse(&server.url()).unwrap(),
            credential: CredentialRef::None,
            discovery: ProviderDiscovery::Static,
            extra_headers: BTreeMap::new(),
            enabled: true,
        })
        .unwrap();
    registry
        .upsert_model(ModelDescriptor {
            key: ModelKey::new("mock", "test-model").unwrap(),
            display_name: "Mock test model".to_owned(),
            description: None,
            wire_api: Some(WireApi::ChatCompletions),
            context_window: Some(128_000),
            capabilities: ModelCapabilities {
                tool_calls: true,
                parallel_tool_calls: true,
                ..ModelCapabilities::default()
            },
            reasoning_efforts: Vec::new(),
            default_effort: None,
            fast_mode: false,
            source: ModelSource::Static,
            enabled: true,
        })
        .unwrap();
    for role in RoleId::ALL {
        registry
            .update_role(role, RoleConfig::new("mock", "test-model").unwrap())
            .unwrap();
    }
    registry.save().unwrap();
}

fn init_git_repo(path: &std::path::Path) {
    let run = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git must be available for worktree E2E");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["init"]);
    run(&["config", "user.email", "atelier-tests@example.invalid"]);
    run(&["config", "user.name", "Atelier Tests"]);
    std::fs::write(path.join("README.md"), "# worktree test\n").unwrap();
    run(&["add", "README.md"]);
    run(&["commit", "-m", "initial"]);
}

fn response_json(response: agent_client_protocol::ExtResponse) -> serde_json::Value {
    serde_json::from_str(response.0.get()).unwrap()
}

fn inference_request_count(server: &MockInferenceServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|request| {
            request.method == "POST"
                && matches!(
                    request.path.as_str(),
                    "/v1/chat/completions" | "/v1/responses" | "/v1/messages"
                )
        })
        .count()
}

fn agent_inference_request_count(server: &MockInferenceServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|request| {
            request.method == "POST"
                && request
                    .body
                    .as_ref()
                    .and_then(|body| body.get("tools"))
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|tools| tools.len() >= 2)
        })
        .count()
}

async fn wait_for(description: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a pre-built Atelier binary"]
async fn btw_is_one_tool_free_request_and_does_not_mutate_parent_history() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("side answer");
            server.set_chunk_delay(Some(Duration::from_millis(200)));
            let workdir = tempfile::tempdir().unwrap();
            let home = TempDir::new().unwrap();
            configure_mock_runtime(&home, &server);
            let client =
                Rc::new(AtelierStdioClient::spawn_with_home(&server, workdir.path(), home).await);
            client.initialize_with_timeout().await;
            let session_id = client.create_session_with_timeout(workdir.path()).await;

            let before = response_json(
                client
                    .ext_method(
                        "_atelier/context_snapshot/create",
                        serde_json::json!({"sessionId": session_id.0.as_ref()}),
                    )
                    .await
                    .unwrap(),
            );
            let baseline_requests = inference_request_count(&server);

            let btw_client = Rc::clone(&client);
            let btw_session = session_id.clone();
            let btw_request = tokio::task::spawn_local(async move {
                btw_client
                    .ext_method(
                        "_atelier/btw/ask",
                        serde_json::json!({
                            "sessionId": btw_session.0.as_ref(),
                            "question": "what is happening?",
                            "persist": false
                        }),
                    )
                    .await
            });
            wait_for("running side-query request", || {
                inference_request_count(&server) > baseline_requests
            })
            .await;
            let running_tasks = response_json(
                client
                    .ext_method(
                        "_atelier/task/list",
                        serde_json::json!({"sessionId": session_id.0.as_ref()}),
                    )
                    .await
                    .unwrap(),
            );
            let running_btw = running_tasks["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|task| {
                    task["attachable"] == false
                        && task["taskId"]
                            .as_str()
                            .is_some_and(|id| id.starts_with("btw-"))
                })
                .expect("running side query must be visible in the task registry");
            assert_ne!(running_btw["state"], "completed");
            assert_ne!(running_btw["state"], "failed");

            let btw = response_json(
                tokio::time::timeout(Duration::from_secs(10), btw_request)
                    .await
                    .expect("side query timed out")
                    .unwrap()
                    .unwrap(),
            );

            assert_eq!(btw["answer"], "side answer");
            assert_eq!(inference_request_count(&server), baseline_requests + 1);
            let request = server
                .requests()
                .into_iter()
                .filter(|request| request.method == "POST")
                .last()
                .expect("side-query inference request");
            let body = request.body.expect("side-query request body");
            assert!(
                body.get("tools")
                    .and_then(serde_json::Value::as_array)
                    .is_none_or(Vec::is_empty),
                "side query must not advertise tools: {body}"
            );

            let after = response_json(
                client
                    .ext_method(
                        "_atelier/context_snapshot/create",
                        serde_json::json!({"sessionId": session_id.0.as_ref()}),
                    )
                    .await
                    .unwrap(),
            );
            assert_eq!(before["items"], after["items"]);

            let task = response_json(
                client
                    .ext_method(
                        "_atelier/task/get",
                        serde_json::json!({"taskId": btw["btwId"]}),
                    )
                    .await
                    .unwrap(),
            );
            assert_eq!(task["task"]["state"], "completed");
            assert_eq!(task["task"]["attachable"], false);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a pre-built Atelier binary"]
async fn detached_turn_continues_and_attach_replay_is_cursor_strict() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            server.set_agent_turns(["detached answer".to_owned()]);
            server.hold_agent_completions();
            let workdir = tempfile::tempdir().unwrap();
            let home = TempDir::new().unwrap();
            configure_mock_runtime(&home, &server);
            let client =
                Rc::new(AtelierStdioClient::spawn_with_home(&server, workdir.path(), home).await);
            client.initialize_with_timeout().await;
            let session_id = client.create_session_with_timeout(workdir.path()).await;
            let baseline_agent_requests = agent_inference_request_count(&server);

            let prompt_client = Rc::clone(&client);
            let prompt_session = session_id.clone();
            let prompt = tokio::task::spawn_local(async move {
                prompt_client
                    .prompt(&prompt_session, "keep running in background")
                    .await
            });
            wait_for("held inference request", || {
                agent_inference_request_count(&server) > baseline_agent_requests
            })
            .await;

            let tasks = response_json(
                client
                    .ext_method(
                        "_atelier/task/list",
                        serde_json::json!({"sessionId": session_id.0.as_ref()}),
                    )
                    .await
                    .unwrap(),
            );
            let task_id = tasks["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|task| task["attachable"] == true)
                .and_then(|task| task["taskId"].as_str())
                .expect("active attachable main task")
                .to_owned();

            let detached = response_json(
                client
                    .ext_method(
                        "_atelier/task/detach",
                        serde_json::json!({"taskId": task_id}),
                    )
                    .await
                    .unwrap(),
            );
            assert_eq!(detached["detached"], true);
            let prompt_response = tokio::time::timeout(Duration::from_secs(5), prompt)
                .await
                .expect("foreground prompt must return at detach")
                .unwrap()
                .unwrap();
            assert_eq!(
                prompt_response
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.get("detached"))
                    .and_then(serde_json::Value::as_bool),
                Some(true)
            );

            let active = response_json(
                client
                    .ext_method("_atelier/task/get", serde_json::json!({"taskId": task_id}))
                    .await
                    .unwrap(),
            );
            assert_ne!(active["task"]["state"], "completed");
            assert_ne!(active["task"]["state"], "failed");

            server.release_agent_completions();
            let completion_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                let task = response_json(
                    client
                        .ext_method("_atelier/task/get", serde_json::json!({"taskId": task_id}))
                        .await
                        .unwrap(),
                );
                if task["task"]["state"] == "completed" {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < completion_deadline,
                    "detached task did not complete: {task}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }

            let first = response_json(
                client
                    .ext_method(
                        "_atelier/task/attach",
                        serde_json::json!({"taskId": task_id, "afterEventId": 0}),
                    )
                    .await
                    .unwrap(),
            );
            let first_ids = first["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|event| event["eventId"].as_u64())
                .collect::<Vec<_>>();
            assert!(
                first_ids.len() >= 2,
                "completed detached task must retain multiple lifecycle events: {first}"
            );
            let replay_after = first_ids[0];
            let expected_replay = first_ids
                .iter()
                .copied()
                .filter(|event_id| *event_id > replay_after)
                .collect::<Vec<_>>();

            let second = response_json(
                client
                    .ext_method(
                        "_atelier/task/attach",
                        serde_json::json!({"taskId": task_id, "afterEventId": replay_after}),
                    )
                    .await
                    .unwrap(),
            );
            let second_ids = second["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|event| event["eventId"].as_u64())
                .collect::<Vec<_>>();
            assert_eq!(second_ids, expected_replay);
            assert!(!second_ids.contains(&replay_after));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a pre-built Atelier binary"]
async fn task_cancel_terminates_inference_and_leaves_the_session_reusable() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            server.set_agent_turns(["must not finish normally".to_owned()]);
            server.hold_agent_completions();
            let workdir = tempfile::tempdir().unwrap();
            let home = TempDir::new().unwrap();
            configure_mock_runtime(&home, &server);
            let client =
                Rc::new(AtelierStdioClient::spawn_with_home(&server, workdir.path(), home).await);
            client.initialize_with_timeout().await;
            let session_id = client.create_session_with_timeout(workdir.path()).await;
            let baseline_agent_requests = agent_inference_request_count(&server);

            let prompt_client = Rc::clone(&client);
            let prompt_session = session_id.clone();
            let prompt = tokio::task::spawn_local(async move {
                prompt_client
                    .prompt(&prompt_session, "cancel this turn")
                    .await
            });
            wait_for("held cancellable inference request", || {
                agent_inference_request_count(&server) > baseline_agent_requests
            })
            .await;

            let tasks = response_json(
                client
                    .ext_method(
                        "_atelier/task/list",
                        serde_json::json!({"sessionId": session_id.0.as_ref()}),
                    )
                    .await
                    .unwrap(),
            );
            let task_id = tasks["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|task| task["attachable"] == true)
                .and_then(|task| task["taskId"].as_str())
                .expect("active cancellable main task")
                .to_owned();

            let cancelled = response_json(
                client
                    .ext_method(
                        "_atelier/task/cancel",
                        serde_json::json!({"taskId": task_id}),
                    )
                    .await
                    .unwrap(),
            );
            assert_eq!(cancelled["cancelled"], true);
            let response = tokio::time::timeout(Duration::from_secs(5), prompt)
                .await
                .expect("cancelled inference must release the foreground request")
                .unwrap()
                .unwrap();
            assert_eq!(
                response.stop_reason,
                agent_client_protocol::StopReason::Cancelled
            );

            let task = response_json(
                client
                    .ext_method("_atelier/task/get", serde_json::json!({"taskId": task_id}))
                    .await
                    .unwrap(),
            );
            assert_eq!(task["task"]["state"], "completed");
            assert_eq!(task["task"]["diagnosticMessage"], "cancelled by client");

            server.release_agent_completions();
            server.set_response("session still works");
            let next = client
                .prompt_with_timeout(&session_id, "next turn")
                .await
                .expect("session must remain usable after task cancellation");
            assert_eq!(next.stop_reason, agent_client_protocol::StopReason::EndTurn);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a pre-built Atelier binary"]
async fn derived_agent_worktree_isolation_uses_a_real_independent_checkout() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            let workdir = tempfile::tempdir().unwrap();
            init_git_repo(workdir.path());
            let home = TempDir::new().unwrap();
            configure_mock_runtime(&home, &server);
            let client =
                Rc::new(AtelierStdioClient::spawn_with_home(&server, workdir.path(), home).await);
            client.initialize_with_timeout().await;
            let session_id = client.create_session_with_timeout(workdir.path()).await;

            let spawned = response_json(
                client
                    .ext_method(
                        "_atelier/agent/spawn_derived",
                        serde_json::json!({
                            "sessionId": session_id.0.as_ref(),
                            "role": "explore",
                            "prompt": "",
                            "fresh": true,
                            "background": false,
                            "isolation": "worktree"
                        }),
                    )
                    .await
                    .unwrap(),
            );
            assert_eq!(spawned["isolation"], "worktree");
            let worktree_path = std::path::PathBuf::from(
                spawned["worktreePath"]
                    .as_str()
                    .expect("worktree response path"),
            );
            assert!(worktree_path.is_dir(), "worktree directory must exist");
            assert_ne!(
                std::fs::canonicalize(&worktree_path).unwrap(),
                std::fs::canonicalize(workdir.path()).unwrap(),
                "derived session must not reuse the source workspace"
            );
            assert!(
                worktree_path.join("README.md").is_file(),
                "derived worktree must contain the committed source checkout"
            );

            atelier_fast_worktree::remove_worktree(&worktree_path).unwrap();
        })
        .await;
}
