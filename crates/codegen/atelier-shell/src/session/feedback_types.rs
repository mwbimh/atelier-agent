use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientType {
    #[default]
    Agent,
    Tui,
    Web,
    Extension,
    Nebula,
    Desktop,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackType {
    #[default]
    Rating,
    Text,
    RatingWithText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RatingType {
    Thumbs,
    Stars,
    Nps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextType {
    Message,
    Session,
    Feature,
    ToolUse,
    General,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackMode {
    Thumbs,
    Stars,
    Text,
    ThumbsText,
    StarsText,
    Comparison,
    Survey,
    Nps,
    NpsText,
}

pub fn parse_feedback_mode_str(value: &str) -> FeedbackMode {
    match value.trim().to_ascii_lowercase().as_str() {
        "stars" => FeedbackMode::Stars,
        "text" => FeedbackMode::Text,
        "thumbs_text" | "thumbs-text" => FeedbackMode::ThumbsText,
        "stars_text" | "stars-text" => FeedbackMode::StarsText,
        "comparison" => FeedbackMode::Comparison,
        "survey" => FeedbackMode::Survey,
        "nps" => FeedbackMode::Nps,
        "nps_text" | "nps-text" => FeedbackMode::NpsText,
        _ => FeedbackMode::Thumbs,
    }
}

#[derive(Debug, Clone)]
pub enum FeedbackContent {
    Rating {
        rating_type: RatingType,
        rating_value: i32,
    },
    Text(String),
    RatingWithText {
        rating_type: RatingType,
        rating_value: i32,
        text: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackSubmission {
    pub session_id: String,
    pub user_id: Option<String>,
    pub client_type: ClientType,
    pub feedback_type: FeedbackType,
    pub turn_number: Option<i64>,
    pub rating_type: Option<RatingType>,
    pub rating_value: Option<i32>,
    pub feedback_text: Option<String>,
    #[serde(default)]
    pub feedback_categories: Vec<String>,
    pub message_id: Option<String>,
    pub model_id: Option<String>,
    pub resolved_model_id: Option<String>,
    pub model_fingerprint: Option<String>,
    pub context_type: Option<ContextType>,
    pub feature_name: Option<String>,
    pub tool_name: Option<String>,
    pub request_id: Option<String>,
    pub client_version: Option<String>,
    pub shell_version: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub last_user_message: Option<String>,
    pub last_assistant_message: Option<String>,
    #[serde(default)]
    pub tool_outcomes: Vec<FeedbackToolOutcome>,
    pub session_cwd: Option<String>,
    pub compaction_count: Option<i64>,
    pub context_window_usage: Option<u8>,
    pub context_tokens_used: Option<u64>,
    pub context_window_tokens: Option<u64>,
    pub terminal_info: Option<FeedbackTerminalInfo>,
    pub unified_log_url: Option<String>,
}

impl FeedbackSubmission {
    pub fn with_content(
        session_id: String,
        client_type: ClientType,
        content: FeedbackContent,
    ) -> Self {
        let mut submission = Self {
            session_id,
            client_type,
            ..Self::default()
        };
        match content {
            FeedbackContent::Rating {
                rating_type,
                rating_value,
            } => {
                submission.feedback_type = FeedbackType::Rating;
                submission.rating_type = Some(rating_type);
                submission.rating_value = Some(rating_value);
            }
            FeedbackContent::Text(text) => {
                submission.feedback_type = FeedbackType::Text;
                submission.feedback_text = Some(text);
            }
            FeedbackContent::RatingWithText {
                rating_type,
                rating_value,
                text,
            } => {
                submission.feedback_type = FeedbackType::RatingWithText;
                submission.rating_type = Some(rating_type);
                submission.rating_value = Some(rating_value);
                submission.feedback_text = Some(text);
            }
        }
        submission
    }

    pub fn merge_metadata(&mut self, extra: serde_json::Value) {
        match (&mut self.metadata, extra) {
            (Some(serde_json::Value::Object(existing)), serde_json::Value::Object(extra)) => {
                existing.extend(extra);
            }
            (slot, value) => *slot = Some(value),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackTerminalInfo {
    pub brand: String,
    pub multiplexer: String,
    pub is_ssh: bool,
    pub is_byobu: bool,
    pub term_var: String,
    pub tmux_version: Option<String>,
    pub hyperlink_osc8_support: Option<String>,
    pub clipboard_route: Option<String>,
    pub clipboard_native_tool: Option<String>,
    pub display_server: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackToolOutcome {
    pub tool_name: String,
    pub calls: u32,
    pub failures: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FeedbackHeuristicsConfig {
    pub enabled: bool,
    pub cooldown_seconds: i64,
    pub max_requests_per_session: i64,
    pub tier1_enabled: bool,
    pub tier1_sample_rate: f64,
    pub tier1_min_turns: i64,
    pub tier1_min_tool_calls: i64,
    pub tier1_min_compactions: i64,
    pub tier1_no_cancellations: bool,
    pub tier1_feedback_mode: String,
    pub tier1_dismissible: bool,
    pub tier1_prompt: String,
    pub tier1_max_triggers: i64,
    pub tier2_enabled: bool,
    pub tier2_sample_rate: f64,
    pub tier2_min_turns: i64,
    pub tier2_min_tool_calls: i64,
    pub tier2_min_compactions: i64,
    pub tier2_min_errors: i64,
    pub tier2_feedback_mode: String,
    pub tier2_dismissible: bool,
    pub tier2_prompt: String,
    pub tier2_max_triggers: i64,
    pub tier3_enabled: bool,
    pub tier3_sample_rate: f64,
    pub tier3_min_turns: i64,
    pub tier3_requires_cancellation: bool,
    pub tier3_requires_revert: bool,
    pub tier3_requires_recovery: bool,
    pub tier3_feedback_mode: String,
    pub tier3_dismissible: bool,
    pub tier3_prompt: String,
    pub tier3_max_triggers: i64,
    pub target_user_cohorts: Vec<String>,
    pub priority: i64,
}

impl Default for FeedbackHeuristicsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cooldown_seconds: 300,
            max_requests_per_session: 3,
            tier1_enabled: true,
            tier1_sample_rate: 0.0005,
            tier1_min_turns: 10,
            tier1_min_tool_calls: 5,
            tier1_min_compactions: 2,
            tier1_no_cancellations: true,
            tier1_feedback_mode: "thumbs".into(),
            tier1_dismissible: true,
            tier1_prompt: "How is Atelier doing?".into(),
            tier1_max_triggers: 1,
            tier2_enabled: true,
            tier2_sample_rate: 0.0002,
            tier2_min_turns: 15,
            tier2_min_tool_calls: 10,
            tier2_min_compactions: 3,
            tier2_min_errors: 1,
            tier2_feedback_mode: "thumbs_text".into(),
            tier2_dismissible: true,
            tier2_prompt: "How was this session?".into(),
            tier2_max_triggers: 1,
            tier3_enabled: true,
            tier3_sample_rate: 0.0001,
            tier3_min_turns: 20,
            tier3_requires_cancellation: false,
            tier3_requires_revert: false,
            tier3_requires_recovery: true,
            tier3_feedback_mode: "stars_text".into(),
            tier3_dismissible: true,
            tier3_prompt: "Please rate this session.".into(),
            tier3_max_triggers: 1,
            target_user_cohorts: vec!["all".into()],
            priority: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TierConfig;
