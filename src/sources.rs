use crate::config::Config;
use crate::models::Entry;
use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

thread_local! {
    static SUPPRESS_SOURCE_LOGS: Cell<bool> = const { Cell::new(false) };
}

const DOCKERHUB_TAGS_MARKER: &str = "\nWEB_FZF_TAGS\t";

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
    remote_cache_path(source)
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| parse_cached_entries(&text).ok())
        .map(|cached| cached.entries)
        .unwrap_or_default()
}

pub fn remote_cache_needs_refresh(source: &str, refresh_interval: Duration) -> bool {
    let entries = load_cached_remote_entries(source);
    if entries.is_empty() {
        return true;
    }
    match remote_cache_age(source) {
        Some(age) => age >= refresh_interval,
        None => true,
    }
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

pub fn fetch_gitlab_page(config: &Config, page: usize) -> Result<Vec<Entry>, String> {
    let mut url = format!(
        "{}/projects?simple=true&per_page=100&page={}&order_by=last_activity_at&sort=desc",
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
            "browser-history",
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
            "browser-history",
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
    let Some(home) = home_dir() else {
        return Ok(entries);
    };

    let bookmarks = home.join("Library/Safari/Bookmarks.plist");
    if bookmarks.exists() {
        match load_safari_bookmarks(&bookmarks) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => source_log(format!("safari bookmarks {}: {err}", bookmarks.display())),
        }
    }

    let history = home.join("Library/Safari/History.db");
    if history.exists() {
        match load_sqlite_history(
            &history,
            "SELECT COALESCE(history_items.title, history_items.url), history_items.url FROM history_visits JOIN history_items ON history_items.id = history_visits.history_item ORDER BY history_visits.visit_time DESC LIMIT 250;",
            "browser-history",
            "Safari",
        ) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => source_log(format!("safari history {}: {err}", history.display())),
        }
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
    let script = r#"
import json
import sys

path = sys.argv[1]

with open(path, "r", encoding="utf-8") as fh:
    data = json.load(fh)

def clean(value):
    return str(value or "").replace("\t", " ").replace("\n", " ")

def walk(node, folder=""):
    roots = node.get("roots", {})
    if folder == "" and isinstance(roots, dict):
        for child in roots.values():
            walk(child, "")
        return

    children = node.get("children", [])
    for child in children:
        if child.get("type") == "url":
            title = clean(child.get("name") or child.get("url"))
            url = clean(child.get("url"))
            if url:
                print(f"{title}\t{url}\t{clean(folder)}")
        elif child.get("type") == "folder":
            name = clean(child.get("name"))
            next_folder = name if not folder else (folder + "/" + name if name else folder)
            walk(child, next_folder)

walk(data, "")
"#;
    let output = run_python(script, &[path.to_string_lossy().to_string()])?;
    Ok(parse_tsv_entries(
        &output,
        "bookmark",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Bookmarks"),
    ))
}

fn load_safari_bookmarks(path: &Path) -> Result<Vec<Entry>, String> {
    let script = r#"
import plistlib
import sys

path = sys.argv[1]

with open(path, "rb") as fh:
    data = plistlib.load(fh)

def clean(value):
    return str(value or "").replace("\t", " ").replace("\n", " ")

def walk(node, folder=""):
    if isinstance(node, list):
        for item in node:
            walk(item, folder)
        return
    if isinstance(node, dict):
        if node.get("URLString"):
            title = clean(node.get("URIDictionary", {}).get("title") or node.get("Title") or node.get("URLString"))
            url = clean(node.get("URLString"))
            print(f"{title}\t{url}\t{clean(folder)}")
        children = node.get("Children") or []
        next_folder = folder
        title = clean(node.get("Title"))
        if title:
            next_folder = title if not folder else folder + "/" + title
        for child in children:
            walk(child, next_folder)

walk(data, "")
"#;
    let output = run_python(script, &[path.to_string_lossy().to_string()])?;
    Ok(parse_tsv_entries(&output, "bookmark", "Safari"))
}

fn fetch_github_rows(url: &str, token: Option<&str>) -> Result<Vec<Entry>, String> {
    let mut args = vec![
        "-fsSL",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        "User-Agent: web-fzf",
    ];
    let auth_header;
    if let Some(token) = token {
        auth_header = format!("Authorization: Bearer {token}");
        args.push("-H");
        args.push(auth_header.as_str());
    }

    let output = command_output("curl", &with_url(args, url))?;
    let script = r#"
import json
import sys

data = json.loads(sys.stdin.read() or "[]")
for item in data:
    title = str(item.get("full_name") or item.get("name") or item.get("html_url") or "").replace("\t", " ").replace("\n", " ")
    url = str(item.get("html_url") or "").replace("\t", " ").replace("\n", " ")
    detail = str(item.get("description") or "").replace("\t", " ").replace("\n", " ")
    if url:
        print(f"{title}\t{url}\t{detail}")
"#;
    let parsed = run_python_stdin(script, &output, &[])?;
    Ok(parse_tsv_entries(&parsed, "github", "GitHub"))
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
    let script = r#"
import json
import sys

data = json.loads(sys.stdin.read() or "[]")
for item in data:
    title = str(item.get("path_with_namespace") or item.get("name") or item.get("web_url") or "").replace("\t", " ").replace("\n", " ")
    url = str(item.get("web_url") or "").replace("\t", " ").replace("\n", " ")
    detail = str(item.get("description") or "").replace("\t", " ").replace("\n", " ")
    if url:
        print(f"{title}\t{url}\t{detail}")
"#;
    let parsed = run_python_stdin(script, &output, &[])?;
    Ok(parse_tsv_entries(&parsed, "gitlab", "GitLab"))
}

fn parse_tsv_entries(text: &str, source: &str, default_detail: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let title = parts.next()?.trim();
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

// Parses 4-column TSV output from fetch_dockerhub_page:
// title \t url \t source(public|private) \t detail
fn parse_dockerhub_repo_entries(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, '\t');
            let title = parts.next()?.trim();
            let url = parts.next()?.trim();
            let source = parts.next().unwrap_or("public").trim();
            let detail = parts.next().unwrap_or("").trim();
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

    let script = r#"
import json, sys
data = json.loads(sys.stdin.read() or "{}")
for item in (data.get("results") or []):
    name = str(item.get("name") or "").strip()
    if name:
        print(name)
"#;
    let mut command = std::process::Command::new("python3");
    command.arg("-c").arg(script);
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to run python3: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(output.as_bytes())
            .map_err(|err| format!("write python stdin: {err}"))?;
    }
    let result = child
        .wait_with_output()
        .map_err(|err| format!("collect python output: {err}"))?;

    let tags = String::from_utf8_lossy(&result.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    Ok(tags)
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

fn run_python(script: &str, args: &[String]) -> Result<String, String> {
    let mut command = Command::new("python3");
    command.arg("-c").arg(script);
    for arg in args {
        command.arg(arg);
    }
    let output = command
        .output()
        .map_err(|err| format!("failed to run python3: {err}"))?;
    if !output.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_python_stdin(script: &str, stdin_input: &str, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("python3");
    command.arg("-c").arg(script);
    for arg in args {
        command.arg(arg);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to run python3: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(stdin_input.as_bytes())
            .map_err(|err| format!("failed to write python stdin: {err}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("failed to collect python output: {err}"))?;
    if !output.status.success() {
        return Ok(String::new());
    }
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
    Some(base.join("web-fzf").join(format!("{source}.json")))
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
    let filename = format!("web-fzf-{}-{}.sqlite", std::process::id(), suffix);
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

    let script = r#"
import json
import sys

raw = sys.stdin.read()
try:
    data = json.loads(raw or "{}")
except Exception as e:
    print(f"JSON_PARSE_ERROR: {e}", file=sys.stderr)
    sys.exit(0)

if "message" in data or "detail" in data:
    msg = data.get("message") or data.get("detail") or ""
    print(f"API_ERROR: {msg}", file=sys.stderr)

results = data.get("results") or []
for item in results:
    namespace = str(item.get("namespace") or "").replace("\t", " ").replace("\n", " ")
    name = str(item.get("name") or "").replace("\t", " ").replace("\n", " ")
    description = str(item.get("description") or "").replace("\t", " ").replace("\n", " ")
    is_private = item.get("is_private", False)
    source = "private" if is_private else "public"
    title = f"{namespace}/{name}" if namespace else name
    url = f"https://hub.docker.com/r/{namespace}/{name}" if namespace else f"https://hub.docker.com/r/{name}"
    if title:
        print(f"{title}\t{url}\t{source}\t{description}")
"#;

    let mut command = std::process::Command::new("python3");
    command.arg("-c").arg(script);
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to run python3: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(output.as_bytes())
            .map_err(|err| format!("failed to write python stdin: {err}"))?;
    }
    let result = child
        .wait_with_output()
        .map_err(|err| format!("failed to collect python output: {err}"))?;

    if config.debug {
        let stderr_out = String::from_utf8_lossy(&result.stderr);
        if !stderr_out.trim().is_empty() {
            eprintln!("[dockerhub] python stderr: {stderr_out}");
        }
        let stdout_out = String::from_utf8_lossy(&result.stdout);
        eprintln!(
            "[dockerhub] python parsed {} lines",
            stdout_out.lines().count()
        );
    }

    let parsed = String::from_utf8_lossy(&result.stdout).into_owned();
    let entries: Vec<Entry> = parse_dockerhub_repo_entries(&parsed)
        .into_iter()
        .map(|entry| with_dockerhub_tags(entry, config.dockerhub_token.as_deref()))
        .collect();

    if config.debug {
        eprintln!("[dockerhub] page {page}: {} entries", entries.len());
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::{
        deduplicate, dockerhub_detail_with_tags, dockerhub_entry_description, dockerhub_entry_tags,
        parse_tsv_entries,
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
}
