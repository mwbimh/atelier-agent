//! Tests for feedback / remember / btw / recap dispatchers.

use super::*;
use crate::app::dispatch::{recap_unavailable_toast, scrollback_has_user_messages};

fn btw_response(btw_id: &str, answer: &str) -> crate::app::actions::BtwResponseData {
    crate::app::actions::BtwResponseData {
        btw_id: btw_id.to_owned(),
        snapshot_id: None,
        answer: answer.to_owned(),
        provider: None,
        model: "test-model".to_owned(),
        wire_api: None,
        wire_api_source: None,
    }
}

fn send_btw(app: &mut AppView, question: &str) -> crate::app::agent::BtwRequest {
    let effects = dispatch(Action::SendBtw(question.into()), app);
    match effects.as_slice() {
        [Effect::SendBtw { request, .. }] => request.clone(),
        other => panic!("expected one SendBtw effect, got {other:?}"),
    }
}

fn complete_btw(
    app: &mut AppView,
    agent_id: AgentId,
    request: crate::app::agent::BtwRequest,
    response: crate::app::actions::BtwResponseData,
) {
    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id,
            result: crate::app::actions::BtwTaskResult::Answer {
                request,
                result: Ok(response),
            },
        }),
        app,
    );
}

#[test]
fn btw_response_after_overlay_close_is_ignored() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    let mut app = test_app_with_agent();
    let id = AgentId(0);

    let request = send_btw(&mut app, "question A");
    app.handle_input(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(app.agents[&id].btw_state.is_none());
    assert!(app.agents[&id].btw_request.is_none());

    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: id,
            result: crate::app::actions::BtwTaskResult::Answer {
                request,
                result: Ok(btw_response("btw-a", "answer A")),
            },
        }),
        &mut app,
    );

    assert!(
        app.agents[&id].btw_state.is_none(),
        "a response for a dismissed BTW must not reopen the overlay"
    );
}

#[test]
fn earlier_btw_response_does_not_replace_newer_question() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    let request_a = send_btw(&mut app, "question A");
    let request_b = send_btw(&mut app, "question B");
    assert_ne!(request_a.request_id, request_b.request_id);
    assert_eq!(request_a.question, "question A");
    assert_eq!(request_b.question, "question B");
    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: id,
            result: crate::app::actions::BtwTaskResult::Answer {
                request: request_a,
                result: Ok(btw_response("btw-a", "answer A")),
            },
        }),
        &mut app,
    );

    assert!(matches!(
        app.agents[&id].btw_state.as_ref(),
        Some(crate::views::btw_overlay::BtwOverlayState::Loading { question })
            if question == "question B"
    ));
}

#[test]
fn btw_persist_completion_only_marks_the_btw_it_was_sent_for() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let session_id = app.agents[&id]
        .session
        .session_id
        .as_ref()
        .unwrap()
        .to_string();

    let request_a = send_btw(&mut app, "question A");
    complete_btw(
        &mut app,
        id,
        request_a.clone(),
        btw_response("btw-a", "answer A"),
    );
    let params = app.agents[&id]
        .btw_state
        .as_ref()
        .unwrap()
        .persist_request(&session_id)
        .unwrap();
    let persist_effects = dispatch(
        Action::RuntimeExtension {
            method: "_atelier/btw/persist".into(),
            params,
        },
        &mut app,
    );
    assert!(matches!(
        persist_effects.as_slice(),
        [Effect::PersistBtw { request, .. }] if request == &request_a
    ));

    let request_b = send_btw(&mut app, "question B");
    complete_btw(&mut app, id, request_b, btw_response("btw-b", "answer B"));
    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: id,
            result: crate::app::actions::BtwTaskResult::Persist {
                request: request_a,
                result: Ok(true),
            },
        }),
        &mut app,
    );

    assert!(
        app.agents[&id]
            .btw_state
            .as_ref()
            .unwrap()
            .persist_request(&session_id)
            .is_some(),
        "persisting A must not mark the currently displayed B as persisted"
    );
}

#[test]
fn stale_btw_persist_action_does_not_target_the_newer_answer() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let session_id = app.agents[&id]
        .session
        .session_id
        .as_ref()
        .unwrap()
        .to_string();

    let request_a = send_btw(&mut app, "question A");
    complete_btw(&mut app, id, request_a, btw_response("btw-a", "answer A"));
    let stale_params = app.agents[&id]
        .btw_state
        .as_ref()
        .unwrap()
        .persist_request(&session_id)
        .unwrap();

    let request_b = send_btw(&mut app, "question B");
    complete_btw(&mut app, id, request_b, btw_response("btw-b", "answer B"));

    let effects = dispatch(
        Action::RuntimeExtension {
            method: "_atelier/btw/persist".into(),
            params: stale_params,
        },
        &mut app,
    );

    assert!(
        effects.is_empty(),
        "a delayed persist action for A must not be rebound to the displayed B: {effects:?}"
    );
}

#[test]
fn runtime_task_attach_dispatches_without_an_active_session() {
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::AgentDashboard;

    let effects = dispatch(
        Action::RuntimeExtension {
            method: "_atelier/task/attach".into(),
            params: serde_json::json!({ "taskId": "task-21" }),
        },
        &mut app,
    );

    assert!(matches!(
        effects.as_slice(),
        [Effect::RuntimeExtension {
            agent_id: None,
            method,
            params,
        }] if method == "_atelier/task/attach" && params["taskId"] == "task-21"
    ));
}

