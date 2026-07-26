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

pub fn environment_for_sandbox_child(parent_environment: &[u16]) -> Vec<u16> {
    let mut parent = parse_environment_block(parent_environment);
    let atelier_home = parent
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(ATELIER_HOME_ENV))
        .map(|(_, value)| std::path::PathBuf::from(value));
    parent.retain(|key, _| !is_user_identity_environment_name(key));
    let overrides = parent
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect();
    make_environment_block(&overrides, atelier_home.as_deref())
}

fn parse_environment_block(block: &[u16]) -> BTreeMap<String, String> {
    block
        .split(|value| *value == 0)
        .take_while(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let item = String::from_utf16_lossy(entry);
            let (key, value) = item.split_once('=')?;
            (!key.is_empty()).then(|| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn is_user_identity_environment_name(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "USERPROFILE"
            | "HOME"
            | "HOMEDRIVE"
            | "HOMEPATH"
            | "APPDATA"
            | "LOCALAPPDATA"
            | "TEMP"
            | "TMP"
            | "USERNAME"
            | "USERDOMAIN"
            | "USERDOMAIN_ROAMINGPROFILE"
            | "LOGONSERVER"
    )
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

    values = filter_environment_values(values);

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

fn filter_environment_values(mut values: BTreeMap<String, String>) -> BTreeMap<String, String> {
    values.retain(|key, _| !is_sensitive_environment_name(key));
    values
}

fn is_sensitive_environment_name(name: &str) -> bool {
    let normalized = name.to_ascii_uppercase();
    if matches!(
        normalized.as_str(),
        "AUTHORIZATION"
            | "SSH_AUTH_SOCK"
            | "SSH_AGENT_PID"
            | "GOOGLE_APPLICATION_CREDENTIALS"
            | "AZURE_CLIENT_CERTIFICATE_PATH"
    ) {
        return true;
    }
    if [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
        "CLIENT_SECRET",
        "ACCESS_KEY",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        return true;
    }
    [
        "AWS_",
        "AZURE_",
        "GCP_",
        "GOOGLE_CLOUD_",
        "OPENAI_",
        "ANTHROPIC_",
        "XAI_",
        "GITHUB_",
        "GITLAB_",
        "NPM_",
        "HUGGING_FACE_",
        "HF_",
    ]
    .iter()
    .any(|prefix| normalized.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_child_environment_keeps_parent_toolchain_but_not_parent_identity() {
        let parent = BTreeMap::from([
            ("CARGO_HOME".to_owned(), r"C:\parent-cargo".to_owned()),
            (ATELIER_HOME_ENV.to_owned(), r"C:\parent-atelier".to_owned()),
            ("USERPROFILE".to_owned(), r"C:\Users\parent".to_owned()),
            ("TEMP".to_owned(), r"C:\Users\parent\Temp".to_owned()),
        ]);
        let mut block = Vec::new();
        for (key, value) in parent {
            let mut item = to_wide(format!("{key}={value}"));
            item.pop();
            block.extend(item);
            block.push(0);
        }
        block.push(0);

        let child = parse_environment_block(&environment_for_sandbox_child(&block));
        assert_eq!(
            child.get("CARGO_HOME").map(String::as_str),
            Some(r"C:\parent-cargo")
        );
        assert_eq!(
            child.get(ATELIER_HOME_ENV).map(String::as_str),
            Some(r"C:\parent-atelier")
        );
        assert_ne!(
            child.get("USERPROFILE").map(String::as_str),
            Some(r"C:\Users\parent")
        );
        assert_ne!(
            child.get("TEMP").map(String::as_str),
            Some(r"C:\Users\parent\Temp")
        );
    }

    #[test]
    fn sandbox_environment_drops_common_credentials_but_keeps_toolchain_state() {
        let input = BTreeMap::from([
            ("Path".to_owned(), "C:\\tools".to_owned()),
            ("SystemRoot".to_owned(), "C:\\Windows".to_owned()),
            ("CARGO_HOME".to_owned(), "C:\\cargo".to_owned()),
            ("EXAMPLE_API_KEY".to_owned(), "secret".to_owned()),
            ("AWS_SECRET_ACCESS_KEY".to_owned(), "secret".to_owned()),
            ("GITHUB_TOKEN".to_owned(), "secret".to_owned()),
            ("SSH_AUTH_SOCK".to_owned(), "pipe".to_owned()),
            ("CUSTOM_PASSWORD".to_owned(), "secret".to_owned()),
        ]);

        let filtered = filter_environment_values(input);

        assert_eq!(filtered.get("Path").map(String::as_str), Some("C:\\tools"));
        assert_eq!(
            filtered.get("SystemRoot").map(String::as_str),
            Some("C:\\Windows")
        );
        assert_eq!(
            filtered.get("CARGO_HOME").map(String::as_str),
            Some("C:\\cargo")
        );
        for secret in [
            "EXAMPLE_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "SSH_AUTH_SOCK",
            "CUSTOM_PASSWORD",
        ] {
            assert!(!filtered.contains_key(secret), "leaked {secret}");
        }
    }
}
