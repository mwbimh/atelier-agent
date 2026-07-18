//! Pure hook policy primitives.
//!
//! This module deliberately has no filesystem, process, network, or logging
//! side effects. The existing hook dispatcher remains responsible for running
//! hooks and retaining its fail-open behavior.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Typed hook lifecycle and operation events used by the policy layer.
///
/// The existing crate::event::HookEventName remains the wire-facing event
/// name for the legacy dispatcher. This enum is intentionally independent so
/// policy evaluation can evolve without changing dispatcher behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    #[serde(alias = "SessionStart", alias = "sessionStart")]
    SessionStart,
    #[serde(alias = "SessionResume", alias = "sessionResume")]
    SessionResume,
    #[serde(alias = "UserPromptSubmit", alias = "userPromptSubmit")]
    UserPromptSubmit,
    #[serde(alias = "BeforeContextBuild", alias = "beforeContextBuild")]
    BeforeContextBuild,
    #[serde(alias = "AfterContextBuild", alias = "afterContextBuild")]
    AfterContextBuild,
    #[serde(alias = "BeforeProviderRequest", alias = "beforeProviderRequest")]
    BeforeProviderRequest,
    #[serde(alias = "AfterProviderResponse", alias = "afterProviderResponse")]
    AfterProviderResponse,
    #[serde(alias = "BeforeToolCall", alias = "beforeToolCall")]
    BeforeToolCall,
    #[serde(alias = "AfterToolCall", alias = "afterToolCall")]
    AfterToolCall,
    #[serde(alias = "BeforeFileRead", alias = "beforeFileRead")]
    BeforeFileRead,
    #[serde(alias = "BeforeFileWrite", alias = "beforeFileWrite")]
    BeforeFileWrite,
    #[serde(alias = "BeforeProcessSpawn", alias = "beforeProcessSpawn")]
    BeforeProcessSpawn,
    #[serde(alias = "PermissionRequest", alias = "permissionRequest")]
    PermissionRequest,
    #[serde(alias = "BeforeCompact", alias = "beforeCompact")]
    BeforeCompact,
    #[serde(alias = "AfterCompact", alias = "afterCompact")]
    AfterCompact,
    #[serde(alias = "SubagentStart", alias = "subagentStart")]
    SubagentStart,
    #[serde(alias = "SubagentStop", alias = "subagentStop")]
    SubagentStop,
    #[serde(alias = "TurnStop", alias = "turnStop")]
    TurnStop,
    #[serde(alias = "SessionStop", alias = "sessionStop")]
    SessionStop,
    #[serde(alias = "BeforeRoleResolve", alias = "beforeRoleResolve")]
    BeforeRoleResolve,
    #[serde(alias = "AfterRoleResolve", alias = "afterRoleResolve")]
    AfterRoleResolve,
}

impl HookEvent {
    /// Stable snake_case name for logs and wire formats.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::SessionResume => "session_resume",
            Self::UserPromptSubmit => "user_prompt_submit",
            Self::BeforeContextBuild => "before_context_build",
            Self::AfterContextBuild => "after_context_build",
            Self::BeforeProviderRequest => "before_provider_request",
            Self::AfterProviderResponse => "after_provider_response",
            Self::BeforeToolCall => "before_tool_call",
            Self::AfterToolCall => "after_tool_call",
            Self::BeforeFileRead => "before_file_read",
            Self::BeforeFileWrite => "before_file_write",
            Self::BeforeProcessSpawn => "before_process_spawn",
            Self::PermissionRequest => "permission_request",
            Self::BeforeCompact => "before_compact",
            Self::AfterCompact => "after_compact",
            Self::SubagentStart => "subagent_start",
            Self::SubagentStop => "subagent_stop",
            Self::TurnStop => "turn_stop",
            Self::SessionStop => "session_stop",
            Self::BeforeRoleResolve => "before_role_resolve",
            Self::AfterRoleResolve => "after_role_resolve",
        }
    }

    /// Default policy scope associated with this event.
    pub const fn scope(self) -> PolicyScope {
        match self {
            Self::SessionStart | Self::SessionResume | Self::SessionStop => PolicyScope::Session,
            Self::UserPromptSubmit | Self::TurnStop => PolicyScope::Turn,
            Self::BeforeContextBuild | Self::AfterContextBuild => PolicyScope::Context,
            Self::BeforeCompact | Self::AfterCompact => PolicyScope::Compaction,
            Self::BeforeProviderRequest | Self::AfterProviderResponse => PolicyScope::Provider,
            Self::BeforeToolCall | Self::AfterToolCall => PolicyScope::Tool,
            Self::BeforeFileRead | Self::BeforeFileWrite => PolicyScope::File,
            Self::BeforeProcessSpawn => PolicyScope::Process,
            Self::PermissionRequest => PolicyScope::Permission,
            Self::SubagentStart | Self::SubagentStop => PolicyScope::Subagent,
            Self::BeforeRoleResolve | Self::AfterRoleResolve => PolicyScope::Role,
        }
    }
}

