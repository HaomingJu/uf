use crate::config::Config;
use crate::models::Entry;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn load_entries(config: &Config) -> Vec<Entry> {
    let mut entries = Vec::new();

    if config.include_browser {
        match load_local_browser_entries() {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => eprintln!("browser sources: {err}"),
        }
    }
    if config.include_github {
        match load_github_entries(config) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => eprintln!("github sources: {err}"),
        }
    }
    if config.include_gitlab {
        match load_gitlab_entries(config) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => eprintln!("gitlab sources: {err}"),
        }
    }

    deduplicate(entries)
}

pub fn load_local_browser_entries() -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    match load_chromium_family() {
        Ok(mut rows) => entries.append(&mut rows),
        Err(err) => eprintln!("chromium browser sources: {err}"),
    }
    match load_firefox_family() {
        Ok(mut rows) => entries.append(&mut rows),
        Err(err) => eprintln!("firefox browser sources: {err}"),
    }
    match load_safari() {
        Ok(mut rows) => entries.append(&mut rows),
        Err(err) => eprintln!("safari browser sources: {err}"),
    }
    Ok(deduplicate(entries))
}

pub fn load_github_entries(config: &Config) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut page = 1usize;

    if let Some(token) = &config.github_token {
        loop {
            let url = format!(
                "{}/user/repos?per_page=100&page={}&sort=updated&affiliation=owner,collaborator,organization_member",
                config.github_api.trim_end_matches('/'),
                page
            );
            let rows = fetch_github_rows(&url, Some(token))?;
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
    } else if let Some(user) = &config.github_user {
        loop {
            let url = format!(
                "{}/users/{}/repos?per_page=100&page={}&sort=updated&type=owner",
                config.github_api.trim_end_matches('/'),
                user,
                page
            );
            let rows = fetch_github_rows(&url, None)?;
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

        let rows = fetch_gitlab_rows(&url, config.gitlab_token.as_deref())?;
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
            Err(err) => eprintln!("chromium history {}: {err}", path.display()),
        }
    }
    for path in chromium_bookmark_paths() {
        match load_chromium_bookmarks(&path) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => eprintln!("chromium bookmarks {}: {err}", path.display()),
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
            Err(err) => eprintln!("firefox history {}: {err}", db.display()),
        }

        match load_sqlite_history(
            &db,
            "SELECT COALESCE(b.title, COALESCE(p.title, p.url)), p.url, COALESCE(parent.title, '') FROM moz_bookmarks b JOIN moz_places p ON p.id = b.fk LEFT JOIN moz_bookmarks parent ON parent.id = b.parent WHERE p.url IS NOT NULL AND p.url != '' ORDER BY b.dateAdded DESC LIMIT 500;",
            "bookmark",
            profile.file_name().and_then(|n| n.to_str()).unwrap_or("Firefox"),
        ) {
            Ok(mut rows) => entries.append(&mut rows),
            Err(err) => eprintln!("firefox bookmarks {}: {err}", db.display()),
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
            Err(err) => eprintln!("safari bookmarks {}: {err}", bookmarks.display()),
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
            Err(err) => eprintln!("safari history {}: {err}", history.display()),
        }
    }

    Ok(entries)
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

#[cfg(test)]
mod tests {
    use super::{deduplicate, parse_tsv_entries};
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
}
