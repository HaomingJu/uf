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
    pub debug: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            include_browser: true,
            include_github: true,
            include_gitlab: true,
            include_dockerhub: true,
            github_token: std::env::var("GITHUB_TOKEN").ok(),
            gitlab_token: std::env::var("GITLAB_TOKEN").ok(),
            github_api: std::env::var("GITHUB_API")
                .unwrap_or_else(|_| "https://api.github.com".to_string()),
            gitlab_api: std::env::var("GITLAB_API")
                .unwrap_or_else(|_| "https://gitlab.com/api/v4".to_string()),
            github_user: std::env::var("GITHUB_USER").ok(),
            dockerhub_token: std::env::var("DOCKERHUB_TOKEN").ok(),
            dockerhub_username: std::env::var("DOCKERHUB_USERNAME").ok(),
            history_refresh_interval: refresh_interval_from_env(
                "WEB_FZF_HISTORY_REFRESH",
                Duration::from_secs(5),
            ),
            github_refresh_interval: refresh_interval_from_env(
                "WEB_FZF_GITHUB_REFRESH",
                Duration::from_secs(60),
            ),
            gitlab_refresh_interval: refresh_interval_from_env(
                "WEB_FZF_GITLAB_REFRESH",
                Duration::from_secs(60),
            ),
            dockerhub_refresh_interval: refresh_interval_from_env(
                "WEB_FZF_DOCKERHUB_REFRESH",
                Duration::from_secs(60),
            ),
            debug: false,
        }
    }
}

fn refresh_interval_from_env(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| parse_refresh_interval(&value))
        .unwrap_or(default)
}

fn parse_refresh_interval(value: &str) -> Option<Duration> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }

    let (number, multiplier) = if let Some(number) = value.strip_suffix("minutes") {
        (number, 60)
    } else if let Some(number) = value.strip_suffix("minute") {
        (number, 60)
    } else if let Some(number) = value.strip_suffix("mins") {
        (number, 60)
    } else if let Some(number) = value.strip_suffix("min") {
        (number, 60)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60)
    } else if let Some(number) = value.strip_suffix("seconds") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix("second") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix("secs") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix("sec") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1)
    } else {
        (value.as_str(), 1)
    };

    let seconds = number.trim().parse::<u64>().ok()?.checked_mul(multiplier)?;
    if seconds == 0 {
        return None;
    }
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::parse_refresh_interval;
    use std::time::Duration;

    #[test]
    fn parses_refresh_interval_with_units() {
        assert_eq!(parse_refresh_interval("5s"), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_refresh_interval("1min"),
            Some(Duration::from_secs(60))
        );
        assert_eq!(parse_refresh_interval("90"), Some(Duration::from_secs(90)));
    }

    #[test]
    fn rejects_empty_or_zero_refresh_interval() {
        assert_eq!(parse_refresh_interval(""), None);
        assert_eq!(parse_refresh_interval("0s"), None);
    }
}
