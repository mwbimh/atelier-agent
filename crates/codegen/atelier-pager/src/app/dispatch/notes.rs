//! Feedback, remember-note, btw, and recap dispatchers.

use super::ctx::with_active_agent;
use crate::app::actions::Effect;
use crate::app::agent::AgentId;
use crate::app::agent_view::{AgentView, PromptInputMode};
use crate::app::app_view::{ActiveView, AppView};
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::{SessionEvent, ToolCallBlock};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic counter for correlating async rewrite responses with the modal
/// that requested them. Prevents stale results from populating a different
/// note's review modal when the user closes and re-opens quickly.
static REWRITE_NONCE: AtomicU64 = AtomicU64::new(0);

fn next_rewrite_nonce() -> u64 {
    REWRITE_NONCE.fetch_add(1, Ordering::Relaxed)
}

/// Enter feedback mode: visual change to prompt bar (teal accent, pencil prefix).
/// No side effects — the user types feedback text and presses Enter to send.
pub(super) fn dispatch_enter_feedback_mode(app: &mut AppView) -> Vec<Effect> {
    with_active_agent(app, |agent| {
        agent.prompt_input_mode = PromptInputMode::Feedback;
        agent.prompt.set_text("");
    });
    vec![]
}

/// Enter remember mode: visual change to prompt bar (remember accent, `#` prefix).
/// No side effects — the user types a memory note and presses Enter to send.
pub(super) fn dispatch_enter_remember_mode(app: &mut AppView) -> Vec<Effect> {
    with_active_agent(app, |agent| {
        agent.prompt_input_mode = PromptInputMode::Remember;
        agent.prompt.set_text("");
    });
    vec![]
}

/// Send feedback text to the server. Shows a thank-you message immediately
/// and fires the HTTP POST as a background effect.
pub(super) fn dispatch_send_feedback(app: &mut AppView, text: String) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    agent.prompt_input_mode = PromptInputMode::Normal;
    agent.prompt.set_text("");
    // Submitting feedback retires any edit-contextual ephemeral tip.
    agent.ephemeral_tip.clear_on_submit();

    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        agent.scrollback.push_block(RenderBlock::system(
            "Please provide feedback text.".to_string(),
        ));
        return vec![];
    }

    let Some(session_id) = agent.session.session_id.clone() else {
        agent
            .scrollback
            .push_block(RenderBlock::system("No active session.".to_string()));
        return vec![];
    };

    agent.scrollback.push_block(RenderBlock::system(
        "Thanks for the feedback! The Atelier team is on it.".to_string(),
    ));

    vec![Effect::SendFeedback {
        agent_id: id,
        session_id,
        feedback_text: trimmed,
    }]
}

/// Send a raw remember note for LLM-powered rewriting via `atelier/memory/rewrite`.
/// Clears remember mode and prompts the LLM to reformat the note with session
/// context. Falls back to direct `SaveMemoryNote` when no session is available.
pub(super) fn dispatch_send_remember_note(app: &mut AppView, text: String) -> Vec<Effect> {
    use crate::views::modal::ActiveModal;

    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    agent.prompt_input_mode = PromptInputMode::Normal;
    agent.prompt.set_text("");
    // Submitting a memory note retires any edit-contextual ephemeral tip.
    agent.ephemeral_tip.clear_on_submit();

    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        agent.scrollback.push_block(RenderBlock::system(
            "Please provide a memory note.".to_string(),
        ));
        return vec![];
    }

    let cwd = agent.session.cwd.clone();

    let Some(session_id) = agent.session.session_id.clone() else {
        // No session — open modal with raw content only (no LLM rewrite).
        agent.active_modal = Some(ActiveModal::RememberNoteReview {
            raw_content: trimmed.clone(),
            enhanced_content: None, // no session → no LLM rewrite, Tab disabled
            showing_enhanced: false,
            scroll: 0,
            window: crate::views::modal_window::ModalWindowState::new(),
            cached_lines: None,
            cwd,
            agent_id: id,
            rewrite_nonce: 0, // no rewrite in flight, nonce unused
        });
        return vec![];
    };

    // Open modal with raw content, LLM rewrite in flight.
    let nonce = next_rewrite_nonce();
    agent.active_modal = Some(ActiveModal::RememberNoteReview {
        raw_content: trimmed.clone(),
        enhanced_content: None,
        showing_enhanced: false,
        scroll: 0,
        window: crate::views::modal_window::ModalWindowState::new(),
        cached_lines: None,
        cwd: cwd.clone(),
        agent_id: id,
        rewrite_nonce: nonce,
    });

    let context_summary = extract_session_context(agent);

    vec![Effect::RewriteMemoryNote {
        agent_id: id,
        session_id,
        raw_text: trimmed,
        context_summary,
        nonce,
    }]
}

