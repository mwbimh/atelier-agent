#![cfg(windows)]

use atelier_windows_sandbox::{CommandRequest, NetworkPolicy, SandboxMode, run_command};
use std::ffi::OsString;
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn powershell() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"))
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

fn run_script(root: &Path, policy: NetworkPolicy, script: String) -> i32 {
    let mut request = CommandRequest::new(
        SandboxMode::WorkspaceWrite,
        vec![root.to_path_buf()],
        root.to_path_buf(),
        powershell(),
        vec![
            OsString::from("-NoLogo"),
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-Command"),
            OsString::from(script),
        ],
    )
    .with_network_policy(policy);
    request.timeout = Some(Duration::from_secs(15));
    let output = run_command(request).expect("run sandboxed network probe");
    assert!(!output.timed_out, "network probe timed out");
    output.exit_code
}

fn tcp_probe(root: &Path, policy: NetworkPolicy, expected_exit: i32) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind TCP listener");
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        "$ErrorActionPreference='Stop'; try {{ $c=[Net.Sockets.TcpClient]::new(); $c.Connect('127.0.0.1',{port}); $c.Dispose(); exit 0 }} catch {{ exit 23 }}"
    );

    assert_eq!(run_script(root, policy, script), expected_exit);
}

fn udp_probe(root: &Path, policy: NetworkPolicy) {
    let listener = UdpSocket::bind(("127.0.0.1", 0)).expect("bind UDP listener");
    listener
        .set_read_timeout(Some(Duration::from_millis(750)))
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        "$ErrorActionPreference='Stop'; $u=[Net.Sockets.UdpClient]::new(); try {{ $b=[byte[]](1,2,3); [void]$u.Send($b,$b.Length,'127.0.0.1',{port}); exit 0 }} catch {{ exit 24 }} finally {{ $u.Dispose() }}"
    );

    let exit = run_script(root, policy, script);
    let mut packet = [0_u8; 16];
    match policy {
        NetworkPolicy::AllowAll => {
            assert_eq!(exit, 0);
            let (size, _) = listener
                .recv_from(&mut packet)
                .expect("receive allowed UDP packet");
            assert_eq!(&packet[..size], &[1, 2, 3]);
        }
        NetworkPolicy::Disabled => {
            assert!(
                exit == 0 || exit == 24,
                "disabled UDP send returned unexpected exit code {exit}"
            );
            let error = listener
                .recv_from(&mut packet)
                .expect_err("disabled network policy must not emit a UDP packet");
            assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ),
                "unexpected UDP receive error: {error}"
            );
        }
    }
}

#[test]
#[ignore = "requires `ate sandbox setup` with elevated WFP provisioning"]
fn disabled_network_policy_blocks_tcp_and_udp_at_the_os_boundary() {
    let status = atelier_windows_sandbox::setup_status().expect("inspect sandbox setup");
    assert!(status.wfp_filters_ready, "run `ate sandbox setup` first");
    let root = tempfile::tempdir().expect("network probe root");

    tcp_probe(root.path(), NetworkPolicy::Disabled, 23);
    udp_probe(root.path(), NetworkPolicy::Disabled);
}

#[test]
#[ignore = "requires `ate sandbox setup` with elevated WFP provisioning"]
fn allow_all_network_policy_does_not_apply_the_offline_wfp_identity() {
    let status = atelier_windows_sandbox::setup_status().expect("inspect sandbox setup");
    assert!(status.wfp_filters_ready, "run `ate sandbox setup` first");
    let root = tempfile::tempdir().expect("network probe root");

    tcp_probe(root.path(), NetworkPolicy::AllowAll, 0);
    udp_probe(root.path(), NetworkPolicy::AllowAll);
}
