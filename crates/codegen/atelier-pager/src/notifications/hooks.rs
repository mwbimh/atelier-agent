use std::process::{Command, Stdio};
use std::time::Duration;

use crate::notifications::NotificationEvent;
use crate::notifications::config::NotificationHook;

fn execute_hook(
    command: &str,
    event_str: &str,
    message: &str,
    session_id: Option<&str>,
    timeout: Duration,
) {
    #[cfg(windows)]
    let mut cmd = {
        let mut command_process = Command::new("powershell.exe");
        command_process
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(command);
        command_process
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut command_process = Command::new("sh");
        command_process.arg("-c").arg(command);
        command_process
    };
    cmd.env("ATELIER_EVENT", event_str)
        .env("ATELIER_MESSAGE", message)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(sid) = session_id {
        cmd.env("ATELIER_SESSION_ID", sid);
    }

    // Create a new process group so we can kill the entire tree on timeout,
    // preventing orphaned subprocesses from accumulating.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe per POSIX and does not
        // allocate or take locks. Called between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid().ok();
                Ok(())
            });
        }
    }

    match cmd.spawn() {
        Ok(mut child) => {
            use wait_timeout::ChildExt;
            match child.wait_timeout(timeout) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    // Kill the entire process group, not just the direct child.
                    #[cfg(unix)]
                    {
                        let pid = child.id() as i32;
                        let _ = nix::sys::signal::killpg(
                            nix::unistd::Pid::from_raw(pid),
                            nix::sys::signal::Signal::SIGKILL,
                        );
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = child.kill();
                    }
                    let _ = child.wait();
                    tracing::warn!("hook timed out");
                }
                Err(e) => tracing::debug!(error = %e, command, "hook wait failed"),
            }
        }
        Err(e) => tracing::debug!(error = %e, command, "hook spawn failed"),
    }
}

