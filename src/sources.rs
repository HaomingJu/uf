use crate::config::Config;
use crate::models::Entry;
use plist::Value as PlistValue;
use serde_json::Value as JsonValue;
use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

thread_local! {
    static SUPPRESS_SOURCE_LOGS: Cell<bool> = const { Cell::new(false) };
}

const DOCKERHUB_TAGS_MARKER: &str = "\nWEB_FZF_TAGS\t";
const REPO_STARS_MARKER: &str = "\nWEB_FZF_STARS\t";

struct CachedEntries {
    saved_at: u64,
    entries: Vec<Entry>,
}

pub fn load_entries(config: &Config) -> Vec<Entry> {
    let mut entries = Vec::new();

    if config.include_browser {
        match load_local_browser_entries() {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => source_log(format!("browser sources: {err}")),
        }
    }
    if config.include_github {
        entries.extend(load_cached_remote_entries("github"));
        match load_github_entries(config) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => source_log(format!("github sources: {err}")),
        }
    }
    if config.include_gitlab {
        entries.extend(load_cached_remote_entries("gitlab"));
        match load_gitlab_entries(config) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => source_log(format!("gitlab sources: {err}")),
        }
    }

    deduplicate(entries)
}

pub fn load_local_browser_entries() -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    match load_chromium_family() {
        Ok(mut rows) => entries.append(&mut rows),
        Err(err) => source_log(format!("chromium browser sources: {err}")),
    }
    match load_firefox_family() {
        Ok(mut rows) => entries.append(&mut rows),
        Err(err) => source_log(format!("firefox browser sources: {err}")),
    }
    match load_safari() {
        Ok(mut rows) => entries.append(&mut rows),
        Err(err) => source_log(format!("safari browser sources: {err}")),
    }
    Ok(deduplicate(entries))
}

pub fn with_source_logs_suppressed<F, T>(f: F) -> T
where
    F: FnOnce() -> T,
{
    SUPPRESS_SOURCE_LOGS.with(|flag| {
        let previous = flag.replace(true);
        let result = f();
        flag.set(previous);
        result
    })
}

pub fn browser_source_signature() -> Option<u64> {
    let mut hasher = DefaultHasher::new();
    let mut seen = false;

    for path in chromium_history_paths()
        .into_iter()
        .chain(chromium_bookmark_paths())
        .chain(firefox_place_paths())
        .chain(safari_paths())
    {
        if let Ok(meta) = fs::metadata(&path) {
            seen = true;
            path.to_string_lossy().hash(&mut hasher);
            meta.len().hash(&mut hasher);
            if let Ok(modified) = meta.modified() {
                if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
                    duration.as_secs().hash(&mut hasher);
                    duration.subsec_nanos().hash(&mut hasher);
                }
            }
        }
    }

    if seen {
        Some(hasher.finish())
    } else {
        None
    }
}

pub fn load_cached_remote_entries(source: &str) -> Vec<Entry> {
    load_cached_remote(source, None).0
}

pub fn remote_cache_needs_refresh(source: &str, refresh_interval: Duration) -> bool {
    let (entries, needs_refresh) = load_cached_remote(source, Some(refresh_interval));
    entries.is_empty() || needs_refresh
}

// 一次读取同时返回 entries 和是否需要刷新，避免两次读文件
pub fn load_cached_remote_with_refresh_check(
    source: &str,
    refresh_interval: Duration,
) -> (Vec<Entry>, bool) {
    let (entries, needs_refresh) = load_cached_remote(source, Some(refresh_interval));
    let needs_refresh = entries.is_empty() || needs_refresh;
    (entries, needs_refresh)
}

fn load_cached_remote(source: &str, refresh_interval: Option<Duration>) -> (Vec<Entry>, bool) {
    let Some(path) = remote_cache_path(source) else {
        return (Vec::new(), true);
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return (Vec::new(), true);
    };
    let Ok(cached) = parse_cached_entries(&text) else {
        return (Vec::new(), true);
    };
    let needs_refresh = match refresh_interval {
        None => false,
        Some(interval) => {
            let age = remote_cache_age(source).unwrap_or(Duration::MAX);
            age >= interval
        }
    };
    (cached.entries, needs_refresh)
}

pub fn save_remote_cache(source: &str, entries: &[Entry]) -> Result<(), String> {
    let path =
        remote_cache_path(source).ok_or_else(|| "cache directory unavailable".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("create cache directory {}: {err}", parent.display()))?;
    }
    let payload = CachedEntries {
        saved_at: now_secs(),
        entries: entries.to_vec(),
    };
    let text = format_cached_entries(&payload);
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, text)
        .map_err(|err| format!("write cache {}: {err}", temp_path.display()))?;
    fs::rename(&temp_path, &path)
        .map_err(|err| format!("persist cache {}: {err}", path.display()))?;
    Ok(())
}

