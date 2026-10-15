# pidx — project index MCP server for Laravel, Vue and Nuxt

`pidx` reads a Laravel, Vue 2, Vue 3 or Nuxt (2/3/4) project and serves facts about it to Claude over MCP, so Claude checks what exists instead of guessing:

- **PHP symbols:** classes, interfaces, traits, enums, methods, functions, properties, constants, enum cases. Names are fully resolved, with signatures and doc summaries.
- **Routes:** static analysis of `withRouting()`, `RouteServiceProvider` and route files, including groups, resources and includes. Optionally `php artisan route:list`.
- **Eloquent models:** table, fillable/guarded/hidden, casts, appends, relations, accessors, scopes, traits.
- **Database schema:** rebuilt by replaying migrations (and a squashed `schema:dump`), with the migration and line that created or last changed each column.
- **Form Requests:** validation rules, linked to the controller methods that use them.
- **Vue / Nuxt components:** props (type, required, default), emits, `v-model`, slots, which components each one renders and who renders it. This covers `<script setup>`, the Options API, `defineComponent` and class components.
- **Component names:** resolved the way the app resolves them. That means local imports, `Vue.component()` / `app.component()`, `require.context` / `import.meta.glob` and `Vue.use()` plugins. For Nuxt it also covers auto-imports (from `.nuxt/components.d.ts` when present) and libraries such as Vuetify, Nuxt UI and PrimeVue.
- **Pages / screens:**
  - Nuxt file-based pages (Nuxt 2 `_id`, Nuxt 3+ `[id]`, `[[opt]]`, `[...slug]`, route groups, nesting), with `definePageMeta`, layouts and middleware.
  - vue-router routes stitched together across files: spreads, imported arrays, nested `children`, lazy components, and simple helper functions that return a route.
- **Frontend ↔ backend:**
  - HTTP calls in Vue / Nuxt code (axios, `$fetch`, `useFetch`, `fetch`, and project wrappers such as `api.get` or `useApi()`) are matched to Laravel routes, in the same repository or in a linked backend.
  - pidx reports calls to endpoints that don't exist or use the wrong method.
  - It also reports fields the frontend reads (in script and template) that the endpoint's Resource or array doesn't return.
- **Translations:**
  - Locale files are read for vue-i18n / nuxt-i18n (JSON, JS, TS; one file per locale, or split files merged) and for Laravel (`lang/*.php`, `lang/*.json`).
  - Keys are read from code: `$t`, `t`, `$tc`, `v-t`, `<i18n-t keypath>`, `__()`, `trans()`, `@lang`.
  - pidx reports keys missing per locale and keys used but defined nowhere. A wrapper namespace such as `message.` is inferred automatically, and dynamic keys count as prefixes.
- **Filament admin panels:** each admin URL (`/admin/merchants/{record}/edit`) mapped to its page class, resource, model, form fields and table columns, plus custom pages such as settings screens. Filament registers these at runtime, so they are not in `routes/`; pidx follows Filament's own naming rules (v3–v5).
- **Settings and their defaults** (`get_schema` with `settings: true`):
  - Key/value settings read and written in code (`Setting::get(KEY, DEFAULT)`, `setting('x', 5)`, and helper methods that wrap them), with the default, where each is read and written, and the admin page that edits it.
  - `config()` keys backed by `env()`, with the variable name and default.
  - `.env` values are never read.
