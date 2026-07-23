//! Plan-mode edit gate through the real `prepare_tool_call` path: plan mode
//! is read-only except the plan file in EVERY permission mode. The fixture's
//! `PermissionHandle::allow_all()` is the always-approve worst case — before
//! the gate, it silently approved any edit in plan mode (the "yolo edits in
//! plan mode" bug); these tests pin that the gate rejects
//! BEFORE the permission layer can auto-approve.
use super::support::*;
use super::*;
/// Build an actor whose toolset parses atelier `search_replace` plus the plan
/// tools (so `${{ tools.by_kind.exit_plan }}` resolves in the rejection
/// message), with a gateway drain answering session notifications.
async fn build_gate_actor() -> SessionActor {
    use atelier_tools::implementations::atelier_build::enter_plan_mode::EnterPlanModeTool;
    use atelier_tools::implementations::atelier_build::exit_plan_mode::ExitPlanModeTool;
    use atelier_tools::registry::types::ToolConfig;
    let (gateway_tx, mut gateway_rx) =
        tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
    let (persistence_tx, _persistence_rx) =
        tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
    let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    *actor.agent.borrow_mut() = test_agent_with_tools(vec![
        ToolConfig::from_id("AtelierBuild:read_file"),
        ToolConfig::from_id("AtelierBuild:search_replace"),
        ToolConfig::for_tool::<EnterPlanModeTool>(),
        ToolConfig::for_tool::<ExitPlanModeTool>(),
    ])
    .await;
    tokio::task::spawn_local(async move {
        while let Some(msg) = gateway_rx.recv().await {
            if let xai_acp_lib::AcpClientMessage::SessionNotification(args) = msg {
                let _ = args.response_tx.send(Ok(()));
            }
        }
    });
    actor
}
/// Flip the fixture's tracker to Active (plan file: `/tmp/test-session/plan.md`).
fn activate_plan_mode(actor: &SessionActor) {
    let mut tracker = actor.plan_mode.lock();
    assert!(tracker.enter_pending());
    assert!(tracker.activate());
}
fn plan_file(actor: &SessionActor) -> String {
    actor
        .plan_mode
        .lock()
        .plan_file_path()
        .to_string_lossy()
        .into_owned()
}
fn search_replace_call(id: &str, path: &str) -> ToolCallResponse {
    ToolCallResponse {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::sampling::types::ToolCallFunction::new(
            "search_replace",
            serde_json::json!({
                "file_path": path,
                "old_string": "a",
                "new_string": "b",
            })
            .to_string(),
        ),
    }
}
async fn prepare(
    actor: &SessionActor,
    call: ToolCallResponse,
) -> Result<PreparedToolCall, ToolLoop> {
    let mut deferred = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        actor.prepare_tool_call(call, &mut deferred),
    )
    .await
    .expect("prepare_tool_call must not hang (a hang means a permission prompt was issued)")
    .expect("prepare_tool_call must not error")
}
/// Last tool_result pushed for `call_id`, or panic.
async fn tool_result_text(actor: &SessionActor, call_id: &str) -> String {
    let conv = actor.chat_state_handle.get_conversation().await;
    conv.iter()
        .rev()
        .find_map(|item| match item {
            atelier_sampling_types::ConversationItem::ToolResult(tr)
                if tr.tool_call_id == call_id =>
            {
                Some(tr.content.to_string())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no tool_result for {call_id} in {conv:?}"))
}
/// The headline: plan mode Active + allow-all permissions (the always-approve
/// worst case) still rejects a atelier edit outside the plan file, without ever
/// reaching the permission layer, and steers the model to `exit_plan_mode`.
#[tokio::test(flavor = "current_thread")]
async fn plan_mode_rejects_atelier_edit_outside_plan_file_despite_allow_all_permissions() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = build_gate_actor().await;
            activate_plan_mode(&actor);
            let expected_plan_file = plan_file(&actor);
            let result =
                prepare(&actor, search_replace_call("call_gate", "/tmp/src/main.rs")).await;
            assert!(
                matches!(result, Err(ToolLoop::Continue)),
                "gate must reject with Continue (tool not executed); got {result:?}"
            );
            let text = tool_result_text(&actor, "call_gate").await;
            assert!(
                text.contains("Rejected: file edits are not allowed in plan mode"),
                "rejection text: {text}"
            );
            assert!(
                text.contains(&expected_plan_file),
                "must name the plan file so the model knows the one editable path: {text}"
            );
            assert!(
                !text.contains("exit_plan_mode"),
                "rejection should stay short (no exit-tool steering): {text}"
            );
        })
        .await;
}
/// The carve-out: the plan file itself prepares cleanly (the gate defers to
/// `should_auto_approve_edit`, the same predicate as the permission bypass).
#[tokio::test(flavor = "current_thread")]
async fn plan_mode_allows_plan_file_edit() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = build_gate_actor().await;
            activate_plan_mode(&actor);
            let plan_file = plan_file(&actor);
            let result = prepare(&actor, search_replace_call("call_plan_file", &plan_file)).await;
            assert!(
                result.is_ok(),
                "plan-file edit must pass the gate and prepare; got {:?}",
                result.err()
            );
        })
        .await;
}
/// Control: with plan mode inactive the same edit prepares cleanly — the gate
/// is plan-scoped, not a general edit block.
#[tokio::test(flavor = "current_thread")]
async fn inactive_plan_mode_does_not_gate_edits() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = build_gate_actor().await;
            let result = prepare(
                &actor,
                search_replace_call("call_no_plan", "/tmp/src/main.rs"),
            )
            .await;
            assert!(
                result.is_ok(),
                "edit outside plan mode must prepare; got {:?}",
                result.err()
            );
        })
        .await;
}

