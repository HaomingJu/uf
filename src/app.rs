use crate::config::{Config, RuntimeConfig};
use crate::sources::{
    browser_source_signature, fetch_dockerhub_page, fetch_github_page, fetch_gitlab_page,
    load_cached_remote_with_refresh_check, load_local_browser_entries, save_remote_cache,
    with_source_logs_suppressed,
};
use crate::ui::{run_ui, RefreshRequest, UiEvent};
use std::env;
use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicBool, Ordering as AtomicOrdering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

pub fn run() -> Result<(), String> {
    let config = parse_args(env::args().skip(1))?;
    let runtime_config = RuntimeConfig::new(&config);
    let shared_config = Arc::new(Mutex::new(config.clone()));

    // 并行加载：浏览器历史 + 三个远程缓存同时读取，同时判断是否需要刷新
    let include_browser = config.include_browser;
    let include_github = config.include_github;
    let include_gitlab = config.include_gitlab;
    let include_dockerhub = config.include_dockerhub;
    let github_interval = config.github_refresh_interval;
    let gitlab_interval = config.gitlab_refresh_interval;
    let dockerhub_interval = config.dockerhub_refresh_interval;

    let browser_handle = thread::spawn(move || {
        if include_browser {
            match load_local_browser_entries() {
                Ok(rows) => rows,
                Err(err) => {
                    eprintln!("browser sources: {err}");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        }
    });
    let github_handle = thread::spawn(move || {
        if include_github {
            load_cached_remote_with_refresh_check("github", github_interval)
        } else {
            (Vec::new(), false)
        }
    });
    let gitlab_handle = thread::spawn(move || {
        if include_gitlab {
            load_cached_remote_with_refresh_check("gitlab", gitlab_interval)
        } else {
            (Vec::new(), false)
        }
    });
    let dockerhub_handle = thread::spawn(move || {
        if include_dockerhub {
            load_cached_remote_with_refresh_check("dockerhub", dockerhub_interval)
        } else {
            (Vec::new(), false)
        }
    });

    let browser_entries = browser_handle.join().unwrap_or_default();
    let (github_entries, github_needs_refresh) = github_handle.join().unwrap_or_default();
    let (gitlab_entries, gitlab_needs_refresh) = gitlab_handle.join().unwrap_or_default();
    let (dockerhub_entries, dockerhub_needs_refresh) = dockerhub_handle.join().unwrap_or_default();

    let mut entries = Vec::with_capacity(
        browser_entries.len()
            + github_entries.len()
            + gitlab_entries.len()
            + dockerhub_entries.len(),
    );
    entries.extend(browser_entries);
    entries.extend(github_entries);
    entries.extend(gitlab_entries);
    entries.extend(dockerhub_entries);

    let (tx, rx) = mpsc::channel::<UiEvent>();
    let (refresh_tx, refresh_rx) = mpsc::channel::<RefreshRequest>();
    let refresh_state = RefreshState::default();

    if github_needs_refresh {
        try_spawn_github_refresh(
            config.clone(),
            runtime_config.clone(),
            tx.clone(),
            refresh_state.github.clone(),
            Duration::from_millis(800),
        );
    }
    if gitlab_needs_refresh {
        try_spawn_gitlab_refresh(
            config.clone(),
            runtime_config.clone(),
            tx.clone(),
            refresh_state.gitlab.clone(),
            Duration::from_millis(800),
        );
    }
    if dockerhub_needs_refresh {
        try_spawn_dockerhub_refresh(
            config.clone(),
            runtime_config.clone(),
            tx.clone(),
            refresh_state.dockerhub.clone(),
            Duration::from_millis(800),
        );
    }
    if config.include_browser {
        spawn_browser_refresh(
            config.history_refresh_interval,
            runtime_config.clone(),
            tx.clone(),
            refresh_state.history.clone(),
        );
    }
    spawn_refresh_request_handler(
        config.clone(),
        shared_config.clone(),
        runtime_config.clone(),
        tx.clone(),
        refresh_state,
        refresh_rx,
    );

    drop(tx);
    run_ui(
        entries,
        config,
        runtime_config,
        shared_config,
        rx,
        refresh_tx,
    )
}

#[derive(Clone, Default)]
struct RefreshState {
    history: Arc<AtomicBool>,
    github: Arc<AtomicBool>,
    gitlab: Arc<AtomicBool>,
    dockerhub: Arc<AtomicBool>,
}

struct RefreshGuard(Arc<AtomicBool>);

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        self.0.store(false, AtomicOrdering::Release);
    }
}

