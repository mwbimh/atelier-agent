use crate::winutil::to_wide;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

pub const ATELIER_HOME_ENV: &str = "ATELIER_HOME";

pub fn default_atelier_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .map(|home| home.join(".atelier"))
}

pub fn make_environment_block(
    overrides: &BTreeMap<OsString, OsString>,
    atelier_home: Option<&Path>,
) -> Vec<u16> {
    let mut values = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (key, value) in overrides {
        values.insert(
            key.to_string_lossy().into_owned(),
            value.to_string_lossy().into_owned(),
        );
    }
    let home = atelier_home
        .map(Path::to_path_buf)
        .or_else(default_atelier_home);
    if let Some(home) = home.as_deref() {
        values.insert(
            ATELIER_HOME_ENV.to_owned(),
            home.to_string_lossy().into_owned(),
        );
    }

    // The first-stage crate has no telemetry sink. Remove caller-provided OTEL
    // endpoints and force standard exporters to the documented no-op value.
    values.retain(|key, _| !key.to_ascii_uppercase().starts_with("OTEL_EXPORTER_"));
    values.insert("OTEL_TRACES_EXPORTER".to_owned(), "none".to_owned());
    values.insert("OTEL_METRICS_EXPORTER".to_owned(), "none".to_owned());
    values.insert("OTEL_LOGS_EXPORTER".to_owned(), "none".to_owned());

    let mut block = Vec::new();
    for (key, value) in values {
        let mut item = to_wide(format!("{key}={value}"));
        item.pop();
        block.extend(item);
        block.push(0);
    }
    block.push(0);
    block
}
