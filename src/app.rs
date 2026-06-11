use crate::config::Config;
use crate::sources::{
    fetch_github_page, fetch_gitlab_page, load_cached_remote_entries, load_local_browser_entries,
    remote_cache_needs_refresh, save_remote_cache,
};
use crate::ui::{run_ui, UiEvent};
use std::env;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

pub fn run() -> Result<(), String> {
    let config = parse_args(env::args().skip(1))?;
    let mut entries = Vec::new();

    if config.include_browser {
        match load_local_browser_entries() {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => eprintln!("browser sources: {err}"),
        }
    }

    if config.include_github {
        entries.extend(load_cached_remote_entries("github"));
    }
    if config.include_gitlab {
        entries.extend(load_cached_remote_entries("gitlab"));
    }

    let (tx, rx) = mpsc::channel::<UiEvent>();

    if config.include_github && remote_cache_needs_refresh("github") {
        spawn_github_refresh(config.clone(), tx.clone());
    }
    if config.include_gitlab && remote_cache_needs_refresh("gitlab") {
        spawn_gitlab_refresh(config.clone(), tx.clone());
    }

    drop(tx);
    run_ui(entries, rx)
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

fn spawn_github_refresh(config: Config, tx: mpsc::Sender<UiEvent>) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(800));
        let _ = tx.send(UiEvent::Status(
            "Refreshing GitHub in background...".to_string(),
        ));
        let mut page = 1usize;
        let mut all_rows = Vec::new();
        loop {
            match fetch_github_page(&config, page, config.github_token.as_deref()) {
                Ok(rows) => {
                    if rows.is_empty() {
                        break;
                    }
                    let row_count = rows.len();
                    all_rows.extend(rows.clone());
                    let _ = tx.send(UiEvent::AddEntries(rows));
                    let _ = tx.send(UiEvent::Status(format!(
                        "GitHub refreshing... page {page} loaded ({row_count} items)."
                    )));
                    if row_count < 100 || page >= 20 {
                        break;
                    }
                    page += 1;
                }
                Err(err) => {
                    let _ = tx.send(UiEvent::Status(format!("GitHub refresh failed: {err}")));
                    return;
                }
            }
        }
        let _ = save_remote_cache("github", &all_rows);
        let _ = tx.send(UiEvent::Status("GitHub refresh complete.".to_string()));
    });
}

fn spawn_gitlab_refresh(config: Config, tx: mpsc::Sender<UiEvent>) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(800));
        let _ = tx.send(UiEvent::Status(
            "Refreshing GitLab in background...".to_string(),
        ));
        let mut page = 1usize;
        let mut all_rows = Vec::new();
        loop {
            match fetch_gitlab_page(&config, page) {
                Ok(rows) => {
                    if rows.is_empty() {
                        break;
                    }
                    let row_count = rows.len();
                    all_rows.extend(rows.clone());
                    let _ = tx.send(UiEvent::AddEntries(rows));
                    let _ = tx.send(UiEvent::Status(format!(
                        "GitLab refreshing... page {page} loaded ({row_count} items)."
                    )));
                    if row_count < 100 || page >= 20 {
                        break;
                    }
                    page += 1;
                }
                Err(err) => {
                    let _ = tx.send(UiEvent::Status(format!("GitLab refresh failed: {err}")));
                    return;
                }
            }
        }
        let _ = save_remote_cache("gitlab", &all_rows);
        let _ = tx.send(UiEvent::Status("GitLab refresh complete.".to_string()));
    });
}