fn parse_args<I>(args: I) -> Result<Config, String>
where
    I: IntoIterator<Item = String>,
{
    let mut config = Config::load();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--no-browser" => config.include_browser = false,
            "--no-github" => config.include_github = false,
            "--no-gitlab" => config.include_gitlab = false,
            "--no-dockerhub" => config.include_dockerhub = false,
            "--debug" => config.debug = true,
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
            "--dockerhub-token" => {
                config.dockerhub_token = iter.next();
            }
            "--dockerhub-user" => {
                config.dockerhub_username = iter.next();
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
        "uf\n\n\
Usage: uf [options]\n\n\
Options:\n\
  --no-browser\n\
  --no-github\n\
  --no-gitlab\n\
  --no-dockerhub\n\
  --github-token <token>\n\
  --gitlab-token <token>\n\
  --github-api <url>\n\
  --gitlab-api <url>\n\
  --github-user <user>\n\
  --dockerhub-token <token>\n\
  --dockerhub-user <user>\n\
  --debug               write diagnostic logs to stderr\n"
    );
}

fn spawn_refresh_request_handler(
    config: Config,
    shared_config: Arc<Mutex<Config>>,
    runtime_config: RuntimeConfig,
    tx: mpsc::Sender<UiEvent>,
    state: RefreshState,
    rx: mpsc::Receiver<RefreshRequest>,
) {
    thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            match request {
                RefreshRequest::History if runtime_config.include_browser() => {
                    try_spawn_history_refresh(tx.clone(), state.history.clone());
                }
                RefreshRequest::GitHub if runtime_config.include_github() => {
                    let current = shared_config
                        .lock()
                        .map(|config| config.clone())
                        .unwrap_or_else(|_| config.clone());
                    try_spawn_github_refresh(
                        current,
                        runtime_config.clone(),
                        tx.clone(),
                        state.github.clone(),
                        Duration::ZERO,
                    );
                }
                RefreshRequest::GitLab if runtime_config.include_gitlab() => {
                    let current = shared_config
                        .lock()
                        .map(|config| config.clone())
                        .unwrap_or_else(|_| config.clone());
                    try_spawn_gitlab_refresh(
                        current,
                        runtime_config.clone(),
                        tx.clone(),
                        state.gitlab.clone(),
                        Duration::ZERO,
                    );
                }
                RefreshRequest::DockerHub if runtime_config.include_dockerhub() => {
                    let current = shared_config
                        .lock()
                        .map(|config| config.clone())
                        .unwrap_or_else(|_| config.clone());
                    try_spawn_dockerhub_refresh(
                        current,
                        runtime_config.clone(),
                        tx.clone(),
                        state.dockerhub.clone(),
                        Duration::ZERO,
                    );
                }
                RefreshRequest::History => {
                    let _ = tx.send(UiEvent::Status("History source is disabled.".to_string()));
                }
                RefreshRequest::GitHub => {
                    let _ = tx.send(UiEvent::Status("GitHub source is disabled.".to_string()));
                }
                RefreshRequest::GitLab => {
                    let _ = tx.send(UiEvent::Status("GitLab source is disabled.".to_string()));
                }
                RefreshRequest::DockerHub => {
                    let _ = tx.send(UiEvent::Status("DockerHub source is disabled.".to_string()));
                }
            }
        }
    });
}

fn try_mark_refreshing(source: &str, in_progress: &AtomicBool, tx: &mpsc::Sender<UiEvent>) -> bool {
    if in_progress
        .compare_exchange(false, true, AtomicOrdering::AcqRel, AtomicOrdering::Acquire)
        .is_ok()
    {
        true
    } else {
        let _ = tx.send(UiEvent::Status(format!("{source} is already refreshing.")));
        false
    }
}

fn try_spawn_github_refresh(
    config: Config,
    runtime_config: RuntimeConfig,
    tx: mpsc::Sender<UiEvent>,
    in_progress: Arc<AtomicBool>,
    delay: Duration,
) {
    if !try_mark_refreshing("GitHub", &in_progress, &tx) {
        return;
    }
    thread::spawn(move || {
        let _guard = RefreshGuard(in_progress);
        thread::sleep(delay);
        if !runtime_config.include_github() {
            return;
        }
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
                    all_rows.extend(rows);
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
        if all_rows.is_empty() {
            let _ = tx.send(UiEvent::Status(
                "GitHub refresh returned no entries; keeping existing data.".to_string(),
            ));
            return;
        }
        let count = all_rows.len();
        let _ = save_remote_cache("github", &all_rows);
        let _ = tx.send(UiEvent::ReplaceSourceEntries {
            sources: vec!["github".to_string()],
            entries: all_rows,
        });
        let _ = tx.send(UiEvent::Status(format!(
            "GitHub refresh complete ({count} items)."
        )));
    });
}

