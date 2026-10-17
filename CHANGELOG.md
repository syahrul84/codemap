# Changelog

## [0.1.3] - 2026-10-17

Fixes for the issues reported through `report_wrong_answer` (#3–#9):

- `api_usage`: a variable URL prefix with known values (`kind === 'x' ? 'area/x' : 'area'`, also through a small helper function or a local constant) is expanded and each full URL matched; on irems-sg this resolved 45 of 64 unresolved calls. Suggestions for calls that stay unresolved are ranked by how many words of the URL tail match (#8).
- `api_usage`: method spoofing is read from a request body in a variable, a helper's returned object, `payload._method = 'PUT'` and `form.append('_method', 'PUT')` (#3).
- `api_usage`: a call no longer matches a route by crossing wildcards (`/widgets/{}/archive` against `widgets/history/{widgetId}`): it is reported as having no route (#5).
- Routes registered by Laravel Passport (`oauth/token`, `oauth/authorize`, …) and Sanctum (`sanctum/csrf-cookie`) are known without artisan mode (#4).
- `composer.lock` and other JSON files without a `.json` name are no longer read as JavaScript (#6).
- Every tool refuses unknown arguments and names the accepted ones (`uri` → `uri_contains`); so does `verify`'s `claims` (#7).
- Wrong-answer report examples may use Laravel's standard app names (`AuthServiceProvider`, `App\Providers`, …) and any class name declared by an installed package (#9).

New, from the feature requests (#10–#15):

- Routes registered by packages are known without artisan mode: Sanctum, Broadcasting (`broadcasting/auth`), Fortify (following the features enabled in `config/fortify.php`), Horizon, Telescope, Pulse, Cashier (`stripe/webhook`) and Livewire (v2 and v3), with prefixes from their config files; Passport follows its version (automatic from v11 unless `ignoreRoutes()`) (#10).
- `get_schema` can read tables and columns from a live database (`[[databases]] schema = "live"`: information_schema for MySQL / MariaDB and PostgreSQL, pragma for SQLite, read-only, re-read at most every 10 minutes) or from a structure dump (`dump = "file.sql"`). Each column says where it comes from (`source`), and differences from the migrations are kept as `drift` (#11).
- `api_usage` checks what a call sends against what the action accepts: the Form Request's `rules()`, an inline `validate()`, or the fields it reads (also in the controller's helper methods). New findings `missing_required` and `unknown_field` (`status: payload`), with the evidence; payloads built through spreads or helpers are `payload_unchecked`, not guessed (#12).
- `api_usage` takes `git_changes` / `changed_files` (only the calls the change touches, including calls to backend routes whose route file or controller changed) and `new_only` (hide findings saved by the new `codemap baseline`). `codemap check api [--changed]` prints new findings and exits 1 on a new missing or wrong-method call, for CI and pre-commit hooks (#13).
- Fields findings carry `evidence`: where the response shape came from (Resource and its `toArray` file:line, or the action's `return`), every key it returns with conditional ones marked, and the paths the frontend read; brief answers name the shape too (#14).
- A generated stress project (4,000 routes, 300 modules) is part of the tests; when a prefix is not expanded, the reason says whether a limit was hit or the value comes from code codemap does not follow (#15).

Fixes from the dev-build retest (17 Oct):

- `_method` spoofing follows conditions and scope: `if (editing) payload._method = 'PATCH'` makes the call "POST or PATCH" (either method fits), a helper that sets it under a parameter is decided by the argument at each call (`widgetForm(record, false)` stays POST), and a same-named variable in another function no longer inherits it. This was a regression in the #3 fix.
- A payload copied into `FormData` with `Object.entries(…).forEach(…)` or `for … of Object.entries(…)`, also through a local `append` helper, carries the object's keys. A body handed to other code, or one whose keys can't be read, is unchecked instead of "sends nothing".
- `accepts_at` for rules in a controller helper names the `validate()` line and the call: `NoteController.php:28 (in project(), called at line 14)`; it never says `:0`.
- Less noise from the payload check: unread query parameters of a GET (or of a call with an unknown method) are a note (`unknown_query_note`, listed with `status: "payload_all"`); no `unknown_field` when the payload was not fully followed; a missing required field and an unknown sent field on one call are reported as `possible_rename` (`unit_id → tenant_id`); `page` and `cursor` are accepted only when the action or a helper paginates. The evidence says `sends_complete` and `accepts_complete` separately.

Fixes for two more reports (17 Oct):

- A URL value with a small known choice anywhere in the template is expanded and each full URL matched, including a parameter typed as a string union or an alias of one (`` `/${kind}-permit/${id}` `` with `kind: PermitKind`, `type PermitKind = 'loading' | 'unloading'`). When none of the full URLs has a route, the call is reported as missing rather than matched loosely.
- A variable inside a URL segment stands for one word: `{}-permit` no longer fits `spfm-work-permit`.
- `_method` spoofing across files: a function that posts a body parameter takes the method its callers pass in that argument (`invitationAction(id, 'void', { …, _method: 'PUT' })` in another file makes it "POST or PUT"), and a body built by an imported helper (also copied into `FormData`) takes the method that helper sets.

Fixes from the accuracy check on 9d73973 (17 Oct):

- More `_method` spoofing is followed: a caller that builds the body with a helper (inline or through a variable), a function that spreads its body parameter into a new body (`{ ...record, reason }`) or converts it (`toFormData(payload)`), Vue 2 component state (`this.record._method = 'PATCH'` then posting `this.record` or `jsonFormData(this.record)`), and `?_method=PUT` in the URL. On the test apps, wrong-method findings went from 6 to 3 and from 25 to 20, the ones checked by hand as real.
- Calls to imported functions from packages (`reactive`, `ref`) no longer count as body helpers; facts from both script blocks of a `.vue` file are kept.
- URLs naming a scaffold placeholder (`/_api_url_/{}`, `/unitURL`) get their own `placeholder` status instead of `missing`, and do not fail `codemap check api`.
- A call whose URL has several known values says which ones have no route (`variants_without_route`), also when another one matched; values from a component's computed property (`this.reportSegment`) are expanded.
- A broken call inside an object's method that nothing calls (`billingDraftApi.post`) is marked as dead code.
- URL variants are built per value of the variables the URL depends on, so one variable that decides two parts (`${kind}${kind === 'rates' ? '/bulk' : ''}`) only gives the URLs it can make. When a part cannot be tied to a known value, its URLs are tried but never reported as missing.

Also:

- Two folders of one repository (a monorepo's `frontend/` and `backend/` linked with `[[links]]`) get their own index; they shared one and re-indexed each other on every call. Such folders are indexed once more after updating.
- Parsing threads get a larger stack, so very deeply nested files cannot crash a scan.

## [0.1.2] - 2026-10-17

- Fixes from the 17 Oct retest (round 8):
  - `find_usages` counts functions called by bare name in Vue templates (`{{ formatMyr(x) }}`), which in Nuxt are auto-imported helpers; such a helper is no longer reported unused.
  - A JS constant or function used only inside its own file counts those uses (`same file`) and is no longer reported unused.
  - The send result names the AI app (recorded when it connects), and an added comment on an existing issue says so: a short "seen again" note with this build's answer, not the whole issue again.
- Fixes from the 17 Oct retest (round 6):
  - `codemap reports --send` refuses a closed report, and checks the report on the current build first: if its check passes or its example no longer reproduces, the report is closed as fixed and nothing is sent.
  - The issue names the build the example was checked on just now, with that build's answer, plus "first seen on" the build it was reported on.
  - A report closed because its example no longer reproduces records that ("does not reproduce (reproduced on …)") with the new answer; the list shows a closed report's example as "reproduced on …".
- Fixes from the 17 Oct retest (round 4):
  - Test commands never include a base test class (abstract, or without test methods), which made PHPUnit run nothing; the tests that extend it are run instead.
  - A brief change summary that leaves out a long test list says so (`tests_note`).
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
