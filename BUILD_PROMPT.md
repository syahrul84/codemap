# Build Prompt — `project-index` (pidx): Rust MCP server for Laravel / Vue / Nuxt projects

Paste this file into the project root and tell Claude Code: "Read BUILD_PROMPT.md fully. Enter plan mode, propose the plan for Phase 0, then wait for my approval."

## 0. How you (Claude Code) must work on this project

* Plan first. Start each phase in plan mode. Show the plan, wait for approval, then implement.
* One phase at a time. Stop at the end of each phase, summarise what was built, show test results, and wait.
* Everything runs in Docker. Never run `cargo`, `rustc`, or the binary on the host. Use `docker compose run --rm dev <cmd>` (see §3).
* Verify crate APIs before using them. Crate APIs change (especially `rmcp`). Check current docs (use the context7 MCP if available, or docs.rs) and pin versions in `Cargo.toml`. Do not write code from memory for crate APIs you have not checked.
* Tests are not optional. Every extractor gets fixture tests + `insta` snapshots. A phase is done only when `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` all pass inside Docker.
* Commit after each phase with a clear message (conventional commits).
* Ask, don't guess. If a requirement is ambiguous, ask me. If you can't resolve a Laravel/Vue behaviour, mark it `unresolved` in the output rather than inventing data.
* stdout is sacred. In MCP serve mode, stdout carries only MCP protocol messages. All logs go to stderr (`tracing` → stderr).

## 1. What we are building

A single Rust binary, `pidx`, that:

1. Scans a project (Laravel 9+ first; Vue 2/3 and Nuxt 2/3/4 in later milestones).
2. Extracts structured facts: PHP symbols, routes, models, relationships, DB schema (from migrations), Form Request rules.
3. Stores them in a local SQLite index with incremental updates.
4. Serves them over MCP (stdio) so Claude can query facts instead of reading files.

### Goals (in priority order)

1. No hallucination — Claude verifies that classes, methods, routes, tables and columns exist before using them. Every answer is either a fact with `file:line`, or an explicit `found: false` with suggestions.
2. Save tokens — compact, paginated, budgeted responses. `detail: "brief" | "full"` on every tool.
3. Faster decisions — one call gives the overview Claude would otherwise build by reading 10+ files.
4. Code reuse — surface existing helpers/classes (fuzzy search now; `find_similar` in Milestone 3).

### Context

* Projects are usually separate repos: Laravel API in one repo, Nuxt/Vue frontend in another. Design for linking multiple projects later (Milestone 2), but Milestone 1 indexes one Laravel project.
* Must support Laravel 9, 10, 11, 12 and newer, including apps upgraded to 11+ that still use the old 9/10 structure.

### Non-goals for Milestone 1

* No Vue/Nuxt parsing yet (Milestone 2).
* No embeddings/semantic search yet (Milestone 3).
* No call graph / impact analysis yet (Milestone 3).
* Does not index `vendor/` (but can check whether a vendor class exists — see §8).
* Never modifies the scanned project.

## 2. Design principles (apply everywhere)

* Accuracy over coverage. Unknown → `unresolved` with a reason. Never fabricate.
* Provenance on every fact. Each fact has `file` (relative path), `line`, and `source` (`static` | `artisan` | `inferred`).
* Confidence. Inferred facts (e.g. table name derived from model class name) are marked `inferred: true`.
* Freshness on every response. Include `indexed_at`, `git_commit`, `git_dirty`, `stale`.
* Explicit not-found. `{ "found": false, "query": ..., "suggestions": [...] }` — never empty arrays that look like "nothing exists".
* Small responses. Default `limit = 20`, cursor pagination, relative paths, no source code unless asked.
* No secrets. Never index or return `.env*` values, keys, tokens. (May record env key names later, never values.)
* Fast. Cold index of a 2,000-PHP-file project < 5s; incremental re-index of a few changed files < 300ms.

## 3. Docker setup (development and runtime)

### 3.1 Files to create

* `Dockerfile` — multi-stage:
  * builder: official `rust` image (slim, Debian-based), builds release binary. Use cargo-chef or layer caching so dependency builds are cached.
  * runtime: `debian:bookworm-slim` (or similar) with `git` and `ca-certificates`, non-root user, `ENTRYPOINT ["pidx"]`.
  * runtime-php (optional target): runtime + `php-cli` + common extensions, for artisan mode (§7.3).
