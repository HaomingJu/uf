use serde_json::{json, Value as JsonValue};
use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub include_browser: bool,
    pub include_github: bool,
    pub include_gitlab: bool,
    pub include_dockerhub: bool,
    pub github_token: Option<String>,
    pub gitlab_token: Option<String>,
    pub github_api: String,
    pub gitlab_api: String,
    pub github_user: Option<String>,
    pub dockerhub_token: Option<String>,
    pub dockerhub_username: Option<String>,
    pub history_refresh_interval: Duration,
    pub github_refresh_interval: Duration,
    pub gitlab_refresh_interval: Duration,
    pub dockerhub_refresh_interval: Duration,
    pub preview_enabled: bool,
    pub debug: bool,
    pub language: Language,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    English,
    Chinese,
    Japanese,
    Korean,
    French,
    Russian,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::Chinese => "zh",
            Language::Japanese => "ja",
            Language::Korean => "ko",
            Language::French => "fr",
            Language::Russian => "ru",
        }
    }

    pub fn from_code(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Some(Language::English),
            "zh" | "zh-cn" | "zh-hans" | "chinese" => Some(Language::Chinese),
            "ja" | "japanese" => Some(Language::Japanese),
            "ko" | "korean" => Some(Language::Korean),
            "fr" | "french" => Some(Language::French),
            "ru" | "russian" => Some(Language::Russian),
            _ => None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            include_browser: true,
            include_github: true,
            include_gitlab: true,
            include_dockerhub: true,
            github_token: None,
            gitlab_token: None,
            github_api: "https://api.github.com".to_string(),
            gitlab_api: "https://gitlab.com/api/v4".to_string(),
            github_user: None,
            dockerhub_token: None,
            dockerhub_username: None,
            history_refresh_interval: Duration::from_secs(5),
            github_refresh_interval: Duration::from_secs(60),
            gitlab_refresh_interval: Duration::from_secs(60),
            dockerhub_refresh_interval: Duration::from_secs(60),
            preview_enabled: preview_flag_from_env().unwrap_or(false),
            debug: false,
            language: Language::English,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut config = Self::default();
        config.apply_file_overrides();
        config.apply_env_overrides();
        config.normalize_urls();
        config
    }

    pub fn save_to_disk(&self) -> Result<(), String> {
        let path = config_file_path().ok_or_else(|| "config directory unavailable".to_string())?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("create config directory {}: {err}", parent.display()))?;
        }
        let text = format_persisted_config(self);
        fs::write(&path, text).map_err(|err| format!("write config {}: {err}", path.display()))
    }

    fn apply_file_overrides(&mut self) {
        let Some(path) = config_file_path() else {
            return;
        };
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<JsonValue>(&text) else {
            return;
        };
        apply_json_overrides(self, &value);
    }

    fn apply_env_overrides(&mut self) {
        self.github_token = std::env::var("GITHUB_TOKEN")
            .ok()
            .or(self.github_token.clone());
        self.gitlab_token = std::env::var("GITLAB_TOKEN")
            .ok()
            .or(self.gitlab_token.clone());
        self.github_api = std::env::var("GITHUB_API").unwrap_or_else(|_| self.github_api.clone());
        self.gitlab_api = std::env::var("GITLAB_API").unwrap_or_else(|_| self.gitlab_api.clone());
        self.github_user = std::env::var("GITHUB_USER")
            .ok()
            .or(self.github_user.clone());
        self.dockerhub_token = std::env::var("DOCKERHUB_TOKEN")
            .ok()
            .or(self.dockerhub_token.clone());
        self.dockerhub_username = std::env::var("DOCKERHUB_USERNAME")
            .ok()
            .or(self.dockerhub_username.clone());
        self.history_refresh_interval =
            refresh_interval_from_env("WEB_FZF_HISTORY_REFRESH", self.history_refresh_interval);
        self.github_refresh_interval =
            refresh_interval_from_env("WEB_FZF_GITHUB_REFRESH", self.github_refresh_interval);
        self.gitlab_refresh_interval =
            refresh_interval_from_env("WEB_FZF_GITLAB_REFRESH", self.gitlab_refresh_interval);
        self.dockerhub_refresh_interval =
            refresh_interval_from_env("WEB_FZF_DOCKERHUB_REFRESH", self.dockerhub_refresh_interval);
        if let Some(value) = preview_flag_from_env() {
            self.preview_enabled = value;
        }
    }

    fn normalize_urls(&mut self) {
        self.gitlab_api = normalize_gitlab_api_url(&self.gitlab_api);
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    include_browser: Arc<AtomicBool>,
    include_github: Arc<AtomicBool>,
    include_gitlab: Arc<AtomicBool>,
    include_dockerhub: Arc<AtomicBool>,
}

