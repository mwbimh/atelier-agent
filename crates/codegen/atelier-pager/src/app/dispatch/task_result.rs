//! Async task-result application: routes task results into state.
use super::auth::{
    ensure_login_method, handle_auth_complete, handle_auth_url_ready, handle_mcp_auth_trigger_done,
};
use super::cta::{
    handle_cta_plugin_install_done, handle_cta_plugin_reload_done,
    handle_plugin_cta_catalog_loaded, handle_plugin_cta_debounce_expired,
    handle_plugin_cta_mcps_loaded,
};
use super::ctx::find_agent_by_session_id;
use super::notes::{handle_btw_response, handle_memory_note_saved};
use super::prompt::{
    defer_to_open_reload_window, handle_compact_complete, handle_prompt_response,
    handle_suggestion_debounce_expired,
};
use super::rewind::{
    dispatch_rewind_success, handle_rewind_execute_failed, handle_rewind_points_loaded,
    handle_rewind_preview_complete, handle_rewind_preview_failed,
};
use super::router::{dispatch, dispatch_action_result};
use super::session::foreign::{
    handle_foreign_sessions_scanned, handle_session_list_failed, handle_session_list_loaded,
};
use super::session::fork::{
    handle_fork_session_failed, handle_fork_session_ready, handle_worktree_forked,
};
use super::session::lifecycle::{
    dispatch_exit_session, handle_session_created, handle_switch_model_complete,
    handle_worktree_session_created, handle_worktree_session_failed,
};
use super::session::load::{
    handle_card_detail_loaded, handle_deep_search_results, handle_session_load_failed,
    handle_session_loaded, handle_session_restore_failed, handle_session_restored,
    handle_session_search_debounce_expired, remove_session_from_pickers,
};
use super::settings::ui::apply_setting_rollback;
use super::status::{handle_context_info_complete, scrub_error_for_toast};

fn format_runtime_extension_response(method: &str, response: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(response)
        .unwrap_or_else(|_| serde_json::Value::String(response.to_owned()));
    let result = value.get("result").unwrap_or(&value);
    match method.trim_start_matches('_') {
        "atelier/provider/list" => format_provider_list_response(response),
        "atelier/model/list" => format_wire_api_list_response(result),
        "atelier/model/get" => format_wire_api_model_response(result),
        "atelier/role/list" => format_role_list_response(result),
        "atelier/role/get" | "atelier/role/update" | "atelier/role/update_payload" => {
            format_role_response(result)
        }
        "atelier/role/delete" => format_status_response("Role override reset", result),
        "atelier/role/test" => format_status_response("Role check", result),
        "atelier/role/set_fast_mode" => format_status_response("Role fast mode updated", result),
        "atelier/model/update_wire_api" => format_status_response("Wire API updated", result),
        "atelier/model_provider_override/set" => {
            format_status_response("Model override updated", result)
        }
        "atelier/model_provider_override/delete" => {
            format_status_response("Model override removed", result)
        }
        "atelier/model_provider_override/test" => format_status_response("Wire API check", result),
        "atelier/provider/create" => format_status_response("Provider added", result),
        "atelier/provider/update" => format_status_response("Provider updated", result),
        "atelier/provider/delete" => format_status_response("Provider deleted", result),
        "atelier/provider/enable" => format_status_response("Provider enabled", result),
        "atelier/provider/disable" => format_status_response("Provider disabled", result),
        "atelier/provider/test" => format_status_response("Provider check", result),
        "atelier/provider/oauth_logout" => format_status_response("Provider signed out", result),
        "atelier/config/update" => format_status_response("Default model updated", result),
        "atelier/config/reset_defaults" => format_status_response("Defaults restored", result),
        "atelier/runtime/status" => format_runtime_status_response(result),
        "atelier/runtime/doctor" => format_runtime_doctor_response(result),
        "atelier/runtime/recover" => format_status_response("Runtime recovery", result),
        "atelier/request/list" => format_runtime_request_list_response(result),
        "atelier/request/get" => format_runtime_request_response(result),
        "atelier/trace/get" => format_runtime_trace_response(result),
        _ => safe_runtime_result(result),
    }
}

fn runtime_string<'a>(value: &'a serde_json::Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(serde_json::Value::as_str))
}

fn model_key_from_value(model: &serde_json::Value) -> Option<String> {
    if let Some(key) = runtime_string(model, &["modelKey", "model_key"]) {
        return Some(key.to_owned());
    }
    let key = model.get("key")?;
    if let Some(key) = key.as_str() {
        return Some(key.to_owned());
    }
    let provider = runtime_string(key, &["providerId", "provider_id"])?;
    let model_id = runtime_string(key, &["modelId", "model_id"])?;
    Some(format!("{provider}/{model_id}"))
}

fn effective_wire_api(model: &serde_json::Value) -> (&str, bool) {
    let wire = runtime_string(model, &["wireApi", "wire_api"]);
    (wire.unwrap_or("chat_completions"), wire.is_none())
}

fn effective_context_window(model: &serde_json::Value) -> (u64, bool) {
    let context = model
        .get("contextWindow")
        .or_else(|| model.get("context_window"))
        .and_then(serde_json::Value::as_u64);
    (context.unwrap_or(100_000), context.is_none())
}

fn format_wire_api_list_response(result: &serde_json::Value) -> String {
    let Some(models) = result.get("models").and_then(serde_json::Value::as_array) else {
        return "Wire APIs\nCould not read the model catalog.".to_owned();
    };
    if models.is_empty() {
        return "Wire APIs\nNo Provider models discovered.\nRefresh a Provider or add one with /provider add."
            .to_owned();
    }
    let mut lines = vec!["Wire APIs".to_owned()];
    for model in models {
        let key = model_key_from_value(model).unwrap_or_else(|| "unknown/unknown".to_owned());
        let (wire_api, wire_default) = effective_wire_api(model);
        let (context, context_default) = effective_context_window(model);
        let mut details = format!("{wire_api} · {context} tokens");
        if wire_default || context_default {
            details.push_str(" · default metadata");
        }
        lines.push(format!("- {key} | {details}"));
    }
    lines.push("Change a model with /wire-api wire <provider/model> <wire-api>.".to_owned());
    lines.join("\n")
}

fn format_wire_api_model_response(result: &serde_json::Value) -> String {
    let model = result.get("model").unwrap_or(result);
    let key = model_key_from_value(model).unwrap_or_else(|| "unknown/unknown".to_owned());
    let resolved = result.get("wireApi").and_then(serde_json::Value::as_str);
    let (model_wire, defaulted) = effective_wire_api(model);
    let wire = resolved.unwrap_or(model_wire);
    let (context, context_default) = effective_context_window(model);
    let mut lines = vec![
        "Wire API".to_owned(),
        format!("Model: {key}"),
        format!("Type: {wire}"),
        format!("Context: {context} tokens"),
    ];
    if defaulted || context_default {
        lines.push("Source: default metadata".to_owned());
    } else if let Some(source) = runtime_string(result, &["wireApiSource", "wire_api_source"]) {
        lines.push(format!("Source: {}", source.replace('_', " ")));
    }
    lines.join("\n")
}

fn role_model(config: Option<&serde_json::Value>) -> Option<String> {
    let config = config?;
    let provider = runtime_string(config, &["provider"])?;
    let model = runtime_string(config, &["model"])?;
    Some(format!("{provider}/{model}"))
}

fn role_context_label(role: &serde_json::Value) -> Option<String> {
    let source = role
        .get("contextSource")
        .or_else(|| role.get("context_source"))?;
    let package = runtime_string(source, &["package"])?;
    let context_role = runtime_string(source, &["role"])?;
    let empty = source
        .get("empty")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Some(if empty {
        format!("Context {package}/{context_role} (empty)")
    } else {
        format!("Context {package}/{context_role}")
    })
}

fn exact_role_override_fields(config: Option<&serde_json::Value>) -> Vec<&'static str> {
    let Some(config) = config else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    for (key, label) in [
        ("provider", "provider"),
        ("model", "model"),
        ("effort", "effort"),
        ("fast_mode", "fast mode"),
        ("payload", "payload"),
    ] {
        if config.get(key).is_some() {
            fields.push(label);
        }
    }
    fields
}

fn format_role_list_response(result: &serde_json::Value) -> String {
    let Some(roles) = result.get("roles").and_then(serde_json::Value::as_array) else {
        return "Roles\nCould not read Role configuration.".to_owned();
    };
    let mut lines = vec!["Roles".to_owned()];
    for role in roles {
        let role_id = runtime_string(role, &["roleId", "role_id"]).unwrap_or("unknown");
        let display_role = if role_id == "main" { "MAIN" } else { role_id };
        let configured = role
            .get("configured")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let exact = role.get("config").filter(|value| !value.is_null());
        let effective = role
            .get("effectiveConfig")
            .or_else(|| role.get("effective_config"))
            .filter(|value| !value.is_null());
        let mut details = Vec::new();
        if role_id == "main" {
            details.push(
                runtime_string(role, &["model"])
                    .map(str::to_owned)
                    .or_else(|| role_model(effective))
                    .unwrap_or_else(|| "not selected".to_owned()),
            );
            details.push("config.toml".to_owned());
        } else {
            if let Some(model) = role_model(effective) {
                details.push(model);
            }
            if !configured {
                let source = runtime_string(role, &["effectiveSource", "effective_source"])
                    .unwrap_or("main");
                let source = if source == "main" { "MAIN" } else { source };
                details.push(format!("inherits {source}"));
            } else {
                let fields = exact_role_override_fields(exact);
                if !fields.is_empty() {
                    details.push(format!("overrides {}", fields.join(", ")));
                }
            }
            if let Some(effort) = effective.and_then(|config| runtime_string(config, &["effort"])) {
                details.push(format!("effort {effort}"));
            }
        }
        if let Some(context) = role_context_label(role) {
            details.push(context);
        }
        lines.push(format!("- {display_role} | {}", details.join(" · ")));
    }
    lines.push("Configure a Role with /roles set.".to_owned());
    lines.join("\n")
}

