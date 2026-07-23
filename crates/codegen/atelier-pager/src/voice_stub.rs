use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub const AUDIO_SUPPORTED: bool = false;
pub const STT_LANGUAGE_AUTO: &str = "auto";
pub const STT_LANGUAGE_DEFAULT: &str = "en";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SttLanguage {
    pub code: &'static str,
    pub name: &'static str,
}

pub const STT_LANGUAGES: &[SttLanguage] = &[
    SttLanguage {
        code: "ar",
        name: "Arabic",
    },
    SttLanguage {
        code: "cs",
        name: "Czech",
    },
    SttLanguage {
        code: "da",
        name: "Danish",
    },
    SttLanguage {
        code: "nl",
        name: "Dutch",
    },
    SttLanguage {
        code: "en",
        name: "English",
    },
    SttLanguage {
        code: "fil",
        name: "Filipino",
    },
    SttLanguage {
        code: "fr",
        name: "French",
    },
    SttLanguage {
        code: "de",
        name: "German",
    },
    SttLanguage {
        code: "hi",
        name: "Hindi",
    },
    SttLanguage {
        code: "id",
        name: "Indonesian",
    },
    SttLanguage {
        code: "it",
        name: "Italian",
    },
    SttLanguage {
        code: "ja",
        name: "Japanese",
    },
    SttLanguage {
        code: "ko",
        name: "Korean",
    },
    SttLanguage {
        code: "mk",
        name: "Macedonian",
    },
    SttLanguage {
        code: "ms",
        name: "Malay",
    },
    SttLanguage {
        code: "fa",
        name: "Persian",
    },
    SttLanguage {
        code: "pl",
        name: "Polish",
    },
    SttLanguage {
        code: "pt",
        name: "Portuguese",
    },
    SttLanguage {
        code: "ro",
        name: "Romanian",
    },
    SttLanguage {
        code: "ru",
        name: "Russian",
    },
    SttLanguage {
        code: "es",
        name: "Spanish",
    },
    SttLanguage {
        code: "sv",
        name: "Swedish",
    },
    SttLanguage {
        code: "th",
        name: "Thai",
    },
    SttLanguage {
        code: "tr",
        name: "Turkish",
    },
    SttLanguage {
        code: "vi",
        name: "Vietnamese",
    },
];

pub fn stt_language_by_code(code: &str) -> Option<&'static SttLanguage> {
    STT_LANGUAGES.iter().find(|language| language.code == code)
}

pub fn canonicalize_stt_language(value: Option<&str>) -> &'static str {
    let raw = value.unwrap_or_default().trim();
    if raw.eq_ignore_ascii_case(STT_LANGUAGE_AUTO) {
        return STT_LANGUAGE_AUTO;
    }
    let primary = raw.split(['_', '-', '.']).next().unwrap_or_default();
    if primary.eq_ignore_ascii_case("tl") {
        return "fil";
    }
    STT_LANGUAGES
        .iter()
        .find(|language| language.code.eq_ignore_ascii_case(primary))
        .map_or(STT_LANGUAGE_DEFAULT, |language| language.code)
}

pub fn language_for_api(stored: &str) -> &'static str {
    let language = canonicalize_stt_language(Some(stored));
    if language == STT_LANGUAGE_AUTO {
        STT_LANGUAGE_DEFAULT
    } else {
        language
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceConfig {
    pub language: String,
    pub client_identifier: String,
    pub user_agent: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            language: STT_LANGUAGE_DEFAULT.to_owned(),
            client_identifier: String::new(),
            user_agent: String::new(),
        }
    }
}

impl VoiceConfig {
    pub fn from_config_table(_root: &toml::Table) -> Self {
        Self::default()
    }
}

pub trait VoiceAuthProvider: std::fmt::Debug + Send + Sync + 'static {
    fn bearer(&self) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>>;
}

pub type SharedVoiceAuth = Arc<dyn VoiceAuthProvider>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCommand {
    PttPress,
    PttRelease,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEvent {
    InterimTranscript { text: String },
    UtteranceFinal { text: String },
    Error { message: String },
}

pub async fn run_voice_pipeline(
    _config: VoiceConfig,
    _auth: SharedVoiceAuth,
    mut commands: tokio::sync::mpsc::Receiver<VoiceCommand>,
    _events: tokio::sync::mpsc::Sender<VoiceEvent>,
) {
    while let Some(command) = commands.recv().await {
        if command == VoiceCommand::Shutdown {
            break;
        }
    }
}