/// Save the currently displayed remember note from the review modal.
pub(super) fn dispatch_save_remember_note_from_modal(app: &mut AppView) -> Vec<Effect> {
    use crate::views::modal::ActiveModal;

    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    let (content, cwd) = if let Some(ActiveModal::RememberNoteReview {
        ref raw_content,
        ref enhanced_content,
        showing_enhanced,
        ref cwd,
        ..
    }) = agent.active_modal
    {
        let text = if showing_enhanced {
            enhanced_content.as_deref().unwrap_or(raw_content)
        } else {
            raw_content
        };
        (text.trim().to_string(), cwd.clone())
    } else {
        return vec![];
    };

    agent.active_modal = None;
    agent
        .scrollback
        .push_block(RenderBlock::system("Saving memory note...".to_string()));

    vec![Effect::SaveMemoryNote {
        agent_id: id,
        text: content,
        cwd,
    }]
}

/// Extract session context for the LLM memory rewrite request.
///
/// Walks scrollback in reverse, collecting:
/// - Last 5 user prompts
/// - File paths from recent tool calls (Read, Edit, ListDir)
/// - CWD and git branch
fn extract_session_context(agent: &AgentView) -> String {
    let mut user_prompts: Vec<String> = Vec::new();
    let mut file_paths: Vec<String> = Vec::new();

    // Walk scrollback entries in reverse to collect recent context.
    let len = agent.scrollback.len();
    for i in (0..len).rev() {
        let Some(entry) = agent.scrollback.entry(i) else {
            continue;
        };
        match &entry.block {
            RenderBlock::UserPrompt(prompt) => {
                if user_prompts.len() < 5 {
                    let text = if prompt.text.len() > 200 {
                        let end = prompt
                            .text
                            .char_indices()
                            .map(|(i, _)| i)
                            .take_while(|&i| i <= 200)
                            .last()
                            .unwrap_or(0);
                        format!("{}...", &prompt.text[..end])
                    } else {
                        prompt.text.clone()
                    };
                    user_prompts.push(text);
                }
            }
            RenderBlock::ToolCall(tc) => {
                if file_paths.len() < 20 {
                    match tc {
                        ToolCallBlock::Read(b) => {
                            file_paths.push(b.path.clone());
                        }
                        ToolCallBlock::Edit(b) => {
                            file_paths.push(b.path.clone());
                        }
                        ToolCallBlock::ListDir(b) => {
                            file_paths.push(b.path.clone());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        // Stop early once we have enough context.
        if user_prompts.len() >= 5 && file_paths.len() >= 20 {
            break;
        }
    }

    let mut parts: Vec<String> = Vec::new();

    // CWD
    parts.push(format!("CWD: {}", agent.session.cwd.display()));

    // Git branch
    if let Some(ref branch) = agent.current_branch {
        parts.push(format!("Branch: {branch}"));
    }

    // Recent prompts (chronological order)
    if !user_prompts.is_empty() {
        user_prompts.reverse();
        parts.push("Recent prompts:".to_string());
        for p in &user_prompts {
            parts.push(format!("- {p}"));
        }
    }

    // Recent file paths (deduplicated, preserving first-seen order)
    if !file_paths.is_empty() {
        let mut seen = std::collections::HashSet::new();
        file_paths.retain(|p| seen.insert(p.clone()));
        parts.push("Recent files:".to_string());
        for p in &file_paths {
            parts.push(format!("- {p}"));
        }
    }

    parts.join("\n")
}

/// Send a /btw side question. Bypasses the prompt queue — works even while
/// the agent is mid-turn. Fires an ACP ext method and shows a loading overlay.
pub(super) fn dispatch_send_btw(app: &mut AppView, question: String) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let Some(session_id) = agent.session.session_id.clone() else {
        agent.show_toast("No active session");
        return vec![];
    };

    let request = crate::app::agent::BtwRequest {
        request_id: agent.next_btw_request_id,
        question: question.clone(),
    };
    agent.next_btw_request_id = agent
        .next_btw_request_id
        .checked_add(1)
        .expect("BTW request ID exhausted");
    agent.prompt.set_text("");
    agent.btw_state = Some(crate::views::btw_overlay::BtwOverlayState::Loading {
        question: question.clone(),
    });
    agent.btw_request = Some(request.clone());
    // Prompt keeps focus while the answer is in flight (panel focuses on Done).
    agent.btw_focused = false;

    vec![Effect::SendBtw {
        agent_id: id,
        session_id,
        request,
    }]
}

pub(super) fn dispatch_open_roles_modal(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(agent_id) = app.active_view else {
        app.show_toast("Open a session to manage Runtime Roles");
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    agent.active_modal = Some(crate::views::modal::ActiveModal::Roles {
        state: Box::new(crate::views::roles_modal::RolesModalState::loading()),
    });
    vec![Effect::RuntimeExtension {
        agent_id: Some(agent_id),
        method: "_atelier/role/list".into(),
        params: serde_json::json!({}),
    }]
}

/// Dispatch a slash command backed by an Atelier control-plane extension.
/// The slash layer stays transport-neutral; this helper adds the active
/// session id for object-shaped requests before the effect crosses ACP.
pub(super) fn dispatch_runtime_extension(
    app: &mut AppView,
    method: String,
    mut params: serde_json::Value,
) -> Vec<Effect> {
    if is_btw_persist_method(&method) {
        let ActiveView::Agent(agent_id) = app.active_view else {
            return vec![];
        };
        let Some(agent) = app.agents.get(&agent_id) else {
            return vec![];
        };
        let Some(session_id) = agent.session.session_id.as_ref() else {
            return vec![];
        };
        let Some(request) = agent.btw_request.as_ref() else {
            return vec![];
        };
        let Some(state) = agent.btw_state.as_ref() else {
            return vec![];
        };
        if state.question() != request.question.as_str() {
            return vec![];
        }
        let Some(expected_params) = state.persist_request(&session_id.to_string()) else {
            return vec![];
        };
        if params.get("btwId") != expected_params.get("btwId")
            || params.get("question") != expected_params.get("question")
        {
            return vec![];
        }
        return vec![Effect::PersistBtw {
            agent_id,
            request: request.clone(),
            params: expected_params,
        }];
    }

    let requires_session = runtime_extension_requires_session(&method);
    let agent_id = match app.active_view {
        ActiveView::Agent(id) if app.agents.contains_key(&id) => Some(id),
        _ => None,
    };
    let session_id = agent_id
        .and_then(|id| app.agents.get(&id))
        .and_then(|agent| agent.session.session_id.clone());

    if requires_session && session_id.is_none() {
        if let Some(id) = agent_id {
            if let Some(agent) = app.agents.get_mut(&id) {
                agent.show_toast("No active session");
            }
        } else {
            app.show_toast("No active session");
        }
        return vec![];
    }
    if requires_session
        && let (Some(session_id), Some(object)) = (session_id, params.as_object_mut())
    {
        object
            .entry("sessionId".to_owned())
            .or_insert_with(|| serde_json::Value::String(session_id.to_string()));
    }
    vec![Effect::RuntimeExtension {
        agent_id,
        method,
        params,
    }]
}

fn is_btw_persist_method(method: &str) -> bool {
    matches!(method, "_atelier/btw/persist" | "atelier/btw/persist")
}

fn runtime_extension_requires_session(method: &str) -> bool {
    if matches!(
        method,
        "_atelier/role/set_fast_mode" | "atelier/role/set_fast_mode"
    ) {
        return true;
    }
    !method.starts_with("_atelier/provider/")
        && !method.starts_with("atelier/provider/")
        && !method.starts_with("_atelier/model/")
        && !method.starts_with("atelier/model/")
        && !method.starts_with("_atelier/model_provider_override/")
        && !method.starts_with("atelier/model_provider_override/")
        && !method.starts_with("_atelier/credential/")
        && !method.starts_with("atelier/credential/")
        && !method.starts_with("_atelier/role/")
        && !method.starts_with("atelier/role/")
        && !method.starts_with("_atelier/config/")
        && !method.starts_with("atelier/config/")
        && !matches!(
            method,
            "_atelier/task/list"
                | "atelier/task/list"
                | "_atelier/task/attach"
                | "atelier/task/attach"
                | "_atelier/task/cancel"
                | "atelier/task/cancel"
        )
}

/// Toast when a manual `/recap` produces no summary. Empty sessions get a clear
/// empty-state message; anything else (model failure, empty summary, etc.) keeps
/// the generic failure toast.
pub(crate) fn recap_unavailable_toast(has_user_messages: bool) -> &'static str {
    if has_user_messages {
        "Couldn't generate recap"
    } else {
        "No messages yet"
    }
}

/// Whether scrollback already has a user prompt. Scans entries (not
/// `turn_count`) so it stays correct during `begin_batch`/`end_batch` session
/// load, when `push` defers `rebuild_turns` and `turn_count` can stay 0 while
/// replayed prompts are already present.
pub(crate) fn scrollback_has_user_messages(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    scrollback
        .iter_entries()
        .any(|(_, entry)| entry.block.is_user_prompt())
}

/// Request a session recap. Bypasses the prompt queue — works even while the
/// agent is mid-turn. Fires the `atelier/recap` ext method; the recap arrives
/// asynchronously as a `SessionRecap` notification (rendered in scrollback).
///
/// `auto` is `false` for an explicit `/recap` and `true` for the automatic
/// return-from-away recap. For the manual path we clear the prompt and, when
/// no session exists yet, surface a toast; the auto path is best-effort and
/// silently no-ops without an active session.
pub(super) fn dispatch_send_recap(app: &mut AppView, auto: bool) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    // Shell is authoritative (remote settings / config / env). Skip client requests
    // entirely when the feature is off so we never hit `atelier/recap`.
    if !app.session_recap_available {
        if !auto {
            agent.show_toast("Session recap is not enabled");
        }
        return vec![];
    }

    let Some(session_id) = agent.session.session_id.clone() else {
        if !auto {
            agent.show_toast("No active session");
        }
        return vec![];
    };

    if !auto {
        agent.prompt.set_text("");
        // Nothing to summarize yet — show a clear empty-state toast instead of
        // a spinner that ends in "Couldn't generate recap".
        //
        // Skip the short-circuit while session replay is still loading (prompts
        // may not have arrived yet). Prefer an entry scan over `turn_count()`
        // so mid-batch resume (deferred `rebuild_turns`) still sees history.
        if !agent.session.loading_replay && !scrollback_has_user_messages(&agent.scrollback) {
            agent.show_toast(recap_unavailable_toast(false));
            return vec![];
        }
        // Show an immediate loading block with the animated "running" sidebar so
        // the user has feedback that a recap is being generated. The
        // `SessionRecap` handler fills this entry in and stops the animation.
        // Reuse an existing in-flight loading block instead of stacking spinners
        // when `/recap` is pressed repeatedly.
        let already_loading = agent.pending_recap_entry.is_some_and(|eid| {
            agent
                .scrollback
                .get_by_id(eid)
                .is_some_and(|entry| entry.is_running)
        });
        if !already_loading {
            let entry_id =
                agent
                    .scrollback
                    .push(crate::scrollback::entry::ScrollbackEntry::running(
                        RenderBlock::session_event(SessionEvent::Recap {
                            summary: String::new(),
                            auto: false,
                        }),
                    ));
            agent.pending_recap_entry = Some(entry_id);
        }
    } else {
        // Retry backoff only — do not consume the away period on dispatch.
        // The shell often no-ops auto recap until ≥3 min since the last main
        // turn; mark_recap_shown runs when any SessionRecap arrives (auto or
        // manual `/recap`).
        app.notification_service
            .focus_tracker
            .note_auto_recap_attempt();
    }

    vec![Effect::SendRecap { session_id, auto }]
}

// TaskResult handlers.

pub(super) fn handle_memory_note_saved(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<(), String>,
) -> Vec<Effect> {
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        match result {
            Ok(()) => {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Memory saved to {}",
                        crate::util::display_user_atelier_path("memory/MEMORY.md")
                    )));
            }
            Err(error) => {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't save memory note: {error}"
                    )));
            }
        }
    }
    vec![]
}

