//! Prompt construction for conversation history compaction.
//!
//! The developer and user prompts are intentionally identical so the model
//! sees the instructions on both turns.

use anyhow::Result;

/// Builds the developer prompt to send to the compaction model.
pub fn format_compaction_developer_prompt() -> Result<String> {
    Ok(atelier_config::runtime_defaults::runtime_context_prompt(
        atelier_config::runtime_defaults::ContextPrompt::CompactionDeveloper,
        include_str!("../templates/compaction_developer_prompt.txt"),
    ))
}

/// Builds the user prompt to send to the compaction model.
pub fn format_compaction_user_prompt() -> Result<String> {
    Ok(atelier_config::runtime_defaults::runtime_context_prompt(
        atelier_config::runtime_defaults::ContextPrompt::CompactionUser,
        include_str!("../templates/compaction_user_prompt.txt"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_non_empty() {
        let dev = format_compaction_developer_prompt().expect("dev prompt renders");
        assert!(!dev.trim().is_empty(), "developer prompt empty");
        let user = format_compaction_user_prompt().expect("user prompt renders");
        assert!(!user.trim().is_empty(), "user prompt empty");
    }

    /// Belt-and-suspenders: the developer and user prompts are intentionally
    /// identical so the model sees the instructions on both turns. If you edit
    /// one, edit the other — this test catches drift.
    #[test]
    fn compaction_prompts_match() {
        let dev = format_compaction_developer_prompt().expect("dev prompt renders");
        let user = format_compaction_user_prompt().expect("user prompt renders");
        assert_eq!(
            dev, user,
            "compaction_developer_prompt.txt and compaction_user_prompt.txt must stay in sync"
        );
    }
}