fn format_role_response(result: &serde_json::Value) -> String {
    let role_id = runtime_string(result, &["roleId", "role_id"]).unwrap_or("Role");
    let exact = result.get("config").filter(|value| !value.is_null());
    let effective = result
        .get("effectiveConfig")
        .or_else(|| result.get("effective_config"))
        .filter(|value| !value.is_null());
    let mut lines = vec![format!("Role {role_id}")];
    if let Some(model) = role_model(effective).or_else(|| role_model(exact)) {
        lines.push(format!("Effective model: {model}"));
    }
    let fields = exact_role_override_fields(exact);
    if !fields.is_empty() {
        lines.push(format!("Exact overrides: {}", fields.join(", ")));
    }
    if let Some(source) = runtime_string(result, &["effectiveSource", "effective_source"]) {
        lines.push(format!("Execution source: {source}"));
    }
    if let Some(context) = role_context_label(result) {
        lines.push(context);
    }
    if lines.len() == 1 {
        lines.push("No effective model is configured.".to_owned());
    }
    lines.join("\n")
}

fn format_status_response(title: &str, result: &serde_json::Value) -> String {
    if let Some(message) = runtime_string(result, &["message", "status"]) {
        return format!("{title}\n{message}");
    }
    if let Some(model) = runtime_string(result, &["model", "modelKey", "model_key"]) {
        return format!("{title}\nModel: {model}");
    }
    if let Some(provider) = runtime_string(result, &["providerId", "provider_id", "id"]) {
        return format!("{title}\nProvider: {provider}");
    }
    format!("{title}.")
}

fn format_runtime_status_response(result: &serde_json::Value) -> String {
    let Some(statuses) = result.get("statuses").and_then(serde_json::Value::as_array) else {
        return "Runtime status\nNo status is available.".to_owned();
    };
    let mut lines = vec!["Runtime status".to_owned()];
    for status in statuses {
        let session = runtime_string(status, &["sessionId", "session_id"]).unwrap_or("unknown");
        let state = runtime_string(status, &["state"]).unwrap_or("unknown");
        let provider = runtime_string(status, &["provider"]);
        let model = runtime_string(status, &["model"]);
        let target = match (provider, model) {
            (Some(provider), Some(model)) => format!("{provider}/{model}"),
            (_, Some(model)) => model.to_owned(),
            _ => "no model".to_owned(),
        };
        lines.push(format!("- {session} | {state} | {target}"));
        if let Some(message) = runtime_string(status, &["diagnosticMessage", "diagnostic_message"])
        {
            lines.push(format!("  {message}"));
        }
    }
    lines.join("\n")
}

fn format_runtime_doctor_response(result: &serde_json::Value) -> String {
    let Some(issues) = result.get("issues").and_then(serde_json::Value::as_array) else {
        return "Runtime doctor\nNo diagnostic report is available.".to_owned();
    };
    if issues.is_empty() {
        return "Runtime doctor\nNo issues found.".to_owned();
    }
    let mut lines = vec![format!("Runtime doctor\n{} issue(s)", issues.len())];
    for issue in issues {
        let state = runtime_string(issue, &["state"]).unwrap_or("unknown");
        let message = runtime_string(issue, &["message"]).unwrap_or("No details");
        lines.push(format!("- {state} | {message}"));
    }
    lines.join("\n")
}

fn request_summary_line(request: &serde_json::Value) -> String {
    let id = runtime_string(request, &["requestId", "request_id"]).unwrap_or("unknown");
    let state = runtime_string(request, &["state"]).unwrap_or("unknown");
    let provider = runtime_string(request, &["provider"]);
    let model = runtime_string(request, &["model"]);
    let target = match (provider, model) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        (_, Some(model)) => model.to_owned(),
        _ => "no model".to_owned(),
    };
    let wire = runtime_string(request, &["wireApi", "wire_api"])
        .map(|wire| format!(" · {wire}"))
        .unwrap_or_default();
    format!("- {id} | {state} | {target}{wire}")
}

fn format_runtime_request_list_response(result: &serde_json::Value) -> String {
    let Some(requests) = result.get("requests").and_then(serde_json::Value::as_array) else {
        return "Runtime requests\nNo request history is available.".to_owned();
    };
    if requests.is_empty() {
        return "Runtime requests\nNo requests recorded.".to_owned();
    }
    let mut lines = vec!["Runtime requests".to_owned()];
    lines.extend(requests.iter().map(request_summary_line));
    lines.push("Inspect one with /request <request-id>.".to_owned());
    lines.join("\n")
}

fn format_runtime_request_response(result: &serde_json::Value) -> String {
    let Some(request) = result.get("request").filter(|request| !request.is_null()) else {
        return "Runtime request\nRequest not found.".to_owned();
    };
    let mut lines = vec!["Runtime request".to_owned(), request_summary_line(request)];
    if let Some(input) = request
        .get("inputTokens")
        .and_then(serde_json::Value::as_u64)
    {
        lines.push(format!("Input tokens: {}", format_token_number(input)));
    }
    if let Some(duration) = request
        .get("totalDurationMs")
        .and_then(serde_json::Value::as_u64)
    {
        lines.push(format!("Duration: {duration} ms"));
    }
    lines.join("\n")
}

fn format_runtime_trace_response(result: &serde_json::Value) -> String {
    let Some(events) = result.get("events").and_then(serde_json::Value::as_array) else {
        return "Runtime trace\nNo trace is available.".to_owned();
    };
    if events.is_empty() {
        return "Runtime trace\nNo events recorded.".to_owned();
    }
    let mut lines = vec!["Runtime trace".to_owned()];
    for event in events {
        let id = event
            .get("eventId")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let kind = runtime_string(event, &["kind"]).unwrap_or("event");
        let request = runtime_string(event, &["requestId", "request_id"])
            .map(|request| format!(" | {request}"))
            .unwrap_or_default();
        lines.push(format!("- #{id} | {kind}{request}"));
    }
    if result.get("truncated").and_then(serde_json::Value::as_bool) == Some(true) {
        lines.push("Older events were truncated.".to_owned());
    }
    lines.join("\n")
}

fn format_token_number(value: u64) -> String {
    let raw = value.to_string();
    let mut rendered = String::new();
    for (index, character) in raw.chars().enumerate() {
        if index > 0 && (raw.len() - index) % 3 == 0 {
            rendered.push(',');
        }
        rendered.push(character);
    }
    rendered
}

fn safe_runtime_result(result: &serde_json::Value) -> String {
    match result {
        serde_json::Value::String(text) if !text.trim().is_empty() => text.clone(),
        serde_json::Value::Object(_) => runtime_string(result, &["message", "status"])
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Operation completed.".to_owned()),
        _ => "Operation completed.".to_owned(),
    }
}

fn provider_oauth_begin_ui(result: &serde_json::Value) -> Option<(String, String)> {
    let login_id = result.get("loginId")?.as_str()?.to_owned();
    let provider_id = result.get("providerId")?.as_str()?;
    let flow = result.get("flow")?.as_str()?;
    let verification_url = result.get("verificationUrl")?.as_str()?;
    let user_code = result.get("userCode").and_then(serde_json::Value::as_str);
    let browser_opened = result
        .get("browserOpened")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut rendered = format!(
        "Provider OAuth login\nProvider: {provider_id}\nFlow: {flow}\nURL: {verification_url}"
    );
    if let Some(user_code) = user_code {
        rendered.push_str(&format!("\nCode: {user_code}"));
    }
    if !browser_opened {
        rendered.push_str("\nBrowser could not be opened automatically; open the URL manually.");
    }
    rendered.push_str("\nWaiting for authorization...");
    Some((login_id, rendered))
}

fn show_dashboard_runtime_output(app: &mut AppView, output: &str) -> bool {
    if !matches!(app.active_view, ActiveView::AgentDashboard) {
        return false;
    }
    let Some(dashboard) = app.dashboard.as_mut() else {
        return false;
    };
    dashboard.dispatch.set_text(output);
    dashboard.error_toast = None;
    true
}

#[cfg(test)]
mod runtime_task_format_tests {
    use super::{
        format_provider_list_response, format_runtime_extension_response,
        format_runtime_task_attach_response, format_runtime_task_response, provider_oauth_begin_ui,
    };

