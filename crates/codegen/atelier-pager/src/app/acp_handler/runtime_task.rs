//! Runtime task subscription notifications.

use super::*;

/// Consume live `_atelier/task/subscribe` updates.
///
/// Session output itself still travels through the normal ACP session-update
/// stream. This notification only carries the runtime control-plane state, so
/// the pager uses it for actionable terminal/permission feedback and leaves
/// the detailed event replay to `/attach` and the runtime inspector.
pub(super) fn handle_task_update(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(notif.params.get()) else {
        tracing::warn!("Failed to parse atelier/task/update");
        return false;
    };
    let Some(session_id) = payload.get("sessionId").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Some(task) = payload.get("task") else {
        return false;
    };
    let Some(task_id) = task.get("taskId").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Some(state) = task.get("state").and_then(serde_json::Value::as_str) else {
        return false;
    };

    let message = task_update_message(
        task_id,
        state,
        task.get("diagnosticMessage")
            .and_then(serde_json::Value::as_str),
    );
    let session_id = acp::SessionId::new(session_id.to_owned());
    let Some(matched) = find_session_match(app, &session_id) else {
        let Some(message) = message else {
            return false;
        };
        let affected = match app.active_view {
            ActiveView::Agent(agent_id) => app.agents.contains_key(&agent_id),
            ActiveView::AgentDashboard => app.dashboard.is_some(),
            ActiveView::Welcome => false,
        };
        app.show_toast(&message);
        return affected;
    };
    let agent_id = matched.agent_id();
    let is_active = is_matched_agent_active(app, agent_id);
    if let Some(message) = message
        && let Some(agent) = app.agents.get_mut(&agent_id)
    {
        agent.show_toast(&message);
    }
    is_active
}

fn task_update_message(task_id: &str, state: &str, diagnostic: Option<&str>) -> Option<String> {
    match state {
        "waiting_for_permission" => Some(format!(
            "Runtime task {task_id} needs input; use /attach {task_id}"
        )),
        "completed" => Some(format!("Runtime task {task_id} completed")),
        "failed" => diagnostic
            .filter(|message| !message.trim().is_empty())
            .map(|message| format!("Runtime task {task_id} failed: {message}"))
            .or_else(|| Some(format!("Runtime task {task_id} failed"))),
        "paused" => Some(format!("Runtime task {task_id} paused")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::task_update_message;

    #[test]
    fn task_update_messages_only_surface_actionable_states() {
        assert_eq!(
            task_update_message("task-1", "waiting_for_permission", None).as_deref(),
            Some("Runtime task task-1 needs input; use /attach task-1")
        );
        assert_eq!(
            task_update_message("task-1", "failed", Some("provider timeout")).as_deref(),
            Some("Runtime task task-1 failed: provider timeout")
        );
        assert!(task_update_message("task-1", "streaming_response", None).is_none());
    }
}