impl fmt::Display for HookEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HookEvent {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "SessionStart" | "session_start" | "sessionStart" => Ok(Self::SessionStart),
            "SessionResume" | "session_resume" | "sessionResume" => Ok(Self::SessionResume),
            "UserPromptSubmit" | "user_prompt_submit" | "userPromptSubmit" => {
                Ok(Self::UserPromptSubmit)
            }
            "BeforeContextBuild" | "before_context_build" | "beforeContextBuild" => {
                Ok(Self::BeforeContextBuild)
            }
            "AfterContextBuild" | "after_context_build" | "afterContextBuild" => {
                Ok(Self::AfterContextBuild)
            }
            "BeforeProviderRequest" | "before_provider_request" | "beforeProviderRequest" => {
                Ok(Self::BeforeProviderRequest)
            }
            "AfterProviderResponse" | "after_provider_response" | "afterProviderResponse" => {
                Ok(Self::AfterProviderResponse)
            }
            "BeforeToolCall" | "before_tool_call" | "beforeToolCall" => Ok(Self::BeforeToolCall),
            "AfterToolCall" | "after_tool_call" | "afterToolCall" => Ok(Self::AfterToolCall),
            "BeforeFileRead" | "before_file_read" | "beforeFileRead" => Ok(Self::BeforeFileRead),
            "BeforeFileWrite" | "before_file_write" | "beforeFileWrite" => {
                Ok(Self::BeforeFileWrite)
            }
            "BeforeProcessSpawn" | "before_process_spawn" | "beforeProcessSpawn" => {
                Ok(Self::BeforeProcessSpawn)
            }
            "PermissionRequest" | "permission_request" | "permissionRequest" => {
                Ok(Self::PermissionRequest)
            }
            "BeforeCompact" | "before_compact" | "beforeCompact" => Ok(Self::BeforeCompact),
            "AfterCompact" | "after_compact" | "afterCompact" => Ok(Self::AfterCompact),
            "SubagentStart" | "subagent_start" | "subagentStart" => Ok(Self::SubagentStart),
            "SubagentStop" | "subagent_stop" | "subagentStop" => Ok(Self::SubagentStop),
            "TurnStop" | "turn_stop" | "turnStop" => Ok(Self::TurnStop),
            "SessionStop" | "session_stop" | "sessionStop" => Ok(Self::SessionStop),
            "BeforeRoleResolve" | "before_role_resolve" | "beforeRoleResolve" => {
                Ok(Self::BeforeRoleResolve)
            }
            "AfterRoleResolve" | "after_role_resolve" | "afterRoleResolve" => {
                Ok(Self::AfterRoleResolve)
            }
            other => Err(format!("unknown hook event: {other}")),
        }
    }
}

/// The action a hook may request from the policy layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum HookDecision {
    /// Continue without changing the operation.
    Continue,
    /// Hard-block the operation.
    Deny { reason: String },
    /// Ask the user or host for an explicit decision.
    Ask { prompt: String },
    /// Replace the relevant text or payload with a sanitized value.
    Modify { replacement: String },
    /// Add policy-controlled context to the operation.
    AddContext { context: String },
}