impl RuntimeConfig {
    pub fn new(config: &Config) -> Self {
        Self {
            include_browser: Arc::new(AtomicBool::new(config.include_browser)),
            include_github: Arc::new(AtomicBool::new(config.include_github)),
            include_gitlab: Arc::new(AtomicBool::new(config.include_gitlab)),
            include_dockerhub: Arc::new(AtomicBool::new(config.include_dockerhub)),
        }
    }

    pub fn include_browser(&self) -> bool {
        self.include_browser.load(Ordering::Acquire)
    }

    pub fn include_github(&self) -> bool {
        self.include_github.load(Ordering::Acquire)
    }

    pub fn include_gitlab(&self) -> bool {
        self.include_gitlab.load(Ordering::Acquire)
    }

    pub fn include_dockerhub(&self) -> bool {
        self.include_dockerhub.load(Ordering::Acquire)
    }

    pub fn set_include_browser(&self, value: bool) {
        self.include_browser.store(value, Ordering::Release);
    }

    pub fn set_include_github(&self, value: bool) {
        self.include_github.store(value, Ordering::Release);
    }

    pub fn set_include_gitlab(&self, value: bool) {
        self.include_gitlab.store(value, Ordering::Release);
    }

    pub fn set_include_dockerhub(&self, value: bool) {
        self.include_dockerhub.store(value, Ordering::Release);
    }
}

fn refresh_interval_from_env(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| parse_refresh_interval(&value))
        .unwrap_or(default)
}

pub fn parse_refresh_interval(value: &str) -> Option<Duration> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }

    let number = if let Some(number) = value.strip_suffix("seconds") {
        number
    } else if let Some(number) = value.strip_suffix("second") {
        number
    } else if let Some(number) = value.strip_suffix("secs") {
        number
    } else if let Some(number) = value.strip_suffix("sec") {
        number
    } else if let Some(number) = value.strip_suffix('s') {
        number
    } else {
        value.as_str()
    };

    let seconds = number.trim().parse::<u64>().ok()?;
    if seconds == 0 {
        return None;
    }
    Some(Duration::from_secs(seconds))
}

pub fn normalize_gitlab_api_url(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return "https://gitlab.com/api/v4".to_string();
    }
    if trimmed.ends_with("/api/v4") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/v4")
    }
}

pub fn config_file_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join(".uf").join("config.json"))
}

