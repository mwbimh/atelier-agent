use atelier_tools::implementations::{BashToolInput, SandboxPermissions};

#[test]
fn bash_defaults_to_the_session_sandbox() {
    let input: BashToolInput = serde_json::from_value(serde_json::json!({
        "command": "whoami",
        "description": "Inspect the current execution identity",
        "is_background": false
    }))
    .expect("bash input without an override should deserialize");

    assert_eq!(input.sandbox_permissions, SandboxPermissions::UseDefault);
    assert_eq!(input.justification, None);
}

#[test]
fn bash_accepts_an_explicit_per_command_escalation_request() {
    let input: BashToolInput = serde_json::from_value(serde_json::json!({
        "command": "git config --global user.name Atelier",
        "description": "Update the user's global Git configuration",
        "is_background": false,
        "sandbox_permissions": "require_escalated",
        "justification": "This command writes outside the workspace sandbox."
    }))
    .expect("require_escalated should be part of the bash wire contract");

    assert_eq!(
        input.sandbox_permissions,
        SandboxPermissions::RequireEscalated
    );
    assert_eq!(
        input.justification.as_deref(),
        Some("This command writes outside the workspace sandbox.")
    );
}

#[test]
fn bash_schema_exposes_the_codex_compatible_override_fields() {
    let schema = serde_json::to_value(schemars::schema_for!(BashToolInput))
        .expect("bash schema should serialize");
    let properties = schema["properties"]
        .as_object()
        .expect("bash schema properties");

    assert!(properties.contains_key("sandbox_permissions"));
    assert!(properties.contains_key("justification"));
    assert_eq!(
        properties["sandbox_permissions"]["default"],
        serde_json::json!("use_default")
    );
}