impl Default for HookDecision {
    fn default() -> Self {
        Self::Continue
    }
}

impl HookDecision {
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }

    pub fn ask(prompt: impl Into<String>) -> Self {
        Self::Ask {
            prompt: prompt.into(),
        }
    }

    pub fn modify(replacement: impl Into<String>) -> Self {
        Self::Modify {
            replacement: replacement.into(),
        }
    }

    pub fn add_context(context: impl Into<String>) -> Self {
        Self::AddContext {
            context: context.into(),
        }
    }

    pub fn is_hard_deny(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }
}

/// Scope used by a policy rule or evaluation context.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyScope {
    /// Match every scope.
    Any,
    /// Alias for a policy-wide rule; it also matches every scope.
    Global,
    Session,
    Turn,
    Context,
    Provider,
    Tool,
    File,
    Process,
    Permission,
    Compaction,
    Subagent,
    Role,
    /// Extension point for a named scope owned by a caller.
    Custom(String),
}

impl Default for PolicyScope {
    fn default() -> Self {
        Self::Any
    }
}

impl PolicyScope {
    fn matches(&self, actual: &Self) -> bool {
        matches!(self, Self::Any | Self::Global)
            || matches!(actual, Self::Any | Self::Global)
            || self == actual
    }
}

/// Result of evaluating policy rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PolicyDecision {
    /// The policy did not block or otherwise alter the operation.
    Allow,
    /// Hard-block the operation.
    Deny { reason: String },
    /// Require an explicit user or host decision.
    Ask { prompt: String },
    /// Replace the relevant text or payload.
    Modify { replacement: String },
    /// Add policy-controlled context.
    AddContext { context: String },
}

impl Default for PolicyDecision {
    fn default() -> Self {
        Self::Allow
    }
}

impl PolicyDecision {
    pub fn allow() -> Self {
        Self::Allow
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }

    pub fn ask(prompt: impl Into<String>) -> Self {
        Self::Ask {
            prompt: prompt.into(),
        }
    }

    pub fn modify(replacement: impl Into<String>) -> Self {
        Self::Modify {
            replacement: replacement.into(),
        }
    }

    pub fn add_context(context: impl Into<String>) -> Self {
        Self::AddContext {
            context: context.into(),
        }
    }

    pub fn is_hard_deny(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }

    fn precedence_rank(&self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::AddContext { .. } => 1,
            Self::Modify { .. } => 2,
            Self::Ask { .. } => 3,
            Self::Deny { .. } => 4,
        }
    }

    /// Convert a policy result to the hook-facing action vocabulary.
    pub fn into_hook_decision(self) -> HookDecision {
        match self {
            Self::Allow => HookDecision::Continue,
            Self::Deny { reason } => HookDecision::Deny { reason },
            Self::Ask { prompt } => HookDecision::Ask { prompt },
            Self::Modify { replacement } => HookDecision::Modify { replacement },
            Self::AddContext { context } => HookDecision::AddContext { context },
        }
    }
}

/// The dimensions used while evaluating a PolicyRule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyContext {
    pub event: HookEvent,
    pub scope: PolicyScope,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

impl Default for PolicyContext {
    fn default() -> Self {
        Self::from(HookEvent::SessionStart)
    }
}

impl PolicyContext {
    /// Build a context from an event or a scope.
    pub fn new<T>(seed: T) -> Self
    where
        T: Into<Self>,
    {
        seed.into()
    }

    pub fn with_event(mut self, event: HookEvent) -> Self {
        self.event = event;
        self.scope = event.scope();
        self
    }

