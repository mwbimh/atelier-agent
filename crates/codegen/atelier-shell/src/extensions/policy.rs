//! Typed policy evaluation ACP methods.
//!
//! Policy evaluation is pure and local. The runtime may use the same
//! `atelier_hooks::PolicyEngine` at execution sites; these methods expose the
//! exact evaluator to UI and diagnostics clients without executing hooks.

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use agent_client_protocol as acp;
use atelier_hooks::{
    HardGate, HookEvent, PolicyContext, PolicyDecision, PolicyEngine, PolicyRule, PolicyScope,
    RedactionRule,
};
use serde::Deserialize;
use std::str::FromStr;

/// Apply configured DLP redactions to the exact conversation request that is
/// subsequently submitted to the sampler. This deliberately runs before both
/// Inspector serialization and `SamplerHandle::submit`, so diagnostics and the
/// provider see the same sanitized payload.
pub(crate) fn redact_outbound_request(
    engine: &PolicyEngine,
    request: &mut atelier_sampling_types::ConversationRequest,
) -> Result<bool, String> {
    if engine.redaction_rules().is_empty() {
        return Ok(false);
    }

    let mut changed = false;
    for item in &mut request.items {
        let mut value = serde_json::to_value(&*item)
            .map_err(|error| format!("failed to inspect outbound conversation item: {error}"))?;
        let mut item_changed = false;
        redact_json_value(engine, &mut value, None, &mut item_changed);
        if item_changed {
            *item = serde_json::from_value(value).map_err(|error| {
                format!("DLP redaction produced an invalid conversation item: {error}")
            })?;
            changed = true;
        }
    }
    for tool in &mut request.tools {
        if let Some(description) = tool.description.as_mut() {
            changed |= redact_string(engine, description);
        }
        redact_json_value(engine, &mut tool.parameters, None, &mut changed);
    }
    for hosted_tool in &mut request.hosted_tools {
        if let atelier_sampling_types::HostedTool::WebSearch {
            allowed_domains: Some(domains),
        } = hosted_tool
        {
            for domain in domains {
                changed |= redact_string(engine, domain);
            }
        }
    }
    if let Some(schema) = request.json_schema.as_mut() {
        redact_json_value(engine, schema, None, &mut changed);
    }
    Ok(changed)
}

fn redact_string(engine: &PolicyEngine, value: &mut String) -> bool {
    let redacted = engine.redact_text(value);
    if redacted == *value {
        return false;
    }
    *value = redacted;
    true
}

