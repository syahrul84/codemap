# Changelog

## [Unreleased]

- `found: false` and `not_found` answers list `not_scanned`: top-level folders holding code that the scan does not read (outside `[scan] include`), so the AI does not take "not found" as "does not exist" and write a duplicate.
- `search_symbol` ranks the app's own code above test, fixture and stub copies with the same name.
- `codemap setup --dry-run` shows what would change and changes nothing; setup keeps the old `CLAUDE.md` / `settings.json` as `<name>.codemap-backup` before changing them; `--no-hook` installs without the search hook.
- Workspaces: beside a Laravel or web app, Swift, Android and Rust folders are found as apps too (a `backend/` + `ios/` folder no longer leaves the iOS app out). A folder of mobile apps alone stays one project.
- Laravel names: `databases` → `database` and `knowledge_bases` → `knowledge_base` (`bases` → `basis` only for the whole word); `drives`, `dives`, `olives` and `gloves` keep their `e`. Checked against Laravel's own `Str::singular`, including its `reserves` → `reserf`.
- Tool descriptions and server instructions cover Next.js / React, Swift, Android, Rust and Supabase, the payload checks, `unused: true` and why each tool beats reading files. README: a one-line summary, measured answer sizes, a quick start for every platform with a "see it working" step, the `setup` / `hook` commands and `report_wrong_answer`.

## [0.1.8] - 2026-10-22

- Exact references from an `index.scip` file: when the project root has one (written by rust-analyzer, scip-typescript, scip-php, scip-java, scip-python and other SCIP indexers), `find_usages` uses it for the symbol it resolved. Uses matched by name ("possible") that the index confirms are listed; the others are dropped; references only the index knows are added. The answer says how many were confirmed, and when files changed after the index was written. Without the file, nothing changes.
- One view model for every UI framework: Vue / React components, SwiftUI views and Compose functions, and which view shows which, in shared `views` / `view_uses` tables. `verify` (`components`), `find_usages` and the release check read it, so the release check now also lists SwiftUI views and composables that nothing shows.
- Reports become tests: `codemap reports --to-fixture <id>` writes a reproducing report's made-up example to `fixtures/report-cases/<name>/` (`project/` and `case.json`), and a test runs every case, so a fixed bug cannot come back.
- Every tool accepts the argument names AI apps use in practice instead of refusing them: `name` / `symbol` / `query` for `find_usages`' `target`, `files` for `changed_files`, singular claim names in `verify` (`class`, `method`, `route`, `column`, …), `uri` / `path` for `list_routes`, `route` / `url` for `api_usage`'s `endpoint`, and booleans sent as strings (`"true"`). Unknown names are still refused with the accepted ones. In a week of real use, 11 of 390 calls were refused for these.
- `search_symbol`: `namespace_prefix` works for Rust (`my_crate::store`) and Swift / Kotlin (`com.app.ui`) paths, not only PHP. A query with several word parts (`cart store`, `order_status`, `MallCard`) matches names holding all of them in any order when nothing matches as written.
- `find_usages` change impact works for Swift, Kotlin, Java and Rust files: who uses the changed file's types and functions, and the test commands (`cargo test -p <crate> --test <name>`, `gradle :app:test --tests <class>`, `swift test --filter <name>`) instead of `npx vitest`. It also loads the symbols once per call, not once per changed file (a 250-file branch loaded them 250 times).
- Rust uses keep the path they were written with (`views::replace(..)`, `crate::store::open()`, or the `use` line that brought the name in), so `find_usages` and change impact tell `config::load()` from `store::load()`. A name declared in the same file, or bound by `let` there, is that file's own. Inside macros (`format!`, `assert!`), `s.replace(..)` is a method and `names.len()` a variable, not calls of a function named `replace` or `names`. A top-level function's `x.name()` uses are listed as possible, not certain. On codemap's own code, changing `precise.rs` went from 18 affected files to the 1 that uses it.
- `codemap reports`: an open report whose answer changed since it was filed but still does not match the check says `answer changed`, so a person can decide (`not_found` that became `unresolved`).
- `verify` with `components` checks SwiftUI views and Compose functions in Swift and Android apps (it said "no Vue / Nuxt app detected").
- Swift, Android and Rust fixture projects under `fixtures/`, indexed end to end by the tests (screens, views and composables, usages, translations, verify).
- README: a five-minute start at the top.

## [0.1.7] - 2026-10-20