    pub fn with_scope(mut self, scope: PolicyScope) -> Self {
        self.scope = scope;
        self
    }

    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

impl From<HookEvent> for PolicyContext {
    fn from(event: HookEvent) -> Self {
        Self {
            event,
            scope: event.scope(),
            role: None,
            provider: None,
            tool: None,
            path: None,
        }
    }
}

impl From<&HookEvent> for PolicyContext {
    fn from(event: &HookEvent) -> Self {
        (*event).into()
    }
}

impl From<PolicyScope> for PolicyContext {
    fn from(scope: PolicyScope) -> Self {
        Self {
            event: HookEvent::SessionStart,
            scope,
            role: None,
            provider: None,
            tool: None,
            path: None,
        }
    }
}

impl From<&PolicyScope> for PolicyContext {
    fn from(scope: &PolicyScope) -> Self {
        scope.clone().into()
    }
}

impl From<&PolicyContext> for PolicyContext {
    fn from(context: &PolicyContext) -> Self {
        context.clone()
    }
}

/// Alias for callers that prefer request-oriented naming.
pub type PolicyRequest = PolicyContext;

/// One pure policy rule. All optional dimensions are conjunctive filters;
/// omitted dimensions match any value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub scope: PolicyScope,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    pub decision: PolicyDecision,
}

impl Default for PolicyRule {
    fn default() -> Self {
        Self {
            scope: PolicyScope::Any,
            role: None,
            provider: None,
            tool: None,
            path: None,
            decision: PolicyDecision::Allow,
        }
    }
}

impl PolicyRule {
    pub fn new(scope: PolicyScope, decision: PolicyDecision) -> Self {
        Self {
            scope,
            decision,
            ..Self::default()
        }
    }

    pub fn allow(scope: PolicyScope) -> Self {
        Self::new(scope, PolicyDecision::Allow)
    }

    pub fn deny(scope: PolicyScope, reason: impl Into<String>) -> Self {
        Self::new(
            scope,
            PolicyDecision::Deny {
                reason: reason.into(),
            },
        )
    }

    pub fn ask(scope: PolicyScope, prompt: impl Into<String>) -> Self {
        Self::new(
            scope,
            PolicyDecision::Ask {
                prompt: prompt.into(),
            },
        )
    }

    pub fn modify(scope: PolicyScope, replacement: impl Into<String>) -> Self {
        Self::new(
            scope,
            PolicyDecision::Modify {
                replacement: replacement.into(),
            },
        )
    }

    pub fn add_context(scope: PolicyScope, context: impl Into<String>) -> Self {
        Self::new(
            scope,
            PolicyDecision::AddContext {
                context: context.into(),
            },
        )
    }

    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn for_role(self, role: impl Into<String>) -> Self {
        self.with_role(role)
    }

    pub fn for_provider(self, provider: impl Into<String>) -> Self {
        self.with_provider(provider)
    }

    pub fn for_tool(self, tool: impl Into<String>) -> Self {
        self.with_tool(tool)
    }

    pub fn for_path(self, path: impl Into<String>) -> Self {
        self.with_path(path)
    }

    /// Test this rule without performing any I/O.
    pub fn matches(&self, context: &PolicyContext) -> bool {
        self.scope.matches(&context.scope)
            && matches_dimension(&self.role, context.role.as_deref())
            && matches_dimension(&self.provider, context.provider.as_deref())
            && matches_dimension(&self.tool, context.tool.as_deref())
            && matches_dimension(&self.path, context.path.as_deref())
    }
}

fn matches_dimension(pattern: &Option<String>, actual: Option<&str>) -> bool {
    let Some(pattern) = pattern else {
        return true;
    };

    pattern.split('|').any(|candidate| {
        if candidate.is_empty() || candidate == "*" {
            return true;
        }
        actual.is_some_and(|value| simple_pattern_matches(candidate, value))
    })
}

/// Match exact strings plus *, ?, and |-list patterns. This is intentionally
/// small and deterministic; policy matching does not compile or execute
/// user-provided code.
fn simple_pattern_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut pattern_index = 0;
    let mut value_index = 0;
    let mut star_index = None;
    let mut star_value_index = 0;

    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_value_index = value_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_value_index += 1;
            value_index = star_value_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

/// A compiled text redaction rule. The source text passed to Self::apply is
/// never retained by this type or by PolicyEngine.
#[derive(Debug, Clone)]
pub struct RedactionRule {
    pub pattern: String,
    pub replacement: String,
    matcher: Regex,
}

