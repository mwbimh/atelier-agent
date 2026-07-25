#![cfg(windows)]

use atelier_windows_sandbox::{PublicSandboxCommand, parse_public_sandbox_command};

#[test]
fn public_sandbox_commands_parse_full_and_safe_forms() {
    assert_eq!(
        parse_public_sandbox_command(["setup"]).unwrap(),
        PublicSandboxCommand::Setup
    );
    assert_eq!(
        parse_public_sandbox_command(["status"]).unwrap(),
        PublicSandboxCommand::Status { json: false }
    );
    assert_eq!(
        parse_public_sandbox_command(["status", "--json"]).unwrap(),
        PublicSandboxCommand::Status { json: true }
    );
    assert_eq!(
        parse_public_sandbox_command(["reset"]).unwrap(),
        PublicSandboxCommand::Reset { yes: false }
    );
    assert_eq!(
        parse_public_sandbox_command(["reset", "--yes"]).unwrap(),
        PublicSandboxCommand::Reset { yes: true }
    );
}

#[test]
fn public_sandbox_parser_rejects_implicit_or_ambiguous_mutations() {
    assert!(parse_public_sandbox_command(std::iter::empty::<&str>()).is_err());
    assert!(parse_public_sandbox_command(["reset", "yes"]).is_err());
    assert!(parse_public_sandbox_command(["setup", "--yes"]).is_err());
    assert!(parse_public_sandbox_command(["unknown"]).is_err());
}
