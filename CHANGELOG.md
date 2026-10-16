# Changelog

## [Unreleased]

- Release check: `release_steps`, the server steps a release needs (installs, migrations, caches, queue and server restarts, rebuilds, new environment variables), worked out from what changed.
- Project rules tied to files (`[[rules]]` in `.codemap.toml`), returned by the release check and by change impact when a matching file changes.
- Leftover mentions: names deleted or renamed on the branch that code, tests, comments or docs still use.
- Tests that depend on live data: browser and PHP tests that check data they never create.
- New type errors only: `verify` with `type_errors` (the type check's output) reports only errors not in the saved starting point.

## [0.1.1] - 2026-10-16

- `codemap update` installs the latest release, checked against its SHA256SUMS (`--check` only reports, `--force` reinstalls).
- A once-a-day check for new releases; `codemap doctor` and `project_overview` mention a newer version. Turn it off with `CODEMAP_NO_UPDATE_CHECK=1`.

## [0.1.0] - 2026-10-16

First release. codemap reads a Laravel, Vue 2, Vue 3, Nuxt (2/3/4) or Next.js project and serves facts about it to Claude over MCP, so Claude checks what exists instead of guessing.

- **Ten MCP tools:** project overview, symbol search and details, Vue / React components, pages, routes, the frontend ↔ backend API link, usages and change impact (with the tests to run), database schema (plus settings, migrations not yet applied, cache keys, background work) and `verify`.
- **Release check** (`verify` with `release_check: true`): missing endpoints, fields not returned, translation gaps in every shipped language, validation messages that would show raw field keys, unused code on the branch, migrations not applied to configured databases, config and env keys, settings without defaults and house copy rules. Lines can be marked as intended with `codemap-ignore`.
- **Code map** (`codemap map`): one self-contained web page from screens to API calls, endpoints, controllers, models and tables, with the release check's problems.
- **AI mistakes log** (`codemap misses`): names Claude asked for that do not exist, ranked, with the real name and what to do about it.
- **Workspaces:** several apps in one folder, each with its own index, linked frontend to backend.
- Ready-made programs for macOS (Apple Silicon, Intel) and Linux (x86-64, ARM64), or build with Cargo or Docker.