fn format_cached_entries(cache: &CachedEntries) -> String {
    let mut out = String::new();
    out.push_str("saved_at\t");
    out.push_str(&cache.saved_at.to_string());
    out.push('\n');
    for entry in &cache.entries {
        out.push_str(&escape_field(&entry.title));
        out.push('\t');
        out.push_str(&escape_field(&entry.url));
        out.push('\t');
        out.push_str(&escape_field(&entry.source));
        out.push('\t');
        out.push_str(&escape_field(&entry.detail));
        out.push('\n');
    }
    out
}

fn parse_cached_entries(text: &str) -> Result<CachedEntries, String> {
    let mut lines = text.lines();
    let header = lines.next().ok_or_else(|| "empty cache".to_string())?;
    let saved_at = header
        .strip_prefix("saved_at\t")
        .ok_or_else(|| "invalid cache header".to_string())?
        .parse::<u64>()
        .map_err(|err| format!("invalid cache timestamp: {err}"))?;

    let mut entries = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(4, '\t');
        let title = parts
            .next()
            .ok_or_else(|| "invalid cache row".to_string())
            .map(unescape_field)?;
        let url = parts
            .next()
            .ok_or_else(|| "invalid cache row".to_string())
            .map(unescape_field)?;
        let source = parts
            .next()
            .ok_or_else(|| "invalid cache row".to_string())
            .map(unescape_field)?;
        let detail = parts.next().unwrap_or_default();
        entries.push(Entry::new(title, url, source, unescape_field(detail)));
    }

    Ok(CachedEntries { saved_at, entries })
}

fn escape_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '\t' => out.push_str("%09"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            _ => out.push(ch),
        }
    }
    out
}

fn unescape_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            match &bytes[i + 1..i + 3] {
                b"25" => {
                    out.push('%');
                    i += 3;
                    continue;
                }
                b"09" => {
                    out.push('\t');
                    i += 3;
                    continue;
                }
                b"0A" => {
                    out.push('\n');
                    i += 3;
                    continue;
                }
                b"0D" => {
                    out.push('\r');
                    i += 3;
                    continue;
                }
                _ => {}
            }
        }

        let ch = value[i..].chars().next().unwrap_or_default();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

pub fn load_github_entries(config: &Config) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut page = 1usize;

    if let Some(token) = &config.github_token {
        loop {
            let rows = fetch_github_page(config, page, Some(token))?;
            if rows.is_empty() {
                break;
            }
            let row_count = rows.len();
            entries.extend(rows);
            if row_count < 100 {
                break;
            }
            page += 1;
            if page > 20 {
                break;
            }
        }
    } else if config.github_user.is_some() {
        loop {
            let rows = fetch_github_page(config, page, None)?;
            if rows.is_empty() {
                break;
            }
            let row_count = rows.len();
            entries.extend(rows);
            if row_count < 100 {
                break;
            }
            page += 1;
            if page > 20 {
                break;
            }
        }
    }

    Ok(deduplicate(entries))
}

pub fn load_gitlab_entries(config: &Config) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut page = 1usize;

    loop {
        let rows = fetch_gitlab_page(config, page)?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        entries.extend(rows);
        if row_count < 100 {
            break;
        }
        page += 1;
        if page > 20 {
            break;
        }
    }

    Ok(deduplicate(entries))
}

pub fn fetch_github_page(
    config: &Config,
    page: usize,
    token: Option<&str>,
) -> Result<Vec<Entry>, String> {
    let url = if token.is_some() {
        format!(
            "{}/user/repos?per_page=100&page={}&sort=updated&affiliation=owner,collaborator,organization_member",
            config.github_api.trim_end_matches('/'),
            page
        )
    } else if let Some(user) = &config.github_user {
        format!(
            "{}/users/{}/repos?per_page=100&page={}&sort=updated&type=owner",
            config.github_api.trim_end_matches('/'),
            user,
            page
        )
    } else {
        return Ok(Vec::new());
    };

    fetch_github_rows(&url, token)
}

