use std::process::Command;

fn temporary_atelier_home() -> std::path::PathBuf {
    let unique = format!(
        "atelier-cli-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos()
    );
    let home = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&home).expect("create temporary Atelier home");
    home
}

#[test]
fn version_does_not_load_runtime_configuration() {
    let home = temporary_atelier_home();
    std::fs::write(home.join("config.toml"), "[models]\ndefault = \"stale\"\n")
        .expect("write intentionally invalid current config");

    let output = Command::new(env!("CARGO_BIN_EXE_ate"))
        .arg("--version")
        .env("ATELIER_HOME", &home)
        .output()
        .expect("run ate --version");

    let _ = std::fs::remove_dir_all(&home);

    assert!(
        output.status.success(),
        "ate --version must not depend on config validity; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(env!("CARGO_PKG_VERSION")),
        "version output should contain the package version"
    );
}

#[test]
fn help_uses_the_public_ate_command_name() {
    let home = temporary_atelier_home();
    let output = Command::new(env!("CARGO_BIN_EXE_ate"))
        .arg("--help")
        .env("ATELIER_HOME", &home)
        .output()
        .expect("run ate --help");
    let _ = std::fs::remove_dir_all(&home);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: ate "), "help output: {stdout}");
    assert!(!stdout.contains("Usage: atelier "), "help output: {stdout}");
}