- `codemap setup` makes codemap the AI app's first stop for questions about code (Claude Code): it registers the MCP server, writes a rule block into `CLAUDE.md` between markers, and adds a hook pair that refuses grep / rg / find code searches until a codemap tool has run in the session (logs, git history, other machines, dependencies and build output are never refused). `--check` reports what is installed, `--remove` undoes it, `--scope project` writes into the folder; `CLAUDE_CONFIG_DIR` is honored. The hooks are `codemap hook check|used` and fail open. `[agent] search_window_minutes` and `allow_search` in `.codemap.toml`; `codemap usage` counts refused and allowed searches. The server instructions now open by saying to prefer these tools over grep (#34).
- PHP: a cast in front of `match` (`(int) match ($x) { … }`) no longer stops parsing (#30).
- Routes defined in a closure held in a variable (`$shared = function () { … }; Route::prefix('{lang}')->group($shared);`) are found; variables in route files are followed (#35).
- Migrations: `Schema::hasTable` / `hasColumn` guards are evaluated against the tables replayed so far, and an early `return` stops the rest of `up()`, so two guarded migrations no longer report "created twice" (#32).
- `doctor`: a warning repeated in many files is one line with a count (`--all-warnings` lists them), parse failures come first, and `ALTER INDEX … RENAME`, `DROP INDEX`, sequences, comments and grants are not schema warnings (#33).
- A linked backend counts only the calls no better-matching backend explains, so a CMS linked next to the API no longer shows "42 of 140 calls match"; one the app never calls says so (#31).

## [0.1.6] - 2026-10-19

- A Windows program: `codemap-x86_64-pc-windows-msvc.zip` on each release, built and tested on Windows (the whole test suite, plus indexing a project with the built program). WSL and Docker Desktop still work.
- `codemap update` works on Windows: it downloads the zip with `curl.exe`, unpacks it with Windows' own `tar.exe`, and moves the running `codemap.exe` aside (Windows cannot overwrite a running program); the old copy is removed the next time codemap starts.

## [0.1.5] - 2026-10-18

Fixes for issues #16–#29:

- `verify`: a method missing from a model and from its vendor parent is `not_found`; codemap reads the installed vendor files (Composer's maps, or the usual package folders) instead of saying "may be inherited". A parent with `__call` gives `unresolved` (#16).
- `verify`: a class in a stale Composer classmap (its file is gone) is `not_found`, not a vendor class (#19). A composable or store asked for as a function exists (#24).
- `find_usages` change impact: tests of the classes that call a changed PHP class are suggested (`ViewWidgetTest` for a change to `WidgetActions`, used by `ViewWidget`) (#17).
- `get_schema`: indexes over several columns (`$table->unique(['shelf_id', 'slug'])`, composite primary keys) are listed with their name and migration, also after `dropUnique` / `renameColumn` (#18).
- Cache map: keys built in a variable first (`$key = 'page:'.$id; Cache::remember($key, …)`), from `array_map(fn … => 'row:'.$id)`, `array_fill_keys`, and `Cache::many` / `putMany` (#20).
- `search_symbol` finds functions used only in their own file (a `<script setup>` helper) and the fields of TypeScript interfaces and object types (`ShopCard.visits_7d`) (#21, #22).
- `api_usage`: a URL taken from a constant object (`ENDPOINT[kind]`, `ENDPOINT.shops`) is expanded to each of its values (#23). With `git_changes` / `changed_files`, the calls in scope are listed, matched ones too, and agree with the summary (#27). A static helper on a Resource (`OtherResource::gallery(…)`) is no longer described as that Resource; only `new`, `::make` and `::collection` are (#28).
- `codemap usage` finds the logs of apps served on their own (`serve --project backend`) when run from the workspace, and the other way round (#25).
- Release check: exported constants, types and interfaces nothing uses, exports only used in their own file, and translation keys added on the branch that nothing reads are listed (#26).
- `find_usages` on a database column (`orders.collected_at`): the model's `$fillable` / `$casts` / `$hidden` entries, queries and properties in files that use the model or name the table, factories, frontend reads and tests; other PHP mentions are listed as possible, and the migration that created it is named (#29).

## [0.1.4] - 2026-10-18

Rust support, for crates and Cargo workspaces:

- Structs, enums, traits, functions, methods from `impl` blocks (named after the type even when the `impl` is in another file, through its `use` imports), fields, enum variants, consts, statics, type aliases and `macro_rules!` are symbols named by their Rust path (`my_crate::store::Store::open`), for `search_symbol`, `get_symbol` and `verify` (`Type::method`). Module paths follow Cargo's layout: `lib.rs` / `main.rs`, `mod.rs`, `src/bin/`, `tests/`, and a binary-only crate under its binary's name.
- `find_usages` lists where a type, function, constant or method is used, including names inside macro calls (`println!("{}", MAX)`); uses in `#[cfg(test)]` modules and `#[test]` functions are listed as tests. Methods are matched by name as for Swift and Kotlin.
- `list_pages` lists the binaries and their command-line subcommands (enums deriving clap's `Subcommand`, with `#[command(name = …)]` and doc comments).
- `project_overview` gives a Rust section: crates with their folder, library and binaries, file counts and which other crates of the workspace each uses; symbol counts and commands. PHP files without a `composer.json` (test fixtures) no longer make a repository look like a PHP app.
- `get_symbol` prefers the name spelled exactly as asked (`Store` over `store()`), and a method over a field of the same name.
- In a Cargo project every top-level folder is read and `target/` is skipped. codemap's own 135 Rust files in 10 crates index in 0.4 s.

Android support, for Kotlin and Java apps with Jetpack Compose:

- Classes, interfaces, objects, enums and their members (functions, properties including `class X(val a: Int)`, enum entries, typealiases; companion members as static) are symbols, named by package (`com.example.ui.Root`), for `search_symbol`, `get_symbol` and `verify` (`Class::method` or `Class.method`).
- `get_component` takes a `@Composable` function: its parameters (callbacks marked), the state it keeps (`remember`, `rememberSaveable`, `collectAsState`, `viewModel`, … with starting values), the composables it shows, where it is shown and how it is opened.
- `list_pages` lists the app's screens: activities, services, receivers and providers from `AndroidManifest.xml` (with the launcher activity and what its `setContent` shows), Navigation Compose destinations (`composable("route")`, `composable<Route>`, `dialog`), dialogs and bottom sheets that show the project's composables, Fragments, and app-made navigation: a `when` whose branches show different composables (`is Route.Detail -> DetailScreen()`, `0 -> TimesScreen()`).
- `find_usages` lists where a class or composable is used, and members by name as for Swift. String keys (`app_name`) are found in code (`R.string.x`, `stringResource`) and XML (`@string/x`).
- Translations from `res/values*/strings.xml` (`<string>`, `<plurals>`, `<string-array>`; `values-pt-rBR` → `pt-BR`, `values-b+sr+Latn` → `sr-Latn`), with `res/values/` as the base and `translatable="false"` strings expected in no other language.
- `project_overview` gives an Android section: modules, packages, counts, composables, screens and the manifest's components.
- In a Gradle project (at the root, or one folder down beside an iOS app) every top-level folder is read; `build/`, `.gradle/`, `.idea/` are skipped. The countSolat Android app (36 Kotlin files) indexes in about 0.1 s.
- Two Kotlin forms the grammar cannot read are rewritten before parsing, keeping lines and columns: `var x by …; private set`, and a modifier word used as a name (`var open by remember { … }`).

Swift support, for Swift packages and Xcode apps (iOS, macOS) with SwiftUI:

- Classes, structs, enums, actors, protocols and their members (methods, `init`, properties with their wrappers, enum cases, typealiases) are symbols for `search_symbol`, `get_symbol` and `verify` (`Type::method` or `Type.method`). Members added in an extension belong to the type.
- `get_component` takes a SwiftUI view: what callers pass in (`let`, `@Binding`, `@ObservedObject`), its own state (`@State`, `@StateObject`, `@AppStorage`, …) with starting values, what it reads from the environment, the views it shows, where it is shown, and how it is opened.
- `list_pages` lists the app's screens: windows (`WindowGroup`, `Window`, `Settings`, `MenuBarExtra`, `DocumentGroup`), tabs, `NavigationLink` and `.navigationDestination` pushes, sheets, full-screen covers, popovers and inspectors, each with the view it shows and where it is opened.
- `find_usages` lists where a type is used; for a member it lists the uses in its own file and, since there is no type checker, other uses of the same name as possible.
- Translations: keys from `xx.lproj/*.strings` (UTF-8 or UTF-16) and `.xcstrings` catalogs; texts used through `Text("…")`, `Label`, `Button`, `.navigationTitle` and other SwiftUI calls (shown as written when not translated), `NSLocalizedString` and `String(localized:)`. `InfoPlist` tables are left out.
- `project_overview` gives a Swift section: package or Xcode project, app entry points, counts of types, views and screens, imported frameworks and folders.
- In a Swift project every top-level folder is read (not only the usual Laravel / JS folders); `.build`, `DerivedData`, `Pods`, `Carthage` and asset catalogs are skipped. Five apps with 9–38 files each index in about 0.1 s.

Faster, measured on a Laravel API with 6,105 files and 6,126 routes and its two Vue frontends (10,305 files):

- First index of the API 9.4 s → about 4.5 s: translations are scanned in parallel, inserts reuse prepared statements, and a first index builds the reference indexes once at the end. A workspace's apps are indexed side by side.
- `find_usages` on a controller behind 1,024 routes: 31 s → 0.2 s, and its brief answer lists 50 routes with the total (126 KB → 8 KB).
- `api_usage` on a frontend with 5,300 calls: 5.6–6 s → about 1 s: routes are looked up by length and first and last word, the controllers are parsed on all cores once, and "closest route" suggestions compare each distinct word once.
- `get_schema` with `background`: 2.9 s → 0.2–0.7 s. Several reference lookups (`X` or its members, imports, tables) ran as a scan of every reference; they now use indexes, with a new `refs(kind, target)` index.
- The release check searches for leftover names on all cores while its other checks run.

- `codemap usage` shows how codemap is used in a project: AI app sessions; tool calls per tool with time taken, answer size, answers that found nothing, refused arguments, errors and calls the AI had to repeat; wrong answers reported by the AI or caught by codemap, and names the AI asked for that do not exist; index runs with why they ran, files read and time taken (also for a linked project read by its frontend). `--json` gives every number, per tool and per day. Each app keeps `usage.jsonl` next to its index for 90 days, with code cut out of the arguments; `CODEMAP_NO_USAGE_LOG=1` turns it off.

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
