# web-fzf

Terminal search launcher for browser history, bookmarks, GitHub, GitLab, and DockerHub.

This implementation is in Rust and uses a third-party TUI stack for the full-screen interface.

## What it does

- Searches local browser history and bookmarks from common browser data locations
- Searches GitHub repositories through the REST API
- Searches GitLab projects through the REST API
- Searches DockerHub repositories through the REST API
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
- `DockerHub` shows repositories belonging to the configured DockerHub user

GitHub, GitLab, and DockerHub results are cached locally under the user cache directory. The app opens immediately from cache when available and refreshes stale data in the background.

If any remote source or protected browser source cannot be read, the app keeps the remaining sources available instead of exiting.

## Configuration

Command-line flags:

- `--no-browser`
- `--no-github`
- `--no-gitlab`
- `--no-dockerhub`
- `--github-token <token>`
- `--gitlab-token <token>`
- `--github-api <url>`
- `--gitlab-api <url>`
- `--github-user <user>`
- `--dockerhub-token <token>`
- `--dockerhub-user <user>`
- `--debug` — write diagnostic logs to stderr (see [Debugging](#debugging))

Environment variables:

- `GITHUB_TOKEN`
- `GITLAB_TOKEN`
- `GITHUB_API`
- `GITLAB_API`
- `GITHUB_USER`
- `DOCKERHUB_TOKEN`
- `DOCKERHUB_USERNAME`
- `WEB_FZF_PREVIEW` to re-enable the preview pane (`1`, `true`, `yes`, or `on`)

DockerHub shows only public repositories when no token is provided. With a Personal Access Token (PAT), private repositories are also visible.

## Debugging

Use `--debug` to write diagnostic logs to stderr. Redirect stderr to a file so the output does not interfere with the TUI:

```bash
web-fzf --debug 2>debug.log
```

Open the app as usual, then quit. Inspect the log:

```bash
cat debug.log
```

For DockerHub specifically, the log covers every step of the fetch pipeline:

| Log line | Meaning |
|---|---|
| `[dockerhub] skipped: DOCKERHUB_USERNAME not set` | No username configured; source is skipped entirely |
| `[dockerhub] GET https://hub.docker.com/v2/...` | The URL being requested |
| `[dockerhub] token: present` / `none (public repos only)` | Whether a token was supplied |
| `[dockerhub] curl response (N bytes): ...` | First 500 bytes of the raw API response |
| `[dockerhub] curl returned empty body` | curl returned nothing — likely a network or auth failure |
| `[dockerhub] python stderr: API_ERROR: ...` | Error message returned by the DockerHub API |
| `[dockerhub] python parsed N lines` | Number of rows the parser extracted |
| `[dockerhub] page N: N entries` | Final entry count for the page |

Example showing a token auth problem:

```bash
DOCKERHUB_USERNAME=myuser web-fzf --debug 2>debug.log
# quit the app, then:
grep dockerhub debug.log
# [dockerhub] GET https://hub.docker.com/v2/repositories/myuser/...
# [dockerhub] token: none (public repos only)
# [dockerhub] curl response (42 bytes): {"message": "access denied"}
```

## Tests

```bash
cargo test
```