#[test]
fn runtime_task_cancel_dispatches_without_an_active_session() {
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::AgentDashboard;

    let effects = dispatch(
        Action::RuntimeExtension {
            method: "_atelier/task/cancel".into(),
            params: serde_json::json!({ "taskId": "task-21" }),
        },
        &mut app,
    );

    assert!(matches!(
        effects.as_slice(),
        [Effect::RuntimeExtension {
            agent_id: None,
            method,
            params,
        }] if method == "_atelier/task/cancel" && params["taskId"] == "task-21"
    ));
}

#[test]
fn recap_unavailable_toast_empty_vs_with_messages() {
    assert_eq!(recap_unavailable_toast(false), "No messages yet");
    assert_eq!(recap_unavailable_toast(true), "Couldn't generate recap");
}

#[test]
fn manual_recap_with_no_messages_toasts_empty_state_and_skips_request() {
    let mut app = test_app_with_agent();
    app.session_recap_available = true;
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.set_text("/recap");
        assert!(!scrollback_has_user_messages(&agent.scrollback));
    }

    let effects = dispatch(Action::SendRecap { auto: false }, &mut app);

    assert!(
        effects.is_empty(),
        "empty session must not fire atelier/recap: {effects:?}"
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_recap_entry.is_none(), "no loading spinner");
    assert_eq!(
        agent.toast.as_ref().map(|(s, _)| s.as_str()),
        Some("No messages yet"),
        "empty session should say No messages yet, not Couldn't generate recap"
    );
    assert_eq!(agent.prompt.text(), "", "slash command text is cleared");
}

#[test]
fn manual_recap_with_messages_requests_and_shows_spinner() {
    let mut app = test_app_with_agent();
    app.session_recap_available = true;
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("hello"));
        assert!(scrollback_has_user_messages(&agent.scrollback));
    }

    let effects = dispatch(Action::SendRecap { auto: false }, &mut app);

    assert!(
        matches!(effects.as_slice(), [Effect::SendRecap { auto: false, .. }]),
        "expected SendRecap effect, got {effects:?}"
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(
        agent.pending_recap_entry.is_some(),
        "manual recap shows a loading spinner when there is something to summarize"
    );
    assert!(agent.toast.is_none());
}

/// Regression: during session/load, scrollback is batched so
/// `turn_count()` stays 0 until `end_batch`, but UserPrompt entries may already
/// be present. Manual `/recap` must still request a recap.
#[test]
fn manual_recap_during_batch_load_with_prompts_still_requests() {
    let mut app = test_app_with_agent();
    app.session_recap_available = true;
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.scrollback.begin_batch();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("hello from resume"));
        // Batched push defers rebuild_turns — turn index is stale, entries aren't.
        assert_eq!(agent.scrollback.turn_count(), 0);
        assert!(scrollback_has_user_messages(&agent.scrollback));
    }

    let effects = dispatch(Action::SendRecap { auto: false }, &mut app);

    assert!(
        matches!(effects.as_slice(), [Effect::SendRecap { auto: false, .. }]),
        "batched resume with user prompts must still fire atelier/recap: {effects:?}"
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_recap_entry.is_some());
    assert!(agent.toast.is_none());
    // Clean up batch for the test fixture (not required for the assertion).
    app.agents.get_mut(&id).unwrap().scrollback.end_batch();
}

/// While session replay is still streaming, don't claim "No messages yet" even
/// if scrollback looks empty — history may arrive on the next notification.
#[test]
fn manual_recap_while_loading_replay_still_requests() {
    let mut app = test_app_with_agent();
    app.session_recap_available = true;
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.loading_replay = true;
        assert!(!scrollback_has_user_messages(&agent.scrollback));
    }

    let effects = dispatch(Action::SendRecap { auto: false }, &mut app);

    assert!(
        matches!(effects.as_slice(), [Effect::SendRecap { auto: false, .. }]),
        "loading_replay must not short-circuit to No messages yet: {effects:?}"
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_recap_entry.is_some());
    assert!(agent.toast.is_none());
}

#[test]
fn recap_request_transport_failure_with_no_turns_uses_empty_toast() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let session_id = app.agents[&id].session.session_id.clone().unwrap();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let spinner = agent
            .scrollback
            .push(crate::scrollback::entry::ScrollbackEntry::running(
                RenderBlock::session_event(SessionEvent::Recap {
                    summary: String::new(),
                    auto: false,
                }),
            ));
        agent.pending_recap_entry = Some(spinner);
        assert!(!scrollback_has_user_messages(&agent.scrollback));
    }

    dispatch(
        Action::TaskComplete(TaskResult::RecapRequested {
            session_id,
            auto: false,
            error: Some("transport down".into()),
        }),
        &mut app,
    );

    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_recap_entry.is_none());
    assert_eq!(
        agent.toast.as_ref().map(|(s, _)| s.as_str()),
        Some("No messages yet")
    );
}

#[test]
fn recap_request_transport_failure_with_turns_uses_generic_toast() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let session_id = app.agents[&id].session.session_id.clone().unwrap();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("hello"));
        let spinner = agent
            .scrollback
            .push(crate::scrollback::entry::ScrollbackEntry::running(
                RenderBlock::session_event(SessionEvent::Recap {
                    summary: String::new(),
                    auto: false,
                }),
            ));
        agent.pending_recap_entry = Some(spinner);
        assert!(scrollback_has_user_messages(&agent.scrollback));
    }

    dispatch(
        Action::TaskComplete(TaskResult::RecapRequested {
            session_id,
            auto: false,
            error: Some("transport down".into()),
        }),
        &mut app,
    );

    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_recap_entry.is_none());
    assert_eq!(
        agent.toast.as_ref().map(|(s, _)| s.as_str()),
        Some("Couldn't generate recap")
    );
}