impl RedactionRule {
    /// Create a literal redaction rule.
    pub fn literal(pattern: impl Into<String>, replacement: impl Into<String>) -> Self {
        let pattern = pattern.into();
        let matcher = Regex::new(&regex::escape(&pattern))
            .expect("regex::escape always produces a valid regular expression");
        Self {
            pattern,
            replacement: replacement.into(),
            matcher,
        }
    }

    /// Create a regular-expression redaction rule.
    pub fn regex(
        pattern: impl Into<String>,
        replacement: impl Into<String>,
    ) -> Result<Self, regex::Error> {
        let pattern = pattern.into();
        let matcher = Regex::new(&pattern)?;
        Ok(Self {
            pattern,
            replacement: replacement.into(),
            matcher,
        })
    }

    pub fn new(pattern: impl Into<String>, replacement: impl Into<String>) -> Self {
        Self::literal(pattern, replacement)
    }

    pub fn apply(&self, text: &str) -> String {
        self.matcher
            .replace_all(text, self.replacement.as_str())
            .into_owned()
    }
}

/// Pure policy evaluator. It only inspects supplied values and returns a
/// decision; it never reads files, starts processes, accesses the network, or
/// logs input text.
#[derive(Debug, Clone, Default)]
pub struct PolicyEngine {
    rules: Vec<PolicyRule>,
    redactions: Vec<RedactionRule>,
    precedence: PolicyPrecedence,
}

impl PolicyEngine {
    pub fn new<I>(rules: I) -> Self
    where
        I: IntoIterator<Item = PolicyRule>,
    {
        Self {
            rules: rules.into_iter().collect(),
            ..Self::default()
        }
    }

    pub fn from_rules<I>(rules: I) -> Self
    where
        I: IntoIterator<Item = PolicyRule>,
    {
        Self::new(rules)
    }

    pub fn rules(&self) -> &[PolicyRule] {
        &self.rules
    }

    pub fn redaction_rules(&self) -> &[RedactionRule] {
        &self.redactions
    }

    pub fn precedence(&self) -> &PolicyPrecedence {
        &self.precedence
    }

    pub fn with_rule(mut self, rule: PolicyRule) -> Self {
        self.rules.push(rule);
        self
    }

    pub fn add_rule(&mut self, rule: PolicyRule) {
        self.rules.push(rule);
    }

    pub fn with_redaction_rule(mut self, rule: RedactionRule) -> Self {
        self.redactions.push(rule);
        self
    }

    pub fn add_redaction_rule(&mut self, rule: RedactionRule) {
        self.redactions.push(rule);
    }

    pub fn with_redaction(
        self,
        pattern: impl Into<String>,
        replacement: impl Into<String>,
    ) -> Self {
        self.with_redaction_rule(RedactionRule::literal(pattern, replacement))
    }

    pub fn add_redaction(&mut self, pattern: impl Into<String>, replacement: impl Into<String>) {
        self.add_redaction_rule(RedactionRule::literal(pattern, replacement));
    }

    pub fn with_precedence(mut self, precedence: PolicyPrecedence) -> Self {
        self.precedence = precedence;
        self
    }

    /// Evaluate matching rules. A hard deny always wins, independent of rule
    /// order. Among non-deny decisions, the strongest decision wins and the
    /// first rule wins ties. No matching rule returns PolicyDecision::Allow.
    pub fn evaluate<T>(&self, context: T) -> PolicyDecision
    where
        T: Into<PolicyContext>,
    {
        let context = context.into();
        let mut selected = PolicyDecision::Allow;

        for rule in &self.rules {
            if !rule.matches(&context) {
                continue;
            }

            if rule.decision.is_hard_deny() {
                return self.redact_decision(rule.decision.clone());
            }

            if rule.decision.precedence_rank() > selected.precedence_rank() {
                selected = rule.decision.clone();
            }
        }

        self.redact_decision(selected)
    }

