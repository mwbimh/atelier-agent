use crate::winutil::to_wide;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TOOL_ROOT_PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

    let approved_tool_roots = approved_tool_roots();
    replace_path_with_controlled(&mut values, &approved_tool_roots);
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

fn replace_path_with_controlled(
    values: &mut BTreeMap<String, String>,
    approved_tool_roots: &[std::path::PathBuf],
) {
    values.retain(|key, _| !key.eq_ignore_ascii_case("Path"));
    let system_root = values
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("SystemRoot"))
        .map(|(_, value)| std::path::PathBuf::from(value))
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
    let mut roots = vec![
        system_root.join("System32"),
        system_root.clone(),
        system_root.join("System32/Wbem"),
        system_root.join("System32/WindowsPowerShell/v1.0"),
        system_root.join("System32/OpenSSH"),
    ];
    for root in approved_tool_roots {
        if !roots.iter().any(|existing| existing == root) {
            roots.push(root.clone());
        }
    }
    values.insert(
        "Path".to_owned(),
        std::env::join_paths(roots)
            .unwrap_or_else(|_| std::ffi::OsString::from(r"C:\Windows\System32"))
            .to_string_lossy()
            .into_owned(),
    );
}

fn approved_tool_roots() -> Vec<std::path::PathBuf> {
    #[derive(serde::Deserialize)]
    struct Registry {
        schema_version: u32,
        #[serde(default)]
        roots: Vec<ToolRoot>,
    }
    #[derive(serde::Deserialize)]
    struct ToolRoot {
        path: String,
        #[serde(default = "enabled")]
        enabled: bool,
    }
    fn enabled() -> bool {
        true
    }

    let Some(program_data) = std::env::var_os("ProgramData") else {
        return Vec::new();
    };
    let path = std::path::PathBuf::from(program_data).join("Atelier/tools/registry.json");
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(registry) = serde_json::from_str::<Registry>(&source) else {
        tracing::warn!("ignoring invalid Atelier Toolchain Registry");
        return Vec::new();
    };
    if registry.schema_version != 1 {
        tracing::warn!("ignoring unsupported Atelier Toolchain Registry schema");
        return Vec::new();
    }
    registry
        .roots
        .into_iter()
        .filter(|root| root.enabled)
        .map(|root| std::path::PathBuf::from(root.path))
        .filter(|root| approved_executable_root(root))
        .collect()
}

fn approved_executable_root(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    if !path.is_absolute()
        || path
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("\\windowsapps\\")
    {
        return false;
    }
    let Ok(canonical) = dunce::canonicalize(path) else {
        return false;
    };
    if !approved_tool_location(&canonical) || sandbox_identity_can_create_in(path) {
        return false;
    }
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    })
}

fn approved_tool_location(path: &Path) -> bool {
    let mut prefixes = ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>();
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        prefixes.push(std::path::PathBuf::from(system_root).join("System32"));
    }
    if let Some(program_data) = std::env::var_os("ProgramData") {
        let atelier = std::path::PathBuf::from(program_data).join("Atelier");
        prefixes.push(atelier.join("tools"));
        prefixes.push(atelier.join("runtimes/powershell"));
    }
    prefixes
        .iter()
        .any(|prefix| path_is_within_case_insensitive(path, prefix))
}

fn path_is_within_case_insensitive(path: &Path, prefix: &Path) -> bool {
    let path = path.to_string_lossy().replace('/', "\\");
    let prefix = prefix
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_owned();
    path.eq_ignore_ascii_case(&prefix)
        || path
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
            && path.as_bytes().get(prefix.len()) == Some(&b'\\')
}

fn sandbox_identity_can_create_in(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    let sequence = TOOL_ROOT_PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let probe = path.join(format!(
        ".atelier-tool-root-probe-{}-{sequence}.tmp",
        std::process::id()
    ));
    let opened = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(&probe);
    match opened {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
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

    #[test]
    fn registry_rejects_an_ordinary_but_user_writable_root() {
        let root = tempfile::tempdir().unwrap();
        assert!(root.path().is_absolute());
        assert!(!approved_executable_root(root.path()));
    }

    #[test]
    fn controlled_path_never_inherits_the_parent_path() {
        let mut values = BTreeMap::from([
            (
                "Path".to_owned(),
                r"C:\Users\parent\bin;C:\unapproved".to_owned(),
            ),
            ("SystemRoot".to_owned(), r"C:\Windows".to_owned()),
        ]);
        replace_path_with_controlled(&mut values, &[]);
        let path = values.get("Path").unwrap();
        assert!(!path.contains(r"C:\Users\parent\bin"));
        assert!(!path.contains(r"C:\unapproved"));
        assert!(path.contains(r"C:\Windows\System32"));
    }
}
