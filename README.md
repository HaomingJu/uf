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

## Runtime requirements

- Rust toolchain
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
uf
```

## Controls

- Type to filter
- `Up` / `Down` to move selection
- `PageUp` / `PageDown` to jump
- `Ctrl+U` / `Ctrl+D` to jump up or down
- `Left` / `Right` or `Tab` to switch between tabs
- `Ctrl+F` to refresh the current tab
- `Enter` to open the selected URL
- On `GitHub` and `GitLab`, `Enter` opens an action menu with `Open in browser` and `Copy repository address`, while the preview pane keeps showing the project details
- `Backspace` to delete search text, or go back one level inside DockerHub tag/action views
- `Esc` to quit from the main page, or go back from DockerHub tag/action views

Tabs:

- `History` shows local browser history and bookmarks
- `GitHub` shows repositories visible to the configured GitHub account or user, with `path/repo` shown in the list and details in the preview pane
- `GitLab` shows visible GitLab projects by `path_with_namespace`, with details in the preview pane
- `DockerHub` shows repositories belonging to the configured DockerHub user, with the list focused on the repository name and the secondary menu showing the repository description in the preview pane

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
- `WEB_FZF_GITHUB_REFRESH` GitHub refresh interval, default `1min`
- `WEB_FZF_GITLAB_REFRESH` GitLab refresh interval, default `1min`
- `WEB_FZF_DOCKERHUB_REFRESH` DockerHub refresh interval, default `1min`

Refresh interval values may be plain seconds such as `60`, seconds such as `5s`, or minutes such as `1min`.

DockerHub shows only public repositories when no token is provided. With a Personal Access Token (PAT), private repositories are also visible.

## Refresh behavior

Remote tabs always load local cache first. Background refreshes then run according to each source's configured interval. Local data is replaced only after a refresh succeeds with non-empty results. When a background refresh fails or returns no entries, the previous local cache remains available and is not expired or removed.

Press `Ctrl+F` to request an immediate refresh for the current tab. If that tab is already refreshing, the request is ignored.

## Debugging

Use `--debug` to write diagnostic logs to stderr. Redirect stderr to a file so the output does not interfere with the TUI:

```bash
uf --debug 2>debug.log
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
