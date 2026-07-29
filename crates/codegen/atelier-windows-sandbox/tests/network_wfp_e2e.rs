#![cfg(windows)]

use atelier_windows_sandbox::{CommandRequest, NetworkPolicy, SandboxMode, run_command};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn system32_executable(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"))
        .join("System32")
        .join(name)
}

fn run_program(
    root: &Path,
    policy: NetworkPolicy,
    program: PathBuf,
    args: Vec<OsString>,
) -> (i32, String, String) {
    let mut request = CommandRequest::new(
        SandboxMode::WorkspaceWrite,
        vec![root.to_path_buf()],
        root.to_path_buf(),
        program,
        args,
    )
    .with_network_policy(policy);
    request.timeout = Some(Duration::from_secs(15));
    let output = run_command(request).expect("run sandboxed network probe");
    assert!(!output.timed_out, "network probe timed out");
    (
        output.exit_code,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn tcp_probe(root: &Path, policy: NetworkPolicy, should_connect: bool) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind TCP listener");
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut request = [0_u8; 1024];
                    let _ = stream.read(&mut request);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK",
                    );
                    let _ = accepted_tx.send(true);
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        let _ = accepted_tx.send(false);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => {
                    let _ = accepted_tx.send(false);
                    return;
                }
            }
        }
    });

    let url = format!("http://127.0.0.1:{port}/");
    let (exit, stdout, stderr) = run_program(
        root,
        policy,
        system32_executable("curl.exe"),
        vec![
            OsString::from("--silent"),
            OsString::from("--show-error"),
            OsString::from("--connect-timeout"),
            OsString::from("2"),
            OsString::from("--max-time"),
            OsString::from("4"),
            OsString::from(url),
        ],
    );
    let accepted = accepted_rx
        .recv_timeout(Duration::from_secs(7))
        .expect("TCP probe server result");
    server.join().expect("TCP probe server thread");

    if should_connect {
        assert_eq!(
            exit, 0,
            "policy={policy:?} stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(accepted, "allowed TCP probe never reached the listener");
    } else {
        assert_ne!(exit, 0, "disabled TCP probe unexpectedly succeeded");
        assert!(!accepted, "disabled TCP probe reached the listener");
    }
}

fn udp_probe(root: &Path, policy: NetworkPolicy) {
    let listener = UdpSocket::bind(("127.0.0.1", 0)).expect("bind UDP listener");
    listener
        .set_read_timeout(Some(Duration::from_millis(1500)))
        .unwrap();
    let port = listener.local_addr().unwrap().port();

    // curl.exe's TFTP transport emits a UDP request and remains usable under
    // the sandbox account's Constrained Language Mode. Its eventual exit code
    // is irrelevant because this listener intentionally is not a TFTP server;
    // the OS-boundary contract is whether a datagram reaches the listener.
    let _ = run_program(
        root,
        policy,
        system32_executable("curl.exe"),
        vec![
            OsString::from("--silent"),
            OsString::from("--show-error"),
            OsString::from("--max-time"),
            OsString::from("2"),
            OsString::from(format!("tftp://127.0.0.1:{port}/atelier-probe")),
        ],
    );

    let mut packet = [0_u8; 512];
    match policy {
        NetworkPolicy::AllowAll => {
            let (size, _) = listener
                .recv_from(&mut packet)
                .expect("receive allowed UDP DNS query");
            assert!(size > 0, "allowed UDP probe emitted an empty datagram");
        }
        NetworkPolicy::Disabled => {
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

    tcp_probe(root.path(), NetworkPolicy::Disabled, false);
    udp_probe(root.path(), NetworkPolicy::Disabled);
}

#[test]
#[ignore = "requires `ate sandbox setup` with elevated WFP provisioning"]
fn allow_all_network_policy_does_not_apply_the_offline_wfp_identity() {
    let status = atelier_windows_sandbox::setup_status().expect("inspect sandbox setup");
    assert!(status.wfp_filters_ready, "run `ate sandbox setup` first");
    let root = tempfile::tempdir().expect("network probe root");

    tcp_probe(root.path(), NetworkPolicy::AllowAll, true);
    udp_probe(root.path(), NetworkPolicy::AllowAll);
}