pub fn test_github_connectivity(config: &Config) -> Result<String, String> {
    let Some(token) = config.github_token.as_deref() else {
        return Err("GitHub token is missing.".to_string());
    };

    let user_url = format!("{}/user", config.github_api.trim_end_matches('/'));
    let user_body = github_api_get(&user_url, token)?;
    let user_json: JsonValue =
        serde_json::from_str(&user_body).map_err(|err| format!("parse GitHub user JSON: {err}"))?;
    if let Some(message) = clean_json_str(user_json.get("message")) {
        if !message.is_empty() {
            return Err(format!("GitHub user check failed: {message}"));
        }
    }
    let login = clean_json_str(user_json.get("login"))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "GitHub user check failed: login not found.".to_string())?;

    let repos_url = format!(
        "{}/user/repos?per_page=1&page=1&sort=updated&affiliation=owner,collaborator,organization_member",
        config.github_api.trim_end_matches('/')
    );
    let repos_body = github_api_get(&repos_url, token)?;
    let repos_json: JsonValue = serde_json::from_str(&repos_body)
        .map_err(|err| format!("parse GitHub repos JSON: {err}"))?;
    match repos_json {
        JsonValue::Array(_) => Ok(format!("Connected as {login}. Repository access OK.")),
        JsonValue::Object(ref obj) => {
            let message = clean_json_str(obj.get("message")).unwrap_or_default();
            if message.is_empty() {
                Err("GitHub repository check failed.".to_string())
            } else {
                Err(format!("GitHub repository check failed: {message}"))
            }
        }
        _ => Err("GitHub repository check returned an unexpected response.".to_string()),
    }
}

pub fn fetch_gitlab_page(config: &Config, page: usize) -> Result<Vec<Entry>, String> {
    let mut url = format!(
        "{}/projects?per_page=100&page={}&order_by=last_activity_at&sort=desc",
        config.gitlab_api.trim_end_matches('/'),
        page
    );
    if config.gitlab_token.is_some() {
        url.push_str("&membership=true&min_access_level=20");
    } else {
        url.push_str("&visibility=public");
    }

    fetch_gitlab_rows(&url, config.gitlab_token.as_deref())
}

fn load_chromium_family() -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for path in chromium_history_paths() {
        match load_sqlite_history(
            &path,
            "SELECT COALESCE(title, ''), url FROM urls WHERE url IS NOT NULL AND url != '' ORDER BY last_visit_time DESC LIMIT 500;",
            "history",
            path.parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("Chromium"),
        ) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => source_log(format!("chromium history {}: {err}", path.display())),
        }
    }
    for path in chromium_bookmark_paths() {
        match load_chromium_bookmarks(&path) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => source_log(format!("chromium bookmarks {}: {err}", path.display())),
        }
    }
    Ok(entries)
}

fn load_firefox_family() -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for profile in firefox_profiles() {
        let db = profile.join("places.sqlite");
        if !db.exists() {
            continue;
        }

        match load_sqlite_history(
            &db,
            "SELECT COALESCE(title, ''), url FROM moz_places WHERE url IS NOT NULL AND url != '' ORDER BY last_visit_date DESC LIMIT 500;",
            "history",
            profile.file_name().and_then(|n| n.to_str()).unwrap_or("Firefox"),
        ) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => source_log(format!("firefox history {}: {err}", db.display())),
        }

        match load_sqlite_history(
            &db,
            "SELECT COALESCE(b.title, COALESCE(p.title, p.url)), p.url, COALESCE(parent.title, '') FROM moz_bookmarks b JOIN moz_places p ON p.id = b.fk LEFT JOIN moz_bookmarks parent ON parent.id = b.parent WHERE p.url IS NOT NULL AND p.url != '' ORDER BY b.dateAdded DESC LIMIT 500;",
            "bookmark",
            profile.file_name().and_then(|n| n.to_str()).unwrap_or("Firefox"),
        ) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => source_log(format!("firefox bookmarks {}: {err}", db.display())),
        }
    }
    Ok(entries)
}

fn load_safari() -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut macos_permission_denied = false;
    let Some(home) = home_dir() else {
        return Ok(entries);
    };

    let bookmarks = home.join("Library/Safari/Bookmarks.plist");
    if bookmarks.exists() {
        match load_safari_bookmarks(&bookmarks) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) if is_macos_permission_denied_message(&err) => {
                macos_permission_denied = true;
            }
            Err(err) => source_log(format!("safari bookmarks {}: {err}", bookmarks.display())),
        }
    }

    let history = home.join("Library/Safari/History.db");
    if history.exists() {
        match load_sqlite_history(
            &history,
            "SELECT COALESCE(history_items.title, history_items.url), history_items.url FROM history_visits JOIN history_items ON history_items.id = history_visits.history_item ORDER BY history_visits.visit_time DESC LIMIT 250;",
            "history",
            "Safari",
        ) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) if is_macos_permission_denied_message(&err) => {
                macos_permission_denied = true;
            }
            Err(err) => source_log(format!("safari history {}: {err}", history.display())),
        }
    }

    if macos_permission_denied {
        source_log(
            "safari data unavailable: macOS denied access. Grant Full Disk Access to your terminal to include Safari bookmarks and history."
                .to_string(),
        );
    }

    Ok(entries)
}

