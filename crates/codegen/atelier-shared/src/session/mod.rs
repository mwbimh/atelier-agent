use std::path::PathBuf;

pub mod info;

pub use info::Info;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackTerminalInfo {
    pub brand: String,
    pub multiplexer: String,
    pub is_ssh: bool,
    pub is_byobu: bool,
    pub term_var: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyperlink_osc8_support: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_native_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_server: Option<String>,
}

pub fn session_dir(info: &Info) -> PathBuf {
    atelier_tools::util::atelier_home::sessions_cwd_dir(&info.cwd).join(info.id.to_string())
}