pub(super) fn handle_btw_response(
    app: &mut AppView,
    agent_id: AgentId,
    result: crate::app::actions::BtwTaskResult,
) -> Vec<Effect> {
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        let (request, result) = match result {
            crate::app::actions::BtwTaskResult::Answer { request, result } => {
                (request, EitherBtwResult::Answer(result))
            }
            crate::app::actions::BtwTaskResult::Persist { request, result } => {
                (request, EitherBtwResult::Persist(result))
            }
        };
        if agent.btw_request.as_ref() != Some(&request) {
            return vec![];
        }
        use crate::views::btw_overlay::BtwOverlayState;
        match result {
            EitherBtwResult::Answer(result) => {
                if !matches!(
                    agent.btw_state.as_ref(),
                    Some(BtwOverlayState::Loading { question }) if question == &request.question
                ) {
                    return vec![];
                }
                match result {
                    Ok(response) => {
                        // Answer arrived: show it (until Esc) and focus the panel
                        // so Up/Down scroll it until the user returns to the prompt.
                        agent.btw_state =
                            Some(BtwOverlayState::done_with_data(request.question, response));
                        agent.btw_focused = true;
                    }
                    Err(error) => {
                        // Error stays until Esc; nothing to scroll, keep prompt focus.
                        agent.btw_state = Some(BtwOverlayState::Error {
                            question: request.question,
                            error,
                        });
                        agent.btw_focused = false;
                    }
                }
            }
            EitherBtwResult::Persist(result) => {
                if !matches!(
                    agent.btw_state.as_ref(),
                    Some(BtwOverlayState::Done { question, .. }) if question == &request.question
                ) {
                    return vec![];
                }
                match result {
                    Ok(true) => {
                        if let Some(state) = agent.btw_state.as_mut() {
                            state.mark_persisted();
                        }
                        agent.show_toast("BTW saved locally");
                    }
                    Ok(false) => agent.show_toast("BTW was not saved"),
                    Err(error) => {
                        agent.show_toast(&format!("_atelier/btw/persist failed: {error}"))
                    }
                }
            }
        }
    }
    vec![]
}

enum EitherBtwResult {
    Answer(Result<crate::app::actions::BtwResponseData, String>),
    Persist(Result<bool, String>),
}