fn source_log(message: String) {
    SUPPRESS_SOURCE_LOGS.with(|flag| {
        if !flag.get() {
            eprintln!("{message}");
        }
    });
}

fn is_macos_permission_denied_message(message: &str) -> bool {
    message.contains("Operation not permitted")
        || message.contains("PermissionDenied")
        || message.to_ascii_lowercase().contains("permission denied")
}

fn chromium_history_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = home_dir() {
        for candidate in [
            "Library/Application Support/Google/Chrome/Default/History",
            "Library/Application Support/Chromium/Default/History",
            ".config/google-chrome/Default/History",
            ".config/chromium/Default/History",
            ".config/brave/Default/History",
            ".config/BraveSoftware/Brave-Browser/Default/History",
        ] {
            let path = home.join(candidate);
            if path.exists() {
                paths.push(path);
            }
        }
    }
    paths
}

fn chromium_bookmark_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = home_dir() {
        for candidate in [
            "Library/Application Support/Google/Chrome/Default/Bookmarks",
            "Library/Application Support/Chromium/Default/Bookmarks",
            ".config/google-chrome/Default/Bookmarks",
            ".config/chromium/Default/Bookmarks",
            ".config/brave/Default/Bookmarks",
            ".config/BraveSoftware/Brave-Browser/Default/Bookmarks",
        ] {
            let path = home.join(candidate);
            if path.exists() {
                paths.push(path);
            }
        }
    }
    paths
}

fn firefox_place_paths() -> Vec<PathBuf> {
    firefox_profiles()
        .into_iter()
        .map(|profile| profile.join("places.sqlite"))
        .filter(|path| path.exists())
        .collect()
}

fn safari_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = home_dir() {
        let bookmarks = home.join("Library/Safari/Bookmarks.plist");
        if bookmarks.exists() {
            paths.push(bookmarks);
        }
        let history = home.join("Library/Safari/History.db");
        if history.exists() {
            paths.push(history);
        }
    }
    paths
}

fn firefox_profiles() -> Vec<PathBuf> {
    let mut profiles = Vec::new();
    if let Some(home) = home_dir() {
        for base in [
            home.join(".mozilla/firefox"),
            home.join("Library/Application Support/Firefox/Profiles"),
        ] {
            if let Ok(read_dir) = fs::read_dir(base) {
                for entry in read_dir.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        profiles.push(path);
                    }
                }
            }
        }
    }
    profiles
}

fn load_sqlite_history(
    db_path: &Path,
    query: &str,
    source: &str,
    detail: &str,
) -> Result<Vec<Entry>, String> {
    match query_sqlite(db_path, query) {
        Ok(rows) if !rows.is_empty() => Ok(parse_tsv_entries(&rows, source, detail)),
        Ok(_) => Ok(Vec::new()),
        Err(_) => {
            let copied = copy_sqlite_to_temp(db_path)?;
            let rows = query_sqlite(&copied, query)?;
            Ok(parse_tsv_entries(&rows, source, detail))
        }
    }
}

fn load_chromium_bookmarks(path: &Path) -> Result<Vec<Entry>, String> {
    let text = fs::read_to_string(path)
        .map_err(|err| format!("read bookmarks {}: {err}", path.display()))?;
    let data: JsonValue =
        serde_json::from_str(&text).map_err(|err| format!("parse bookmarks JSON: {err}"))?;
    let mut entries = Vec::new();
    walk_chromium_bookmarks(&data, "", &mut entries);
    Ok(entries)
}

fn load_safari_bookmarks(path: &Path) -> Result<Vec<Entry>, String> {
    let data = PlistValue::from_file(path)
        .map_err(|err| format!("parse safari bookmarks {}: {err}", path.display()))?;
    let mut entries = Vec::new();
    walk_safari_bookmarks(&data, "", &mut entries);
    Ok(entries)
}

fn fetch_github_rows(url: &str, token: Option<&str>) -> Result<Vec<Entry>, String> {
    let mut args = vec![
        "-fsSL",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        "User-Agent: uf",
    ];
    let auth_header;
    if let Some(token) = token {
        auth_header = format!("Authorization: Bearer {token}");
        args.push("-H");
        args.push(auth_header.as_str());
    }

    let output = command_output("curl", &with_url(args, url))?;
    parse_github_entries(&output)
}

