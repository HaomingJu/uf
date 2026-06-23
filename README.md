# uf

Terminal search launcher for browser history, bookmarks, GitHub, GitLab, and DockerHub.

This implementation is in Rust and uses a third-party TUI stack for the full-screen interface.

## What it does

- Searches local browser history and bookmarks from common browser data locations
- Searches GitHub repositories through the REST API
- Searches GitLab projects through the REST API
- Searches DockerHub repositories through the REST API
- Shows results in a tabbed, full-screen terminal UI
- Opens the selected result in the default browser

Search uses `nucleo-matcher` for fuzzy matching. The UI keeps precomputed searchable text for loaded entries so each keystroke can reuse it while ranking results.

## Installation

### Homebrew (macOS / Linux)

```bash
brew trust haomingju/tap
brew install HaomingJu/tap/uf
```

### One-line install script (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/HaomingJu/uf/main/install.sh | sh
```

To install a specific version or to a custom directory:

```bash
UF_VERSION=v0.1.0 UF_INSTALL_DIR=/usr/local/bin sh install.sh
```

### Build from source

Requires a Rust toolchain, `sqlite3`, `curl`, and `stty`.

```bash
cargo install --git https://github.com/HaomingJu/uf
```

## Runtime requirements

- `sqlite3`
- `curl`
- `stty`
- On macOS, the standard `open` command
- On Linux, `xdg-open`

## Controls

- Type to filter
- `Up` / `Down` to move selection
- `PageUp` / `PageDown` to jump
- `Ctrl+U` / `Ctrl+D` to jump up or down
- `Left` / `Right` or `Tab` to switch between tabs
- `Ctrl+R` to refresh the current tab
- `Ctrl+B` to hide or show the `Config` tab
- `Enter` to open the selected URL
- On `GitHub` and `GitLab`, `Enter` opens an action menu with `Open in browser` and `Copy repository address`, while the preview pane keeps showing the project details
- `Backspace` to delete search text, or go back one level inside DockerHub tag/action views
- `Esc` to quit from the main page, or go back from DockerHub tag/action views

Tabs:

- `History` shows local browser history and bookmarks
- `GitHub` shows repositories visible to the configured GitHub account or user, with `path/repo` shown in the list and details in the preview pane
- `GitLab` shows visible GitLab projects by `path_with_namespace`, with details in the preview pane
- `DockerHub` shows repositories belonging to the configured DockerHub user, with the list focused on the repository name and the secondary menu showing the repository description in the preview pane
- `Config` is shown by default. Press `Ctrl+B` to hide or show it. It is organized as a multi-level menu: level 1 is the source or app area such as `Browser`, `GitHub`, `GitLab`, `DockerHub`, or `System`; level 2 is the feature group such as `Source`, `Auth`, `API`, `Refresh`, `Display`, `Diagnostics`, or `Automation`; level 3 is the concrete setting. Top-level groups that contain child items are shown with a `▼` prefix, and child configuration rows are indented by four spaces. Type in the search box to filter configuration items by group, name, environment variable, command-line flag, or setup guidance. Use `Up` / `Down` to select a row and `Enter` to toggle or edit the selected value.

For DockerHub results, `Enter` opens a tag picker when tags are cached. If no tags are available, it opens the action menu directly so the repository can still be opened in the browser or copied as a `docker pull` command. The action menu keeps the repository description visible in the preview pane.

GitHub, GitLab, and DockerHub results are cached locally under the user cache directory. The app opens immediately from cache when available and attempts background refreshes on each source's configured interval.

If any remote source or protected browser source cannot be read, the app keeps the remaining sources available instead of exiting. Remote cache data remains usable when refreshes fail.

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
- `WEB_FZF_HISTORY_REFRESH` history refresh interval, default `5s`
- `WEB_FZF_GITHUB_REFRESH` GitHub refresh interval, default `60s`
- `WEB_FZF_GITLAB_REFRESH` GitLab refresh interval, default `60s`
- `WEB_FZF_DOCKERHUB_REFRESH` DockerHub refresh interval, default `60s`

Refresh interval values are seconds only, for example `60` or `5s`.

DockerHub shows only public repositories when no token is provided. With a Personal Access Token (PAT), private repositories are also visible.

### Token setup

The `Config` tab shows token status as `present` or `missing`; token values are never displayed. Editing values in the Config tab changes the current in-app configuration view only. Persist values by exporting the shown environment variables or passing the shown command-line flags when launching `uf`.

GitHub:

```bash
gh auth login
export GITHUB_TOKEN="$(gh auth token)"
```

For public user repositories without a token:

```bash
export GITHUB_USER=myuser
```

GitLab:

```bash
glab auth login
export GITLAB_TOKEN="$(glab auth token)"
```

For self-hosted GitLab:

```bash
export GITLAB_API="https://gitlab.example.com/api/v4"
```

DockerHub:

```bash
export DOCKERHUB_USERNAME=myuser
export DOCKERHUB_TOKEN="<personal-access-token>"
```

Future automatic token acquisition should use explicit user action from the `Config` tab. The intended scheme is to read GitHub tokens through `gh auth token`, GitLab tokens through `glab auth token`, and DockerHub credentials only from an existing Docker login or a user-provided PAT. The UI should continue to show only presence, never raw token values.

## Refresh behavior

Remote tabs always load local cache first. Background refreshes then run according to each source's configured interval. Local data is replaced only after a refresh succeeds with non-empty results. When a background refresh fails or returns no entries, the previous local cache remains available and is not expired or removed.

Press `Ctrl+R` to request an immediate refresh for the current tab. If that tab is already refreshing, the request is ignored.

## Debugging

Use `--debug` to write diagnostic logs to stderr. Redirect stderr to a file so the output does not interfere with the TUI:

```bash
uf --debug 2>debug.log
```

If `stderr` is still attached to the terminal, tab/source diagnostics are suppressed while the full-screen UI is running so the TUI does not get corrupted. This applies both to `--debug` output and to source-level diagnostics such as browser access warnings.

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
| `[dockerhub] API_ERROR: ...` | Error message returned by the DockerHub API |
| `[dockerhub] parsed N repository entries` | Number of rows extracted from the API response |
| `[dockerhub] page N: N entries` | Final entry count for the page |

Example showing a token auth problem:

```bash
DOCKERHUB_USERNAME=myuser uf --debug 2>debug.log
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