    #[test]
    fn provider_list_is_rendered_as_a_summary_without_internal_json_or_models() {
        let rendered = format_provider_list_response(
            r#"{"providers":[{"id":"openai","display_name":"OpenAI","enabled":true,"authentication":"OAuth","base_url":"https://must-not-render.example","credential":{"type":"environment","variable":"SECRET_NAME"}}],"models":[{"display_name":"Must Not Render"}]}"#,
        );

        assert!(rendered.contains("Providers"));
        assert!(rendered.contains("OpenAI"));
        assert!(rendered.contains("openai"));
        assert!(rendered.contains("OAuth"));
        assert!(rendered.contains("enabled"));
        for forbidden in [
            "_atelier/provider/list",
            "must-not-render",
            "Must Not Render",
            "base_url",
            "credential",
            "SECRET_NAME",
            "{",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "Provider summary leaked {forbidden}: {rendered}"
            );
        }
    }

    #[test]
    fn empty_provider_list_has_an_actionable_empty_state() {
        assert_eq!(
            format_provider_list_response(r#"{"providers":[]}"#),
            "Providers\nNo Providers configured.\nAdd one with /provider add."
        );
    }

    #[test]
    fn wire_api_list_is_rendered_without_internal_rpc_names_or_json() {
        let rendered = format_runtime_extension_response(
            "_atelier/model/list",
            r#"{"models":[{"key":{"provider_id":"example","model_id":"alpha"},"display_name":"Alpha","wire_api":"chat_completions","context_window":100000},{"key":{"provider_id":"example","model_id":"beta"},"display_name":"Beta","wire_api":null,"context_window":null}]}"#,
        );

        assert!(rendered.contains("Wire APIs"));
        assert!(rendered.contains("example/alpha"));
        assert!(rendered.contains("chat_completions"));
        assert!(rendered.contains("example/beta"));
        for forbidden in ["_atelier/", "provider_id", "display_name", "{", "\""] {
            assert!(
                !rendered.contains(forbidden),
                "leaked {forbidden}: {rendered}"
            );
        }
    }

    #[test]
    fn role_list_is_rendered_as_a_human_readable_summary() {
        let rendered = format_runtime_extension_response(
            "_atelier/role/list",
            r#"{"roles":[{"roleId":"main","displayName":"MAIN","source":"config.toml","configured":true,"model":"example/alpha","config":null},{"roleId":"review","configured":false,"inherited":true,"effectiveSource":"general","config":null}]}"#,
        );

        assert!(rendered.contains("Roles"));
        assert!(rendered.contains("MAIN"));
        assert!(rendered.contains("example/alpha"));
        assert!(rendered.contains("review"));
        assert!(rendered.contains("inherits general"));
        for forbidden in ["_atelier/", "payload", "secret", "{", "\""] {
            assert!(
                !rendered.contains(forbidden),
                "leaked {forbidden}: {rendered}"
            );
        }
    }

    #[test]
    fn sparse_role_list_shows_exact_effective_and_context_sources_without_payload_values() {
        let rendered = format_runtime_extension_response(
            "_atelier/role/list",
            r#"{"roles":[{"roleId":"review","configured":true,"inherited":false,"effectiveSource":"review","config":{"effort":"high","payload":{"secret":"must-not-render"}},"effectiveConfig":{"provider":"example","model":"alpha","effort":"high","fast_mode":false,"payload":{"secret":"[REDACTED]"}},"contextSource":{"package":"custom","role":"general","empty":false}}]}"#,
        );

        assert!(rendered.contains("review"));
        assert!(rendered.contains("example/alpha"));
        assert!(rendered.contains("overrides effort, payload"));
        assert!(rendered.contains("Context custom/general"));
        assert!(!rendered.contains("must-not-render"));
        assert!(!rendered.contains("[REDACTED]"));
        assert!(!rendered.contains('{'));
    }

    #[test]
    fn runtime_status_is_rendered_without_payload_json() {
        let rendered = format_runtime_extension_response(
            "_atelier/runtime/status",
            r#"{"protocolVersion":1,"statuses":[{"sessionId":"session-1","state":"streaming_response","provider":"example","model":"alpha","diagnosticMessage":"working"}]}"#,
        );
        assert!(rendered.contains("Runtime status"));
        assert!(rendered.contains("example/alpha"));
        assert!(rendered.contains("working"));
        assert!(!rendered.contains("_atelier/"));
        assert!(!rendered.contains('{'));
    }

    #[test]
    fn unknown_runtime_responses_never_fall_back_to_raw_json() {
        let rendered = format_runtime_extension_response(
            "_atelier/future/method",
            r#"{"base_url":"https://private.example","credential":{"type":"environment","variable":"SECRET"}}"#,
        );

        assert_eq!(rendered, "Operation completed.");
    }

    #[test]
    fn provider_oauth_begin_is_rendered_before_completion_polling() {
        let (login_id, rendered) = provider_oauth_begin_ui(&serde_json::json!({
            "providerId": "example",
            "loginId": "login-1",
            "flow": "device-code",
            "verificationUrl": "https://login.example/device",
            "userCode": "ABCD-EFGH",
            "browserOpened": false,
        }))
        .expect("OAuth begin response");
        assert_eq!(login_id, "login-1");
        assert!(rendered.contains("https://login.example/device"));
        assert!(rendered.contains("ABCD-EFGH"));
        assert!(rendered.contains("open the URL manually"));
    }

    #[test]
    fn runtime_tasks_are_rendered_as_a_control_table() {
        let rendered = format_runtime_task_response(
            r#"{"tasks":[{"taskId":"task-1","sessionId":"session-1","role":"main","state":"waiting_for_permission","attachable":true}]}"#,
        );
        assert!(rendered.contains("ID"));
        assert!(rendered.contains("task-1"));
        assert!(rendered.contains("NEEDS INPUT"));
        assert!(rendered.contains("ATTACH"));
        assert!(rendered.contains("/attach <task-id>"));
    }

    #[test]
    fn auxiliary_tasks_are_marked_result_only() {
        let rendered = format_runtime_task_response(
            r#"{"tasks":[{"taskId":"btw-1","sessionId":"session-1","role":"main","state":"completed","attachable":false}]}"#,
        );
        assert!(rendered.contains("RESULT ONLY"));
        assert!(!rendered.contains("btw-1                         ATTACH"));
    }

    #[test]
    fn empty_runtime_tasks_have_a_clear_empty_state() {
        assert_eq!(
            format_runtime_task_response(r#"{"tasks":[]}"#),
            "Runtime tasks\nNo runtime tasks."
        );
    }

    #[test]
    fn runtime_task_ids_are_rendered_in_full_for_copying() {
        let task_id = "task-019f95b4-2ef1-7c9a-a14f-9c21f5a0f817";
        let rendered = format_runtime_task_response(
            &serde_json::json!({
                "tasks": [{
                    "taskId": task_id,
                    "sessionId": "session-1",
                    "role": "main",
                    "state": "running_model",
                    "attachable": true,
                }]
            })
            .to_string(),
        );

        assert!(rendered.contains(task_id));
    }

    #[test]
    fn attach_response_renders_replay_and_replay_metadata() {
        let rendered = format_runtime_task_attach_response(
            r#"{"task":{"taskId":"task-1"},"events":[{"eventId":7,"kind":"runtime.task_state_changed","details":{"state":"running_tool"}}],"cursor":7,"gap":true,"truncated":true}"#,
        )
        .expect("attach response");

        assert_eq!(rendered.task_id, "task-1");
        assert_eq!(rendered.cursor, 7);
        assert!(rendered.text.contains("runtime.task_state_changed"));
        assert!(rendered.text.contains("running_tool"));
        assert!(rendered.text.contains("gap"));
        assert!(rendered.text.contains("truncated"));
    }
}

fn provider_summary_field(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn provider_authentication_summary(provider: &serde_json::Value) -> &'static str {
    if let Some(authentication) = provider
        .get("authentication")
        .and_then(serde_json::Value::as_str)
    {
        return match authentication {
            "OAuth" => "OAuth",
            "Environment credential" => "Environment credential",
            "Stored credential" => "Stored credential",
            "Credential helper" => "Credential helper",
            "None" => "None",
            _ => "Configured",
        };
    }
    match provider
        .get("credential")
        .and_then(|credential| credential.get("type"))
        .and_then(serde_json::Value::as_str)
    {
        Some("o_auth" | "oauth") => "OAuth",
        Some("environment") => "Environment credential",
        Some("secret_store") => "Stored credential",
        Some("command") => "Credential helper",
        Some("none") | None => "None",
        Some(_) => "Configured",
    }
}

fn format_provider_list_response(response: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(response).unwrap_or_default();
    let result = value.get("result").unwrap_or(&value);
    let Some(providers) = result
        .get("providers")
        .and_then(serde_json::Value::as_array)
    else {
        return "Providers\nCould not read the Provider list.".to_owned();
    };
    if providers.is_empty() {
        return "Providers\nNo Providers configured.\nAdd one with /provider add.".to_owned();
    }

    let mut lines = vec!["Providers".to_owned()];
    for provider in providers {
        let id = provider
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(provider_summary_field)
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "unknown".to_owned());
        let display_name = provider
            .get("displayName")
            .or_else(|| provider.get("display_name"))
            .and_then(serde_json::Value::as_str)
            .map(provider_summary_field)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.clone());
        let authentication = provider_authentication_summary(provider);
        let status = if provider
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
        {
            "enabled"
        } else {
            "disabled"
        };
        if display_name == id {
            lines.push(format!("- {id} | {authentication} | {status}"));
        } else {
            lines.push(format!(
                "- {display_name} ({id}) | {authentication} | {status}"
            ));
        }
    }
    lines.push("Manage Providers with /provider add, /provider edit, or /provider delete.".into());
    lines.join("\n")
}