fn redact_json_value(
    engine: &PolicyEngine,
    value: &mut serde_json::Value,
    field: Option<&str>,
    changed: &mut bool,
) {
    match value {
        serde_json::Value::String(text) => {
            if field == Some("arguments")
                && let Ok(mut arguments) = serde_json::from_str::<serde_json::Value>(text)
            {
                let mut arguments_changed = false;
                redact_json_value(engine, &mut arguments, None, &mut arguments_changed);
                if arguments_changed && let Ok(serialized) = serde_json::to_string(&arguments) {
                    *text = serialized;
                    *changed = true;
                }
                return;
            }
            // Keep protocol discriminators and linkage identifiers stable.
            // Redacting these can make an otherwise safe request impossible to
            // deserialize or can detach a tool result from its tool call.
            if matches!(
                field,
                Some(
                    "type"
                        | "role"
                        | "status"
                        | "id"
                        | "call_id"
                        | "tool_call_id"
                        | "tool_type"
                        | "name"
                        | "model_id"
                        | "model_fingerprint"
                        | "encrypted_content"
                )
            ) {
                return;
            }
            if field == Some("url") && text.starts_with("data:") {
                return;
            }
            *changed |= redact_string(engine, text);
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_json_value(engine, value, field, changed);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                redact_json_value(engine, value, Some(key.as_str()), changed);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// Apply the mutable decisions supported at context/provider request
/// boundaries. `Modify` replaces the most recent user text while preserving
/// attached images; `AddContext` appends policy-controlled system context.
pub(crate) fn apply_request_decision(
    request: &mut atelier_sampling_types::ConversationRequest,
    decision: PolicyDecision,
) -> Result<bool, String> {
    use atelier_sampling_types::{ContentPart, ConversationItem};

    match decision {
        PolicyDecision::Allow => Ok(false),
        PolicyDecision::Modify { replacement } => {
            let replacement = std::sync::Arc::<str>::from(replacement);
            let user_index = request
                .items
                .iter()
                .rposition(|item| {
                    matches!(item, ConversationItem::User(user) if user.synthetic_reason.is_none())
                })
                .or_else(|| {
                    request
                        .items
                        .iter()
                        .rposition(|item| matches!(item, ConversationItem::User(_)))
                });
            if let Some(user) = user_index.and_then(|index| match &mut request.items[index] {
                ConversationItem::User(user) => Some(user),
                _ => None,
            }) {
                let mut replaced = false;
                user.content.retain_mut(|part| match part {
                    ContentPart::Text { text } if !replaced => {
                        *text = replacement.clone();
                        replaced = true;
                        true
                    }
                    ContentPart::Text { .. } => false,
                    ContentPart::Image { .. } => true,
                });
                if !replaced {
                    user.content
                        .insert(0, ContentPart::Text { text: replacement });
                }
            } else {
                request
                    .items
                    .push(ConversationItem::user(replacement.to_string()));
            }
            Ok(true)
        }
        PolicyDecision::AddContext { context } => {
            request.items.push(ConversationItem::system(context));
            Ok(true)
        }
        PolicyDecision::Deny { reason } => Err(reason),
        PolicyDecision::Ask { prompt } => Err(format!("approval required: {prompt}")),
    }
}

pub const POLICY_INFO: &str = "_atelier/policy/info";
pub const POLICY_EVALUATE: &str = "_atelier/policy/evaluate";
pub const POLICY_REDACT: &str = "_atelier/policy/redact";
pub const POLICY_CONFIGURE: &str = "_atelier/policy/configure";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyOperation {
    ContextBuild,
    ProviderRequest,
    ToolCall,
    FileRead,
    FileWrite,
    ProcessSpawn,
}

impl PolicyOperation {
    pub(crate) const fn event(self) -> HookEvent {
        match self {
            Self::ContextBuild => HookEvent::AfterContextBuild,
            Self::ProviderRequest => HookEvent::BeforeProviderRequest,
            Self::ToolCall => HookEvent::BeforeToolCall,
            Self::FileRead => HookEvent::BeforeFileRead,
            Self::FileWrite => HookEvent::BeforeFileWrite,
            Self::ProcessSpawn => HookEvent::BeforeProcessSpawn,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        self.event().as_str()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PolicyGates {
    pub plan_mode: bool,
    pub sandbox_blocked: bool,
}

/// Evaluate a runtime operation with the non-bypassable gates applied after
/// the configured rules. Plan Mode only blocks writes; the sandbox gate is
/// supplied by the caller when the sandbox has rejected the operation.
pub(crate) fn evaluate_runtime_policy(
    engine: &PolicyEngine,
    operation: PolicyOperation,
    role: Option<&str>,
    provider: Option<&str>,
    tool: Option<&str>,
    path: Option<&str>,
    gates: PolicyGates,
) -> PolicyDecision {
    let context = PolicyContext::new(operation.event());
    let context = match role {
        Some(role) => context.with_role(role),
        None => context,
    };
    let context = match provider {
        Some(provider) => context.with_provider(provider),
        None => context,
    };
    let context = match tool {
        Some(tool) => context.with_tool(tool),
        None => context,
    };
    let context = match path {
        Some(path) => context.with_path(path),
        None => context,
    };

    let mut hard_gates = Vec::new();
    let plan_mode_write = matches!(operation, PolicyOperation::FileWrite)
        || (matches!(operation, PolicyOperation::ToolCall)
            && tool.is_some_and(is_plan_mode_write_tool));
    if gates.plan_mode && plan_mode_write {
        hard_gates.push((
            HardGate::PlanMode,
            PolicyDecision::deny("Plan Mode forbids file writes"),
        ));
    }
    if gates.sandbox_blocked {
        hard_gates.push((
            HardGate::Sandbox,
            PolicyDecision::deny("Sandbox blocked the operation"),
        ));
    }
    engine.evaluate_with_gates(context, hard_gates)
}

fn is_plan_mode_write_tool(tool: &str) -> bool {
    let tool = tool.to_ascii_lowercase();
    ["write", "edit", "patch", "delete", "move", "rename"]
        .iter()
        .any(|verb| tool.contains(verb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelier_hooks::PolicyDecision;
    use atelier_sampling_types::{ConversationItem, ConversationRequest};

    #[test]
    fn plan_mode_hard_gate_wins_over_a_matching_non_deny_rule() {
        let engine =
            PolicyEngine::new([PolicyRule::ask(PolicyScope::File, "review before editing")]);

        let decision = evaluate_runtime_policy(
            &engine,
            PolicyOperation::FileWrite,
            Some("main"),
            None,
            None,
            Some("src/main.rs"),
            PolicyGates {
                plan_mode: true,
                sandbox_blocked: false,
            },
        );

        assert_eq!(
            decision,
            PolicyDecision::Deny {
                reason: "Plan Mode forbids file writes".to_owned(),
            }
        );
    }

    #[test]
    fn sandbox_hard_gate_is_recorded_as_a_deny() {
        let engine = PolicyEngine::default();
        let decision = evaluate_runtime_policy(
            &engine,
            PolicyOperation::ProcessSpawn,
            Some("test"),
            None,
            None,
            None,
            PolicyGates {
                plan_mode: false,
                sandbox_blocked: true,
            },
        );

        assert!(matches!(decision, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn plan_mode_blocks_write_tools_before_the_configured_policy() {
        let decision = evaluate_runtime_policy(
            &PolicyEngine::default(),
            PolicyOperation::ToolCall,
            Some("main"),
            None,
            Some("apply_patch"),
            None,
            PolicyGates {
                plan_mode: true,
                sandbox_blocked: false,
            },
        );

        assert_eq!(
            decision,
            PolicyDecision::Deny {
                reason: "Plan Mode forbids file writes".to_owned(),
            }
        );
    }

    #[test]
    fn outbound_redaction_mutates_the_request_that_will_be_sent() {
        let engine = PolicyEngine::default()
            .with_redaction_rule(RedactionRule::literal("secret-value", "[REDACTED]"));
        let mut request = ConversationRequest::from_items(vec![
            ConversationItem::system("system secret-value"),
            ConversationItem::user("user secret-value"),
            ConversationItem::Assistant(atelier_sampling_types::AssistantItem {
                content: std::sync::Arc::from("assistant secret-value"),
                tool_calls: vec![atelier_sampling_types::ToolCall {
                    id: std::sync::Arc::from("call-1"),
                    name: "read_file".to_owned(),
                    arguments: std::sync::Arc::from(r#"{"path":"secret-value"}"#),
                }],
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            }),
            ConversationItem::tool_result("call-1", "tool secret-value"),
        ]);

        assert!(redact_outbound_request(&engine, &mut request).unwrap());

        let serialized = request
            .items
            .iter()
            .map(|item| serde_json::to_string(item).unwrap())
            .collect::<String>();
        assert!(!serialized.contains("secret-value"), "{serialized}");
        assert!(serialized.contains("[REDACTED]"), "{serialized}");
    }

    #[test]
    fn modify_and_add_context_change_the_real_provider_request() {
        let mut modified = ConversationRequest::from_items(vec![
            ConversationItem::system("system"),
            ConversationItem::user("original user prompt"),
        ]);
        apply_request_decision(
            &mut modified,
            PolicyDecision::Modify {
                replacement: "sanitized prompt".to_owned(),
            },
        )
        .unwrap();
        let modified_json = serde_json::to_string(&modified.items).unwrap();
        assert!(!modified_json.contains("original user prompt"));
        assert!(modified_json.contains("sanitized prompt"));

        apply_request_decision(
            &mut modified,
            PolicyDecision::AddContext {
                context: "policy supplied context".to_owned(),
            },
        )
        .unwrap();
        let modified_json = serde_json::to_string(&modified.items).unwrap();
        assert!(modified_json.contains("policy supplied context"));
    }

    #[tokio::test]
    async fn dlp_redaction_reaches_the_actual_http_request_body() {
        let server = atelier_test_support::MockInferenceServer::start()
            .await
            .expect("mock inference server");
        let client = atelier_sampler::SamplingClient::new(atelier_sampler::SamplerConfig {
            base_url: server.url(),
            model: "test-model".to_owned(),
            api_backend: atelier_sampling_types::ApiBackend::ChatCompletions,
            ..Default::default()
        })
        .expect("sampling client");
        let engine = PolicyEngine::default()
            .with_redaction_rule(RedactionRule::literal("live-secret", "[REDACTED]"));
        let mut request = ConversationRequest::from_items(vec![ConversationItem::user(
            "send live-secret to the provider",
        )]);

        redact_outbound_request(&engine, &mut request).unwrap();
        client
            .conversation_collect(request)
            .await
            .expect("mock response");

        let body = serde_json::to_string(&server.request_bodies()).unwrap();
        assert!(!body.contains("live-secret"), "{body}");
        assert!(body.contains("[REDACTED]"), "{body}");
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvaluateParams {
    event: String,
    #[serde(default)]
    scope: Option<PolicyScope>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    rules: Vec<PolicyRule>,
}

#[derive(Debug, Deserialize)]
struct RedactParams {
    text: String,
    #[serde(default)]
    rules: Vec<RedactionSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RedactionSpec {
    pattern: String,
    replacement: String,
    #[serde(default)]
    regex: bool,
}

#[derive(Debug, Deserialize)]
struct ConfigureParams {
    #[serde(default)]
    rules: Vec<PolicyRule>,
    #[serde(default)]
    redactions: Vec<RedactionSpec>,
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        POLICY_INFO | "atelier/policy/info" => to_raw_response(&serde_json::json!({
            "scopes": [
                "session", "turn", "context", "provider", "tool", "file",
                "process", "permission", "compaction", "subagent", "role",
            ],
            "decisions": ["allow", "deny", "ask", "modify", "add_context"],
            "hardGates": ["plan_mode", "unsafe", "sandbox"],
        })),
        POLICY_EVALUATE | "atelier/policy/evaluate" => evaluate(args),
        POLICY_REDACT | "atelier/policy/redact" => redact(args),
        POLICY_CONFIGURE | "atelier/policy/configure" => configure(agent, args),
        _ => Err(acp::Error::method_not_found()),
    }
}

fn configure(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ConfigureParams = parse_params(args)?;
    let mut engine = PolicyEngine::from_rules(params.rules);
    for rule in params.redactions {
        let rule = if rule.regex {
            RedactionRule::regex(rule.pattern, rule.replacement)
                .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?
        } else {
            RedactionRule::literal(rule.pattern, rule.replacement)
        };
        engine.add_redaction_rule(rule);
    }
    agent.replace_runtime_policy(engine);
    to_raw_response(&serde_json::json!({"configured": true}))
}

impl MvpAgent {
    /// Apply the runtime policy to ACP extension operations that directly
    /// perform file, process, or tool work. SessionActor tool execution uses
    /// the same operation helpers when its execution boundary is available.
    pub(crate) fn enforce_extension_policy(
        &self,
        args: &acp::ExtRequest,
    ) -> Result<(), acp::Error> {
        let method = args.method.as_ref();
        let params = serde_json::from_str::<serde_json::Value>(args.params.get()).ok();
        let session_id = params
            .as_ref()
            .and_then(|params| params.get("sessionId"))
            .and_then(|value| value.as_str());
        let request_id = params
            .as_ref()
            .and_then(|params| params.get("requestId").or_else(|| params.get("promptId")))
            .and_then(|value| value.as_str());
        let path = params
            .as_ref()
            .and_then(|params| params.get("path"))
            .and_then(|value| value.as_str());
        let plan_mode = session_id
            .and_then(|session_id| self.session_handle_now(session_id))
            .is_some_and(|handle| handle.plan_mode.lock().is_active());

        if method.starts_with("atelier/fs/") {
            return match method {
                "atelier/fs/write_file" | "atelier/fs/delete_file" => self
                    .enforce_file_write(
                        session_id,
                        request_id,
                        Some("main"),
                        path.unwrap_or("<missing>"),
                        plan_mode,
                        false,
                    )
                    .map(|_| ()),
                "atelier/fs/read_file" | "atelier/fs/list" | "atelier/fs/exists" => self
                    .enforce_file_read(
                        session_id,
                        request_id,
                        Some("main"),
                        path.unwrap_or("<missing>"),
                        false,
                    )
                    .map(|_| ()),
                _ => Ok(()),
            };
        }

        if method.starts_with("atelier/terminal/") {
            return self
                .enforce_process_spawn(session_id, request_id, Some("main"), false)
                .map(|_| ());
        }

        if method.starts_with("atelier/task/")
            || method.starts_with("atelier/subagent/")
            || method.starts_with(crate::extensions::mcp::mcp_methods::PREFIX)
        {
            return self
                .enforce_tool_call(
                    session_id,
                    request_id,
                    Some("main"),
                    method,
                    plan_mode,
                    false,
                )
                .map(|_| ());
        }

        Ok(())
    }

    pub(crate) fn replace_runtime_policy(&self, engine: PolicyEngine) {
        *self.policy_engine.write() = engine;
    }

    pub(crate) fn runtime_policy_decision(
        &self,
        operation: PolicyOperation,
        role: Option<&str>,
        provider: Option<&str>,
        tool: Option<&str>,
        path: Option<&str>,
        gates: PolicyGates,
    ) -> PolicyDecision {
        evaluate_runtime_policy(
            &self.policy_engine.read(),
            operation,
            role,
            provider,
            tool,
            path,
            gates,
        )
    }

    pub(crate) fn enforce_runtime_policy(
        &self,
        session_id: Option<&str>,
        request_id: Option<&str>,
        operation: PolicyOperation,
        role: Option<&str>,
        provider: Option<&str>,
        tool: Option<&str>,
        path: Option<&str>,
        gates: PolicyGates,
    ) -> Result<PolicyDecision, acp::Error> {
        let decision = self.runtime_policy_decision(operation, role, provider, tool, path, gates);
        let safe_path = path.map(xai_acp_lib::redact_text);
        let details = serde_json::json!({
            "operation": operation.as_str(),
            "role": role,
            "provider": provider,
            "tool": tool,
            "path": safe_path,
            "decision": decision.clone(),
        });
        self.runtime_control.lock().record_event(
            session_id.map(str::to_owned),
            request_id.map(str::to_owned),
            "policy.evaluated",
            details,
        );

        match &decision {
            PolicyDecision::Deny { reason } => Err(policy_error(operation, reason)),
            PolicyDecision::Ask { prompt } => Err(policy_error(
                operation,
                &format!("approval required: {prompt}"),
            )),
            PolicyDecision::Allow
            | PolicyDecision::Modify { .. }
            | PolicyDecision::AddContext { .. } => Ok(decision),
        }
    }

    pub(crate) fn enforce_provider_request(
        &self,
        session_id: &str,
        request_id: &str,
        role: Option<&str>,
        provider: Option<&str>,
    ) -> Result<PolicyDecision, acp::Error> {
        self.enforce_runtime_policy(
            Some(session_id),
            Some(request_id),
            PolicyOperation::ProviderRequest,
            role,
            provider,
            None,
            None,
            PolicyGates::default(),
        )
    }

    pub(crate) fn enforce_tool_call(
        &self,
        session_id: Option<&str>,
        request_id: Option<&str>,
        role: Option<&str>,
        tool: &str,
        plan_mode: bool,
        sandbox_blocked: bool,
    ) -> Result<PolicyDecision, acp::Error> {
        self.enforce_runtime_policy(
            session_id,
            request_id,
            PolicyOperation::ToolCall,
            role,
            None,
            Some(tool),
            None,
            PolicyGates {
                plan_mode,
                sandbox_blocked,
                ..PolicyGates::default()
            },
        )
    }

    pub(crate) fn enforce_file_read(
        &self,
        session_id: Option<&str>,
        request_id: Option<&str>,
        role: Option<&str>,
        path: &str,
        sandbox_blocked: bool,
    ) -> Result<PolicyDecision, acp::Error> {
        self.enforce_runtime_policy(
            session_id,
            request_id,
            PolicyOperation::FileRead,
            role,
            None,
            None,
            Some(path),
            PolicyGates {
                sandbox_blocked,
                ..PolicyGates::default()
            },
        )
    }

    pub(crate) fn enforce_file_write(
        &self,
        session_id: Option<&str>,
        request_id: Option<&str>,
        role: Option<&str>,
        path: &str,
        plan_mode: bool,
        sandbox_blocked: bool,
    ) -> Result<PolicyDecision, acp::Error> {
        self.enforce_runtime_policy(
            session_id,
            request_id,
            PolicyOperation::FileWrite,
            role,
            None,
            None,
            Some(path),
            PolicyGates {
                plan_mode,
                sandbox_blocked,
            },
        )
    }

    pub(crate) fn enforce_process_spawn(
        &self,
        session_id: Option<&str>,
        request_id: Option<&str>,
        role: Option<&str>,
        sandbox_blocked: bool,
    ) -> Result<PolicyDecision, acp::Error> {
        self.enforce_runtime_policy(
            session_id,
            request_id,
            PolicyOperation::ProcessSpawn,
            role,
            None,
            None,
            None,
            PolicyGates {
                sandbox_blocked,
                ..PolicyGates::default()
            },
        )
    }
}

fn policy_error(operation: PolicyOperation, reason: &str) -> acp::Error {
    acp::Error::invalid_params().data(format!(
        "policy denied {}: {}",
        operation.as_str(),
        xai_acp_lib::redact_text(reason)
    ))
}

fn evaluate(args: &acp::ExtRequest) -> ExtResult {
    let params: EvaluateParams = parse_params(args)?;
    let event = HookEvent::from_str(&params.event)
        .map_err(|error| acp::Error::invalid_params().data(error))?;
    let mut context = PolicyContext::from(event);
    if let Some(scope) = params.scope {
        context = context.with_scope(scope);
    }
    if let Some(role) = params.role {
        context = context.with_role(role);
    }
    if let Some(provider) = params.provider {
        context = context.with_provider(provider);
    }
    if let Some(tool) = params.tool {
        context = context.with_tool(tool);
    }
    if let Some(path) = params.path {
        context = context.with_path(path);
    }
    let engine = PolicyEngine::from_rules(params.rules);
    to_raw_response(&serde_json::json!({
        "context": context,
        "decision": engine.evaluate(&context),
    }))
}

fn redact(args: &acp::ExtRequest) -> ExtResult {
    let params: RedactParams = parse_params(args)?;
    let mut engine = PolicyEngine::new(std::iter::empty::<PolicyRule>());
    for rule in params.rules {
        let rule = if rule.regex {
            RedactionRule::regex(rule.pattern, rule.replacement)
                .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?
        } else {
            RedactionRule::literal(rule.pattern, rule.replacement)
        };
        engine.add_redaction_rule(rule);
    }
    to_raw_response(&serde_json::json!({
        "text": engine.redact(&params.text),
    }))
}
