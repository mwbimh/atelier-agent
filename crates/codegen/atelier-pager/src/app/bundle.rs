//! Bundle status state retained for bundled skills only.
//!
//! Runtime Agent harnesses, Subagent types, and Roles are compile-time fixed and
//! are never discovered from bundle metadata.

use serde::Deserialize;

/// Pager-local snapshot of bundled skill availability on disk.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BundleState {
    pub has_cache: bool,
    pub version: String,
    pub skills: Vec<String>,
}

/// Deserialized response from `atelier/bundle/status`.
///
/// Unknown legacy catalog fields are intentionally ignored.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BundleStatusResult {
    pub has_cache: bool,
    /// `None` when `has_cache` is false (shell omits the field).
    pub version: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::BundleStatusResult;

    #[test]
    fn status_deserializes_only_bundled_skill_metadata() {
        let json = r#"{
            "hasCache": true,
            "version": "v2",
            "skills": ["commit", "design"],
            "legacyCatalog": ["ignored"],
            "legacyDetails": [{"name":"ignored"}]
        }"#;

        let result: BundleStatusResult = serde_json::from_str(json).expect("parse");

        assert!(result.has_cache);
        assert_eq!(result.version.as_deref(), Some("v2"));
        assert_eq!(result.skills, vec!["commit", "design"]);
    }

    #[test]
    fn status_allows_missing_version_and_skills() {
        let result: BundleStatusResult =
            serde_json::from_str(r#"{"hasCache":false}"#).expect("parse");

        assert!(!result.has_cache);
        assert!(result.version.is_none());
        assert!(result.skills.is_empty());
    }
}