/// Runtime Policy must guard the same real model-driven tool path as Plan
/// Mode. A configured file deny must stop the parsed tool before permission or
/// backend execution, even when the normal permission mode is allow-all.
#[tokio::test(flavor = "current_thread")]
async fn runtime_policy_denies_model_file_write_before_execution() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = build_gate_actor().await;
            *actor.tool_context.runtime_policy.write() =
                atelier_hooks::PolicyEngine::new([atelier_hooks::PolicyRule::deny(
                    atelier_hooks::PolicyScope::File,
                    "workspace writes disabled by runtime policy",
                )]);

            let result = prepare(
                &actor,
                search_replace_call("call_policy", "/tmp/src/main.rs"),
            )
            .await;
            assert!(
                matches!(result, Err(ToolLoop::Continue)),
                "runtime policy must reject the tool before execution; got {result:?}"
            );
            let text = tool_result_text(&actor, "call_policy").await;
            assert!(text.contains("workspace writes disabled by runtime policy"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_policy_modify_reparses_and_prepares_the_replacement_arguments() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = build_gate_actor().await;
            *actor.tool_context.runtime_policy.write() =
                atelier_hooks::PolicyEngine::new([atelier_hooks::PolicyRule::modify(
                    atelier_hooks::PolicyScope::Tool,
                    r#"{"target_file":"/tmp/safe.rs"}"#,
                )
                .with_tool("read_file")]);
            let call = ToolCallResponse {
                id: "call_modify".to_owned(),
                kind: "function".to_owned(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "read_file",
                    r#"{"target_file":"/tmp/secret.rs"}"#,
                ),
            };

            let prepared = prepare(&actor, call)
                .await
                .expect("modified read must prepare");
            assert_eq!(prepared.parsed_args["target_file"], "/tmp/safe.rs");
            assert!(!prepared.raw_arguments.contains("secret.rs"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_policy_modify_cannot_rewrite_around_the_plan_mode_hard_gate() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = build_gate_actor().await;
            activate_plan_mode(&actor);
            *actor.tool_context.runtime_policy.write() =
                atelier_hooks::PolicyEngine::new([atelier_hooks::PolicyRule::modify(
                    atelier_hooks::PolicyScope::Tool,
                    r#"{"file_path":"/tmp/test-session/plan.md","old_string":"a","new_string":"b"}"#,
                )
                .with_tool("search_replace")]);

            let result =
                prepare(&actor, search_replace_call("call_no_bypass", "/tmp/src/main.rs")).await;
            assert!(
                matches!(result, Err(ToolLoop::Continue)),
                "Plan Mode must reject the original forbidden write before Policy Modify: {result:?}"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_policy_add_context_is_returned_to_the_next_model_round() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = build_gate_actor().await;
            *actor.tool_context.runtime_policy.write() =
                atelier_hooks::PolicyEngine::new([atelier_hooks::PolicyRule::add_context(
                    atelier_hooks::PolicyScope::Tool,
                    "policy context for the next round",
                )
                .with_tool("read_file")]);
            let call = ToolCallResponse {
                id: "call_context".to_owned(),
                kind: "function".to_owned(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "read_file",
                    r#"{"target_file":"/tmp/safe.rs"}"#,
                ),
            };
            let mut deferred = Vec::new();

            actor
                .prepare_tool_call(call, &mut deferred)
                .await
                .expect("prepare must not error")
                .expect("tool must prepare");
            let serialized = serde_json::to_string(&deferred).unwrap();
            assert!(serialized.contains("policy context for the next round"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_policy_ask_uses_a_real_permission_request_even_in_allow_all_mode() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            use atelier_tools::registry::types::ToolConfig;
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() =
                test_agent_with_tools(vec![ToolConfig::from_id("AtelierBuild:read_file")]).await;
            *actor.tool_context.runtime_policy.write() =
                atelier_hooks::PolicyEngine::new([atelier_hooks::PolicyRule::ask(
                    atelier_hooks::PolicyScope::Tool,
                    "confirm this read",
                )
                .with_tool("read_file")]);
            let saw_permission = std::rc::Rc::new(std::cell::Cell::new(false));
            let saw_permission_task = saw_permission.clone();
            tokio::task::spawn_local(async move {
                while let Some(message) = gateway_rx.recv().await {
                    match message {
                        xai_acp_lib::AcpClientMessage::RequestPermission(args) => {
                            saw_permission_task.set(true);
                            let allow = args
                                .request
                                .options
                                .iter()
                                .find(|option| option.kind == acp::PermissionOptionKind::AllowOnce)
                                .expect("policy prompt must offer allow-once")
                                .option_id
                                .clone();
                            let _ = args
                                .response_tx
                                .send(Ok(acp::RequestPermissionResponse::new(
                                    acp::RequestPermissionOutcome::Selected(
                                        acp::SelectedPermissionOutcome::new(allow),
                                    ),
                                )));
                        }
                        xai_acp_lib::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });
            let call = ToolCallResponse {
                id: "call_ask".to_owned(),
                kind: "function".to_owned(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "read_file",
                    r#"{"target_file":"/tmp/safe.rs"}"#,
                ),
            };

            let result = prepare(&actor, call).await;
            assert!(
                result.is_ok(),
                "approved Policy Ask must prepare; got {result:?}, saw_permission={}",
                saw_permission.get()
            );
            assert!(saw_permission.get(), "Policy Ask must reach the ACP client");
        })
        .await;
}