fn format_runtime_task_response(response: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(response).unwrap_or_default();
    let result = value.get("result").unwrap_or(&value);
    let Some(tasks) = result.get("tasks").and_then(|tasks| tasks.as_array()) else {
        return format_runtime_extension_response("_atelier/task/list", response);
    };
    if tasks.is_empty() {
        return "Runtime tasks\nNo runtime tasks.".to_owned();
    }
    let mut lines = vec![
        "Runtime tasks".to_owned(),
        "ID                   Session              Role       State                  Access"
            .to_owned(),
    ];
    for task in tasks {
        let id = task
            .get("taskId")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        let session = task
            .get("sessionId")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        let role = task
            .get("role")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        let state = task
            .get("state")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        let state = if state == "waiting_for_permission" {
            "NEEDS INPUT"
        } else {
            state
        };
        let access = if task
            .get("attachable")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
        {
            "ATTACH"
        } else {
            "RESULT ONLY"
        };
        lines.push(format!(
            "{:<20} {:<20} {:<10} {:<22} {}",
            id,
            truncate_runtime_task_field(session, 20),
            truncate_runtime_task_field(role, 10),
            state,
            access,
        ));
    }
    lines.push(String::new());
    lines.push("Attach: /attach <task-id>    Stop: /stop <task-id>".to_owned());
    lines.join("\n")
}

fn truncate_runtime_task_field(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let mut result: String = value.chars().take(max.saturating_sub(1)).collect();
    result.push('…');
    result
}

struct RuntimeTaskAttachReplay {
    task_id: String,
    cursor: u64,
    text: String,
}

struct ForegroundDerivedSession {
    session_id: String,
    pending_first_prompt: Option<String>,
}

fn foreground_derived_session(
    method: &str,
    result: Option<&serde_json::Value>,
) -> Option<ForegroundDerivedSession> {
    if !matches!(
        method,
        "_atelier/agent/spawn_derived" | "atelier/agent/spawn_derived"
    ) {
        return None;
    }
    let result = result?;
    if result
        .get("background")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let session_id = result.get("sessionId")?.as_str()?.to_owned();
    let pending_first_prompt = result
        .get("pendingFirstPrompt")
        .and_then(serde_json::Value::as_str)
        .filter(|prompt| !prompt.trim().is_empty())
        .map(str::to_owned);
    Some(ForegroundDerivedSession {
        session_id,
        pending_first_prompt,
    })
}

#[cfg(test)]
pub(super) fn remembered_task_cursor(
    _agent_id: crate::app::agent::AgentId,
    task_id: &str,
) -> Option<u64> {
    crate::slash::commands::attach::runtime_task_cursor(task_id)
}