    pub fn evaluate_event<T>(&self, event: HookEvent, context: T) -> PolicyDecision
    where
        T: Into<PolicyContext>,
    {
        let mut context = context.into();
        context.event = event;
        context.scope = event.scope();
        self.evaluate(&context)
    }

    pub fn evaluate_for(
        &self,
        event: HookEvent,
        role: Option<&str>,
        provider: Option<&str>,
        tool: Option<&str>,
        path: Option<&str>,
    ) -> PolicyDecision {
        let mut context = PolicyContext::new(event);
        context.role = role.map(str::to_owned);
        context.provider = provider.map(str::to_owned);
        context.tool = tool.map(str::to_owned);
        context.path = path.map(str::to_owned);
        self.evaluate(&context)
    }

    pub fn evaluate_hook<T>(&self, context: T) -> HookDecision
    where
        T: Into<PolicyContext>,
    {
        self.evaluate(context).into_hook_decision()
    }

    pub fn evaluate_event_hook<T>(&self, event: HookEvent, context: T) -> HookDecision
    where
        T: Into<PolicyContext>,
    {
        self.evaluate_event(event, context).into_hook_decision()
    }

    /// Evaluate base policy and then combine it with hard-gate decisions.
    /// Deny remains dominant regardless of which layer produced it.
    pub fn evaluate_with_gates<T, I>(&self, context: T, gate_decisions: I) -> PolicyDecision
    where
        T: Into<PolicyContext>,
        I: IntoIterator<Item = (HardGate, PolicyDecision)>,
    {
        let decision = self.evaluate(context);
        self.redact_decision(self.precedence.resolve_with_base(decision, gate_decisions))
    }

    /// Apply all configured redaction rules in insertion order.
    pub fn redact_text(&self, text: &str) -> String {
        self.redactions
            .iter()
            .fold(text.to_owned(), |current, rule| rule.apply(&current))
    }

    pub fn redact(&self, text: &str) -> String {
        self.redact_text(text)
    }

    pub fn sanitize(&self, text: &str) -> String {
        self.redact_text(text)
    }

    pub fn redact_decision(&self, decision: PolicyDecision) -> PolicyDecision {
        match decision {
            PolicyDecision::Allow => PolicyDecision::Allow,
            PolicyDecision::Deny { reason } => PolicyDecision::Deny {
                reason: self.redact_text(&reason),
            },
            PolicyDecision::Ask { prompt } => PolicyDecision::Ask {
                prompt: self.redact_text(&prompt),
            },
            PolicyDecision::Modify { replacement } => PolicyDecision::Modify {
                replacement: self.redact_text(&replacement),
            },
            PolicyDecision::AddContext { context } => PolicyDecision::AddContext {
                context: self.redact_text(&context),
            },
        }
    }
}

/// The three security gates that may be composed around normal policy rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardGate {
    PlanMode,
    Unsafe,
    Sandbox,
}

/// Short aliases for callers that use gate-oriented naming.
pub type PolicyGate = HardGate;
pub type PrecedenceGate = HardGate;

/// Ordered composition of the PlanMode, Unsafe, and Sandbox hard gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyPrecedence {
    order: Vec<HardGate>,
}

impl Default for PolicyPrecedence {
    fn default() -> Self {
        Self::new()
    }
}

impl PolicyPrecedence {
    /// Default order, from highest to lowest precedence.
    pub fn new() -> Self {
        Self {
            order: vec![HardGate::PlanMode, HardGate::Unsafe, HardGate::Sandbox],
        }
    }

    pub fn empty() -> Self {
        Self { order: Vec::new() }
    }

    pub fn for_gate(gate: HardGate) -> Self {
        Self::empty().then(gate)
    }

    pub fn plan_mode() -> Self {
        Self::for_gate(HardGate::PlanMode)
    }

    pub fn unsafe_mode() -> Self {
        Self::for_gate(HardGate::Unsafe)
    }

    pub fn sandbox() -> Self {
        Self::for_gate(HardGate::Sandbox)
    }

    pub fn compose<I>(parts: I) -> Self
    where
        I: IntoIterator<Item = Self>,
    {
        parts
            .into_iter()
            .fold(Self::empty(), |current, part| current.merge(&part))
    }