fn github_api_get(url: &str, token: &str) -> Result<String, String> {
    let auth_header = format!("Authorization: Bearer {token}");
    let args = vec![
        "-sSL",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        "User-Agent: uf",
        "-H",
        auth_header.as_str(),
    ];
    command_output_with_stderr("curl", &with_url(args, url))
}

fn fetch_gitlab_rows(url: &str, token: Option<&str>) -> Result<Vec<Entry>, String> {
    let mut args = vec!["-fsSL", "-H", "User-Agent: web-fzf"];
    let auth_header;
    if let Some(token) = token {
        auth_header = format!("PRIVATE-TOKEN: {token}");
        args.push("-H");
        args.push(auth_header.as_str());
    }

    let output = command_output("curl", &with_url(args, url))?;
    parse_gitlab_entries(&output)
}

fn walk_chromium_bookmarks(node: &JsonValue, folder: &str, entries: &mut Vec<Entry>) {
    if folder.is_empty() {
        if let Some(roots) = node.get("roots").and_then(JsonValue::as_object) {
            for child in roots.values() {
                walk_chromium_bookmarks(child, "", entries);
            }
            return;
        }
    }

    let Some(children) = node.get("children").and_then(JsonValue::as_array) else {
        return;
    };

    for child in children {
        match json_str(child.get("type")) {
            Some("url") => {
                let url = clean_json_str(child.get("url")).unwrap_or_default();
                if url.is_empty() {
                    continue;
                }
                let title = clean_title(
                    clean_json_str(child.get("name"))
                        .as_deref()
                        .unwrap_or(url.as_str()),
                );
                entries.push(Entry::new(title, url, "bookmark", folder.to_string()));
            }
            Some("folder") => {
                let name = clean_title(clean_json_str(child.get("name")).as_deref().unwrap_or(""));
                let next_folder = if folder.is_empty() {
                    name
                } else if name.is_empty() {
                    folder.to_string()
                } else {
                    format!("{folder}/{name}")
                };
                walk_chromium_bookmarks(child, &next_folder, entries);
            }
            _ => {}
        }
    }
}

fn walk_safari_bookmarks(node: &PlistValue, folder: &str, entries: &mut Vec<Entry>) {
    if let Some(items) = node.as_array() {
        for item in items {
            walk_safari_bookmarks(item, folder, entries);
        }
        return;
    }

    let Some(dict) = node.as_dictionary() else {
        return;
    };

    if let Some(url) = dict.get("URLString").and_then(PlistValue::as_string) {
        let title = dict
            .get("URIDictionary")
            .and_then(PlistValue::as_dictionary)
            .and_then(|dict| dict.get("title"))
            .and_then(PlistValue::as_string)
            .or_else(|| dict.get("Title").and_then(PlistValue::as_string))
            .unwrap_or(url);
        entries.push(Entry::new(
            clean_title(title),
            clean_text(url),
            "bookmark",
            clean_title(folder),
        ));
    }

    let title = dict
        .get("Title")
        .and_then(PlistValue::as_string)
        .map(clean_title)
        .unwrap_or_default();
    let next_folder = if title.is_empty() {
        folder.to_string()
    } else if folder.is_empty() {
        title
    } else {
        format!("{folder}/{title}")
    };

    if let Some(children) = dict.get("Children").and_then(PlistValue::as_array) {
        for child in children {
            walk_safari_bookmarks(child, &next_folder, entries);
        }
    }
}

fn parse_github_entries(text: &str) -> Result<Vec<Entry>, String> {
    let data: JsonValue = serde_json::from_str(if text.trim().is_empty() { "[]" } else { text })
        .map_err(|err| format!("parse GitHub JSON: {err}"))?;
    let Some(items) = data.as_array() else {
        return Ok(Vec::new());
    };

    Ok(items
        .iter()
        .filter_map(|item| {
            let url = clean_json_str(item.get("html_url"))?;
            if url.is_empty() {
                return None;
            }
            let title = clean_json_str(item.get("full_name"))
                .or_else(|| clean_json_str(item.get("name")))
                .unwrap_or_else(|| url.clone());
            let description = clean_json_str(item.get("description")).unwrap_or_default();
            let stars = item
                .get("stargazers_count")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let detail = repo_detail_with_stars(&description, stars);
            Some(Entry::new(title, url, "github", detail))
        })
        .collect())
}