fn preview_flag_from_env() -> Option<bool> {
    match std::env::var("WEB_FZF_PREVIEW")
        .ok()?
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn format_persisted_config(config: &Config) -> String {
    json!({
        "include_browser": config.include_browser,
        "include_github": config.include_github,
        "include_gitlab": config.include_gitlab,
        "include_dockerhub": config.include_dockerhub,
        "github_token": config.github_token,
        "gitlab_token": config.gitlab_token,
        "github_api": config.github_api,
        "gitlab_api": config.gitlab_api,
        "github_user": config.github_user,
        "dockerhub_token": config.dockerhub_token,
        "dockerhub_username": config.dockerhub_username,
        "history_refresh_interval": config.history_refresh_interval.as_secs(),
        "github_refresh_interval": config.github_refresh_interval.as_secs(),
        "gitlab_refresh_interval": config.gitlab_refresh_interval.as_secs(),
        "dockerhub_refresh_interval": config.dockerhub_refresh_interval.as_secs(),
        "preview_enabled": config.preview_enabled,
        "debug": config.debug,
        "language": config.language.code(),
    })
    .to_string()
}

fn apply_json_overrides(config: &mut Config, value: &JsonValue) {
    if let Some(include_browser) = json_bool(value, "include_browser") {
        config.include_browser = include_browser;
    }
    if let Some(include_github) = json_bool(value, "include_github") {
        config.include_github = include_github;
    }
    if let Some(include_gitlab) = json_bool(value, "include_gitlab") {
        config.include_gitlab = include_gitlab;
    }
    if let Some(include_dockerhub) = json_bool(value, "include_dockerhub") {
        config.include_dockerhub = include_dockerhub;
    }
    if let Some(github_token) = json_optional_string(value, "github_token") {
        config.github_token = github_token;
    }
    if let Some(gitlab_token) = json_optional_string(value, "gitlab_token") {
        config.gitlab_token = gitlab_token;
    }
    if let Some(github_api) = json_string(value, "github_api") {
        config.github_api = github_api;
    }
    if let Some(gitlab_api) = json_string(value, "gitlab_api") {
        config.gitlab_api = gitlab_api;
    }
    if let Some(github_user) = json_optional_string(value, "github_user") {
        config.github_user = github_user;
    }
    if let Some(dockerhub_token) = json_optional_string(value, "dockerhub_token") {
        config.dockerhub_token = dockerhub_token;
    }
    if let Some(dockerhub_username) = json_optional_string(value, "dockerhub_username") {
        config.dockerhub_username = dockerhub_username;
    }
    if let Some(seconds) = json_u64(value, "history_refresh_interval") {
        config.history_refresh_interval = Duration::from_secs(seconds.max(1));
    }
    if let Some(seconds) = json_u64(value, "github_refresh_interval") {
        config.github_refresh_interval = Duration::from_secs(seconds.max(1));
    }
    if let Some(seconds) = json_u64(value, "gitlab_refresh_interval") {
        config.gitlab_refresh_interval = Duration::from_secs(seconds.max(1));
    }
    if let Some(seconds) = json_u64(value, "dockerhub_refresh_interval") {
        config.dockerhub_refresh_interval = Duration::from_secs(seconds.max(1));
    }
    if let Some(preview_enabled) = json_bool(value, "preview_enabled") {
        config.preview_enabled = preview_enabled;
    }
    if let Some(debug) = json_bool(value, "debug") {
        config.debug = debug;
    }
    if let Some(language) = json_string(value, "language").and_then(|value| Language::from_code(&value)) {
        config.language = language;
    }
}

fn json_bool(value: &JsonValue, key: &str) -> Option<bool> {
    value.get(key).and_then(JsonValue::as_bool)
}

fn json_u64(value: &JsonValue, key: &str) -> Option<u64> {
    value.get(key).and_then(JsonValue::as_u64)
}

fn json_string(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn json_optional_string(value: &JsonValue, key: &str) -> Option<Option<String>> {
    match value.get(key) {
        Some(JsonValue::Null) => Some(None),
        Some(JsonValue::String(text)) => Some(Some(text.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_json_overrides, format_persisted_config, normalize_gitlab_api_url,
        parse_refresh_interval, Config, Language,
    };
    use serde_json::Value as JsonValue;
    use std::time::Duration;

    #[test]
    fn parses_refresh_interval_with_units() {
        assert_eq!(parse_refresh_interval("5s"), Some(Duration::from_secs(5)));
        assert_eq!(parse_refresh_interval("90"), Some(Duration::from_secs(90)));
        assert_eq!(
            parse_refresh_interval("60sec"),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn rejects_empty_zero_or_minute_refresh_interval() {
        assert_eq!(parse_refresh_interval(""), None);
        assert_eq!(parse_refresh_interval("0s"), None);
        assert_eq!(parse_refresh_interval("1min"), None);
    }

    #[test]
    fn persisted_config_round_trips() {
        let mut config = Config::default();
        config.include_github = false;
        config.github_user = Some("octocat".to_string());
        config.preview_enabled = true;
        config.github_refresh_interval = Duration::from_secs(90);
        config.language = Language::Japanese;

        let text = format_persisted_config(&config);
        let value: JsonValue = serde_json::from_str(&text).unwrap();
        let mut loaded = Config::default();
        apply_json_overrides(&mut loaded, &value);

        assert!(!loaded.include_github);
        assert_eq!(loaded.github_user.as_deref(), Some("octocat"));
        assert!(loaded.preview_enabled);
        assert_eq!(loaded.github_refresh_interval, Duration::from_secs(90));
        assert_eq!(loaded.language, Language::Japanese);
    }

    #[test]
    fn language_defaults_to_english() {
        let config = Config::default();
        assert_eq!(config.language, Language::English);
    }

    #[test]
    fn invalid_language_keeps_default_english() {
        let value: JsonValue = serde_json::json!({
            "language": "invalid-language"
        });
        let mut loaded = Config::default();
        apply_json_overrides(&mut loaded, &value);
        assert_eq!(loaded.language, Language::English);
    }

    #[test]
    fn normalizes_gitlab_base_and_api_urls() {
        assert_eq!(
            normalize_gitlab_api_url("https://gitlab.example.com"),
            "https://gitlab.example.com/api/v4"
        );
        assert_eq!(
            normalize_gitlab_api_url("https://gitlab.example.com/"),
            "https://gitlab.example.com/api/v4"
        );
        assert_eq!(
            normalize_gitlab_api_url("https://gitlab.example.com/api/v4"),
            "https://gitlab.example.com/api/v4"
        );
        assert_eq!(
            normalize_gitlab_api_url("https://gitlab.example.com/api/v4/"),
            "https://gitlab.example.com/api/v4"
        );
    }
}