* `docker-compose.yml` with services:
  * `dev` — Rust toolchain image with `rustfmt`, `clippy`; source bind-mounted at `/src`; cargo registry and `target/` in named volumes (bind-mounted `target/` is very slow on macOS); working dir `/src`.
  * `pidx` — runtime image for manual testing against a project mounted at `/workspace`.
* `.dockerignore` — exclude `target/`, `.git/`, fixtures output, etc.
* `Makefile` (or `justfile`) shortcuts: `make test`, `make lint`, `make build-image`, `make scan PROJECT=/path`, `make serve PROJECT=/path`.

### 3.2 Dev commands (Claude Code must use these)

```bash
docker compose run --rm dev cargo build
docker compose run --rm dev cargo test
docker compose run --rm dev cargo clippy --all-targets -- -D warnings
docker compose run --rm dev cargo fmt --all -- --check
docker compose run --rm dev cargo insta review   # if cargo-insta installed in dev image
```

### 3.3 Runtime layout inside the container

* Scanned project mounted read-only at `/workspace` (`-v /host/project:/workspace:ro`).
* Index stored in a named volume at `/data` → `/data/<project-id>/index.db`, where `project-id` = short hash of the project's host path (passed via `--project-id` or env `PIDX_PROJECT_ID`) or, fallback, of the git remote URL + root commit.
* All paths in responses are relative to the project root (never `/workspace/...`), so they match what Claude sees on the host.
* Git inside the container: set `safe.directory` for `/workspace` (otherwise git refuses with "dubious ownership" because of UID mismatch).

### 3.4 Docker gotchas to handle

* MCP over Docker stdio: run with `docker run -i` (interactive) and never `-t` (a TTY corrupts the stdio stream).
* File watching is unreliable on macOS bind mounts. Do NOT rely on inotify. Use the stat-based freshness check in §6.4.
* Apple Silicon: images must build for `linux/arm64` (and `linux/amd64` via buildx later).
* Container start time: keep the runtime image small; serve must be ready in < 1s.

### 3.5 How Claude Code will use the finished server (document in README)

```bash
docker build -t pidx:latest --target runtime .

# Register for a project (run inside that project's folder):
claude mcp add project-index -- docker run -i --rm \
  -v "$(pwd)":/workspace:ro \
  -v pidx-data:/data \
  -e PIDX_PROJECT_ID="$(pwd | shasum | cut -c1-12)" \
  pidx:latest serve --project /workspace
```

Also provide a `.mcp.json` example. Check current Claude Code docs for whether `.mcp.json` supports env-var expansion; if not, document using absolute paths.

## 4. Architecture (Cargo workspace)

```
project-index/
├── Cargo.toml                 # workspace
├── crates/
│   ├── pidx-core/             # index model, SQLite store, scanner, hashing, freshness, envelope types
│   ├── pidx-php/              # tree-sitter-php parsing → PHP symbols (framework-agnostic)
│   ├── pidx-laravel/          # Laravel adapter: detection, routes, models, migrations→schema, form requests
│   ├── pidx-mcp/              # MCP server (rmcp, stdio), tool definitions, response shaping
│   └── pidx-cli/              # `pidx` binary: scan, serve, query, stats, doctor
├── fixtures/                  # tiny hand-written Laravel projects (no vendor/)
├── Dockerfile
├── docker-compose.yml
├── Makefile
└── README.md
```

### Adapter design

```rust
trait FrameworkAdapter {
    fn id(&self) -> &'static str;                       // "laravel-legacy", "laravel-slim", later "vue2", "nuxt3"...
    fn detect(&self, project: &ProjectContext) -> Option<Detection>; // version + confidence + reasons
    fn extract(&self, ctx: &ExtractContext, store: &mut Store) -> Result<ExtractReport>;
}
```

* Detection by files that exist, not only the version number (an app on Laravel 11 may keep `app/Http/Kernel.php`).
* `ExtractReport` collects warnings (`unresolved` items with reasons) — exposed via `pidx doctor`.