fn parse_gitlab_entries(text: &str) -> Result<Vec<Entry>, String> {
    let data: JsonValue = serde_json::from_str(if text.trim().is_empty() { "[]" } else { text })
        .map_err(|err| format!("parse GitLab JSON: {err}"))?;
    let Some(items) = data.as_array() else {
        return Ok(Vec::new());
    };

    Ok(items
        .iter()
        .filter_map(|item| {
            let url = clean_json_str(item.get("web_url"))?;
            if url.is_empty() {
                return None;
            }
            let title = clean_json_str(item.get("path_with_namespace"))
                .or_else(|| clean_json_str(item.get("name")))
                .unwrap_or_else(|| url.clone());
            let description = clean_json_str(item.get("description")).unwrap_or_default();
            let stars = item
                .get("star_count")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let detail = repo_detail_with_stars(&description, stars);
            Some(Entry::new(title, url, "gitlab", detail))
        })
        .collect())
}

fn clean_json_str(value: Option<&JsonValue>) -> Option<String> {
    json_str(value).map(clean_text)
}

// GitHub/GitLab 仓库的 star 数以 marker 形式拼在 detail 末尾，
// 仿 dockerhub tags 的存储方式，避免给 Entry 新增字段。
fn repo_detail_with_stars(description: &str, stars: u64) -> String {
    if stars == 0 {
        description.to_string()
    } else {
        format!("{description}{REPO_STARS_MARKER}{stars}")
    }
}

pub fn repo_entry_description(entry: &Entry) -> &str {
    entry
        .detail
        .split_once(REPO_STARS_MARKER)
        .map(|(description, _)| description)
        .unwrap_or(&entry.detail)
}

pub fn repo_entry_stars(entry: &Entry) -> Option<u64> {
    entry
        .detail
        .split_once(REPO_STARS_MARKER)
        .and_then(|(_, stars)| stars.trim().parse::<u64>().ok())
}

fn json_str(value: Option<&JsonValue>) -> Option<&str> {
    value.and_then(JsonValue::as_str)
}

fn clean_text(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

fn clean_title(value: &str) -> String {
    value
        .trim_matches(|ch: char| ch.is_whitespace() || is_title_format_char(ch))
        .to_string()
}

fn is_title_format_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{200B}'
            | '\u{200C}'
            | '\u{200D}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{061C}'
            | '\u{2060}'
            | '\u{FEFF}'
            | '\u{202A}'
            | '\u{202B}'
            | '\u{202C}'
            | '\u{202D}'
            | '\u{202E}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
    )
}

fn parse_tsv_entries(text: &str, source: &str, default_detail: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let title = clean_title(parts.next()?.trim());
            let url = parts.next()?.trim();
            let detail = parts.next().unwrap_or(default_detail).trim();
            if url.is_empty() {
                return None;
            }
            Some(Entry::new(
                title.to_string(),
                url.to_string(),
                source.to_string(),
                detail.to_string(),
            ))
        })
        .collect()
}

pub fn dockerhub_entry_description(entry: &Entry) -> &str {
    entry
        .detail
        .split_once(DOCKERHUB_TAGS_MARKER)
        .map(|(description, _)| description)
        .unwrap_or(&entry.detail)
}

