use std::process::Command;

fn temporary_atelier_home() -> std::path::PathBuf {
    let unique = format!(
        "atelier-first-run-test-{}-{}",
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
fn first_run_initializes_without_selecting_a_model() {
    let home = temporary_atelier_home();

    let output = Command::new(env!("CARGO_BIN_EXE_ate"))
        .arg("models")
        .env("ATELIER_HOME", &home)
        .env("ATELIER_SANDBOX", "off")
        .env("ATELIER_SANDBOX_BACKEND", "unsafe")
        .output()
        .expect("run first-use model listing");

    let config = std::fs::read_to_string(home.join("config.toml"))
        .expect("first run should create config.toml");
    let roles = std::fs::read_to_string(home.join("roles.toml"))
        .expect("first run should create roles.toml");
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        output.status.success(),
        "first run should reach model setup without requiring a preselected model; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        config,
        "context = \"default\"\nrequest_agent = \"atelier\"\n"
    );
    assert_eq!(roles, "schema_version = 1\n\n[roles]\n");
    assert!(!config.contains("model ="));
    assert!(!roles.contains("model ="));
}