fn try_spawn_gitlab_refresh(
    config: Config,
    runtime_config: RuntimeConfig,
    tx: mpsc::Sender<UiEvent>,
    in_progress: Arc<AtomicBool>,
    delay: Duration,
) {
    if !try_mark_refreshing("GitLab", &in_progress, &tx) {
        return;
    }
    thread::spawn(move || {
        let _guard = RefreshGuard(in_progress);
        thread::sleep(delay);
        if !runtime_config.include_gitlab() {
            return;
        }
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
                    all_rows.extend(rows);
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
        if all_rows.is_empty() {
            let _ = tx.send(UiEvent::Status(
                "GitLab refresh returned no entries; keeping existing data.".to_string(),
            ));
            return;
        }
        let count = all_rows.len();
        let _ = save_remote_cache("gitlab", &all_rows);
        let _ = tx.send(UiEvent::ReplaceSourceEntries {
            sources: vec!["gitlab".to_string()],
            entries: all_rows,
        });
        let _ = tx.send(UiEvent::Status(format!(
            "GitLab refresh complete ({count} items)."
        )));
    });
}

fn try_spawn_dockerhub_refresh(
    config: Config,
    runtime_config: RuntimeConfig,
    tx: mpsc::Sender<UiEvent>,
    in_progress: Arc<AtomicBool>,
    delay: Duration,
) {
    if !try_mark_refreshing("DockerHub", &in_progress, &tx) {
        return;
    }
    thread::spawn(move || {
        let _guard = RefreshGuard(in_progress);
        thread::sleep(delay);
        if !runtime_config.include_dockerhub() {
            return;
        }
        let _ = tx.send(UiEvent::Status(
            "Refreshing DockerHub in background...".to_string(),
        ));
        let mut page = 1usize;
        let mut all_rows = Vec::new();
        loop {
            match fetch_dockerhub_page(&config, page) {
                Ok(rows) => {
                    if rows.is_empty() {
                        break;
                    }
                    let row_count = rows.len();
                    all_rows.extend(rows);
                    let _ = tx.send(UiEvent::Status(format!(
                        "DockerHub refreshing... page {page} loaded ({row_count} items)."
                    )));
                    if row_count < 100 || page >= 20 {
                        break;
                    }
                    page += 1;
                }
                Err(err) => {
                    let _ = tx.send(UiEvent::Status(format!("DockerHub refresh failed: {err}")));
                    return;
                }
            }
        }
        if all_rows.is_empty() {
            let _ = tx.send(UiEvent::Status(
                "DockerHub refresh returned no entries; keeping existing data.".to_string(),
            ));
            return;
        }
        let count = all_rows.len();
        let _ = save_remote_cache("dockerhub", &all_rows);
        let _ = tx.send(UiEvent::ReplaceSourceEntries {
            sources: vec!["public".to_string(), "private".to_string()],
            entries: all_rows,
        });
        let _ = tx.send(UiEvent::Status(format!(
            "DockerHub refresh complete ({count} items)."
        )));
    });
}

fn spawn_browser_refresh(
    interval: Duration,
    runtime_config: RuntimeConfig,
    tx: mpsc::Sender<UiEvent>,
    in_progress: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut last_signature = browser_source_signature();
        loop {
            thread::sleep(interval);
            if !runtime_config.include_browser() {
                continue;
            }
            let Some(signature) = browser_source_signature() else {
                continue;
            };
            if Some(signature) == last_signature {
                continue;
            }
            last_signature = Some(signature);

            try_spawn_history_refresh(tx.clone(), in_progress.clone());
        }
    });
}

fn try_spawn_history_refresh(tx: mpsc::Sender<UiEvent>, in_progress: Arc<AtomicBool>) {
    if !try_mark_refreshing("History", &in_progress, &tx) {
        return;
    }
    thread::spawn(move || {
        let _guard = RefreshGuard(in_progress);
        let _ = tx.send(UiEvent::Status(
            "Refreshing browser history in background...".to_string(),
        ));
        match with_source_logs_suppressed(load_local_browser_entries) {
            Ok(rows) => {
                if rows.is_empty() {
                    let _ = tx.send(UiEvent::Status(
                        "Browser history refresh returned no entries; keeping existing data."
                            .to_string(),
                    ));
                    return;
                }
                let sources = vec![
                    "history".to_string(),
                    "browser-history".to_string(),
                    "bookmark".to_string(),
                ];
                let count = rows.len();
                let _ = tx.send(UiEvent::ReplaceSourceEntries {
                    sources,
                    entries: rows,
                });
                let _ = tx.send(UiEvent::Status(format!(
                    "Browser history refreshed ({count} items)."
                )));
            }
            Err(err) => {
                let _ = tx.send(UiEvent::Status(format!(
                    "Browser history refresh failed: {err}"
                )));
            }
        }
    });
}
