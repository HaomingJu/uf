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
            debug: false,
        }
    }
}
