# Changelog

## [Unreleased]

- Fixes from the 17 Oct retest (round 3):
  - Change impact links tests that call a route with `$this->call('POST', '/uri', …)` or `$this->json('PATCH', '/uri')`, and requests after set-up calls (`$this->actingAs($u)->postJson(…)`, `withHeaders(…)->post(…)`).
  - Change impact links tests that run a console command (`$this->artisan('name')`, `Artisan::call('name')`) to the command's class, so a change to anything the command uses lists them.
  - The development version stamp follows every commit (it watched only `HEAD`).
- Fixes from the 17 Oct retest (round 2):
  - `find_usages` follows services resolved from the container: `app(X::class)`, `resolve(X::class)`, `App::make(X::class)`, `app()->make(…)`, `$this->app->make(…)`, chained or assigned to a variable first.
  - Live-data tests: browser tests of a page opened from a file (`file://`) or written with `setContent` are skipped.
- Fixes from the 17 Oct retest:
  - Release steps: static Nuxt apps (`nuxt generate`, `ssr: false`, a static preset) get `generate` and "deploy .output/public" instead of a Node server restart; Next.js `output: 'export'` too. Laravel + Vite cites only files the bundler reads.
  - Live-data tests: text the test typed or sent itself, negated checks, sentences, text patterns and text from the app's own code or translations no longer count.
  - Made-up examples: everyday words (`save`, `phone`, `error`) pass; only the project's compound names and its own name are blocked.
  - Resending a report for the same answer updates it instead of counting a repeat, and replaces the evidence.
  - The send result names the AI app when it does not offer question forms, with the terminal command first.
  - Development builds report `<version>-dev+<commit>`; release builds (and a checkout of the release tag) report the plain version.
- Wrong-answer reports: an 11th tool, `report_wrong_answer`, records an answer codemap got wrong (every answer now has `meta.call_id`). codemap also records contradictions, tool failures and slow answers on its own. `codemap reports` lists, re-checks, exports, closes and clears them. A report with a made-up example that reproduces (checked to hold nothing from the real project) can be sent as a GitHub issue after the person agrees.
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