    pub fn then(mut self, gate: HardGate) -> Self {
        if !self.order.contains(&gate) {
            self.order.push(gate);
        }
        self
    }

    pub fn prepend(mut self, gate: HardGate) -> Self {
        self.order.retain(|existing| *existing != gate);
        self.order.insert(0, gate);
        self
    }

    pub fn merge(&self, other: &Self) -> Self {
        let mut merged = self.clone();
        for gate in &other.order {
            merged = merged.then(*gate);
        }
        merged
    }

    pub fn order(&self) -> &[HardGate] {
        &self.order
    }

    pub fn rank(&self, gate: HardGate) -> usize {
        self.order
            .iter()
            .position(|candidate| *candidate == gate)
            .unwrap_or(usize::MAX)
    }

    /// Resolve gate results. Deny is always strongest; the configured gate
    /// order breaks ties between otherwise equal decisions.
    pub fn resolve<I>(&self, decisions: I) -> PolicyDecision
    where
        I: IntoIterator<Item = (HardGate, PolicyDecision)>,
    {
        let mut selected: Option<(usize, u8, PolicyDecision)> = None;
        for (gate, decision) in decisions {
            let candidate = (self.rank(gate), decision.precedence_rank(), decision);
            if selected.as_ref().is_none_or(|current| {
                candidate.1 > current.1 || (candidate.1 == current.1 && candidate.0 < current.0)
            }) {
                selected = Some(candidate);
            }
        }
        selected
            .map(|(_, _, decision)| decision)
            .unwrap_or(PolicyDecision::Allow)
    }

    pub fn resolve_with_base<I>(&self, base: PolicyDecision, decisions: I) -> PolicyDecision
    where
        I: IntoIterator<Item = (HardGate, PolicyDecision)>,
    {
        let mut selected = (usize::MAX, base.precedence_rank(), base);
        for (gate, decision) in decisions {
            let candidate = (self.rank(gate), decision.precedence_rank(), decision);
            if candidate.1 > selected.1 || (candidate.1 == selected.1 && candidate.0 < selected.0) {
                selected = candidate;
            }
        }
        selected.2
    }

