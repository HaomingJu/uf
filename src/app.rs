use crate::config::Config;
use crate::sources::load_entries;
use crate::ui::run_ui;
use std::env;

pub fn run() -> Result<(), String> {
    let config = parse_args(env::args().skip(1))?;
    let entries = load_entries(&config);
    run_ui(entries)
}

fn parse_args<I>(args: I) -> Result<Config, String>
where
    I: IntoIterator<Item = String>,
{
    let mut config = Config::default();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--no-browser" => config.include_browser = false,
            "--no-github" => config.include_github = false,
            "--no-gitlab" => config.include_gitlab = false,
            "--github-token" => {
                config.github_token = iter.next();
            }
            "--gitlab-token" => {
                config.gitlab_token = iter.next();
            }
            "--github-api" => {
                config.github_api = iter
                    .next()
                    .ok_or_else(|| "--github-api requires a value".to_string())?;
            }
            "--gitlab-api" => {
                config.gitlab_api = iter
                    .next()
                    .ok_or_else(|| "--gitlab-api requires a value".to_string())?;
            }
            "--github-user" => {
                config.github_user = iter.next();
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(config)
}

fn print_help() {
    println!(
        "web-fzf\n\n\
Usage: web-fzf [options]\n\n\
Options:\n\
  --no-browser\n\
  --no-github\n\
  --no-gitlab\n\
  --github-token <token>\n\
  --gitlab-token <token>\n\
  --github-api <url>\n\
  --gitlab-api <url>\n\
  --github-user <user>\n"
    );
}
