# AGENT.md

## Scope

This repository contains a Rust terminal search tool that launches browser URLs from local browser data and remote GitHub/GitLab sources.

## Current implementation rules

- Keep the runtime dependency footprint small enough to remain practical for local development and execution.
- Preserve the current `Entry { title, url, source, detail }` data model unless a broader refactor is required.
- Keep browser history, bookmarks, GitHub, and GitLab behavior aligned with the existing CLI.
- GitHub and GitLab should use local cache first and refresh asynchronously instead of blocking startup.
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
- If you change CLI flags or environment variables, update `README.md` in the same change.
