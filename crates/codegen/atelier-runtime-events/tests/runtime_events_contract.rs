use atelier_runtime_events::{Event, EventWriter};

#[test]
fn event_writer_is_local_and_writes_jsonl() {
    let session_dir = tempfile::tempdir().unwrap();
    let writer = EventWriter::open(session_dir.path());

    writer.emit(Event::FirstToken);

    let events = std::fs::read_to_string(session_dir.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"type\":\"first_token\""));
}

#[test]
fn runtime_events_manifest_has_no_file_storage_or_upload_dependencies() {
    let manifest =
        std::fs::read_to_string(format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"))).unwrap();

    for forbidden in [
        "dirs =",
        "sha2 =",
        "reqwest",
        "gcloud",
        "aws",
        "s3",
        "archive",
        "queue",
        "upload",
        "trace_context",
    ] {
        assert!(
            !manifest.to_ascii_lowercase().contains(forbidden),
            "runtime-events dependency surface still contains {forbidden}"
        );
    }
}
