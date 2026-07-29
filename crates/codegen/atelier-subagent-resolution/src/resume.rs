//! Resume identity validation for fixed built-in Subagent types.
//!
//! Model is not an identity gate on resume: the shell inherits and pins the
//! source model. Personas are not part of the runtime contract.

use crate::types::ResumeSourceData;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResumeValidationError {
    #[error(
        "Cannot resume with subagent_type '{requested}': source subagent was '{source_value}'. \
         Resumed sessions must use the same subagent type as the source."
    )]
    TypeMismatch {
        requested: String,
        source_value: String,
    },
}

pub fn validate_resume_identity(
    requested_type: &str,
    source: &ResumeSourceData,
) -> Result<(), ResumeValidationError> {
    if requested_type != source.subagent_type {
        return Err(ResumeValidationError::TypeMismatch {
            requested: requested_type.to_string(),
            source_value: source.subagent_type.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_source(subagent_type: &str, model_id: Option<&str>) -> ResumeSourceData {
        ResumeSourceData {
            subagent_id: "source-id".into(),
            subagent_type: subagent_type.into(),
            model_id: model_id.map(String::from),
            child_cwd: "/workspace".into(),
            worktree_path: None,
            snapshot_ref: None,
            child_session_id: "child-session".into(),
        }
    }

    #[test]
    fn matching_type_is_valid() {
        let source = make_source("general-purpose", None);
        assert!(validate_resume_identity("general-purpose", &source).is_ok());
    }

    #[test]
    fn source_model_is_not_an_identity_gate() {
        let source = make_source("general-purpose", Some("atelier-3"));
        assert!(validate_resume_identity("general-purpose", &source).is_ok());
    }

    #[test]
    fn type_mismatch_is_rejected() {
        let source = make_source("general-purpose", None);
        let error = validate_resume_identity("explore", &source).unwrap_err();
        assert!(matches!(error, ResumeValidationError::TypeMismatch { .. }));
        assert!(error.to_string().contains("explore"));
        assert!(error.to_string().contains("general-purpose"));
    }
}