pub fn dockerhub_entry_tags(entry: &Entry) -> Vec<String> {
    entry
        .detail
        .split_once(DOCKERHUB_TAGS_MARKER)
        .map(|(_, tags)| {
            tags.split(',')
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn dockerhub_detail_with_tags(description: &str, tags: &[String]) -> String {
    if tags.is_empty() {
        description.to_string()
    } else {
        format!("{}{}{}", description, DOCKERHUB_TAGS_MARKER, tags.join(","))
    }
}

fn with_dockerhub_tags(mut entry: Entry, token: Option<&str>) -> Entry {
    let tags = fetch_dockerhub_tags(&entry.title, token).unwrap_or_default();
    entry.detail = dockerhub_detail_with_tags(dockerhub_entry_description(&entry), &tags);
    entry
}

pub fn fetch_dockerhub_tags(repo: &str, token: Option<&str>) -> Result<Vec<String>, String> {
    let url = format!(
        "https://hub.docker.com/v2/repositories/{}/tags/?page_size=100&ordering=last_updated",
        repo
    );

    let mut args = vec!["-sSL", "-H", "Content-Type: application/json"];
    let auth_header;
    if let Some(t) = token {
        auth_header = format!("Authorization: JWT {t}");
        args.push("-H");
        args.push(auth_header.as_str());
    }

    let output = command_output_with_stderr("curl", &with_url(args, &url))?;
    if output.is_empty() {
        return Ok(Vec::new());
    }

    parse_dockerhub_tags(&output)
}

fn parse_dockerhub_tags(text: &str) -> Result<Vec<String>, String> {
    let data: JsonValue = serde_json::from_str(if text.trim().is_empty() { "{}" } else { text })
        .map_err(|err| format!("parse DockerHub tags JSON: {err}"))?;
    Ok(data
        .get("results")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| clean_json_str(item.get("name")))
        .filter(|name| !name.is_empty())
        .collect())
}

fn deduplicate(entries: Vec<Entry>) -> Vec<Entry> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for entry in entries {
        if entry.url.is_empty() {
            continue;
        }
        let key = (entry.title.clone(), entry.url.clone(), entry.source.clone());
        if seen.insert(key) {
            deduped.push(entry);
        }
    }
    deduped
}

fn command_output(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("failed to run {program}: {err}"))?;
    if !output.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// Like command_output but always returns stdout regardless of exit code,
// so API error bodies are visible in debug logs instead of being silently dropped.
fn command_output_with_stderr(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("failed to run {program}: {err}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn with_url<'a>(mut args: Vec<&'a str>, url: &'a str) -> Vec<&'a str> {
    args.push(url);
    args
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn remote_cache_path(source: &str) -> Option<PathBuf> {
    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".cache")))?;
    Some(base.join("uf").join(format!("{source}.json")))
}

