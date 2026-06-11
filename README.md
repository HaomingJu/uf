# web-fzf

Terminal search launcher for browser history, bookmarks, GitHub, and GitLab.

This implementation is in Rust and uses a third-party TUI stack for the full-screen interface.

## What it does

- Searches local browser history and bookmarks from common browser data locations
- Searches GitHub repositories through the REST API
- Searches GitLab projects through the REST API
- Shows results in a tabbed, full-screen terminal UI
- Opens the selected result in the default browser

## Runtime requirements

- Rust toolchain
- `python3`
- `sqlite3`
- `curl`
- `stty`
- On macOS, the standard `open` command
- On Linux, `xdg-open`

## Run

```bash
cargo run --release
```

Or after installation:

```bash
web-fzf
```

## Controls

- Type to filter
- `Up` / `Down` to move selection
- `PageUp` / `PageDown` to jump
- `Left` / `Right` or `Tab` to switch between tabs
- `Enter` to open the selected URL
- `Esc` to quit

Tabs:

- `History` shows local browser history and bookmarks
- `GitHub` shows repositories visible to the configured GitHub account or user
- `GitLab` shows visible GitLab projects

GitHub and GitLab results are cached locally under the user cache directory. The app opens immediately from cache when available and refreshes stale data in the background.

If GitHub, GitLab, or a protected browser source cannot be read, the app keeps the remaining sources available instead of exiting.

## Configuration

Command-line flags:

- `--no-browser`
- `--no-github`
- `--no-gitlab`
- `--github-token <token>`
- `--gitlab-token <token>`
- `--github-api <url>`
- `--gitlab-api <url>`
- `--github-user <user>`

Environment variables:

- `GITHUB_TOKEN`
- `GITLAB_TOKEN`
- `GITHUB_API`
- `GITLAB_API`
- `GITHUB_USER`
- `WEB_FZF_PREVIEW` to re-enable the preview pane (`1`, `true`, `yes`, or `on`)

## Tests

```bash
cargo test
```