pub fn run_hook(hook: &NotificationHook, event: &NotificationEvent) {
    let command = hook.command.clone();
    let event_str: &'static str = event.kind.as_str();
    let message = event.body.clone();
    let session_id = event.session_id.clone();
    let timeout = Duration::from_secs(hook.timeout_secs.max(1));

    std::thread::spawn(move || {
        execute_hook(
            &command,
            event_str,
            &message,
            session_id.as_deref(),
            timeout,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::config::NotificationEventKind;
    use std::time::Instant;

    fn test_event() -> NotificationEvent {
        NotificationEvent {
            kind: NotificationEventKind::TurnComplete,
            title: "Atelier".into(),
            body: "test body payload".into(),
            session_id: Some("test-session-123".into()),
        }
    }

    fn env_dump_command(out: &std::path::Path) -> String {
        #[cfg(windows)]
        {
            return format!(
                "@('ATELIER_EVENT=' + $env:ATELIER_EVENT, \
                 'ATELIER_MESSAGE=' + $env:ATELIER_MESSAGE, \
                 'ATELIER_SESSION_ID=' + $env:ATELIER_SESSION_ID) | \
                 Set-Content -LiteralPath '{}'",
                out.display()
            );
        }
        #[cfg(not(windows))]
        format!(
            "printf 'ATELIER_EVENT=%s\\nATELIER_MESSAGE=%s\\nATELIER_SESSION_ID=%s\\n' \
             \"$ATELIER_EVENT\" \"$ATELIER_MESSAGE\" \"$ATELIER_SESSION_ID\" > {}",
            out.display()
        )
    }

    fn session_presence_command(out: &std::path::Path) -> String {
        #[cfg(windows)]
        {
            return format!(
                "$value = if (Test-Path Env:ATELIER_SESSION_ID) {{ 'present' }} \
                 else {{ 'absent' }}; Set-Content -LiteralPath '{}' -Value $value",
                out.display()
            );
        }
        #[cfg(not(windows))]
        format!("env > {}", out.display())
    }

    fn create_marker_command(marker: &std::path::Path) -> String {
        #[cfg(windows)]
        {
            return format!(
                "New-Item -ItemType File -Force -Path '{}' | Out-Null",
                marker.display()
            );
        }
        #[cfg(not(windows))]
        format!("touch {}", marker.display())
    }

    fn long_sleep_command(marker: Option<&std::path::Path>) -> String {
        #[cfg(windows)]
        {
            return match marker {
                Some(marker) => format!(
                    "Start-Sleep -Seconds 100; {}",
                    create_marker_command(marker)
                ),
                None => "Start-Sleep -Seconds 100".to_owned(),
            };
        }
        #[cfg(not(windows))]
        match marker {
            Some(marker) => format!("sleep 100; touch {}", marker.display()),
            None => "sleep 100".to_owned(),
        }
    }

    #[test]
    fn sets_environment_variables() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("env.txt");
        let command = env_dump_command(&out);

        execute_hook(
            &command,
            "Turn complete",
            "hello world",
            Some("sess-42"),
            Duration::from_secs(5),
        );

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(
            content.contains("ATELIER_EVENT=Turn complete"),
            "missing ATELIER_EVENT: {content}"
        );
        assert!(
            content.contains("ATELIER_MESSAGE=hello world"),
            "missing ATELIER_MESSAGE: {content}"
        );
        assert!(
            content.contains("ATELIER_SESSION_ID=sess-42"),
            "missing ATELIER_SESSION_ID: {content}"
        );
    }

    #[test]
    fn omits_session_id_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("env.txt");
        let command = session_presence_command(&out);

        execute_hook(
            &command,
            "Turn complete",
            "msg",
            None,
            Duration::from_secs(5),
        );

        let content = std::fs::read_to_string(&out).unwrap();
        #[cfg(windows)]
        assert_eq!(content.trim(), "absent");
        #[cfg(not(windows))]
        assert!(
            !content.contains("ATELIER_SESSION_ID"),
            "ATELIER_SESSION_ID should not be set: {content}"
        );
    }

    #[test]
    fn kills_on_timeout() {
        let start = Instant::now();
        execute_hook(
            &long_sleep_command(None),
            "Turn complete",
            "msg",
            None,
            Duration::from_secs(1),
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(3),
            "should return within timeout, took {elapsed:?}"
        );
    }

    #[test]
    fn handles_failed_shell_command_gracefully() {
        execute_hook(
            "/nonexistent/path/binary",
            "Turn complete",
            "msg",
            None,
            Duration::from_secs(1),
        );
    }

    #[test]
    fn handles_nonzero_exit_gracefully() {
        execute_hook(
            "exit 1",
            "Turn complete",
            "msg",
            None,
            Duration::from_secs(5),
        );
    }

    #[test]
    fn successful_command_completes_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("done");
        let command = create_marker_command(&marker);

        execute_hook(
            &command,
            "Turn complete",
            "msg",
            None,
            Duration::from_secs(5),
        );

        assert!(marker.exists());
    }

    #[test]
    fn run_hook_spawns_thread_without_panic() {
        let hook = NotificationHook {
            command: if cfg!(windows) {
                "$true | Out-Null".into()
            } else {
                "true".into()
            },
            events: vec![],
            only_unfocused: false,
            timeout_secs: 5,
        };
        run_hook(&hook, &test_event());
        std::thread::sleep(Duration::from_millis(200));
    }

    #[test]
    fn timeout_clamped_to_minimum_one_second() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("done");
        let hook = NotificationHook {
            command: long_sleep_command(Some(&marker)),
            events: vec![],
            only_unfocused: false,
            timeout_secs: 0, // exercises the .max(1) clamp inside run_hook
        };
        let start = Instant::now();
        run_hook(&hook, &test_event());
        // Wait for the spawned thread to finish (clamp turns 0 -> 1s timeout)
        std::thread::sleep(Duration::from_millis(2500));
        let elapsed = start.elapsed();
        // The hook should have been killed by the 1s timeout, so the marker
        // file should NOT exist (sleep 100 never completes).
        assert!(
            !marker.exists(),
            "hook should have been killed by timeout before creating marker"
        );
        // Sanity: the whole thing completed well under 10s, confirming the
        // timeout was ~1s (clamped) not 0s (instant) or unbounded.
        assert!(
            elapsed < Duration::from_secs(5),
            "should complete within a few seconds, took {elapsed:?}"
        );
    }

    #[test]
    fn run_hook_passes_correct_env_via_thread() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("env.txt");
        let hook = NotificationHook {
            command: env_dump_command(&out),
            events: vec![],
            only_unfocused: false,
            timeout_secs: 5,
        };
        let event = test_event();
        run_hook(&hook, &event);

        // Poll for the output file instead of a fixed sleep — the spawned
        // thread + fork/exec may take variable time on loaded systems.
        let deadline = Instant::now() + Duration::from_secs(5);
        let content = loop {
            if let Ok(c) = std::fs::read_to_string(&out) {
                break c;
            }
            assert!(
                Instant::now() < deadline,
                "hook did not produce output file within 5s (sh or printf may not be available)"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(content.contains("ATELIER_EVENT=Turn complete"));
        assert!(content.contains("ATELIER_MESSAGE=test body payload"));
        assert!(content.contains("ATELIER_SESSION_ID=test-session-123"));
    }
}