fn format_runtime_task_attach_response(response: &str) -> Option<RuntimeTaskAttachReplay> {
    let value = serde_json::from_str::<serde_json::Value>(response).ok()?;
    let result = value.get("result").unwrap_or(&value);
    let task_id = result.get("task")?.get("taskId")?.as_str()?.to_owned();
    let events = result
        .get("events")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let cursor = result
        .get("cursor")
        .or_else(|| result.get("subscriptionCursor"))
        .or_else(|| result.get("lastEventId"))
        .or_else(|| result.get("afterEventId"))
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            events
                .last()
                .and_then(|event| event.get("eventId"))
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or_default();
    let gap = result
        .get("gap")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let truncated = result
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let mut lines = vec![format!("Attached to runtime task {task_id}")];
    if events.is_empty() {
        lines.push(format!("Replay: no events (cursor {cursor})"));
    } else {
        lines.push(format!(
            "Replay: {} event(s) (cursor {cursor})",
            events.len()
        ));
        for event in events {
            let event_id = event
                .get("eventId")
                .and_then(serde_json::Value::as_u64)
                .map(|id| id.to_string())
                .unwrap_or_else(|| "?".to_owned());
            let kind = event
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("runtime.event");
            let details = match event.get("details") {
                None | Some(serde_json::Value::Null) => String::new(),
                Some(serde_json::Value::String(text)) => format!(": {text}"),
                Some(details) => format!(": {details}"),
            };
            lines.push(format!("#{event_id} {kind}{details}"));
        }
    }
    if gap {
        lines.push("Replay gap detected before the returned events.".to_owned());
    }
    if truncated {
        lines.push("Replay was truncated by the runtime event buffer.".to_owned());
    }

    Some(RuntimeTaskAttachReplay {
        task_id,
        cursor,
        text: lines.join("\n"),
    })
}
use super::transcript::{
    handle_hooks_list_loaded, handle_marketplace_list_loaded, handle_marketplace_updates_available,
    handle_mcp_toggle_done, handle_plugins_list_loaded, handle_skills_toggle_done,
};
use super::turn::handle_bg_task_killed;
use crate::app::actions::{
    Action, ClipboardPasteCompletion, ClipboardPasteContext, ClipboardPasteFailure,
    ClipboardPasteTarget, Effect, ProbedAttachment, SubagentKillOutcome, TaskResult,
};
use crate::app::app_view::{ActiveView, AppView, AuthState};
use crate::scrollback::block::RenderBlock;
use agent_client_protocol as acp;
pub(super) fn unregister_session_effect(session_id: Option<acp::SessionId>) -> Vec<Effect> {
    session_id
        .map(|sid| Effect::UnregisterActiveSession { session_id: sid })
        .into_iter()
        .collect()
}
pub(super) fn unregister_all_active_sessions(app: &AppView) -> Vec<Effect> {
    app.agents
        .values()
        .filter_map(|a| {
            a.session
                .session_id
                .as_ref()
                .map(|sid| Effect::UnregisterActiveSession {
                    session_id: sid.clone(),
                })
        })
        .collect()
}
pub(super) const X11_PRIMARY_PASTE_HINT: &str = "Try Shift+Insert to paste selected text";
fn show_clipboard_toast(target: &ClipboardPasteTarget, message: &str, app: &mut AppView) {
    match target {
        ClipboardPasteTarget::AgentPrompt { agent_id, .. } => {
            if let Some(agent) = app.agents.get_mut(agent_id) {
                agent.show_toast(message);
            }
        }
        ClipboardPasteTarget::DashboardDispatch | ClipboardPasteTarget::DashboardPeek { .. } => {
            if let Some(dashboard) = app.dashboard.as_mut() {
                dashboard.error_toast = Some(message.to_owned());
            }
        }
    }
}
pub(super) fn maybe_show_x11_primary_paste_hint(
    eligible: bool,
    completion: ClipboardPasteCompletion,
    target: &ClipboardPasteTarget,
    app: &mut AppView,
) {
    if !eligible || completion != ClipboardPasteCompletion::FullMiss {
        return;
    }
    show_clipboard_toast(target, X11_PRIMARY_PASTE_HINT, app);
}
pub(super) fn show_clipboard_failure(
    target: &ClipboardPasteTarget,
    failure: ClipboardPasteFailure,
    app: &mut AppView,
) {
    let message = match failure {
        ClipboardPasteFailure::AlreadyReported => return,
        ClipboardPasteFailure::TextRead => "Couldn't read clipboard text",
        ClipboardPasteFailure::AttachmentRead => "Couldn't read clipboard contents",
        ClipboardPasteFailure::TargetInsertion => "Couldn't paste clipboard contents",
    };
    show_clipboard_toast(target, message, app);
}
fn apply_clipboard_paste_result(
    ctx: ClipboardPasteContext,
    image: ProbedAttachment,
    file_urls: Option<String>,
    app: &mut AppView,
) -> ClipboardPasteCompletion {
    match ctx.target.clone() {
        ClipboardPasteTarget::AgentPrompt { agent_id, .. } => app
            .agents
            .get_mut(&agent_id)
            .map_or(ClipboardPasteCompletion::Dropped, |agent| {
                agent.complete_clipboard_attachment_paste(ctx, image, file_urls)
            }),
        ClipboardPasteTarget::DashboardDispatch | ClipboardPasteTarget::DashboardPeek { .. } => app
            .dashboard
            .as_mut()
            .map_or(ClipboardPasteCompletion::Dropped, |dashboard| {
                dashboard.complete_clipboard_attachment_paste(ctx, image, file_urls)
            }),
    }
}
fn drain_clipboard_target(target: &ClipboardPasteTarget, app: &mut AppView) -> Vec<Effect> {
    match target {
        ClipboardPasteTarget::AgentPrompt { agent_id, .. } => {
            let is_active = app.active_view == ActiveView::Agent(*agent_id);
            let Some(agent) = app.agents.get_mut(agent_id) else {
                return vec![];
            };
            let resend = agent.take_deferred_send_after_paste();
            let action = if is_active {
                resend.and_then(|kind| agent.build_deferred_send_action(kind))
            } else {
                None
            };
            let mut effects = std::mem::take(&mut agent.pending_effects);
            if let Some(action) = action {
                effects.extend(dispatch(action, app));
            }
            effects
        }
        ClipboardPasteTarget::DashboardDispatch | ClipboardPasteTarget::DashboardPeek { .. } => {
            let Some(dashboard) = app.dashboard.as_mut() else {
                return vec![];
            };
            let resends = dashboard.take_deferred_sends_after_paste();
            let mut effects = std::mem::take(&mut dashboard.pending_effects);
            if matches!(app.active_view, ActiveView::AgentDashboard) {
                for action in resends {
                    effects.extend(dispatch(action, app));
                }
            }
            effects
        }
    }
}
/// Handle a completed async task result.
pub(super) fn dispatch_task_result(result: TaskResult, app: &mut AppView) -> Vec<Effect> {
    match result {
        TaskResult::SessionCreated {
            agent_id,
            session_id,
            models: new_models,
        } => handle_session_created(app, agent_id, session_id, new_models),
        TaskResult::SessionFailed { agent_id, error } => {
            tracing::error!(
                agent = ? agent_id, error = % error, "Session creation failed"
            );
            let failed_from_dashboard = matches!(app.active_view, ActiveView::AgentDashboard);
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.pending_extensions_fetch = false;
                agent.session.prompt_history_loading = false;
                agent.mcp_init_progress = None;
                agent.scrollback.push_block(RenderBlock::system(format!(
                    "Session failed to start: {error}"
                )));
                agent.show_toast("Session failed to start");
            }
            if failed_from_dashboard && app.agents.contains_key(&agent_id) {
                app.active_view = ActiveView::Agent(agent_id);
            }
            vec![]
        }
        TaskResult::WorktreeSessionCreated {
            agent_id,
            session_id,
            worktree_path,
            session_cwd,
            models: new_models,
        } => handle_worktree_session_created(
            app,
            agent_id,
            session_id,
            worktree_path,
            session_cwd,
            new_models,
        ),
        TaskResult::WorktreeForked {
            agent_id,
            session_id,
            worktree_path,
            session_cwd,
            code_restored,
            restore_summary,
            restore_degree,
        } => handle_worktree_forked(
            app,
            agent_id,
            session_id,
            worktree_path,
            session_cwd,
            code_restored,
            restore_summary,
            restore_degree,
        ),
        TaskResult::WorktreeSessionFailed { agent_id, error } => {
            handle_worktree_session_failed(app, agent_id, error)
        }
        TaskResult::ForkSessionReady {
            agent_id,
            new_session_id,
            cwd,
        } => handle_fork_session_ready(app, agent_id, new_session_id, cwd),
        TaskResult::ForkSessionFailed { agent_id, error } => {
            handle_fork_session_failed(app, agent_id, error)
        }
        TaskResult::SessionLoaded {
            agent_id,
            session_id,
            models: new_models,
            code_restored,
            restore_summary,
            restore_degree,
            running_prompt_id,
        } => handle_session_loaded(
            app,
            agent_id,
            session_id,
            new_models,
            code_restored,
            restore_summary,
            restore_degree,
            running_prompt_id,
        ),
        TaskResult::SessionTitleFromDisk { agent_id, title } => {
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some((t, is_manual)) = title.filter(|(s, _)| !s.trim().is_empty())
            {
                if is_manual && agent.display_name.is_none() {
                    agent.display_name = Some(t.clone());
                }
                agent.generated_session_title = Some(t);
            }
            vec![]
        }
        TaskResult::SessionLoadFailed {
            agent_id,
            session_id,
            error,
        } => handle_session_load_failed(app, agent_id, session_id, error),
        TaskResult::SessionListLoaded {
            sessions,
            partial,
            seq,
            query,
        } => handle_session_list_loaded(app, sessions, partial, seq, query),
        TaskResult::ForeignSessionsScanned { entries, seq } => {
            handle_foreign_sessions_scanned(app, entries, seq)
        }
        TaskResult::ForeignResumeCwdCanonicalized {
            requested_cwd,
            canonical_cwd,
            launch_token,
        } => {
            let accepted_cwd = canonical_cwd.clone();
            if app.accept_foreign_resume_canonical_cwd(launch_token, &requested_cwd, canonical_cwd)
                && let Some(canonical_cwd) = accepted_cwd
            {
                vec![Effect::DetectForeignResumeHint {
                    canonical_cwd,
                    compat: app.foreign_session_compat,
                    atelier_home: atelier_tools::util::atelier_home::atelier_home(),
                    launch_token,
                }]
            } else {
                vec![]
            }
        }
        TaskResult::ForeignResumeHintDetected {
            canonical_cwd,
            launch_token,
            hint,
        } => {
            app.apply_foreign_resume_detection(launch_token, &canonical_cwd, hint);
            vec![]
        }
        TaskResult::SessionListFailed { error, seq, query } => {
            handle_session_list_failed(app, error, seq, query)
        }
        TaskResult::SessionSearchDebounceExpired { query, seq } => {
            handle_session_search_debounce_expired(app, query, seq)
        }
        TaskResult::RosterLoaded { sessions } => {
            app.leader_roster = sessions;
            vec![]
        }
        TaskResult::RosterFailed { error } => {
            tracing::debug!(error = % error, "leader roster fetch failed");
            vec![]
        }
        TaskResult::DashboardSessionsLoaded { sessions } => {
            app.dashboard_local_sessions = sessions;
            app.dashboard_sessions_loading = false;
            vec![]
        }
        TaskResult::CardDetailLoaded {
            source,
            session_id,
            generation,
            detail,
        } => handle_card_detail_loaded(app, source, session_id, generation, detail),
        TaskResult::SessionRestored {
            agent_id,
            local_session_id,
        } => handle_session_restored(app, agent_id, local_session_id),
        TaskResult::SessionRestoreFailed { agent_id, error } => {
            handle_session_restore_failed(app, agent_id, error)
        }
        TaskResult::SessionRestoreProgress { agent_id, message } => {
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && !defer_to_open_reload_window(agent, agent_id, "SessionRestoreProgress")
            {
                agent.scrollback.push_block(RenderBlock::system(message));
            }
            vec![]
        }
        TaskResult::PromptResponse {
            agent_id,
            result,
            http_status,
            prompt_id,
        } => handle_prompt_response(app, agent_id, result, http_status, prompt_id),
        TaskResult::SendPromptNowFailed {
            agent_id,
            session_id,
            prompt_id,
            error,
            blocks,
        } => {
            let sid = session_id.0.to_string();
            super::queue::retire_optimistic_echo(
                &mut app.optimistic_prompt_echoes,
                &mut app.shared_prompt_queues,
                &sid,
                &prompt_id,
            );
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.shared_queue.retain(|e| e.id != prompt_id);
                agent.note_queue_echo_retired(&prompt_id);
                if agent.expect_send_now_cancel.as_deref() == Some(prompt_id.as_str())
                    || agent.follow_without_jump_prompt_id.as_deref() == Some(prompt_id.as_str())
                {
                    agent.clear_send_now_expectation();
                }
                agent.retire_send_now_painted_block(&prompt_id);
                let text = blocks
                    .iter()
                    .find_map(|b| match b {
                        acp::ContentBlock::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let id = agent.session.next_queue_id;
                agent.session.next_queue_id += 1;
                agent
                    .session
                    .pending_prompts
                    .push_front(crate::app::agent::QueuedPrompt {
                        wire_blocks: Some(blocks),
                        ..crate::app::agent::QueuedPrompt::plain(
                            id,
                            &text,
                            crate::app::agent::QueueEntryKind::Prompt,
                        )
                    });
                agent.show_toast(&format!("Send now failed — requeued: {error}"));
            }
            vec![]
        }
        TaskResult::CancelComplete => {
            tracing::trace!("Cancel notification sent successfully");
            vec![]
        }
        TaskResult::KillSubagentComplete {
            session_id,
            subagent_id,
            outcome,
        } => {
            if let SubagentKillOutcome::NothingLive { status } = outcome {
                let status = status.as_deref().unwrap_or("cancelled");
                crate::app::acp_handler::finalize_killed_subagent(
                    app,
                    &session_id,
                    &subagent_id,
                    status,
                );
            }
            vec![]
        }
        TaskResult::CompactComplete { agent_id, result } => {
            handle_compact_complete(app, agent_id, result)
        }
        TaskResult::SwitchModelComplete {
            agent_id,
            model_id,
            effort,
            result,
            prev_model_id,
        } => handle_switch_model_complete(app, agent_id, model_id, effort, result, prev_model_id),
        TaskResult::BgTaskKilled {
            session_id,
            task_id,
            outcome,
        } => handle_bg_task_killed(app, session_id, task_id, outcome),
        TaskResult::BgTaskKillFailed {
            session_id,
            task_id,
            error,
        } => {
            tracing::warn!(
                task_id = % task_id, error = % error, "Failed to kill bg task"
            );
            if let Some(agent) = find_agent_by_session_id(&mut app.agents, &session_id)
                && let Some(task) = agent.session.bg_tasks.get_mut(&task_id)
            {
                task.pending_kill = false;
                task.kill_requested_at = None;
            }
            vec![]
        }
        TaskResult::ChangelogFetched { markdown, entries } => {
            app.changelog_markdown = markdown;
            app.changelog_bullets =
                atelier_shell::util::changelog::bullets_from_entries(&entries, 3);
            vec![]
        }
        TaskResult::ClipboardAttachmentProbed {
            ctx,
            image,
            file_urls,
        } => {
            let is_clipboard_key = ctx.source.is_clipboard_key();
            let primary_hint_eligible = is_clipboard_key
                && !app.screen_mode.is_minimal()
                && crate::clipboard::x11_primary_guidance_available();
            let target = ctx.target.clone();
            let wrap_text = if is_clipboard_key {
                ctx.source.text().map(str::to_owned)
            } else {
                None
            };
            let completion = apply_clipboard_paste_result(ctx, image, file_urls, app);
            let wrap_request_emitted = completion == ClipboardPasteCompletion::FullMiss
                && is_clipboard_key
                && crate::wrap_clipboard_image::maybe_request_wrap_host_image(
                    None,
                    wrap_text.as_deref(),
                    None,
                );
            let effects = drain_clipboard_target(&target, app);
            maybe_show_x11_primary_paste_hint(
                primary_hint_eligible && !wrap_request_emitted,
                completion,
                &target,
                app,
            );
            if let ClipboardPasteCompletion::Failed(failure) = completion {
                show_clipboard_failure(&target, failure, app);
            }
            effects
        }
        TaskResult::PromptImagePreviewPrepared => vec![],
        TaskResult::AnnouncementsHiddenPersisted { result } => {
            if let Err(e) = result {
                tracing::warn!("Failed to persist announcements hidden state: {}", e);
            }
            vec![]
        }
        TaskResult::PromptHistoryLoaded { agent_id, prompts } => {
            use atelier_tools::implementations::skills::skill::extract_skill_display_text;
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.session.prompt_history_loading = false;
                agent.session.prompt_history = prompts
                    .into_iter()
                    .map(|p| extract_skill_display_text(&p).unwrap_or(p))
                    .collect();
                if agent.prompt.history_search.is_active() {
                    let history = agent.combined_prompt_history();
                    agent.prompt.history_search.refresh_items(&history);
                    if !agent.prompt.history_search.is_browse() {
                        let query = agent.prompt.text().to_owned();
                        agent.prompt.history_search.update_query(&query);
                    }
                }
            }
            vec![]
        }
        TaskResult::AuthComplete { request_seq, meta } => {
            handle_auth_complete(app, request_seq, meta)
        }
        TaskResult::AuthFailed { request_seq, error } => {
            if let AuthState::Authenticating {
                request_seq: current_seq,
                ..
            } = &app.auth_state
                && *current_seq == request_seq
            {
                app.auth_state = AuthState::Pending { error: Some(error) };
                app.auth_code_input.clear();
            }
            vec![]
        }
        TaskResult::AuthUrlReady {
            request_seq,
            auth_url,
            external,
            mode,
        } => handle_auth_url_ready(app, request_seq, auth_url, external, mode),
        TaskResult::AuthCodeSubmitted { .. } => vec![],
        TaskResult::McpsListLoaded { agent_id, result } => {
            use crate::views::extensions_modal::TabDataState;
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some(ref mut modal) = agent.extensions_modal
            {
                modal.pending_action = None;
                modal.pending_entry_index = None;
                modal.mcps_data = match result {
                    Ok(response) => TabDataState::Loaded(response),
                    Err(e) => TabDataState::Error(e),
                };
            }
            vec![]
        }
        TaskResult::McpAuthTriggerDone {
            agent_id,
            server_name,
            result,
        } => handle_mcp_auth_trigger_done(app, agent_id, server_name, result),
        TaskResult::HooksListLoaded { agent_id, result } => {
            handle_hooks_list_loaded(app, agent_id, result)
        }
        TaskResult::PluginsListLoaded { agent_id, result } => {
            handle_plugins_list_loaded(app, agent_id, result)
        }
        TaskResult::HooksActionResult { agent_id, result }
        | TaskResult::PluginsActionResult { agent_id, result }
        | TaskResult::MarketplaceActionResult { agent_id, result } => {
            dispatch_action_result(app, agent_id, result)
        }
        TaskResult::CtaPluginInstallDone {
            agent_id,
            plugin_name,
            result,
        } => handle_cta_plugin_install_done(app, agent_id, plugin_name, result),
        TaskResult::CtaPluginReloadDone {
            agent_id,
            plugin_name,
            result,
        } => handle_cta_plugin_reload_done(app, agent_id, plugin_name, result),
        TaskResult::PluginCtaMcpsLoaded {
            agent_id,
            plugin_name,
            result,
        } => handle_plugin_cta_mcps_loaded(app, agent_id, plugin_name, result),
        TaskResult::CtaInstalledDismissTimeout {
            agent_id,
            plugin_name,
        } => {
            use crate::app::agent_view::CtaPhase;
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && let CtaPhase::Installed { name } = &agent.plugin_cta.phase
                && *name == plugin_name
            {
                agent.plugin_cta.phase = CtaPhase::Hidden;
            }
            vec![]
        }
        TaskResult::McpToggleDone { agent_id, result } => {
            handle_mcp_toggle_done(app, agent_id, result)
        }
        TaskResult::MarketplaceUpdatesAvailable { agent_id, updates } => {
            handle_marketplace_updates_available(app, agent_id, updates)
        }
        TaskResult::MarketplaceListLoaded { agent_id, result } => {
            handle_marketplace_list_loaded(app, agent_id, result)
        }
        TaskResult::PluginCtaCatalogLoaded { agent_id, result } => {
            handle_plugin_cta_catalog_loaded(app, agent_id, result)
        }
        TaskResult::SkillsListLoaded { agent_id, result } => {
            use crate::views::extensions_modal::TabDataState;
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some(ref mut modal) = agent.extensions_modal
            {
                modal.skills_data = match result {
                    Ok(skills) => TabDataState::Loaded(skills),
                    Err(e) => TabDataState::Error(e),
                };
                modal.pending_action = None;
                modal.pending_entry_index = None;
            }
            vec![]
        }
        TaskResult::SkillsToggleDone { agent_id, result } => {
            handle_skills_toggle_done(app, agent_id, result)
        }
        TaskResult::SessionAgentNameResolved {
            agent_id,
            agent_name,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.session_agent_name = agent_name;
            }
            vec![]
        }
        TaskResult::SessionInfoComplete {
            agent_id,
            info,
            text,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.session_agent_name = info.data.agent_name;
                agent.apply_full_context_info(info.data.context);
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(text));
            }
            vec![]
        }
        TaskResult::SessionInfoFailed { agent_id, error } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't load session info: {error}"
                    )));
            }
            vec![]
        }
        TaskResult::RenameSessionComplete { agent_id, title } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                let safe = crate::views::session_title::sanitize_display_text(&title);
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Session renamed to \"{safe}\""
                    )));
            }
            vec![]
        }
        TaskResult::RenameSessionFailed { agent_id, error } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't rename session: {error}"
                    )));
            }
            vec![]
        }
        TaskResult::DeleteSessionComplete { source, session_id } => {
            remove_session_from_pickers(app, &source, &session_id);
            app.show_toast("Session deleted");
            vec![]
        }
        TaskResult::DeleteSessionFailed {
            source,
            session_id,
            error,
        } => {
            tracing::warn!(
                source, session_id = % session_id, error = % error,
                "session delete failed"
            );
            app.show_toast(&format!("Couldn't delete session: {error}"));
            vec![]
        }
        TaskResult::ContextInfoComplete { agent_id, info } => {
            handle_context_info_complete(app, agent_id, info)
        }
        TaskResult::ContextInfoFailed { agent_id, error } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't load context info: {error}"
                    )));
            }
            vec![]
        }
        TaskResult::FeedbackComplete { .. } => vec![],
        TaskResult::FeedbackFailed { agent_id, error } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't send feedback: {error}"
                    )));
            }
            vec![]
        }
        TaskResult::MemoryNoteSaved { agent_id, result } => {
            handle_memory_note_saved(app, agent_id, result)
        }
        TaskResult::MemoryNoteRewritten {
            agent_id,
            result,
            nonce,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && let Ok(markdown) = result
                && let Some(crate::views::modal::ActiveModal::RememberNoteReview {
                    ref mut enhanced_content,
                    ref mut cached_lines,
                    rewrite_nonce,
                    ..
                }) = agent.active_modal
                && rewrite_nonce == nonce
            {
                *enhanced_content = Some(markdown);
                *cached_lines = None;
            }
            vec![]
        }
        TaskResult::BundleStatusReady {
            has_cache,
            version,
            skills,
        } => {
            app.bundle_state.has_cache = has_cache;
            app.bundle_state.version = version.unwrap_or_default();
            app.bundle_state.skills = skills;
            vec![]
        }
        TaskResult::BundleStatusFailed { error } => {
            tracing::warn!(error = % error, "bundle status fetch failed");
            vec![]
        }
        TaskResult::BtwResponse { agent_id, result } => handle_btw_response(app, agent_id, result),
        TaskResult::RuntimeExtensionFailed {
            agent_id,
            method,
            error,
        } => {
            if method.starts_with("_atelier/role/") || method.starts_with("atelier/role/") {
                if let Some(agent_id) = agent_id
                    && let Some(agent) = app.agents.get_mut(&agent_id)
                    && let Some(crate::views::modal::ActiveModal::Roles { state }) =
                        agent.active_modal.as_mut()
                {
                    state.fail(error);
                    return vec![];
                }
            }
            if matches!(
                method.as_str(),
                "_atelier/provider/create"
                    | "atelier/provider/create"
                    | "_atelier/provider/update"
                    | "atelier/provider/update"
                    | "_atelier/provider/test"
                    | "atelier/provider/test"
                    | "_atelier/provider/oauth_begin"
                    | "atelier/provider/oauth_begin"
                    | "_atelier/provider/oauth_complete"
                    | "atelier/provider/oauth_complete"
            ) && let Some(agent_id) = agent_id
                && let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some(crate::views::modal::ActiveModal::ProviderWizard { state }) =
                    agent.active_modal.as_mut()
            {
                state.fail(error);
                return vec![];
            }
            let toast = format!("{method} failed: {error}");
            if let Some(agent_id) = agent_id
                && let Some(agent) = app.agents.get_mut(&agent_id)
            {
                agent.show_toast(&toast);
            } else if !show_dashboard_runtime_output(app, &toast) {
                app.show_toast(&toast);
            }
            vec![]
        }
        TaskResult::RuntimeExtensionComplete {
            agent_id,
            method,
            response,
        } => {
            let parsed = serde_json::from_str::<serde_json::Value>(&response).ok();
            let result = parsed
                .as_ref()
                .and_then(|value| value.get("result"))
                .or_else(|| parsed.as_ref());
            let is_task_attach =
                method == "_atelier/task/attach" || method == "atelier/task/attach";
            let attach_replay = is_task_attach
                .then(|| format_runtime_task_attach_response(&response))
                .flatten();
            let is_role_list =
                matches!(method.as_str(), "_atelier/role/list" | "atelier/role/list");
            if is_role_list
                && let Some(agent_id) = agent_id
                && let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some(crate::views::modal::ActiveModal::Roles { state }) =
                    agent.active_modal.as_mut()
            {
                if let Err(error) = state.apply_response(&response) {
                    state.fail(error);
                }
                return vec![];
            }
            let is_role_modal_action = matches!(
                method.as_str(),
                "_atelier/role/delete"
                    | "atelier/role/delete"
                    | "_atelier/role/test"
                    | "atelier/role/test"
                    | "_atelier/role/set_fast_mode"
                    | "atelier/role/set_fast_mode"
            );
            if is_role_modal_action
                && let Some(agent_id) = agent_id
                && let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some(crate::views::modal::ActiveModal::Roles { state }) =
                    agent.active_modal.as_mut()
            {
                let status = format_runtime_extension_response(&method, &response);
                if matches!(
                    method.as_str(),
                    "_atelier/role/set_fast_mode" | "atelier/role/set_fast_mode"
                ) && result
                    .and_then(|value| value.get("roleId"))
                    .and_then(|value| value.as_str())
                    == Some("main")
                    && let Some(enabled) = result
                        .and_then(|value| value.get("enabled"))
                        .and_then(serde_json::Value::as_bool)
                {
                    state.apply_fast_mode("main", enabled, status);
                    return vec![];
                }
                state.begin_reload(status);
                return vec![Effect::RuntimeExtension {
                    agent_id: Some(agent_id),
                    method: "_atelier/role/list".into(),
                    params: serde_json::json!({}),
                }];
            }
            if matches!(
                method.as_str(),
                "_atelier/provider/create"
                    | "atelier/provider/create"
                    | "_atelier/provider/update"
                    | "atelier/provider/update"
            ) && let Some(agent_id) = agent_id
                && let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some(crate::views::modal::ActiveModal::ProviderWizard { state }) =
                    agent.active_modal.as_mut()
            {
                state.mark_persisted();
                let provider_id = state.provider_id.clone();
                if let Some(flow) = state.oauth_flow_name() {
                    state.set_status("Starting Provider OAuth login…");
                    return vec![Effect::RuntimeExtension {
                        agent_id: Some(agent_id),
                        method: "_atelier/provider/oauth_begin".into(),
                        params: serde_json::json!({
                            "providerId": provider_id,
                            "flow": flow,
                        }),
                    }];
                }
                state.set_status("Testing Provider connection…");
                return vec![Effect::RuntimeExtension {
                    agent_id: Some(agent_id),
                    method: "_atelier/provider/test".into(),
                    params: serde_json::json!({ "providerId": provider_id }),
                }];
            }
            if matches!(
                method.as_str(),
                "_atelier/provider/test" | "atelier/provider/test"
            ) && let Some(agent_id) = agent_id
                && let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some(crate::views::modal::ActiveModal::ProviderWizard { state }) =
                    agent.active_modal.as_mut()
            {
                let provider_id = state.provider_id.clone();
                state.set_status("Discovering Provider models…");
                return vec![Effect::RefreshProviderModels {
                    agent_id: Some(agent_id),
                    provider_id,
                }];
            }
            if method == "_atelier/provider/oauth_begin" || method == "atelier/provider/oauth_begin"
            {
                let Some(result) = result else {
                    return vec![];
                };
                let Some((login_id, rendered)) = provider_oauth_begin_ui(result) else {
                    return vec![];
                };
                if let Some(agent_id) = agent_id
                    && let Some(agent) = app.agents.get_mut(&agent_id)
                {
                    agent
                        .scrollback
                        .push_block(RenderBlock::system(rendered.clone()));
                    if let Some(crate::views::modal::ActiveModal::ProviderWizard { state }) =
                        agent.active_modal.as_mut()
                    {
                        state.set_status("Completing Provider OAuth login…");
                    }
                } else if !show_dashboard_runtime_output(app, &rendered) {
                    app.show_toast(&rendered);
                }
                return vec![Effect::RuntimeExtension {
                    agent_id,
                    method: "_atelier/provider/oauth_complete".into(),
                    params: serde_json::json!({ "loginId": login_id }),
                }];
            }
            if matches!(
                method.as_str(),
                "_atelier/provider/oauth_complete" | "atelier/provider/oauth_complete"
            ) && let Some(agent_id) = agent_id
                && let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some(crate::views::modal::ActiveModal::ProviderWizard { state }) =
                    agent.active_modal.as_mut()
            {
                let provider_id = state.provider_id.clone();
                state.set_status("Discovering Provider models…");
                return vec![Effect::RefreshProviderModels {
                    agent_id: Some(agent_id),
                    provider_id,
                }];
            }
            if method == "_atelier/config/update" || method == "atelier/config/update" {
                let should_switch = result
                    .and_then(|value| value.get("switch"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let model_id = result
                    .and_then(|value| value.get("model"))
                    .and_then(serde_json::Value::as_str)
                    .map(acp::ModelId::new);
                let effort = result
                    .and_then(|value| value.get("effort"))
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| {
                        value
                            .parse::<atelier_shell::sampling::types::ReasoningEffort>()
                            .ok()
                    });
                if should_switch && let Some(model_id) = model_id {
                    if agent_id.is_some() {
                        return dispatch(Action::SwitchModel { model_id, effort }, app);
                    }
                    app.models.set_current(model_id.clone(), effort);
                    if let Some(dashboard) = app.dashboard.as_mut() {
                        dashboard.models.set_current(model_id, effort);
                    }
                    let rendered = format_runtime_extension_response(&method, &response);
                    if !show_dashboard_runtime_output(app, &rendered) {
                        app.show_toast(&rendered);
                    }
                    return vec![];
                }
            }
            if method == "_atelier/provider/list" || method == "atelier/provider/list" {
                let rendered = format_provider_list_response(&response);
                if let Some(agent_id) = agent_id
                    && let Some(agent) = app.agents.get_mut(&agent_id)
                {
                    agent
                        .scrollback
                        .push_block(crate::scrollback::block::RenderBlock::system(rendered));
                } else if !show_dashboard_runtime_output(app, &rendered) {
                    app.show_toast(&rendered);
                }
                return vec![];
            }
            if method == "_atelier/task/list" || method == "atelier/task/list" {
                let rendered = format_runtime_task_response(&response);
                if let Some(agent_id) = agent_id
                    && let Some(agent) = app.agents.get_mut(&agent_id)
                {
                    agent
                        .scrollback
                        .push_block(crate::scrollback::block::RenderBlock::system(rendered));
                } else if !show_dashboard_runtime_output(app, &rendered) {
                    app.show_toast(&rendered);
                }
                return vec![];
            }
            if method == "_atelier/btw/persist" || method == "atelier/btw/persist" {
                let persisted = result
                    .and_then(|value| value.get("persisted"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if let Some(agent_id) = agent_id
                    && let Some(agent) = app.agents.get_mut(&agent_id)
                {
                    if persisted {
                        if let Some(state) = agent.btw_state.as_mut() {
                            state.mark_persisted();
                        }
                        agent.show_toast("BTW saved locally");
                    } else {
                        agent.show_toast("BTW was not saved");
                    }
                }
                return vec![];
            }
            if is_task_attach {
                if let Some(replay) = attach_replay.as_ref() {
                    crate::slash::commands::attach::remember_runtime_task_cursor(
                        &replay.task_id,
                        replay.cursor,
                    );
                }
                let target_session = result
                    .and_then(|value| value.get("task"))
                    .and_then(|task| task.get("sessionId"))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
                let mut effects = Vec::new();
                if let Some(session_id) = target_session.as_deref() {
                    let active_session = agent_id
                        .and_then(|agent_id| app.agents.get(&agent_id))
                        .and_then(|agent| agent.session.session_id.as_ref())
                        .map(ToString::to_string);
                    if active_session.as_deref() != Some(session_id) {
                        effects =
                            dispatch(Action::LoadSession(session_id.to_owned(), None, false), app);
                    }
                }
                if let Some(replay) = attach_replay.as_ref() {
                    let target_agent_id = if let Some(session_id) = target_session.as_deref() {
                        app.agents.iter().find_map(|(candidate_id, agent)| {
                            agent
                                .session
                                .session_id
                                .as_ref()
                                .is_some_and(|candidate_session| {
                                    candidate_session.0.as_ref() == session_id
                                })
                                .then_some(*candidate_id)
                        })
                    } else {
                        agent_id
                    };
                    if let Some(agent) =
                        target_agent_id.and_then(|agent_id| app.agents.get_mut(&agent_id))
                    {
                        agent
                            .scrollback
                            .push_block(RenderBlock::system(replay.text.clone()));
                    } else {
                        app.show_toast(&replay.text);
                    }
                }
                if !effects.is_empty() {
                    return effects;
                }
            }
            if method == "_atelier/task/detach" || method == "atelier/task/detach" {
                let detached = result
                    .and_then(|value| value.get("detached"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if detached {
                    return dispatch(Action::OpenDashboard, app);
                }
            }
            let derived_session = foreground_derived_session(&method, result);
            if let Some(agent_id) = agent_id
                && let Some(agent) = app.agents.get_mut(&agent_id)
            {
                if derived_session.is_none() {
                    let response = if is_task_attach && attach_replay.is_some() {
                        None
                    } else if method == "_atelier/task/detach" || method == "atelier/task/detach" {
                        let message = result
                            .and_then(|value| value.get("message"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Turn was not detached");
                        Some(message.to_owned())
                    } else {
                        Some(format_runtime_extension_response(&method, &response))
                    };
                    if let Some(response) = response {
                        agent
                            .scrollback
                            .push_block(crate::scrollback::block::RenderBlock::system(response));
                    }
                }
            } else if derived_session.is_none() {
                let rendered = format_runtime_extension_response(&method, &response);
                if !show_dashboard_runtime_output(app, &rendered) {
                    app.show_toast(&rendered);
                }
            }
            if let Some(derived_session) = derived_session {
                let effects = dispatch(
                    Action::LoadSession(derived_session.session_id.clone(), None, false),
                    app,
                );
                if let Some(prompt) = derived_session.pending_first_prompt
                    && let Some(agent) = app.agents.values_mut().find(|agent| {
                        agent.session.session_id.as_ref().is_some_and(|session_id| {
                            session_id.0.as_ref() == derived_session.session_id
                        })
                    })
                {
                    agent.pending_first_prompt = Some(prompt);
                }
                return effects;
            }
            vec![]
        }
        TaskResult::ProviderModelsRefreshed {
            agent_id,
            provider_id,
            result,
        } => {
            if let Some(agent_id) = agent_id {
                let wizard_active = app
                    .agents
                    .get(&agent_id)
                    .and_then(|agent| agent.active_modal.as_ref())
                    .is_some_and(|modal| {
                        matches!(
                            modal,
                            crate::views::modal::ActiveModal::ProviderWizard { state }
                                if state.provider_id == provider_id
                        )
                    });
                if wizard_active {
                    match result {
                        Ok(message) => {
                            if let Some(agent) = app.agents.get_mut(&agent_id) {
                                agent.active_modal = None;
                                agent.scrollback.push_block(RenderBlock::system(format!(
                                    "Provider {provider_id}: {message}"
                                )));
                            }
                            return dispatch(
                                Action::OpenSlashArgPicker {
                                    command: "model".into(),
                                },
                                app,
                            );
                        }
                        Err(error) => {
                            if let Some(agent) = app.agents.get_mut(&agent_id)
                                && let Some(crate::views::modal::ActiveModal::ProviderWizard {
                                    state,
                                }) = agent.active_modal.as_mut()
                            {
                                state.fail(error);
                            }
                            return vec![];
                        }
                    }
                }
            }
            let message = match result {
                Ok(message) => format!("Provider {provider_id}: {message}"),
                Err(error) => format!("Provider {provider_id}: {error}"),
            };
            if let Some(agent_id) = agent_id {
                if let Some(agent) = app.agents.get_mut(&agent_id) {
                    agent.scrollback.push_block(RenderBlock::system(message));
                }
            } else {
                // Welcome has no agent scrollback. The actual model catalog is
                // delivered separately through `atelier/models/update`; keep
                // the completion visible in diagnostics for this sessionless
                // path instead of silently dropping it.
                tracing::info!(%message, "provider model refresh completed without active agent");
                app.show_toast(&message);
            }
            vec![]
        }
        TaskResult::InterjectQueued { .. } => vec![],
        TaskResult::RecapRequested {
            session_id,
            auto,
            error,
        } => {
            if let Some(error) = error {
                tracing::debug!(% error, "recap request failed");
                if !auto
                    && let Some(agent) = find_agent_by_session_id(&mut app.agents, &session_id.0)
                    && let Some(pending_id) = agent.pending_recap_entry.take()
                {
                    agent.scrollback.remove_entry(pending_id);
                    agent.show_toast(super::recap_unavailable_toast(
                        super::scrollback_has_user_messages(&agent.scrollback),
                    ));
                }
            }
            vec![]
        }
        TaskResult::InterjectFailed {
            agent_id,
            error,
            text,
            blocks,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                let id = agent.session.next_queue_id;
                agent.session.next_queue_id += 1;
                agent
                    .session
                    .pending_prompts
                    .push_front(crate::app::agent::QueuedPrompt {
                        id,
                        text,
                        kind: crate::app::agent::QueueEntryKind::Prompt,
                        wire_blocks: blocks,
                        images: Vec::new(),
                        display_as_skill: false,
                        task_id: None,
                        human_schedule: None,
                        chip_elements: Vec::new(),
                        skill_token_ranges: Vec::new(),
                    });
                agent.show_toast(&format!("Interjection failed — requeued: {error}"));
            }
            vec![]
        }
        TaskResult::AvailableCommandsRefreshed { agent_id, commands } => {
            if !commands.is_empty()
                && let Some(agent) = app.agents.get_mut(&agent_id)
            {
                agent.session.available_commands = commands;
                agent.session.available_commands_generation += 1;
            }
            vec![]
        }
        TaskResult::AuthCopiedTimeout => {
            app.auth_clipboard_copied = false;
            vec![]
        }
        TaskResult::LogoutComplete => {
            app.auth_state = AuthState::Pending { error: None };
            app.announcement_cta_impressions_logged.clear();
            app.login_method_id = None;
            ensure_login_method(app);
            app.auth_clipboard_copied = false;
            let effects = dispatch_exit_session(app);
            app.welcome_prompt_focused = false;
            effects
        }
        TaskResult::DeepSearchResults { results, seq } => {
            handle_deep_search_results(app, results, seq)
        }
        TaskResult::RewindPointsLoaded { agent_id, points } => {
            handle_rewind_points_loaded(app, agent_id, points)
        }
        TaskResult::RewindPointsFailed { agent_id, error } => {
            let Some(agent) = app.agents.get_mut(&agent_id) else {
                return vec![];
            };
            agent.rewind_state = None;
            app.show_toast(&format!("Undo failed: {error}"));
            vec![]
        }
        TaskResult::RewindPreviewComplete {
            agent_id,
            response,
            target_prompt_index,
            mode,
        } => handle_rewind_preview_complete(app, agent_id, response, target_prompt_index, mode),
        TaskResult::RewindPreviewFailed { agent_id, error } => {
            handle_rewind_preview_failed(app, agent_id, error)
        }
        TaskResult::RewindExecuteComplete { agent_id, response } => {
            dispatch_rewind_success(app, agent_id, response)
        }
        TaskResult::RewindExecuteFailed { agent_id, error } => {
            handle_rewind_execute_failed(app, agent_id, error)
        }
        TaskResult::SuggestionDebounceExpired {
            agent_id,
            generation,
        } => handle_suggestion_debounce_expired(app, agent_id, generation),
        TaskResult::PluginCtaDebounceExpired {
            agent_id,
            generation,
        } => handle_plugin_cta_debounce_expired(app, agent_id, generation),
        TaskResult::ShellSuggestionsLoaded {
            agent_id,
            response,
            request_text,
            request_cursor,
        } => {
            let Some(agent) = app.agents.get_mut(&agent_id) else {
                return vec![];
            };
            if agent.prompt_input_mode != crate::app::agent_view::PromptInputMode::Bash {
                return vec![];
            }
            let generation = response.generation;
            agent
                .prompt
                .suggestions
                .on_suggestions_loaded(response, &request_text, request_cursor);
            let text = agent.prompt.text().to_owned();
            agent.prompt.suggestions.set_last_request_text(&text);
            let mark = agent.pending_effects.len();
            if agent.prompt.suggestions.take_pending_tab(generation) {
                agent.shell_completion_tab();
            }
            agent.pending_effects.split_off(mark)
        }
        TaskResult::PromptSuggestionLoaded {
            agent_id,
            suggestion,
            generation,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent
                    .prompt
                    .prompt_suggestion
                    .on_loaded(suggestion, generation);
                agent.refresh_prompt_suggestion_gate();
                agent.log_prompt_suggestion_shown_if_visible();
            }
            vec![]
        }
        TaskResult::SettingPersisted { key, value } => {
            tracing::trace!(target : "settings", ? key, ? value, "setting persisted");
            vec![]
        }
        TaskResult::SettingPersistFailed {
            key,
            rollback_value,
            error,
        } => {
            let rollback_effects = apply_setting_rollback(app, key, &rollback_value);
            tracing::warn!(
                target : "settings", ? key, ? rollback_value, % error,
                "setting persist failed; rolled back"
            );
            let scrubbed = scrub_error_for_toast(&error);
            app.show_toast(&format!("\u{2717} Could not save {key}: {scrubbed}"));
            rollback_effects
        }
        TaskResult::SettingPersistFailedBestEffort { key, error } => {
            tracing::warn!(
                target : "settings", ? key, % error,
                "setting persist failed (best-effort); in-memory state stays at optimistic value",
            );
            let scrubbed = scrub_error_for_toast(&error);
            app.show_toast(&format!("\u{2717} Could not save {key}: {scrubbed}"));
            vec![]
        }
    }
}
