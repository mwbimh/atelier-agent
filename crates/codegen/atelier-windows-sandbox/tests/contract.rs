#![cfg(windows)]

use atelier_windows_sandbox::CommandRequest;
use atelier_windows_sandbox::SandboxMode;
use atelier_windows_sandbox::SandboxSession;
use atelier_windows_sandbox::run_command;
use atelier_windows_sandbox::spawn_piped_command;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;

fn command_request(mode: SandboxMode, root: &std::path::Path) -> CommandRequest {
    CommandRequest::new(
        mode,
        vec![root.to_path_buf()],
        root.to_path_buf(),
        PathBuf::from(std::env::var_os("COMSPEC").expect("COMSPEC")),
        vec![
            OsString::from("/D"),
            OsString::from("/C"),
            OsString::from("echo atelier"),
        ],
    )
}

fn restricted_token_supported_or_skip() -> bool {
    match atelier_windows_sandbox::probe_restricted_token() {
        Ok(()) => true,
        Err(error)
            if error
                .to_string()
                .contains("CreateRestrictedToken failed: 87") =>
        {
            eprintln!(
                "skipping Windows restricted-token contract: this host rejects WRITE_RESTRICTED tokens (ERROR_INVALID_PARAMETER 87)"
            );
            false
        }
        Err(error) => panic!("unexpected Windows sandbox probe failure: {error:#}"),
    }
}

#[test]
fn missing_root_is_rejected_fail_closed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let missing = temp.path().join("does-not-exist");
    let request = command_request(SandboxMode::ReadOnly, &missing);

    assert!(request.validate().is_err());
}

#[test]
fn cwd_outside_root_is_rejected_fail_closed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    let outside = temp.path().join("root-other");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&outside).expect("outside");
    let mut request = command_request(SandboxMode::ReadOnly, &root);
    request.cwd = outside;

    assert!(request.validate().is_err());
}

#[test]
fn telemetry_is_explicitly_none_by_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let request = command_request(SandboxMode::ReadOnly, temp.path());

    assert!(request.telemetry.is_none());
}

#[test]
fn restricted_token_runner_executes_a_command() {
    if !restricted_token_supported_or_skip() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let request = command_request(SandboxMode::WorkspaceWrite, temp.path());

    let output = run_command(request).expect("restricted command runner");
    assert_eq!(
        output.exit_code,
        0,
        "stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("atelier"));
}

#[test]
fn workspace_write_session_can_create_and_delete_a_file() {
    if !restricted_token_supported_or_skip() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let created = temp.path().join("created.txt");
    let mut session = SandboxSession::new().expect("sandbox session");
    let create_command = "echo created > created.txt";
    let mut request = command_request(SandboxMode::WorkspaceWrite, temp.path());
    request.args = vec![
        OsString::from("/D"),
        OsString::from("/C"),
        OsString::from(create_command),
    ];
    let output = session.run(request).expect("workspace-write create");
    assert_eq!(
        output.exit_code,
        0,
        "stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(created.is_file());

    let mut request = command_request(SandboxMode::WorkspaceWrite, temp.path());
    request.args = vec![
        OsString::from("/D"),
        OsString::from("/C"),
        OsString::from("del /Q created.txt"),
    ];
    let output = session.run(request).expect("workspace-write delete");
    assert_eq!(
        output.exit_code,
        0,
        "stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!created.exists());
}

#[test]
fn workspace_write_session_cannot_create_a_file_outside_the_root() {
    if !restricted_token_supported_or_skip() {
        return;
    }
    let base = tempfile::tempdir().expect("base");
    let root = base.path().join("root");
    let outside = base.path().join("outside");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&outside).expect("outside");
    let blocked = outside.join("blocked.txt");
    let command = format!("echo denied > {}", blocked.display());
    let mut session = SandboxSession::new().expect("sandbox session");
    let mut request = command_request(SandboxMode::WorkspaceWrite, &root);
    request.args = vec![
        OsString::from("/D"),
        OsString::from("/C"),
        OsString::from(command.clone()),
    ];

    let _output = session.run(request).expect("sandboxed command");
    assert!(
        !blocked.exists(),
        "workspace sandbox escaped to {blocked:?}"
    );
}

#[test]
fn read_only_runner_cannot_create_a_file_under_the_root() {
    if !restricted_token_supported_or_skip() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let blocked = temp.path().join("blocked.txt");
    let command = format!(
        "echo denied > \"{}\"",
        blocked.to_string_lossy().replace('"', "")
    );
    let mut request = command_request(SandboxMode::ReadOnly, temp.path());
    request.args = vec![
        OsString::from("/D"),
        OsString::from("/C"),
        OsString::from(command),
    ];

    let output = run_command(request).expect("read-only command runner");
    assert_ne!(output.exit_code, 0);
    assert!(!blocked.exists());
}

#[test]
fn restricted_piped_process_round_trips_streaming_stdio() {
    if !restricted_token_supported_or_skip() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let mut request = command_request(SandboxMode::ReadOnly, temp.path());
    request.args = vec![
        OsString::from("/D"),
        OsString::from("/Q"),
        OsString::from("/K"),
    ];

    let mut child = spawn_piped_command(request).expect("spawn restricted piped command");
    let mut stdin = child.take_stdin().expect("child stdin");
    let mut stdout = child.take_stdout().expect("child stdout");
    stdin.write_all(b"echo worker-marker\r\nexit\r\n").unwrap();
    drop(stdin);

    let status = child.wait().expect("wait restricted child");
    let mut output = String::new();
    stdout.read_to_string(&mut output).unwrap();

    assert_eq!(status, 0, "output={output:?}");
    assert!(output.contains("worker-marker"), "output={output:?}");
}