    pub fn combine<I>(&self, decisions: I) -> PolicyDecision
    where
        I: IntoIterator<Item = (HardGate, PolicyDecision)>,
    {
        self.resolve(decisions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_event_covers_the_second_batch_and_has_stable_wire_names() {
        let events = [
            HookEvent::SessionStart,
            HookEvent::SessionResume,
            HookEvent::UserPromptSubmit,
            HookEvent::BeforeContextBuild,
            HookEvent::AfterContextBuild,
            HookEvent::BeforeProviderRequest,
            HookEvent::AfterProviderResponse,
            HookEvent::BeforeToolCall,
            HookEvent::AfterToolCall,
            HookEvent::BeforeFileRead,
            HookEvent::BeforeFileWrite,
            HookEvent::BeforeProcessSpawn,
            HookEvent::PermissionRequest,
            HookEvent::BeforeCompact,
            HookEvent::AfterCompact,
            HookEvent::SubagentStart,
            HookEvent::SubagentStop,
            HookEvent::TurnStop,
            HookEvent::SessionStop,
            HookEvent::BeforeRoleResolve,
            HookEvent::AfterRoleResolve,
        ];

        assert_eq!(events.len(), 21);
        assert_eq!(HookEvent::BeforeToolCall.to_string(), "before_tool_call");
        assert_eq!(
            serde_json::to_string(&HookEvent::SessionResume).unwrap(),
            r#""session_resume""#
        );
        assert_eq!(HookEvent::BeforeFileRead.scope(), PolicyScope::File);
        assert_eq!(HookEvent::BeforeRoleResolve.scope(), PolicyScope::Role);
    }

    #[test]
    fn default_policy_allows_and_matching_dimensions_are_conjunctive() {
        let empty = PolicyEngine::default();
        let context = PolicyContext::new(HookEvent::BeforeToolCall)
            .with_role("reviewer")
            .with_provider("openai")
            .with_tool("read_file")
            .with_path("/workspace/src/main.rs");
        assert_eq!(empty.evaluate(&context), PolicyDecision::Allow);

        let engine = PolicyEngine::new(vec![
            PolicyRule::deny(PolicyScope::Tool, "reviewer cannot read this path")
                .with_role("reviewer")
                .with_provider("openai")
                .with_tool("read_*")
                .with_path("/workspace/src/*"),
        ]);

        assert!(matches!(
            engine.evaluate(&context),
            PolicyDecision::Deny { .. }
        ));

        let other_role = context.clone().with_role("builder");
        assert_eq!(engine.evaluate(&other_role), PolicyDecision::Allow);
    }

    #[test]
    fn hard_deny_wins_regardless_of_rule_order() {
        let allow = PolicyRule::allow(PolicyScope::Any).with_tool("run_terminal_command");
        let deny =
            PolicyRule::deny(PolicyScope::Tool, "unsafe command").with_tool("run_terminal_command");
        let context =
            PolicyContext::new(HookEvent::BeforeToolCall).with_tool("run_terminal_command");

        for rules in [vec![allow.clone(), deny.clone()], vec![deny, allow]] {
            assert!(matches!(
                PolicyEngine::new(rules).evaluate(&context),
                PolicyDecision::Deny { .. }
            ));
        }
    }

    #[test]
    fn non_deny_hook_decisions_are_preserved() {
        let context = PolicyContext::new(HookEvent::UserPromptSubmit);

        let ask = PolicyEngine::new(vec![PolicyRule::ask(PolicyScope::Turn, "confirm prompt")]);
        assert_eq!(
            ask.evaluate_hook(&context),
            HookDecision::Ask {
                prompt: "confirm prompt".into()
            }
        );

        let modify = PolicyEngine::new(vec![PolicyRule::modify(
            PolicyScope::Turn,
            "redacted prompt",
        )]);
        assert_eq!(
            modify.evaluate_hook(&context),
            HookDecision::Modify {
                replacement: "redacted prompt".into()
            }
        );

        let context_rule = PolicyEngine::new(vec![PolicyRule::add_context(
            PolicyScope::Turn,
            "remember the project constraints",
        )]);
        assert_eq!(
            context_rule.evaluate_hook(&context),
            HookDecision::AddContext {
                context: "remember the project constraints".into()
            }
        );
    }

    #[test]
    fn redaction_rules_return_only_sanitized_text() {
        let engine = PolicyEngine::default()
            .with_redaction_rule(RedactionRule::literal("super-secret", "[REDACTED]"))
            .with_redaction_rule(RedactionRule::regex(r"token=[^ ]+", "token=[REDACTED]").unwrap());

        let output = engine.redact_text("super-secret token=abc123 safe");
        assert_eq!(output, "[REDACTED] token=[REDACTED] safe");
        assert!(!output.contains("super-secret"));
        assert!(!output.contains("abc123"));
    }

    #[test]
    fn hard_gate_precedence_is_composable_and_deny_wins() {
        let precedence = PolicyPrecedence::new()
            .then(HardGate::PlanMode)
            .then(HardGate::Unsafe)
            .then(HardGate::Sandbox);
        let composed = PolicyPrecedence::compose([
            PolicyPrecedence::for_gate(HardGate::PlanMode),
            PolicyPrecedence::for_gate(HardGate::Sandbox),
        ]);
        assert_eq!(composed.order(), &[HardGate::PlanMode, HardGate::Sandbox]);

        let result = precedence.resolve([
            (
                HardGate::PlanMode,
                PolicyDecision::Ask {
                    prompt: "plan approval".into(),
                },
            ),
            (
                HardGate::Sandbox,
                PolicyDecision::Deny {
                    reason: "sandbox unavailable".into(),
                },
            ),
        ]);
        assert_eq!(
            result,
            PolicyDecision::Deny {
                reason: "sandbox unavailable".into()
            }
        );
    }
}
