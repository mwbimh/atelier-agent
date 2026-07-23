use std::io::ErrorKind;

use agent_client_protocol as acp;
use atelier_shell::session::ContextSnapshot;
use atelier_shell::session::info::Info;

fn test_info() -> Info {
    Info {
        id: acp::SessionId::new("context-snapshot-path-security-test"),
        cwd: "C:/workspace".to_owned(),
    }
}

#[cfg(windows)]
fn absolute_snapshot_id() -> &'static str {
    r"C:\outside\snapshot"
}

#[cfg(not(windows))]
fn absolute_snapshot_id() -> &'static str {
    "/tmp/outside/snapshot"
}

fn unsafe_snapshot_ids() -> [&'static str; 3] {
    [absolute_snapshot_id(), "../outside", "not-a-uuid"]
}

#[test]
fn get_delete_and_inherit_reject_unsafe_snapshot_ids() {
    let info = test_info();

    for snapshot_id in unsafe_snapshot_ids() {
        let get_error = ContextSnapshot::load(&info, snapshot_id)
            .expect_err("get must reject unsafe snapshot ids before filesystem access");
        assert_eq!(
            get_error.kind(),
            ErrorKind::InvalidInput,
            "get {snapshot_id}"
        );

        let delete_error = ContextSnapshot::delete(&info, snapshot_id)
            .expect_err("delete must reject unsafe snapshot ids before filesystem access");
        assert_eq!(
            delete_error.kind(),
            ErrorKind::InvalidInput,
            "delete {snapshot_id}"
        );

        let inherit_error = ContextSnapshot::load(&info, snapshot_id)
            .expect_err("inherit must reject unsafe snapshot ids before filesystem access");
        assert_eq!(
            inherit_error.kind(),
            ErrorKind::InvalidInput,
            "inherit {snapshot_id}"
        );
    }
}

#[test]
fn valid_snapshot_path_stays_under_context_snapshots() {
    let info = test_info();
    let snapshot_id = uuid::Uuid::now_v7().to_string();
    let directory =
        atelier_shell::session::persistence::session_dir(&info).join("context_snapshots");

    let path = ContextSnapshot::path_for(&info, &snapshot_id).unwrap();

    assert_eq!(path.parent(), Some(directory.as_path()));
}