### Suggested crates (verify current versions/APIs first)

* `tree-sitter` + `tree-sitter-php` — PHP parsing
* `rusqlite` (feature `bundled`; check FTS5 availability) — storage
* `rmcp` — official Rust MCP SDK (stdio transport)
* `ignore` — fast gitignore-aware directory walking
* `blake3` — file hashing
* `serde`, `serde_json`, `toml` — data/config
* `clap` — CLI
* `strsim` (or similar) — fuzzy "did you mean" suggestions
* `tracing`, `tracing-subscriber` — logging to stderr
* `thiserror`, `anyhow` — errors
* `insta` — snapshot tests
* `rayon` — parallel parsing
* An English pluralizer for Laravel table-name inference (pick a crate or port Laravel's `Str::plural` rules; mark results `inferred`).

## 5. Configuration — `.pidx.toml` (optional, in the scanned project root)

```toml
[project]
name = "goodbite-api"
kind = "laravel"            # auto-detected if omitted

[scan]
include = ["app", "routes", "database", "config", "bootstrap"]
exclude = ["vendor", "node_modules", "storage", "public", "bootstrap/cache"]

[laravel]
artisan = "off"             # "off" | "auto" | "on"  (see §7.3)
artisan_command = "php artisan"

[freshness]
check_interval_ms = 2000

# Milestone 2 — linking separate repos (parse but ignore for now)
[[links]]
name = "goodbite-web"
path = "../goodbite-web"
kind = "nuxt"
api_base = "/api"
```

Config is optional; sensible defaults must work with zero config.

## 6. Storage (SQLite)

### 6.1 Tables (adjust as needed, keep migrations versioned)

* `meta(key PRIMARY KEY, value)` — `schema_version`, `indexed_at`, `git_commit`, `git_dirty`, `project_root`, `laravel_version`, `laravel_layout` (`legacy` | `slim`), `php_constraint`.
* `files(id, path UNIQUE, hash, size, mtime, lang, indexed_at)`
* `symbols(id, file_id, kind, name, fqn, parent_fqn, signature, visibility, is_static, is_abstract, start_line, end_line, doc_summary)`
  * kinds: `class`, `interface`, `trait`, `enum`, `method`, `function`, `property`, `const`, `enum_case`
  * plus `symbol_meta` (extends, implements, uses-traits) or JSON columns.
* `symbols_fts` — FTS5 over `name`, `fqn`, `doc_summary` (fallback to LIKE + strsim if FTS5 unavailable).
* `routes(id, methods, uri, name, action_kind, controller_fqn, controller_method, middleware_json, prefix, domain, file_id, line, source, unresolved_reason)`
* `models(id, fqn, table_name, table_inferred, connection, fillable_json, guarded_json, hidden_json, casts_json, appends_json, file_id, line)`
* `model_relations(id, model_fqn, method, type, related_fqn, foreign_key, owner_key, pivot_table, line)`
* `model_accessors(id, model_fqn, attribute, style, line)` — `style`: `legacy_get` | `attribute_make`
* `model_scopes(id, model_fqn, scope_name, line)`
* `db_tables(name PRIMARY KEY, created_in, last_changed_in, dropped)`
* `db_columns(table_name, name, type, nullable, default_raw, unsigned, unique, indexed, primary, fk_table, fk_column, created_in, last_changed_in, confidence)`
* `form_requests(fqn, rules_json, authorize_raw, file_id, line)`
* `warnings(id, file_id, line, adapter, message)`

### 6.2 Incremental scanning

* Walk with `ignore` (respect `.gitignore` + config excludes).
* For each file: compare `mtime` + `size`; only if changed, hash with blake3; only if hash changed, re-parse.
* On change, delete that file's rows (cascade) and re-insert.
* Schema replay depends on all migrations in order → if any migration/schema dump changed, re-run schema replay entirely (it's cheap).
* Route extraction depends on route files + layout files → re-run routes if any of those changed.

### 6.3 Git info

* `git rev-parse HEAD`, `git status --porcelain` (dirty flag). If git is unavailable, set `null`, don't fail.

### 6.4 Freshness (no file watcher)

* On every MCP tool call: if more than `check_interval_ms` since the last check, do a quick stat-walk and incrementally re-index changed files before answering.
* Every response's `meta` says `reindexed_files: n` and `stale: false`. If re-index fails or times out (> 1s budget), answer from the existing index with `stale: true` and a warning.

## 7. Laravel extraction (Milestone 1)

### 7.1 Detection

* Version from `composer.lock` → `laravel/framework` version (fallback: `composer.json` constraint, mark `inferred`).
* Layout:
  * `legacy` if `app/Http/Kernel.php` exists (typical 9/10, or upgraded 11+).
  * `slim` if `bootstrap/app.php` contains `Application::configure(` / `->withRouting(` (11+ skeleton).
  * Record both signals; if mixed, prefer what the files actually say and add a warning.
* PSR-4 roots from `composer.json` `autoload.psr-4` (don't hardcode `app/`).

### 7.2 PHP symbols (`pidx-php`)

* Parse every PHP file under the include dirs with tree-sitter-php (parallel with rayon).
* Resolve namespace and `use` imports (including aliases and grouped `use`) so every referenced class name becomes an FQN.
* Extract: classes/interfaces/traits/enums, `extends`, `implements`, used traits, methods (params with types + defaults, return type, visibility, static, abstract), properties (with default values when literal), constants, enum cases, global functions (e.g. `app/helpers.php`).
* `doc_summary` = first sentence of the docblock, max ~120 chars.
* Ignore parse errors gracefully: index what parsed, add a warning with line.

### 7.3 Routes

Static mode (default, always available):

* Find route files:
  * legacy: `RouteServiceProvider` → `->group(base_path('routes/web.php'))` etc. with their `prefix` / `middleware`.
  * slim: `bootstrap/app.php` → `withRouting(web: ..., api: ..., commands: ..., health: ..., then: fn() => ...)`.
  * Defaults: `routes/web.php` (middleware `web`), `routes/api.php` (prefix `api`, middleware `api`) when present.
  * Follow `require` / `include` of other route files.
* Support:
  * `Route::get|post|put|patch|delete|options|any`, `Route::match([...])`
  * Chained `->name()`, `->middleware()`, `->where*()`, `->withoutMiddleware()`
  * Groups: `Route::prefix()->group()`, `Route::middleware()->group()`, `Route::name('x.')->group()`, `Route::controller(X::class)->group()`, `Route::domain()`, nested groups, array-style `Route::group([...], fn)`
  * Actions: `[X::class, 'method']`, `X::class` (invokable → `__invoke`), `'X@method'` strings (legacy, resolve against namespace if a controller namespace is set), closures (`action_kind = closure`)
  * `Route::resource` / `apiResource` / `resources` / `apiResources` expanded into concrete routes with correct names, URIs and parameter names; support `->only()`, `->except()`, `->names()`, `->parameters()`, `->shallow()` (mark unsupported options as warnings)
  * `Route::view`, `Route::redirect`, `Route::permanentRedirect`, `Route::fallback`
* Anything dynamic (variables, loops, macros) → record with `unresolved_reason`, never guess.

Artisan mode (optional, most accurate):

* If `artisan = "on"` (or `"auto"` and PHP + `vendor/` exist), run `<artisan_command> route:list --json`, parse, and store with `source = artisan`. Merge with static data for `file:line` where possible.
* In Docker this requires the `runtime-php` image target. `artisan` boots the app: it needs `vendor/` and a usable `.env`, and may try to write `bootstrap/cache` — handle read-only mounts (e.g. set Laravel's cache-path env vars to a temp dir), and fall back to static mode with a warning on any failure. Timeout 15s.

### 7.4 Models

* A model = a class that (transitively, within the project) extends `Illuminate\Database\Eloquent\Model`, `Illuminate\Foundation\Auth\User`, or `Illuminate\Database\Eloquent\Relations\Pivot`.
* Extract: `$table` (else infer snake_case plural → `table_inferred = true`), `$connection`, `$primaryKey`, `$fillable`, `$guarded`, `$hidden`, `$appends`, `$casts` property and `casts()` method (11+).
* Relations: methods whose body returns `$this->hasOne|hasMany|belongsTo|belongsToMany|hasOneThrough|hasManyThrough|morphTo|morphOne|morphMany|morphToMany|morphedByMany(...)`; capture related class FQN, explicit keys, pivot table. Also use return type hints when present.
* Accessors/mutators: `getXAttribute`/`setXAttribute` (legacy) and methods returning `Attribute` (`Attribute::make`) → attribute name in snake_case.
* Scopes: `scopeX` → `x`; also methods with a `#[Scope]` attribute if present.
* Traits on model: record (e.g. `SoftDeletes`, `HasUuids`, `HasFactory`) — `SoftDeletes` implies `deleted_at`.

### 7.5 Migrations → DB schema

* Order: files in `database/migrations` sorted by filename (plus any extra migration paths you can detect statically; warn otherwise).
* If a squashed schema dump exists (`database/schema/*-schema.sql`): parse `CREATE TABLE` statements (MySQL, PostgreSQL, SQLite dialects — basic columns, types, nullability, PKs, FKs) as the starting state, then apply only migrations not included in the dump (the dump's `migrations` table insert lists them).
* Support anonymous-class migrations (`return new class extends Migration`) and named classes. Replay only `up()`.
* Operations:
  * `Schema::create`, `Schema::table`, `Schema::rename`, `Schema::drop`, `Schema::dropIfExists`, `Schema::dropColumns`
  * Column methods → column(s) with type: `id`, `bigIncrements`, `increments`, `string`, `text`, `longText`, `integer`, `bigInteger`, `unsignedBigInteger`, `tinyInteger`, `boolean`, `decimal`, `float`, `double`, `date`, `dateTime`, `timestamp`, `time`, `json`, `jsonb`, `enum`, `uuid`, `ulid`, `foreignId`, `foreignIdFor`, `foreignUuid`, `morphs`, `nullableMorphs`, `uuidMorphs`, `timestamps`, `timestampsTz`, `softDeletes`, `rememberToken`, etc. Unknown methods → column type `unknown(<method>)` + warning (don't drop them).
  * Modifiers: `nullable`, `default`, `unsigned`, `unique`, `index`, `primary`, `after`, `comment`, `change()` (alter existing column), `constrained()`, `references()->on()`, `cascadeOnDelete()` etc.
  * `dropColumn`, `renameColumn`, `dropForeign`, `dropIndex`, `dropUnique`, `dropTimestamps`, `dropSoftDeletes`
* Track for each column which migration created it and which last changed it.
* Conditional logic (`if (Schema::hasColumn(...))`) → apply but mark `confidence = low`.
* Raw `DB::statement(...)` → warning, don't parse.

### 7.6 Form Requests

* Classes extending `Illuminate\Foundation\Http\FormRequest`.
* `rules()` returning an array literal → store field → rules (string `'required|email'` split, or array form; `Rule::...` objects stored as raw text).
* Link controller methods to Form Requests via parameter type hints (store on the route / symbol for `get_symbol` output).

## 8. MCP server — tools (Milestone 1: exactly these 6)

Keep descriptions short and say when to use each tool — that's how Claude picks correctly.

### Common response envelope

```json
{
  "data": { ... },
  "meta": {
    "project": "goodbite-api",
    "indexed_at": "2026-10-15T10:00:00Z",
    "git_commit": "abc1234",
    "git_dirty": true,
    "stale": false,
    "reindexed_files": 2,
    "truncated": false,
    "next_cursor": null,
    "warnings": []
  }
}
```

Not found:

```json
{ "data": { "found": false, "query": "OrderServcie", "suggestions": [
  { "name": "OrderService", "fqn": "App\\Services\\OrderService", "kind": "class", "file": "app/Services/OrderService.php", "line": 12, "score": 0.93 }
], "hint": "Did you mean OrderService?" }, "meta": { ... } }
```

Every tool accepts `detail: "brief" | "full"` (default `brief`) and, where it returns lists, `limit` (default 20, max 100) and `cursor`.

### 8.1 `project_overview`

Use first in every session to understand the project before reading files. Returns: name, Laravel version, layout (legacy/slim), PHP constraint, PSR-4 roots, counts (classes, routes, models, tables, form requests), route files, key directories, top-level domains/modules (by namespace), index health (warning count). Target < 1,500 tokens in `brief`.

### 8.2 `search_symbol`

Use before writing any new class/method/helper to find existing code to reuse, or to locate code. Input: `query`, optional `kind`, optional `namespace_prefix`, `limit`. Returns: ranked matches with `fqn`, `kind`, `signature`, `file`, `line`, `doc_summary`.

### 8.3 `get_symbol`

Use instead of reading a whole file when you need one class or method. Input: `name` (short name or FQN; `Class::method` supported), `include_source` (bool, default false), `max_lines` (default 80). Returns: signature, location, members (for classes), parent/interfaces/traits. Enriched automatically:

* if it's a model → table, fillable, casts, relations, accessors, scopes
* if it's a controller → routes pointing to it, Form Requests used per method
* if it's a Form Request → rules

Ambiguous short name → list of candidates (not a guess).

### 8.4 `list_routes`

Use to find endpoints or which controller handles a URI. Filters: `uri_contains`, `name_prefix`, `method`, `controller`, `middleware`, `limit`, `cursor`. Returns: methods, uri, name, action, middleware, file:line, source.

### 8.5 `get_schema`

Use before writing any query, migration, or model change — never assume column names. Input: optional `table`.

* No table → list tables with column count and mapped model.
* With table → columns (type, nullable, default, keys, FK), mapped model(s), created/changed-in migration.

### 8.6 `verify`

Use before finalising code to check that everything referenced actually exists. Input (either or both):

* `claims`: `{ "classes": [], "methods": ["App\\Models\\Order::markPaid"], "functions": [], "routes": ["orders.store"], "tables": ["orders"], "columns": ["orders.status"], "relations": ["App\\Models\\Order::items"] }`
* `php_snippet`: best-effort extraction of class refs (`use`, `new X`, `X::`, type hints), static calls, `route('name')`, `Schema::`/table names in obvious literals, then verify them.

Returns per item: `exists` | `not_found` (+ suggestions) | `external` (class not in project but present in `vendor/` — check via `vendor/composer/autoload_psr4.php` / `autoload_classmap.php` path existence; `Illuminate\...` etc.) | `unresolved` (couldn't determine). Summary line: `"3 ok, 1 not_found, 1 external"`.

## 9. CLI (`pidx`)

```
pidx scan  --project <path> [--full]        # build/update index, print summary + warnings count
pidx serve --project <path>                 # MCP stdio server (auto-scans on start if needed)
pidx query --project <path> <tool> '<json>' # call any MCP tool from the CLI (debugging)
pidx stats --project <path>                 # counts, timings, index size
pidx doctor --project <path>                # detection reasons, warnings, unresolved items, git/php availability
```

Global: `--project-id`, `--data-dir` (default `/data` in Docker, platform cache dir otherwise), `--log-level`.

## 10. Testing

### Fixtures (hand-written, tiny, no vendor/)

* `fixtures/laravel10-legacy` — `app/Http/Kernel.php`, `RouteServiceProvider`, `$casts` property, legacy accessors, named-class migrations, `'Controller@method'` string routes.
* `fixtures/laravel11-slim` — `bootstrap/app.php` `withRouting`, `casts()` method, `Attribute::make`, anonymous migrations, `Route::controller()` groups, resources with `only/except`.
* `fixtures/laravel11-upgraded` — Laravel 11 in `composer.lock` but legacy Kernel structure.
* `fixtures/schema-dump` — `database/schema/mysql-schema.sql` + 2 later migrations, including `renameColumn` and `->change()`.
* Each fixture has a small `composer.lock` with just `laravel/framework` and a `composer.json` with PSR-4.

### Test types

* Unit tests per extractor.
* `insta` JSON snapshots of the full extracted index per fixture (sorted, deterministic, no timestamps).
* MCP integration test: spawn `pidx serve` and call each tool over stdio with an MCP client (rmcp client feature), assert envelope shape and not-found behaviour.
* Incremental test: scan → modify one file → rescan → only that file re-parsed (assert via report counters).
* Performance smoke test (ignored by default): generate 2,000 synthetic PHP files, assert cold scan time budget.

## 11. Phases & acceptance criteria (stop after each)

* **Phase 0 — Scaffold.** Workspace + 5 crates, Dockerfile (builder/runtime/runtime-php targets), docker-compose `dev` + `pidx`, Makefile, `.dockerignore`, tracing to stderr, error types, CI-style `make lint test`. ✅ `make lint test` passes in Docker; `docker build --target runtime` produces an image; `pidx --help` runs in the container.
* **Phase 1 — Store & scanner.** SQLite schema + migrations, `ignore` walker, mtime/size/blake3 incremental logic, git info, `pidx scan` / `stats`. ✅ Incremental test passes; rescanning an unchanged project re-parses 0 files.
* **Phase 2 — PHP symbols.** tree-sitter-php extraction, namespace/use resolution, FTS search, `pidx query search_symbol`. ✅ Snapshots for all fixtures; search finds by short name, FQN, partial, typo (suggestions).
* **Phase 3 — Laravel detection + routes.** Version/layout detection, static routes (all features in §7.3), resource expansion, artisan mode with fallback. ✅ Route snapshots match expected tables for each fixture; `doctor` explains detection.
* **Phase 4 — Models, schema, Form Requests.** §7.4–7.6 including schema-dump handling. ✅ `get_schema orders` on fixtures matches hand-written expectations, incl. renamed/changed columns.
* **Phase 5 — MCP server.** rmcp stdio server, 6 tools, envelope, pagination, `detail` levels, freshness check before every call. ✅ Integration tests pass; works via `docker run -i` from Claude Code (manual test documented in README).
* **Phase 6 — verify.** Claims mode + php_snippet mode, vendor `external` check. ✅ Tests for each status (`exists`, `not_found` with suggestion, `external`, `unresolved`).
* **Phase 7 — Integration & docs.** README (install, Docker build, `claude mcp add`, `.mcp.json`, troubleshooting), the CLAUDE.md snippet below, run against one real Laravel project and fix issues found. ✅ Real-project scan completes with a reviewed `doctor` report.

## 12. CLAUDE.md snippet to ship for projects that use pidx

```markdown
## Project index (pidx MCP) — required usage
- At the start of a task, call `project_overview`.
- Before creating any class, method or helper, call `search_symbol` to check for an existing one and reuse it.
- Before writing queries, migrations or model code, call `get_schema` — never assume column names.
- Prefer `get_symbol` over reading whole files.
- Before finishing, call `verify` with every class, method, route, table and column your change relies on. Fix all `not_found` items.
- If a tool response says `stale: true`, mention it and re-check files directly.
```

## 13. Roadmap after Milestone 1 (do NOT build now — design so these fit)

### Milestone 2 — Frontend + linking

* Vue 2 (Options API, mixins, filters, `$emit` scanning, Vuex, global components, Vue 2.7 `<script setup>`)
* Vue 3 (`<script setup>`, `defineProps` runtime + TS types, `defineEmits`, `defineModel`, `defineSlots`, composables, provide/inject, Pinia options + setup stores)
* Nuxt 2 (`pages/` with `_id.vue`, `store/` Vuex modules, `asyncData`/`fetch`, `$axios`, plugins, middleware)
* Nuxt 3/4 (`[id].vue`, `app/` srcDir in Nuxt 4, auto-imports with nested component naming, read `.nuxt/imports.d.ts` + `.nuxt/components.d.ts` when present, `server/api` Nitro routes, layers, `definePageMeta`)
* Parsing: `oxc` for JS/TS, SFC block splitting, template parsing for component usage
* Linked repos: match `$fetch`/`useFetch`/`axios`/`$axios` calls to Laravel routes; flag frontend calls to non-existent routes
* New tools: `get_component`, `list_pages`, `api_usage`

### Milestone 3 — Reuse & impact

* `find_similar(description)` using local embeddings (e.g. fastembed-rs) + signature matching
* Duplicate-code detection (AST shape)
* Reference graph: `who_calls`, `impact_of`
* Decisions log: `record_decision`, `get_decisions`
* Claude Code hooks (PreToolUse on Edit/Write → run `verify`)
* Inertia support if needed (`Inertia::render('Page')` → `resources/js/Pages`)

Keep the total MCP tool count ≤ ~10 even after these milestones (merge tools where possible).