fn remote_cache_age(source: &str) -> Option<Duration> {
    let path = remote_cache_path(source)?;
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    SystemTime::now().duration_since(modified).ok()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn query_sqlite(db_path: &Path, query: &str) -> Result<String, String> {
    let output = Command::new("sqlite3")
        .args([
            "-readonly",
            "-separator",
            "\t",
            db_path.to_string_lossy().as_ref(),
            query,
        ])
        .output()
        .map_err(|err| format!("failed to run sqlite3: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn copy_sqlite_to_temp(db_path: &Path) -> Result<PathBuf, String> {
    let mut temp_path = env::temp_dir();
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let filename = format!("uf-{}-{}.sqlite", std::process::id(), suffix);
    temp_path.push(filename);
    fs::copy(db_path, &temp_path)
        .map_err(|err| format!("copy sqlite db {}: {err}", db_path.display()))?;
    Ok(temp_path)
}

pub fn fetch_dockerhub_page(config: &Config, page: usize) -> Result<Vec<Entry>, String> {
    let Some(username) = &config.dockerhub_username else {
        if config.debug {
            eprintln!("[dockerhub] skipped: DOCKERHUB_USERNAME not set");
        }
        return Ok(Vec::new());
    };

    let url = format!(
        "https://hub.docker.com/v2/repositories/{}/?page_size=100&page={}&ordering=last_updated",
        username, page
    );

    if config.debug {
        eprintln!("[dockerhub] GET {url}");
        eprintln!(
            "[dockerhub] token: {}",
            if config.dockerhub_token.is_some() {
                "present"
            } else {
                "none (public repos only)"
            }
        );
    }

    let mut args = vec!["-sSL", "-H", "Content-Type: application/json"];
    let auth_header;
    if let Some(token) = &config.dockerhub_token {
        auth_header = format!("Authorization: JWT {token}");
        args.push("-H");
        args.push(auth_header.as_str());
    }

    let output = command_output_with_stderr("curl", &with_url(args, &url))?;

    if config.debug {
        eprintln!("[dockerhub] curl response ({} bytes):", output.len());
        let preview = if output.len() > 500 {
            format!("{}... (truncated)", &output[..500])
        } else {
            output.clone()
        };
        eprintln!("{preview}");
    }

    if output.is_empty() {
        if config.debug {
            eprintln!(
                "[dockerhub] curl returned empty body (possible auth error or network issue)"
            );
        }
        return Ok(Vec::new());
    }

    let entries_without_tags = parse_dockerhub_repo_entries(&output, config.debug)?;
    if config.debug {
        eprintln!(
            "[dockerhub] parsed {} repository entries",
            entries_without_tags.len()
        );
    }

    let entries: Vec<Entry> = entries_without_tags
        .into_iter()
        .map(|entry| with_dockerhub_tags(entry, config.dockerhub_token.as_deref()))
        .collect();

    if config.debug {
        eprintln!("[dockerhub] page {page}: {} entries", entries.len());
    }

    Ok(entries)
}

fn parse_dockerhub_repo_entries(text: &str, debug: bool) -> Result<Vec<Entry>, String> {
    let data: JsonValue = serde_json::from_str(if text.trim().is_empty() { "{}" } else { text })
        .map_err(|err| format!("parse DockerHub JSON: {err}"))?;

    if debug {
        let message = clean_json_str(data.get("message"))
            .or_else(|| clean_json_str(data.get("detail")))
            .unwrap_or_default();
        if !message.is_empty() {
            eprintln!("[dockerhub] API_ERROR: {message}");
        }
    }

    Ok(data
        .get("results")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let namespace = clean_json_str(item.get("namespace")).unwrap_or_default();
            let name = clean_json_str(item.get("name")).unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            let description = clean_json_str(item.get("description")).unwrap_or_default();
            let is_private = item
                .get("is_private")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            let source = if is_private { "private" } else { "public" };
            let title = if namespace.is_empty() {
                name.clone()
            } else {
                format!("{namespace}/{name}")
            };
            let url = format!("https://hub.docker.com/r/{title}");
            Some(Entry::new(title, url, source, description))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        clean_title, deduplicate, dockerhub_detail_with_tags, dockerhub_entry_description,
        dockerhub_entry_tags, is_macos_permission_denied_message, parse_github_entries,
        parse_gitlab_entries, parse_tsv_entries, repo_entry_description, repo_entry_stars,
    };
    use crate::models::Entry;

    #[test]
    fn parse_tsv_entries_extracts_rows() {
        let rows = parse_tsv_entries("Title\thttps://example.com\tDetail\n", "bookmark", "x");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].url, "https://example.com");
        assert_eq!(rows[0].detail, "Detail");
    }

    #[test]
    fn clean_title_strips_leading_whitespace_and_format_chars() {
        let title = clean_title("\u{200F}\u{200D}   目开发代码合入记录 - 飞书云文档");
        assert_eq!(title, "目开发代码合入记录 - 飞书云文档");
    }

    #[test]
    fn detects_macos_privacy_permission_errors() {
        assert!(is_macos_permission_denied_message(
            "Io(Os { code: 1, kind: PermissionDenied, message: \"Operation not permitted\" })"
        ));
        assert!(is_macos_permission_denied_message(
            "copy sqlite db /Users/me/Library/Safari/History.db: Operation not permitted (os error 1)"
        ));
    }

    #[test]
    fn deduplicate_keeps_unique_entries() {
        let entries = vec![
            Entry::new("A", "https://a", "github", ""),
            Entry::new("A", "https://a", "github", ""),
            Entry::new("B", "https://b", "gitlab", ""),
        ];
        assert_eq!(deduplicate(entries).len(), 2);
    }

    #[test]
    fn dockerhub_detail_stores_description_and_tags_together() {
        let detail = dockerhub_detail_with_tags(
            "Small base image",
            &["latest".to_string(), "1.0".to_string()],
        );
        let entry = Entry::new(
            "me/app",
            "https://hub.docker.com/r/me/app",
            "public",
            detail,
        );

        assert_eq!(dockerhub_entry_description(&entry), "Small base image");
        assert_eq!(dockerhub_entry_tags(&entry), vec!["latest", "1.0"]);
    }

    #[test]
    fn github_entries_carry_star_count() {
        let json = r#"[
            {"html_url":"https://github.com/me/repo","full_name":"me/repo","description":"demo","stargazers_count":13},
            {"html_url":"https://github.com/me/zero","full_name":"me/zero","description":"none","stargazers_count":0}
        ]"#;
        let rows = parse_github_entries(json).unwrap();
        assert_eq!(rows.len(), 2);
        // description 与 star 分别可读出
        assert_eq!(repo_entry_description(&rows[0]), "demo");
        assert_eq!(repo_entry_stars(&rows[0]), Some(13));
        // 0 星不写入 marker
        assert_eq!(repo_entry_description(&rows[1]), "none");
        assert_eq!(repo_entry_stars(&rows[1]), None);
    }

    #[test]
    fn gitlab_entries_carry_star_count() {
        let json = r#"[
            {"web_url":"https://gitlab.com/g/p","path_with_namespace":"g/p","description":"d","star_count":7}
        ]"#;
        let rows = parse_gitlab_entries(json).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(repo_entry_description(&rows[0]), "d");
        assert_eq!(repo_entry_stars(&rows[0]), Some(7));
    }

    #[test]
    fn repo_stars_absent_when_no_marker() {
        let entry = Entry::new("t", "https://x", "github", "plain description");
        assert_eq!(repo_entry_description(&entry), "plain description");
        assert_eq!(repo_entry_stars(&entry), None);
    }
}
