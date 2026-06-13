# AGENT.md

## Scope

This repository contains a Rust terminal search tool that launches browser URLs from local browser data and remote GitHub/GitLab/DockerHub sources.

## Current implementation rules

- Keep the runtime dependency footprint small enough to remain practical for local development and execution.
- Preserve the current `Entry { title, url, source, detail }` data model unless a broader refactor is required.
- Keep browser history, bookmarks, GitHub, GitLab, and DockerHub behavior aligned with the existing CLI.
- Remote sources should use local cache first and refresh asynchronously instead of blocking startup.
- Do not add fixed remote cache expiry. If a remote refresh fails or returns no rows, the previous local cache remains valid and usable indefinitely.
- Refreshes should replace local UI data only after they complete successfully with non-empty results.
- `Ctrl+F` requests an immediate refresh for the current tab. Do not start a duplicate refresh when that tab is already refreshing.
- Keep keyboard handling routed through the input/keymap/action layers; do not add feature behavior directly to raw input parsing.
- Keep refresh intervals source-specific and configurable through environment variables: `WEB_FZF_HISTORY_REFRESH` defaults to `5s`; `WEB_FZF_GITHUB_REFRESH`, `WEB_FZF_GITLAB_REFRESH`, and `WEB_FZF_DOCKERHUB_REFRESH` default to `1min`.
- Prefer third-party TUI libraries when improving the interface instead of hand-drawn ANSI output.
- Avoid introducing new build systems or packaging layers unless the task explicitly requires them.

## Entry points

- CLI: `cargo run --release`
- Installed command: `web-fzf`

## Verification

Run the focused tests before finishing changes:

```bash
cargo test
cargo fmt --check
```

## Editing guidance

- Keep changes tightly scoped to the requested behavior.
- Update tests when changing parsing, matching, caching, source selection, or tab behavior.
- If you change CLI flags, environment variables, or refresh/cache behavior, update `README.md` and this file in the same change.