- **Find usages:** every use of a class, method, function, route name, component, composable or store, translation key, endpoint or file, with `file:line`. Tests that touch it are listed separately (including PHP tests that hit a controller's routes through `$this->getJson('/api/...')`), so you know which tests to run.
- **Stores and composables:** Pinia (options and setup stores), Vuex modules with their namespaces (including Nuxt 2 `store/`), composables and exported JS/TS functions.

Every answer is either a fact with `file:line`, or an explicit `found: false` with suggestions. Anything pidx cannot work out statically is reported as `unresolved` rather than guessed.

Supported:
- Laravel 9 and later, both the 11+ `bootstrap/app.php` layout and the older Kernel/RouteServiceProvider layout (including apps upgraded to 11+ that kept it).
- Vue 2.6/2.7 and Vue 3, built with Vue CLI, Vite, webpack or Laravel Mix (aliases come from `vue.config.js`, `vite.config` and `tsconfig`/`jsconfig`).
- Nuxt 2, 3 and 4, with `srcDir` (Nuxt 4 `app/` or the root), component dirs, `pathPrefix` and layers inside the project.

A repository can be both: a Laravel app with an Inertia/Vue frontend gets both sets of facts.

The full specification is in [BUILD_PROMPT.md](BUILD_PROMPT.md).

## Install

Everything runs in Docker; you don't need Rust or PHP on the host.

```bash
docker build -t pidx:latest --target runtime .
```

## Use it from Claude Code

Register it for a project (run inside that project's folder):

```bash
claude mcp add project-index -- docker run -i --rm -v "$(pwd)":/workspace:ro -v pidx-data:/data -e PIDX_PROJECT_ID="$(pwd | shasum | cut -c1-12)" pidx:latest serve --project /workspace
```

Or commit a `.mcp.json` in the project root. Claude Code expands `${VAR}` and `${VAR:-default}` in `command`, `args` and `env`:

```json
{
  "mcpServers": {
    "project-index": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-v", "${PWD}:/workspace:ro",
        "-v", "pidx-data:/data",
        "pidx:latest", "serve", "--project", "/workspace"
      ]
    }
  }
}
```

Notes:
- **Project id:** without `PIDX_PROJECT_ID`, the index id comes from the git remote + root commit, so it is stable per repository. Set it explicitly if you keep several checkouts of the same repository.
- **`-i`, never `-t`:** a TTY corrupts the MCP stdio stream. All logs go to stderr.
- **Read-only:** the project is mounted read-only and pidx never writes to it. The index lives in the `pidx-data` volume.

Then add [CLAUDE.snippet.md](CLAUDE.snippet.md) to the project's `CLAUDE.md`, so Claude uses the tools at the right moments.

### Tools

| Tool | When Claude should use it |
|---|---|
| `project_overview` | First, in every session: Laravel version and layout, counts, key directories, main namespaces, models, index health. About 1,200 tokens on an 1,200-file app. |
| `search_symbol` | Before writing a class, method or helper, to reuse existing code. Matches short names, FQNs, `Class::method` and partial names; typos give `found: false` + suggestions. |
| `get_symbol` | Instead of reading a whole file. Models include table/fillable/casts/relations; controllers their routes and Form Requests; Form Requests their rules. `include_source` adds the code. |
| `get_component` | Before using, changing or creating a Vue component or Nuxt page: props, emits, `v-model`, slots, what it renders (resolved to files or libraries), who renders it, stores and composables it uses. Takes a name (`OrderCard`), tag (`order-card`) or path. |
| `list_pages` | To find a screen and its code (frontend pages, and Filament admin pages with their form fields): URL, route name, the component file it renders, layout, middleware, and where the route is defined. Filters: `path_contains`, `name_prefix`, `file`. |
| `list_routes` | To find Laravel endpoints or which controller handles a URI (filters: `uri_contains`, `name_prefix`, `method`, `controller`, `middleware`). |
| `api_usage` | To link the frontend to the backend: which route each HTTP call hits, calls to missing endpoints, wrong methods, and fields read but not returned. Pass `endpoint` for its callers and response fields, `file` for one screen, or `status: missing / fields`. |
| `find_usages` | Before changing, renaming or removing anything: every use with `file:line`, the routes involved, and the tests that touch it. Method calls on objects whose class is unknown are listed separately as `possible`. With `changed_files` (or `git_changes: true`) it summarises a whole change instead: what each file affects, the related tests, and the exact commands to run only those (`php artisan test …`, `npx vitest run …`, `npx playwright test …`). |
| `get_schema` | Before writing queries, migrations or model code (or, with `settings: true`, to see app settings and config defaults): tables, columns, keys, foreign keys, mapped models, where each column was created and changed. |
| `verify` | Before finishing: checks classes, `Class::method`, functions, routes (name or `METHOD uri`), tables, `table.column`, `Model::relation`, Vue components, endpoints the frontend calls, translation keys (with the locales they are missing in), or a PHP snippet. Each item is `exists`, `not_found` (with suggestions), `external` (vendor / PHP built-in) or `unresolved`. |

Every tool takes `detail: "brief" | "full"` (default `brief`). List tools take `limit` (default 20, max 100) and `cursor`. Every response has the same envelope:

```json
{ "data": { ... },
  "meta": { "project": "...", "indexed_at": "...", "git_commit": "...", "git_dirty": false,
            "stale": false, "reindexed_files": 0, "truncated": false, "next_cursor": null, "warnings": [] } }
```

### Freshness

There is no file watcher; Docker Desktop bind mounts don't deliver file events reliably. Instead, before answering a call, pidx checks file stats (at most every `check_interval_ms`, default 2 s) and re-indexes what changed. Typical costs through Docker Desktop:
- On a 4,300-file Laravel project: about 0.1 s after editing a controller, about 0.25 s after editing a route file.
- On a 2,500-file Vue 2 project: about 0.1 s for a check that finds nothing changed. A full first index takes about 1–2 s.

If re-indexing takes longer than 1 s, the answer comes from the previous index with `stale: true` and a warning. The very first index is always awaited, because an empty index would wrongly say "not found".

## CLI

```bash
pidx scan   --project <path> [--full] [--json]   # build/update the index
pidx serve  --project <path>                     # MCP server on stdio
pidx query  --project <path> <tool> '<json>'     # call any MCP tool from the shell
pidx stats  --project <path> [--json]            # counts, timings, index size
pidx doctor --project <path> [--json]            # detection reasons, warnings, unresolved routes, broken routes
```

Global options: `--name` / `PIDX_PROJECT_NAME` (display name; inside Docker the folder is always `workspace`), `--link name=path` / `PIDX_LINKS`, `--project-id` / `PIDX_PROJECT_ID`, `--data-dir` / `PIDX_DATA_DIR` (default `/data` in Docker, the platform cache dir elsewhere), `--log-level` / `PIDX_LOG`.

With the Makefile: `make scan PROJECT=/path`, `make doctor PROJECT=/path`, `make serve PROJECT=/path`.

`pidx doctor` explains how the project was understood:
- why it chose a version and layout
- every route it could not resolve, and why
- **routes whose controller or method does not exist**
- grouped warnings (raw SQL in migrations, dynamic table names, code the parser could not read)
- translations: keys missing per locale, keys used but not defined, plain-text keys and possibly unused keys
- for Vue / Nuxt: how the frontend was detected, **routes whose component file is missing**, routes built at runtime, and **component tags that resolve to nothing**. These are usually imports of files that no longer exist, or components used without being registered.

### Several apps in one folder (workspace)

Mount the folder that holds the apps (e.g. `shop/` with `api/`, `storefront/` and `admin-app/`):

```json
"args": ["run", "-i", "--rm",
         "-v", "${PWD}:/workspace:ro", "-v", "pidx-data:/data",
         "pidx:latest", "serve", "--project", "/workspace", "--name", "shop"]
```

When the folder is not itself a project, pidx looks one level down for Laravel apps (`artisan` + `composer.json`) and Vue / Nuxt apps (`package.json` with vue or nuxt). To choose the apps yourself, list them in the workspace's `.pidx.toml`: `[workspace] apps = ["api", "storefront", "admin-app"]`.

How a workspace works:
- Each app keeps its own index.
- Vue / Nuxt apps are linked to the workspace's Laravel apps automatically, sharing their index.
- Every tool takes an optional `app`. Without it, the tool runs on every app:
  - an answer found in one app comes back tagged with that app
  - answers found in several apps are grouped per app
  - `verify` merges item by item
- For `find_usages` with `changed_files`, give paths from the workspace root (`api/app/Models/Order.php`).

### Linking a frontend to its backend

When the Vue / Nuxt app and the Laravel API live in separate repositories, link them. Mount the backend read-only and name it:

```json
"args": ["run", "-i", "--rm",
         "-v", "${PWD}:/workspace:ro",
         "-v", "${PWD}/../backend:/links/backend:ro",
         "-v", "pidx-data:/data",
         "pidx:latest", "serve", "--project", "/workspace", "--link", "backend=/links/backend"]
```

Or put the link in the frontend's `.pidx.toml` (`[[links]] name = "backend"`, `path = "../backend"`, optional `api_base = "/api/v1"`). `PIDX_LINKS=backend=/links/backend` works too.

How linking works:
- The backend's index is kept up to date in the same data volume; the backend repository is only read.
- The API base path (e.g. `/api/v1` when the frontend calls `/orders`) is inferred by trying every route prefix and keeping the one that explains the most calls.
- URLs that start with an unknown variable are matched by their known tail. When that is ambiguous, the call is reported as `unresolved` rather than missing.

How the response check works:
- Fields read are found heuristically: property reads on a call's result, and template reads (including `v-for`) of the state the result is stored in.
- "Read but not returned" is only reported when the endpoint's response is fully known: an API Resource or a literal array, checked one level deep. Responses built from queries are reported by kind only.
- Laravel's `_method` spoofing (`POST` with `_method: 'PUT'`) is understood.

## Configuration — `.pidx.toml` (optional, project root)

```toml
[project]
name = "shop-api"            # display name (default: composer.json name, then git repo name)

[scan]
# Default: Laravel dirs plus Vue/Nuxt source dirs (missing ones are skipped):
include = ["app", "routes", "database", "config", "bootstrap", "lang", "src", "resources/js",
           "components", "pages", "layouts", "composables", "stores", "store", "plugins",
           "middleware", "utils", "mixins", "i18n", "locales", "tests", "test",
           "resources/lang", "resources/views"]
exclude = ["/vendor", "node_modules", "/storage", "/public", "/bootstrap/cache", "/dist", "/.output", "/coverage"]

[laravel]
artisan = "off"              # "off" | "auto" | "on"
artisan_command = "php artisan"
artisan_timeout_ms = 15000

[freshness]
check_interval_ms = 2000
```

Exclude patterns follow `.gitignore` rules. A leading `/` anchors a pattern to the project root, so a source folder that happens to be called `vendor` or `public` is still indexed. `.env*`, `vendor/`, `storage/`, `bootstrap/cache/`, `node_modules/`, `*.d.ts` and minified `*.min.*` bundles are never indexed. Secrets are never read.

### Artisan mode

With `artisan = "on"` (or `"auto"` when `php` and `vendor/` exist), pidx runs `php artisan route:list --json` and uses it as the source of truth, keeping `file:line` from static analysis. Laravel's cache paths are pointed at a temporary directory, so this works on a read-only mount. Any failure or timeout falls back to static routes, with a warning.

**This boots your application, which means it executes project PHP code**, so it is off by default. In Docker it needs the PHP image: `docker build -t pidx-php:latest --target runtime-php .`

## What is (and isn't) understood

**Routes:** these are evaluated statically:
- verbs, `match`, `any`, `view`, `redirect`, `fallback`
- `name` / `prefix` / `middleware` / `withoutMiddleware` / `namespace` / `controller` / `domain` / `where*` / `can`, fluent and array-style groups, nested groups
- `require` / `include` of other route files
- `resource`, `apiResource`, `resources`, `singleton` with `only`, `except`, `names`, `parameters`, `shallow`, nesting
- `'Controller@method'` strings resolved through the provider's `$namespace`

Routes inside loops, with variable URIs or with unknown macros are kept and marked `unresolved`. Routes inside `if` are kept and flagged as conditional.

**Schema:**
- `Schema::create` / `table` / `rename` / `drop*` and the column types and modifiers from Laravel 9–13
- `change()`: before Laravel 11 unstated attributes are kept; from 11 the column is redefined
- table names read from `config()` (including packages' default config, e.g. spatie/laravel-permission), class constants, properties, `foreach` over literal arrays, and string interpolation

Raw `DB::statement()` SQL is not parsed and is reported. Code inside conditions is applied with `confidence: low`.

**Vue / Nuxt:**
- `.vue` files are split into their blocks.
- Scripts are parsed with [oxc](https://oxc.rs). This covers JS, TS, `<script setup>` macros (`defineProps` with runtime or type arguments, including local interfaces), `withDefaults`, destructured defaults, `defineEmits`, `defineModel`, `defineSlots`, `defineOptions` and `definePageMeta`.
- Templates are read with a small tokenizer.

Some things are reported, not guessed:
- props typed with an imported interface (`props_unresolved`)
- Pug templates
- component names built at runtime

Routes built at runtime (from `.map()`, loops, or helpers that aren't simple) are listed as `(dynamic)` with the code that builds them.

**Find usages:**
- PHP references come from the same parse as symbols: `new`, static calls, `$this->` calls, calls on variables typed by parameters, closures or `new`, type hints, `instanceof`, `catch`, `X::class`, `route('name')`, helper functions and test HTTP calls.
- JS / Vue references come from resolved imports (including lazy ones), component tags, and calls by name (Nuxt auto-imports).
- Column usages are not tracked yet.

**Not yet:** column usages, config / settings defaults, embeddings and semantic search.

## Troubleshooting

- **The index is empty or "no indexable files found"**: Docker Desktop only mounts shared host paths, and an unshared one (e.g. `/private/tmp`) appears as an empty folder. Use a path under your home folder.
- **`fatal: detected dubious ownership`**: the image already marks `/workspace` as a safe git directory. If you mount the project elsewhere, add `git config --system --add safe.directory <path>` to your own image.
- **`git_dirty` / slow refreshes on macOS**: pidx calls `git status` with `core.checkStat=minimal`, because inside a container the host's inode/device numbers don't match git's index. Without that, git re-reads every tracked file (seconds on large repositories).
- **Responses say `stale: true`**: re-indexing took more than 1 s (e.g. right after a large `git checkout`). The next call is usually fresh; meanwhile Claude should re-check files directly.
- **Routes look wrong**: run `pidx doctor`, which shows where routes were read from and why. If the app can boot, consider artisan mode.
- **A `docker build` fails once and then succeeds**: seen twice during development without a reproducible cause. Re-run the build; if it persists, run `docker build --target runtime . 2>&1 | tail -50` and open an issue with the output.

## Development

Everything runs in Docker; don't run `cargo` on the host.

| Command | What it does |
|---|---|
| `make dev-image` | Build the dev toolchain image (Rust 1.99, rustfmt, clippy, cargo-insta, git) |
| `make lint` | `cargo fmt --check` + `cargo clippy -D warnings` |
| `make test` | `cargo test` (unit, fixture snapshots, MCP-over-stdio integration) |
| `make check` | lint + test |
| `make build-image` / `make build-image-php` | Runtime images |
| `make shell` | Shell in the dev container |

Performance smoke tests (ignored by default): `docker compose run --rm dev cargo test --release -p pidx-php --test index -- --ignored --nocapture`. Measured: 2,000 classes / 82,000 symbols cold in about 1 s, no-op refresh about 10 ms.

### Layout

```
crates/pidx-core     store (SQLite + migrations), scanner, git, symbols + search, envelope, adapter trait
crates/pidx-php      tree-sitter-php parsing, name resolution, symbol extraction
crates/pidx-laravel  detection, routes (static + artisan), models, schema replay, Form Requests, doctor
crates/pidx-js       oxc JS/TS analysis, Vue SFC splitter, template tokenizer, literal reader
crates/pidx-vue      Vue/Nuxt detection, component naming and registry, stores, get_component, doctor
crates/pidx-mcp      MCP server (rmcp, stdio) and the tool implementations shared with `pidx query`
crates/pidx-cli      the `pidx` binary
fixtures/            hand-written Laravel, Vue 2, Vue 3, Nuxt 2 and Nuxt 3 projects used by the tests
```

Extractor versions (`PHP_EXTRACTOR_VERSION`, `ROUTES_EXTRACTOR_VERSION`, `DATA_EXTRACTOR_VERSION`, `DETECTION_VERSION`, `JS_EXTRACTOR_VERSION`, `REGISTRY_VERSION`, `VUE_DETECTION_VERSION`) make existing indexes re-extract after an upgrade. Bump the relevant one whenever extraction output changes.
