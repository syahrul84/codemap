# codemap — project index MCP server for Laravel, Vue, Nuxt, Next.js, Swift, Android and Rust

codemap gives your AI assistant the facts about your project (classes, routes, columns, components, screens, endpoints), so it looks them up instead of guessing. You get fewer made-up names in the code it writes, fewer tokens spent reading whole files, and a correct picture of the project from the first message.

## Why use codemap

1. **It catches made-up names before they ship.** When the AI invents a column, method, route, component or endpoint (a "hallucination"), `verify` returns `not_found` with the closest real name, and `codemap misses` shows which names it keeps getting wrong. These bugs look correct in review and only fail in production.
2. **It knows what a change will break.** Before something is renamed or removed, `find_usages` lists every place it's used, with `file:line`. After a change, it names the tests that cover it and gives the exact commands to run only those, not the whole suite.
3. **It checks the frontend against the backend.** `api_usage` matches every API call in Vue, Nuxt or Next.js code to the Laravel route that serves it, and flags calls to routes that don't exist, the wrong method, and fields the frontend reads that the backend never returns. Grep can't find these.
4. **The AI knows your project from the first message, and spends fewer tokens.** One `project_overview` call (about 1,200 tokens on a 1,200-file app) gives the framework and version, layout, models, screens, crates or modules, and the names the AI got wrong in earlier sessions. After that it asks for one class, component or table instead of reading whole files. In a real Laravel app, `get_symbol` on the `Order` model answers in 5.3 KB instead of the 19.3 KB model file, and `get_schema` for `orders` in 8.9 KB instead of the 61.8 KB of the 26 migrations that build the table. `codemap usage` shows the answer sizes for your own project.
5. **It catches problems before a release.** `verify` with `release_check: true` gives one list: missing endpoints, fields not returned, missing translations, unused code added on the branch, migrations not yet run, new environment variables, and what the server needs (migrate, cache clear, restart).

The same tools work across Laravel, Vue, Nuxt, Next.js, Swift, Android and Rust. Nothing leaves your machine: your code is never sent anywhere, and the index stays local.

## Five-minute start

```bash
# macOS Apple Silicon: aarch64-apple-darwin · Intel Mac: x86_64-apple-darwin
# Linux: x86_64-unknown-linux-gnu / aarch64-unknown-linux-gnu · Windows: see below
curl -fsSL https://github.com/syahrul84/codemap/releases/latest/download/codemap-aarch64-apple-darwin.tar.gz | tar xz
mkdir -p ~/.local/bin && mv codemap ~/.local/bin/
codemap setup --dry-run   # see what setup would change
codemap setup             # Claude Code: register the server, add the rule block and the search hook
cd ~/code/my-app && codemap doctor   # optional: what was detected
```

`codemap setup` needs Claude Code's `claude` command (check with `claude --version`). It keeps a `.codemap-backup` copy of each file it changes; `--no-hook` leaves the search hook out. `codemap scan` is optional: the server builds the index on first use.

Then start a new Claude Code session in the project and ask: "What does project_overview say about this project?" `/mcp` shows codemap connected, and `codemap usage` afterwards shows the calls Claude made. Windows: see [Windows](#windows); other AI apps: see [Other AI tools](#other-ai-tools).

## What it reads

`codemap` reads a Laravel, Vue 2, Vue 3, Nuxt (2/3/4), Next.js, Swift (iOS / macOS), Android (Kotlin / Java) or Rust project and serves facts about it to Claude over MCP, so Claude checks what exists instead of guessing:

- **PHP symbols:** classes, interfaces, traits, enums, methods, functions, properties, constants, enum cases. Names are fully resolved, with signatures and doc summaries.
- **Routes:** static analysis of `withRouting()`, `RouteServiceProvider` and route files, including groups, resources and includes. Optionally `php artisan route:list`.
- **Eloquent models:** table, fillable/guarded/hidden, casts, appends, relations, accessors, scopes, traits.
- **Database schema:** rebuilt by replaying migrations (and a squashed `schema:dump`), with the migration and line that created or last changed each column.
- **Form Requests:** validation rules, linked to the controller methods that use them.
- **Next.js / React:** app router and pages router screens from the file system (route groups, `[param]`, catch-alls, nearest `layout`), route handlers (`app/**/route.ts` exports, `pages/api/**`) as endpoints the frontend's `fetch` calls are matched to, React function components with their props (destructured or typed), what they render and who renders them, `use client` / `use server`, and Supabase: tables used through `supabase.from('x').select/insert/update/delete` (`find_usages rooms`) and the schema replayed from `supabase/migrations/*.sql` (`get_schema`). Translation catalogs kept per text (`checkIn: { en, ms }`) are read too.
- **Vue / Nuxt components:** props (type, required, default), emits, `v-model`, slots, which components each one renders and who renders it. This covers `<script setup>`, the Options API, `defineComponent` and class components.
- **Component names:** resolved the way the app resolves them. That means local imports, `Vue.component()` / `app.component()`, `require.context` / `import.meta.glob` and `Vue.use()` plugins. For Nuxt it also covers auto-imports (from `.nuxt/components.d.ts` when present) and libraries such as Vuetify, Nuxt UI and PrimeVue.
- **Pages / screens:**
  - Nuxt file-based pages (Nuxt 2 `_id`, Nuxt 3+ `[id]`, `[[opt]]`, `[...slug]`, route groups, nesting), with `definePageMeta`, layouts and middleware.
  - vue-router routes stitched together across files: spreads, imported arrays, nested `children`, lazy components, and simple helper functions that return a route.
- **Frontend ↔ backend:**
  - HTTP calls in Vue / Nuxt code (axios, `$fetch`, `useFetch`, `fetch`, and project wrappers such as `api.get` or `useApi()`) are matched to Laravel routes, in the same repository or in a linked backend.
  - codemap reports calls to endpoints that don't exist or use the wrong method.
  - It also reports fields a call's TypeScript type requires (`useApi()<Paginated<Mall>>(…)`, resolved across imports and generics; optional `?:` fields ignored) that the endpoint never sends (`typed_but_not_returned`).
  - It also reports fields the frontend reads that the endpoint doesn't return. Reads are followed through helper functions (`fetchMalls()` returning `{ malls: r.data }`), list state and `v-for` loops, and into child components through their props (a card's `mall.name`). On the Laravel side, lists built step by step are followed too: `$this->helper()` methods, `Cache::remember(…)`, `array_map(fn ($c) => $c + [...])`, `->map(fn ($m) => [...])`, filters and slices.
- **Translations:**
  - Locale files are read for vue-i18n / nuxt-i18n (JSON, JS, TS; one file per locale, or split files merged) and for Laravel (`lang/*.php`, `lang/*.json`).
  - Keys are read from code: `$t`, `t`, `$tc`, `v-t`, `<i18n-t keypath>`, `__()`, `trans()`, `@lang`.
  - codemap reports keys missing per locale and keys used but defined nowhere, naming them (up to 20) in `project_overview`. A wrapper namespace such as `message.` is inferred automatically, and dynamic keys count as prefixes.
  - Only shipped locales are checked for missing keys: those in `[i18n] locales`, or (when not set) those with at least half of the base locale's keys. Others are listed as partial with their coverage (`th 6%`).
- **Filament admin panels:** each admin URL (`/admin/merchants/{record}/edit`) mapped to its page class, resource, model, form fields and table columns, plus custom pages such as settings screens. Filament registers these at runtime, so they are not in `routes/`; codemap follows Filament's own naming rules (v3–v5).
- **Settings and their defaults** (`get_schema` with `settings: true`):
  - Key/value settings read and written in code (`Setting::get(KEY, DEFAULT)`, `setting('x', 5)`, and helper methods that wrap them), with the default, where each is read and written, and the admin page that edits it.
  - `config()` keys backed by `env()`, with the variable name, default, how often code reads them, and whether `.env.example` lists the variable (only variable names are read from it, never values).
  - Config problems: `config('…')` keys read but defined nowhere, `env()` read in app code (null once config is cached).
  - `.env` values are never read.
- **Find usages:** every use of a class, method, function, route name, component, composable or store, translation key, endpoint or file, with `file:line`. Tests that touch it are listed separately (including PHP tests that hit a controller's routes through `$this->getJson('/api/...')`), so you know which tests to run.
- **Stores and composables:** Pinia (options and setup stores), Vuex modules with their namespaces (including Nuxt 2 `store/`), composables and exported JS/TS functions.
- **Swift (iOS / macOS):** classes, structs, enums, actors, protocols and their members (extensions included). SwiftUI views for `get_component`: what callers pass in (`let`, `@Binding`, `@ObservedObject`), own state (`@State`, `@AppStorage`, …) with starting values, environment, the views each shows and where it is shown. Screens for `list_pages`: windows, settings, menu-bar extras, tabs, `NavigationLink` / `.navigationDestination`, sheets, covers, popovers. Translations from `.strings` and `.xcstrings`.
- **Android (Kotlin / Java):** classes, interfaces, objects, enums and members, named by package. Jetpack Compose functions for `get_component`: parameters, remembered state with starting values, the composables each shows and who shows it. Screens for `list_pages`: manifest activities and other components (with what the launcher activity shows), Navigation Compose destinations, dialogs, bottom sheets, Fragments, and app-made navigation through `when` blocks. Translations from `res/values*/strings.xml`.
- **Rust:** structs, enums, traits, functions, `impl` methods (attached to their type), fields, variants, consts and macros, named by Rust path (`my_crate::store::Store::open`). Binaries and clap subcommands for `list_pages`; a crate map in `project_overview`.

Every answer is either a fact with `file:line`, or an explicit `found: false` with suggestions. Anything codemap cannot work out statically is reported as `unresolved` rather than guessed.

Supported:
- Laravel 9 and later, both the 11+ `bootstrap/app.php` layout and the older Kernel/RouteServiceProvider layout (including apps upgraded to 11+ that kept it).
- Vue 2.6/2.7 and Vue 3, built with Vue CLI, Vite, webpack or Laravel Mix (aliases come from `vue.config.js`, `vite.config` and `tsconfig`/`jsconfig`).
- Nuxt 2, 3 and 4, with `srcDir` (Nuxt 4 `app/` or the root), component dirs, `pathPrefix` and layers inside the project.
- Next.js app router and pages router, React function components, Supabase.
- Swift packages (`Package.swift`) and Xcode apps (`.xcodeproj` / `.xcworkspace`) with SwiftUI. Every top-level folder is read; `.build`, `DerivedData`, `Pods`, `Carthage` and asset catalogs are skipped.
- Android apps built with Gradle, in Kotlin or Java, with Jetpack Compose or Fragments. The Gradle build can be at the root or one folder down (an `android/` folder beside an iOS app); `build/`, `.gradle/` and `.idea/` are skipped.
- Rust crates and Cargo workspaces; `target/` is skipped.

A repository can be several at once: a Laravel app with an Inertia/Vue frontend gets both sets of facts, and an iOS app with an `android/` folder gets the Swift and the Android facts.

The full specification is in [BUILD_PROMPT.md](BUILD_PROMPT.md).

## Install

codemap is one program with no dependencies. Pick one way to get it:

| | Needs | Good for |
|---|---|---|
| **Download** (recommended) | nothing | most developers |
| **Cargo** | Rust 1.99+ and a C compiler | building from source |
| **Docker** | Docker | keeping the program away from your machine; the project is mounted read-only |

On **Windows**, download the Windows program (or use WSL or Docker Desktop): see [Windows](#windows).

### Download

Ready-made programs for macOS (Apple Silicon and Intel), Linux (x86-64 and ARM64) and Windows (x86-64) are on the [Releases](https://github.com/syahrul84/codemap/releases) page. To install the latest one:

```bash
curl -fsSL https://github.com/syahrul84/codemap/releases/latest/download/codemap-aarch64-apple-darwin.tar.gz | tar xz
```

Use the file for your machine: `codemap-aarch64-apple-darwin` (Mac with Apple Silicon), `codemap-x86_64-apple-darwin` (Intel Mac), `codemap-x86_64-unknown-linux-gnu` or `codemap-aarch64-unknown-linux-gnu` (Linux). Then put it on your `PATH` and check it:

```bash
mkdir -p ~/.local/bin && mv codemap ~/.local/bin/
codemap --version
```

If macOS refuses to open a file downloaded with a browser, remove the download flag once: `xattr -d com.apple.quarantine ~/.local/bin/codemap`. Each release has a `SHA256SUMS` file to check the downloads against.

### Windows

**The Windows program (recommended)**

In PowerShell (Windows 10 and later have `curl.exe` and `tar`):

```powershell
curl.exe -fsSLO https://github.com/syahrul84/codemap/releases/latest/download/codemap-x86_64-pc-windows-msvc.zip
tar -xf codemap-x86_64-pc-windows-msvc.zip
New-Item -ItemType Directory -Force "$HOME\bin" | Out-Null
Move-Item -Force codemap.exe "$HOME\bin\"
[Environment]::SetEnvironmentVariable("Path", "$([Environment]::GetEnvironmentVariable('Path', 'User'));$HOME\bin", "User")
```

Open a new terminal, then check it with `codemap --version`, and in your project folder connect it to Claude Code:

```powershell
claude mcp add codemap -- codemap serve --project "$PWD"
```

`codemap update` works the same as on macOS and Linux. Windows cannot overwrite a running program, so the old one is moved aside as `codemap.old.exe` and removed the next time codemap starts. Git for Windows is needed for the git-based features (changed files, release check), as on other systems.

You can also run codemap through WSL or Docker Desktop:

**WSL**

1. Install WSL once, in PowerShell as administrator, then restart:
   ```powershell
   wsl --install
   ```
2. Open **Ubuntu** from the Start menu and install codemap there (use `codemap-aarch64-unknown-linux-gnu` on an ARM PC):
   ```bash
   curl -fsSL https://github.com/syahrul84/codemap/releases/latest/download/codemap-x86_64-unknown-linux-gnu.tar.gz | tar xz
   mkdir -p ~/.local/bin && mv codemap ~/.local/bin/
   codemap --version
   ```
3. Use it from your project folder. Projects inside WSL (`~/code/my-app`) are much faster to read than ones on the Windows drive (`/mnt/c/Users/me/my-app`), but both work.
4. Connect it to Claude Code:
   - **Claude Code running inside WSL** (the usual setup): in the project folder, run `claude mcp add codemap -- codemap serve --project "$(pwd)"`.
   - **Claude Code running on Windows**: let it start codemap through WSL, with the project's path as WSL sees it. In PowerShell, in the project folder:
     ```powershell
     claude mcp add codemap -- wsl bash -lc "codemap serve --project /mnt/c/Users/me/my-app"
     ```
     Replace the path with your project's (`wsl wslpath -a .` prints it).

**Docker Desktop**

Install [Docker Desktop](https://www.docker.com/products/docker-desktop/), then in PowerShell:

```powershell
git clone https://github.com/syahrul84/codemap; cd codemap
docker build -t codemap:latest --target runtime .
```

and in your project folder:

```powershell
claude mcp add codemap -- docker run -i --rm -v "${PWD}:/workspace:ro" -v codemap-data:/data codemap:latest serve --project /workspace
```

The `.mcp.json` in [With Docker](#with-docker) works on Windows too.

### Cargo

```bash
cargo install --locked --git https://github.com/syahrul84/codemap --tag v0.1.9 codemap-cli
```

### Docker

```bash
git clone https://github.com/syahrul84/codemap && cd codemap
docker build -t codemap:latest --target runtime .
```

### Updating

```bash
codemap update           # download and install the latest release (checked against its SHA256SUMS)
codemap update --check   # only say whether there is a newer one
```

Claude Code picks up the new version in its next session. Indexes update themselves the first time the new version runs. With Cargo, run the `cargo install` line again with the new tag; with Docker, `git pull` and build the image again.

Once a day, `codemap serve` and `codemap doctor` ask GitHub whether a newer release exists (a plain request for the public release list; nothing about your project is sent). When there is one, `codemap doctor` and `project_overview` say so, so Claude can tell you. To turn this off, set `CODEMAP_NO_UPDATE_CHECK=1` (in your shell profile, or in the `env` of the MCP server entry).

## Indexing and checking a project

Inside a project folder:

```bash
codemap scan      # build the index (the first time takes a few seconds; later runs only re-read changed files)
codemap doctor    # what was detected, and anything codemap could not read
codemap map --out codemap.html   # the code map page; open it in a browser
```

The index lives outside the project (`~/Library/Caches/codemap` on macOS, `~/.cache/codemap` on Linux; `--data-dir` or `CODEMAP_DATA_DIR` to change it). codemap only reads the project: it never writes to it and never runs its code (unless you turn on artisan mode).

## Use it from Claude Code

The quickest way, for every project:

```bash
codemap setup
```

It does three things for Claude Code:

1. Registers the MCP server for every project (`claude mcp add --scope user codemap -- codemap serve --project .`).
2. Writes a short rule block into `~/.claude/CLAUDE.md`, between `<!-- codemap:begin -->` and `<!-- codemap:end -->`, saying to ask codemap before grep or reading files.
3. Adds a hook pair to `~/.claude/settings.json`: a code search (grep, rg, find, the Grep and Glob tools) is refused until a codemap tool has run in the session in the last 30 minutes. The refusal tells the AI which codemap tool to use. Searches codemap does not answer go through: logs, git history, `ssh`, `docker`, dependencies, build output and temp folders. The hooks are `codemap hook check` / `codemap hook used`; any error lets the search through.

Without the hook, AI apps often keep using grep even with codemap connected. `codemap usage` shows how many searches the hook refused or allowed.

`--scope project` writes into this folder instead (`CLAUDE.md`, `.claude/settings.json`). With `CLAUDE_CONFIG_DIR` set, the user scope writes there, as Claude Code does. `codemap setup --dry-run` shows what would change and changes nothing. Before changing a file that exists, setup keeps the old one next to it as `<name>.codemap-backup`. `--no-hook` installs the server and the rule block without the search hook (and takes out a hook installed before). `codemap setup --check` shows what is installed; `codemap setup --remove` undoes it. In `.codemap.toml`, `[agent] search_window_minutes = 30` and `allow_search = ["docs/", "*.md"]` change the window and add searches that are always allowed.

To register it by hand instead:

Register it for a project (run inside that project's folder):

```bash
claude mcp add codemap -- codemap serve --project "$(pwd)"
```

Or commit a `.mcp.json` in the project root, so everyone on the team gets it. Claude Code expands `${VAR}` and `${VAR:-default}` in `command`, `args` and `env`:

```json
{
  "mcpServers": {
    "codemap": {
      "command": "codemap",
      "args": ["serve", "--project", "${PWD}"]
    }
  }
}
```

### With Docker

```bash
claude mcp add codemap -- docker run -i --rm -v "$(pwd)":/workspace:ro -v codemap-data:/data -e CODEMAP_PROJECT_ID="$(pwd | shasum | cut -c1-12)" codemap:latest serve --project /workspace
```

```json
{
  "mcpServers": {
    "codemap": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-v", "${PWD}:/workspace:ro",
        "-v", "codemap-data:/data",
        "codemap:latest", "serve", "--project", "/workspace"
      ]
    }
  }
}
```

Notes:
- **Project id:** without `CODEMAP_PROJECT_ID`, the index id comes from the git remote + root commit, so it is stable per repository. Set it explicitly if you keep several checkouts of the same repository.
- **`-i`, never `-t`:** a TTY corrupts the MCP stdio stream. All logs go to stderr.
- **Read-only:** the project is mounted read-only and codemap never writes to it. The index lives in the `codemap-data` volume. Commands that write a file (`codemap map`) need a writable folder mounted for it (see [Code map page](#code-map-page)).

If you ran `codemap setup`, the rule block is already in place. With Docker or manual registration only, add [CLAUDE.snippet.md](CLAUDE.snippet.md) to the project's `CLAUDE.md` yourself, so Claude uses the tools at the right moments (setup's search hook needs the `codemap` program on the host).

### Other AI tools

codemap is an MCP server, and MCP is an open standard: Cursor, Windsurf, VS Code (Copilot), Zed, OpenAI Codex CLI, Gemini CLI and other MCP clients use the same tools. Register `codemap serve --project <folder>` (or the Docker command) as an MCP server in that tool's settings. The tool descriptions and server instructions every client reads already say when to use each tool; for stronger guidance, put the contents of [CLAUDE.snippet.md](CLAUDE.snippet.md) in that tool's rules file (`AGENTS.md`, `.cursor/rules`, `GEMINI.md`, `.github/copilot-instructions.md`). The one feature that depends on the client is the question codemap asks before sending a wrong-answer report: a client that cannot show it simply never sends (see [Wrong-answer reports](#wrong-answer-reports)).

### Tools

| Tool | When Claude should use it |
|---|---|
| `project_overview` | First, in every session: framework version and layout, counts, key directories, main namespaces, models, index health. About 1,200 tokens on an 1,200-file app. Swift, Android and Rust projects get their own section: app entry points, modules or crates, counts of types, views / composables and screens. |
| `search_symbol` | Before writing a class, method or helper, to reuse existing code. Matches short names, FQNs, `Class::method` and partial names. A `kind` filter that matches nothing falls back to other kinds (`useCart` as a composable finds the `useCartStore` store); typos and reworded names (`StatusOfOrder`) give `found: false` + suggestions. The app's own code ranks above test and fixture copies of the same name. |
| `get_symbol` | Instead of reading a whole file. Models include table/fillable/casts/relations; controllers their routes and Form Requests; Form Requests their rules. `include_source` adds the code. |
| `get_component` | Before using, changing or creating a Vue component, Nuxt page, SwiftUI view or Compose function: props / inputs / parameters, emits, `v-model`, slots, state, what it renders (resolved to files or libraries), who renders it, how it is opened, stores and composables it uses. Takes a name (`OrderCard`), tag (`order-card`) or path. |
| `list_pages` | To find a screen and its code (frontend pages, Filament admin pages with their form fields, SwiftUI and Android screens, Rust binaries and subcommands): URL or kind, route name, the component file it renders, layout, middleware, and where it is defined or opened. Filters: `path_contains`, `name_prefix`, `file`. |
| `list_routes` | To find Laravel endpoints or which controller handles a URI (filters: `uri_contains`, `name_prefix`, `method`, `controller`, `middleware`). |
| `api_usage` | To link the frontend to the backend: which route each HTTP call hits, calls to missing endpoints (marked as dead code when the function around them is unused), wrong methods, fields read but not returned, and payloads that miss required fields or send fields the action does not take. `git_changes: true` limits it to what the branch touches, `new_only: true` hides findings saved by `codemap baseline`. Calls that can't be tied to one route are listed with `file:line`, the reason and the candidate routes. Pass `endpoint` for its callers and response fields, `file` for one screen, or `status: missing / fields / payload / unresolved`. |
| `find_usages` | Before changing, renaming or removing anything: every use with `file:line`, the routes involved, and the tests that touch it. Method calls on objects whose class is unknown are listed separately as `possible`. With `changed_files` (or `git_changes: true`) it summarises a whole change instead: what each file affects, the related tests, and the exact commands to run only those (`php artisan test …`, `npx vitest run …`, `npx playwright test …`). It also names the browser tests (Playwright, Cypress) that open the screens the change is close to, or look for test ids the changed code sets; tests that reach the change only through shared building blocks are listed separately for a run before release. The five browser tests that use the change most directly come first (`browser_tests_top`, `run_top`). A backend change is followed through its callers to the controllers, routes and the PHP tests that hit them (`routes_via_callers`, `tests_via_route`). Commands use the project's package manager (`pnpm exec`, `yarn`, `bunx`, `npx`), or the commands set in `[tests]`. In a workspace it also lists the frontend calls (in every app) to the backend routes the change touches, and the browser tests for those screens. Code that exists but that nothing uses comes back as `found: true, unused: true`. For Swift, Kotlin, Java and Rust, type uses are exact; members are matched by name, so uses outside their own file are `possible`. Rust uses in `#[cfg(test)]` / `#[test]` code are listed as tests. |
| `get_schema` | Before writing queries, migrations or model code (or, with `settings: true`, to see app settings and config defaults; with `migrations: true`, migrations not yet applied to your local / staging databases): tables, columns, keys, foreign keys, mapped models, where each column was created and changed. |
| `verify` | Before finishing: checks classes, `Class::method`, functions, routes (name or `METHOD uri`), tables, `table.column`, `Model::relation`, Vue components, endpoints the frontend calls, translation keys (with the locales they are missing in), or a PHP snippet. Swift, Kotlin and Rust names work as classes and `Type::method` (or `Type.method`). Each item is `exists`, `not_found` (with suggestions), `external` (vendor / PHP built-in) or `unresolved`. With `release_check: true`: one list of problems before a release (see below). |
| `report_wrong_answer` | When a codemap answer turned out wrong after checking the files: reports it once with the answer's `meta.call_id` (see [Wrong-answer reports](#wrong-answer-reports)). |

A `found: false` (or `not_found`) answer lists `not_scanned` folders when the project has code outside `[scan] include`: a name defined there is not in the index, so "not found" does not mean it does not exist. Add those folders to `[scan] include` if they belong to the app.

Every tool takes `detail: "brief" | "full"` (default `brief`). List tools take `limit` (default 20, max 100) and `cursor`. Every response has the same envelope:

```json
{ "data": { ... },
  "meta": { "project": "...", "indexed_at": "...", "git_commit": "...", "git_dirty": false,
            "stale": false, "reindexed_files": 0, "truncated": false, "next_cursor": null, "warnings": [] } }
```

### Freshness

There is no file watcher; Docker Desktop bind mounts don't deliver file events reliably. Instead, before answering a call, codemap checks file stats (at most every `check_interval_ms`, default 2 s) and re-indexes what changed. Typical costs through Docker Desktop:
- On a 4,300-file Laravel project: about 0.1 s after editing a controller, about 0.25 s after editing a route file.
- On a 2,500-file Vue 2 project: about 0.1 s for a check that finds nothing changed. A full first index takes about 1–2 s.
- On a 6,100-file Laravel API with 6,100 routes: the first index takes about 4.5 s; `find_usages` on a controller behind 1,000 routes about 0.2 s.
- Swift and Android apps of 10–40 files index in about 0.1 s; a 135-file Rust workspace in about 0.4 s.

If re-indexing takes longer than 1 s, the answer comes from the previous index with `stale: true` and a warning. The very first index is always awaited, because an empty index would wrongly say "not found".

## CLI

`--project` defaults to the current folder.

```bash
codemap scan   [--project <path>] [--full] [--json]   # build/update the index
codemap serve  [--project <path>]                     # MCP server on stdio
codemap query  [--project <path>] <tool> '<json>'     # call any MCP tool from the shell
codemap stats  [--project <path>] [--json]            # counts, timings, index size
codemap doctor [--project <path>] [--json]            # detection reasons, warnings, unresolved routes, broken routes
codemap map    [--project <path>] --out <file.html>   # the code map: one page to open in a browser
codemap misses [--project <path>] [--json] [--clear]  # names the AI asked for that don't exist, ranked
codemap reports [--project <path>] [--all] [--json] [--export <dir>] [--close <id>] [--send <id>] [--clear] [--to-fixture <id>]
codemap setup  [--scope user|project] [--dry-run] [--no-hook] [--check] [--remove]   # register with Claude Code (see above)
codemap hook   check|used                             # called by the hooks setup installs
codemap update [--check] [--force]                    # install the latest release
codemap usage  [--project <path>] [--json] [--days N] [--clear]  # how codemap is used here (see below)
codemap baseline [--project <path>] [--out <file>]    # save today's API findings; later runs show only new ones
codemap check api [--project <path>] [--changed] [--base <branch>] [--baseline <file>] [--json]
```

`codemap check api` is a gate for CI and pre-commit hooks. It prints the API findings that are not in the baseline and exits with 1 when a new call has no route or uses the wrong method. Fields read but not returned and payload problems are printed as warnings. `--changed` limits it to what the branch touches: calls in changed files, and calls to backend routes whose route file or controller changed. Commit the baseline as `.codemap-baseline.json` (`codemap baseline --out .codemap-baseline.json`) so CI finds it; without `--out` it is kept next to the index.

Global options: `--name` / `CODEMAP_PROJECT_NAME` (display name; inside Docker the folder is always `workspace`), `--link name=path` / `CODEMAP_LINKS`, `--project-id` / `CODEMAP_PROJECT_ID`, `--data-dir` / `CODEMAP_DATA_DIR` (default `/data` in Docker, the platform cache dir elsewhere), `--log-level` / `CODEMAP_LOG`.

With the Makefile: `make scan PROJECT=/path`, `make doctor PROJECT=/path`, `make serve PROJECT=/path`.

`codemap doctor` explains how the project was understood:
- why it chose a version and layout
- every route it could not resolve, and why
- **routes whose controller or method does not exist**
- grouped warnings (raw SQL in migrations, dynamic table names, code the parser could not read)
- translations: keys missing per locale, keys used but not defined, plain-text keys and possibly unused keys
- for Vue / Nuxt: how the frontend was detected, **routes whose component file is missing**, routes built at runtime, and **component tags that resolve to nothing**. These are usually imports of files that no longer exist, or components used without being registered.

### Usage report

`codemap usage` shows how codemap is used in a project, so you can see whether it helps and where it falls short:
- AI app sessions, and which app connected.
- Tool calls per tool: how many, time taken (average and 95th percentile, answers over 5 s), answer size, answers that found nothing, refused arguments (also those the MCP library refuses before a tool runs), errors, and calls the AI had to repeat with other arguments within 2 minutes (also in another app of a workspace).
- Accuracy signals: wrong answers the AI reported (`report_wrong_answer`) or codemap caught itself, open and closed, and names the AI asked for that do not exist; slow answers codemap reported are counted apart. codemap cannot see whether an answer was right; these are the signals it has.
- Index runs: how many, why (server start-up, before an answer, `codemap scan`, a linked frontend), full or not, files read, and time taken (total, average, slowest, last full index).

`--json` gives every number, also per day, for scripts or for an AI to turn into a page. Each app keeps its log in `usage.jsonl` next to its index for 90 days; it never leaves the machine. Arguments are kept without code: `php_snippet`, type-check output and any long or multi-line text are replaced by their length. `CODEMAP_NO_USAGE_LOG=1` turns the log off; `codemap usage --clear` empties it.

### Code map page

`codemap map` writes one HTML page that shows how the project fits together:

- **Screens** (Nuxt / vue-router / Next.js pages, Filament admin pages) → the **frontend files** whose API calls they reach (following components, imports and the functions a screen uses) → the **endpoints** those calls hit → the **controllers** behind them → the **models** the controllers use → the **tables** they are stored in. Supabase apps link screens and route handlers straight to the tables they read.
- Click anything to see its file and line, methods or columns, middleware, the API calls a file makes (and calls to endpoints that don't exist), model relations and foreign keys, and everything it connects to.
- Search, filter by app in a workspace, light and dark themes, and a **Problems** tab with the release check (problems, low-priority notes, lines marked as intended). Items in a file with a problem are marked red.

A frontend linked to its backend (`--link`, `[[links]]`) gets the backend's endpoints, controllers, models and tables in its map too. The page is self-contained (no external scripts or fonts) and works offline; nothing in it is sent anywhere. The project is mounted read-only, so give the container a writable folder for the page:

```bash
docker run --rm -v "$PWD":/workspace:ro -v codemap-data:/data -v "$PWD/codemap-out":/out \
  codemap:latest map --project /workspace --out /out/codemap.html
```

or `make map PROJECT=/path` (writes `./codemap-out/codemap.html`).

### AI mistakes log

While Claude works through the MCP server, every name it asks for that does not exist is logged: a class, method, column, table, route, component, translation key or endpoint it wrote in `verify` (claims or a PHP snippet), and lookups (`search_symbol`, `get_symbol`, `get_component`, `get_schema` table, `find_usages`, `api_usage` endpoint) that came back `found: false`, with the closest real name. The log is `misses.jsonl` next to each app's index in the data volume; it never leaves it.

- `codemap misses` ranks them, most frequent first, with the real name and an idea: a name the AI keeps guessing wrong (`orders.order_status` when the column is `orders.status`) is often unclear to people too, so it is worth renaming or a line in CLAUDE.md; a name with nothing like it means the AI assumed something that was never built.
- Every entry is asked again when the list is read, so names created since (the AI checked first, then built it) drop out.
- `project_overview` lists the ones made more than once (`often_mistaken`), so Claude avoids them in the next session.
- The code map has an **AI mistakes** tab, and marks the real item each wrong name was closest to.
- `codemap misses --clear` empties the log.

### Wrong-answer reports

When a codemap answer turns out to be wrong, the AI reports it with `report_wrong_answer`, pointing at the answer by its `meta.call_id` (every answer has one; the server keeps the last 50). The report keeps codemap's real answer, what is right, the evidence (`file:line`), the codemap version and the git commit, and optionally:

- an **`expect` check** codemap re-runs: `{ "verify": { "columns": ["orders.collected_at"] }, "is": "exists" }`, or `{ "tool": "get_schema", "args": { "table": "orders" }, "contains": "collected_at" }` (or `"lacks"`). When it passes, the report closes, noting the version that fixed it.
- a **made-up `example`**: the smallest project that makes codemap give the same wrong answer, with neutral names (`Post`, `posts.title`) and none of the real code. codemap indexes it in a temporary folder and keeps it only if it really reproduces, and blocks it when it contains a name from your project, a long line copied from your files, or something that looks like a secret, an email, an IP address or a private link.

codemap also records some wrong answers itself: an answer saying something is missing that a later answer finds with no file changed in between, a tool that fails, and an answer slower than 5 seconds with nothing to re-index.

Reports stay in `reports.jsonl` next to each app's index (at most 2 MB). `project_overview` and `codemap doctor` show how many are open.

```bash
codemap reports                 # open reports, re-checked, most frequent first
codemap reports --all           # with the closed ones and the version that fixed them
codemap reports --export dir    # one folder per report: report, cited lines, example, test name
codemap reports --close r_8f3a  # not a real bug
codemap reports --send r_8f3a   # show the issue text and ask y/n before sending
codemap reports --clear
```

**Sending to the codemap repository.** A report whose made-up example reproduces can become a GitHub issue on [syahrul84/codemap](https://github.com/syahrul84/codemap/issues), so the bug gets fixed for everyone:

- codemap asks **the person**, never the AI: through the AI app's question form (MCP elicitation) when the app supports it, or with `codemap reports --send <id>` in a terminal. No answer means nothing is sent.
- Only the made-up example, codemap's answer on it, the codemap version, the operating system and the project type (e.g. "Laravel + Nuxt") are sent. Your code, file paths and names, and codemap's answers about your project are never sent.
- It is filed with your own GitHub login through `gh`; a matching open issue gets a comment instead of a new issue. Without `gh`, you get a filled-in link to submit yourself.
- Turn it off with `[report] send = "never"` in `.codemap.toml`, `CODEMAP_NO_REPORT_SEND=1`, or "don't ask again" in the question (which means don't send).

### Renamed from pidx

The tool used to be called `pidx`. The old names keep working: `.pidx.toml` is read when there is no `.codemap.toml`, `PIDX_*` variables are used when the `CODEMAP_*` ones are unset (`PIDX_DB_LOCAL` too), `pidx-ignore` comments are honoured, and `make build-image` also tags the image `pidx:latest`, so existing MCP configs keep starting. Indexes are rebuilt once under the new `codemap-data` volume name if you switch to it.

### Several apps in one folder (workspace)

Mount the folder that holds the apps (e.g. `shop/` with `api/`, `storefront/` and `admin-app/`):

```json
"args": ["run", "-i", "--rm",
         "-v", "${PWD}:/workspace:ro", "-v", "codemap-data:/data",
         "codemap:latest", "serve", "--project", "/workspace", "--name", "shop"]
```

When the folder is not itself a project, codemap looks one level down for Laravel apps (`artisan` + `composer.json`) and frontend apps (`package.json` with vue, nuxt, next or react). Beside those, Swift (`Package.swift`, `.xcodeproj`), Android (`settings.gradle`) and Rust (`Cargo.toml`) folders are apps too; a folder holding only mobile apps stays one project. To choose the apps yourself, list them in the workspace's `.codemap.toml`: `[workspace] apps = ["api", "storefront", "admin-app"]`.

How a workspace works:
- Each app keeps its own index.
- Vue / Nuxt apps are linked to the workspace's Laravel apps automatically, sharing their index.
- Every tool takes an optional `app`. Without it, the tool runs on every app:
  - an answer found in one app comes back tagged with that app
  - answers found in several apps are grouped per app
  - `verify` merges item by item
- For `find_usages` with `changed_files`, give paths from the workspace root (`api/app/Models/Order.php`).
- Settings in the workspace's `.codemap.toml` (`[i18n] locales`, `[[links]]`) apply to every app unless the app's own `.codemap.toml` sets them.

### Linking a frontend to its backend

When the Vue / Nuxt app and the Laravel API live in separate repositories, link them. Mount the backend read-only and name it:

```json
"args": ["run", "-i", "--rm",
         "-v", "${PWD}:/workspace:ro",
         "-v", "${PWD}/../backend:/links/backend:ro",
         "-v", "codemap-data:/data",
         "codemap:latest", "serve", "--project", "/workspace", "--link", "backend=/links/backend"]
```

Or put the link in the frontend's `.codemap.toml` (`[[links]] name = "backend"`, `path = "../backend"`, optional `api_base = "/api/v1"`). `CODEMAP_LINKS=backend=/links/backend` works too.

How linking works:
- The backend's index is kept up to date in the same data volume; the backend repository is only read.
- The API base path (e.g. `/api/v1` when the frontend calls `/orders`) is inferred by trying the route prefixes shared by many routes and keeping the one that explains the most calls.
- Small URL builders are evaluated, including their arguments and defaults: `` const url = (id, suffix = '') => `/outlets/${id}/templates${suffix}` ``.
- Project wrappers that build the URL from their own parameters are filled in with the caller's arguments: `api.get(endpoint, id, '/history')` with `` get(endpoint, id, suffix = '') { return http.get(`${endpoint}/${id}${suffix}`) } `` is `{endpoint}/{id}/history`, in object methods or plain functions, imported or local.
- When the caller's part is a variable, a route parameter is preferred over a fixed word among routes with the right method (`/orders/{}` is `orders/{order}`, not `orders/export`).
- URLs that start with an unknown variable are matched by their known tail. When that is ambiguous, the call is reported as `unresolved` (with `file:line`, why, and the candidate routes) rather than missing.

How the response check works:
- Fields read are found heuristically: property reads on a call's result, template reads (including `v-for`) of the state the result is stored in, what child components read from props the data is passed to, and what callers of a helper function read from its return value.
- Functions that return a bundle of helpers (`return { list, remove }`) are not mistaken for response data.
- "Read but not returned" is only reported when the endpoint's response is fully known: an API Resource, a literal array, or a list whose every building step is readable. Item keys are checked one level deep. Responses built from queries are reported by kind only.
- Laravel's `_method` spoofing (`POST` with `_method: 'PUT'`) is understood.
- Each fields finding carries `evidence`: where the response shape came from (the Resource and its `toArray` file:line, or the `return` in the action), every key it returns (conditional ones marked), and the paths the frontend read.

How the payload check works:
- The keys a call sends are read from the body object, the `params` / `query` / `body` options, `FormData.append` / `URLSearchParams.set`, `x.key = …` assignments and the query string in the URL.
- They are compared with what the action accepts: its Form Request's `rules()`, an inline `validate([...])` / `Validator::make`, and the fields it reads (`$request->input('x')`, `$request->x`, `$request['x']`, `request('x')`, `->only([...])`), also in the controller's own helper methods.
- `missing_required`: a required field the call never sends (only when the payload is fully known). `unknown_field`: a sent key the action does not take, often a renamed field (only when the accepted list is complete: no `$request->all()`, and the request is not handed to other code).
- Payloads built through spreads, component state or helpers are reported as `payload_unchecked`, never guessed. Keys added by middleware (`$request->merge([...])`) are not known yet.

## Configuration — `.codemap.toml` (optional, project root)

```toml
[project]
name = "shop-api"            # display name (default: composer.json name, then git repo name)

[scan]
# Default: Laravel dirs plus Vue/Nuxt source dirs (missing ones are skipped):
include = ["app", "routes", "database", "config", "bootstrap", "lang", "src", "resources/js",
           "components", "pages", "layouts", "composables", "stores", "store", "plugins",
           "middleware", "server", "utils", "mixins", "i18n", "locales", "tests", "test",
           "resources/lang", "resources/views",
           # React / Next.js and Supabase:
           "lib", "hooks", "context", "contexts", "providers", "actions", "features",
           "services", "types", "supabase", "middleware.ts", "middleware.js", "proxy.ts", "proxy.js"]
exclude = ["/vendor", "node_modules", "/storage", "/public", "/bootstrap/cache", "/dist", "/.output", "/coverage"]

[laravel]
artisan = "off"              # "off" | "auto" | "on"
artisan_command = "php artisan"
artisan_timeout_ms = 15000

[freshness]
check_interval_ms = 2000

[i18n]
locales = ["en", "ms"]       # locales you ship; others are reported as partial, not missing

[tests]                      # how tests are run; test files are appended
php = "docker compose exec -T app php artisan test"
# Default for PHP: when a docker-compose file (in the app, its parent folder or a
# sibling folder) mounts the app into a service, `docker compose exec -T <service>
# php artisan test`; otherwise `php artisan test` on the host.
# js = "pnpm exec vitest run"
# browser = "pnpm exec playwright test"
# In a workspace's .codemap.toml: [workspace.tests.<app>] with the same keys.

[copy]                       # house copy rules, checked by the release check
banned = { ms = ["beg"], en = ["bag", "bags", "save food"], all = ["rescue"] }

[release]                    # release-check lines to leave out (substring match)
ignore = ["ALLOW_DB_WIPE"]

[[databases]]                # compare migrations with real databases (read-only)
name = "local"
env = "CODEMAP_DB_LOCAL"        # the URL comes from this environment variable, never from a file
[[databases]]
name = "staging"
env = "CODEMAP_DB_STAGING"
# app = "api"                # in a workspace: only for this app (default: every Laravel app)
# table = "migrations"       # default: config/database.php, else `migrations`
# schema = "live"            # also read tables and columns from this database (see below)
# dump = "db/structure.sql"  # or from a structure dump file, without connecting
```

### Background work and permissions

- `get_schema` with `background: true`: the schedule (command, job or closure, frequency, the command's class), queued jobs and where they are dispatched, events with their listeners and where they are fired, notifications and mails and where they are sent (or "nothing sends it").
- `list_routes` with `permissions: true`: per route, the middleware that can decide access, checks inside the action (`authorize`, `Gate::…`, `->can(…)`, `abort_if`), the Form Request's `authorize()`, and the policy method a `can:` middleware or check lands on; routes with none are marked `public`.
- `list_pages` with `permissions: true`: for admin (Filament) pages, the model's policy methods and `canAccess()` / `canViewAny()` overrides.

### Cache map

`get_schema` with `cache: true` lists every cache key (or key pattern, `brands:{}`, with constants resolved) with where it is written and its TTL, where it is read and where it is cleared (`forget`, tagged or full `flush`). It flags keys stored forever that nothing clears, keys read but never written, and keys cleared but never written (a typo or leftover). `key_prefix` narrows it.

### Release check

`verify` with `release_check: true` returns only problems, per app (and one `clean` verdict for a workspace):
- frontend calls to missing endpoints, wrong methods, fields read but not returned
- translation keys missing in a shipped locale, or used but defined nowhere
- unused code added on the branch (compared with origin/HEAD, main, master or develop, or `base`): components nothing renders, functions and classes nothing uses
- migrations not run on the configured databases
- config keys read but not defined, `env()` in app code, new env variables without a default missing from `.env.example`
- settings read without a default that nothing in the app saves (a setting saved by code or an admin page, like a "last alert time", is empty until then by design: it is listed under `low_priority`)
- house copy rules: words banned per locale (`[copy] banned`), in translation files and admin labels
- validation messages that would show a raw field key: a nested field (`translations.*.description`) whose last part has a name in `validation.attributes` (in that language, or in another shipped language when this one names nothing, like Laravel's default `en`) but the full key does not, so its errors show the raw path ("translations.<key>.description …")

Each app's answer has `problems`, `low_priority` (worth a look, not blocking: e.g. fields a frontend type lists that the endpoint doesn't send and nothing reads, or form fields with no translated name), `notes` and `ignored`. A line is ignored when it matches `[release] ignore`, or when the source line it points at (or the line above) has a `codemap-ignore` comment:

```php
// codemap-ignore: only set on throwaway test databases
if (env('ALLOW_DB_WIPE')) { … }
```

Translations: empty lists in a lang file (`'items' => []`) are not keys; uses with a fallback (`$t('a.b', 'Default text')`) or behind an existence check (`Lang::has('a.b')`, `$key === 'x'`) are not reported as missing; sentences used as Laravel keys (`__('Save changes')`) are plain text; a one-word key with no translation (`__('canceled')`) is reported, because speakers of the other shipped languages see the word as written. `verify` with `claims: {"i18n_keys": ["*"]}` lists every translation problem one row each.

#### Release steps, file rules, leftovers and live-data tests

The release check also answers:

- **`release_steps`: what the server needs for this release**, from what changed on the branch: `composer install` (dependencies changed) or `composer dump-autoload --optimize` (classes added, deleted or renamed), `php artisan migrate --force` (new migrations), `php artisan optimize` (config, routes or listeners changed), `filament:optimize`, `horizon:terminate` or `queue:restart` (workers keep the old code), `octane:reload` or a PHP-FPM reload, the frontend install and build, restarting a Nuxt / Next.js server, and environment variables new in `.env.example` (public ones are read at build time). Each step says which files call for it.
- **`rules`: your project's rules tied to files.** In `.codemap.toml`:

  ```toml
  [[rules]]
  files = ["database/migrations/**"]
  when = "modified"            # added | modified | deleted | renamed | changed (default)
  note = "Never edit a migration that has already run; add a new one."
  # level = "problem"          # make the release check fail on it
  ```

  A rule whose files change comes back in the release check and in `find_usages` change impact. In a workspace's file, `app = "backend"` limits a rule to one app.
- **Leftover mentions**: classes, components, functions and methods deleted or renamed on the branch that are still named somewhere. Mentions in code and tests are problems; in comments and docs, low priority. A name that is still defined elsewhere (moved, or another class's method of the same name) is not reported.
- **Tests that depend on live data** (low priority, and marked in change impact as `tests_on_live_data`): browser tests that check page content without seeding, API setup or mocked responses, and PHP feature tests that check data without factories, seeders or inserts (in the test or its parent test class). They pass only while the database holds the right records.

#### New type errors only

codemap does not run the type check; Claude does, and passes the output to `verify`:

```json
{ "app": "consumer", "type_errors": "<output of vue-tsc --noEmit / nuxi typecheck / tsc / phpstan --error-format=raw>" }
```

The first output becomes the starting point (kept next to the app's index). After that, only errors that are new come back (`new`, `new_total`), with how many were fixed since. Line numbers are ignored when comparing, so an old error that moved is not new. Add `"type_errors_save": true` to make the current output the new starting point.

### Migrations not yet applied

`get_schema` with `migrations: true` (and `codemap doctor`) compares the migration files with each database's `migrations` table: migrations a database has not run yet, and rows whose file is gone (squashed into a schema dump, or deleted).

- Connections come only from environment variables you pass to the container: the ones named in `[[databases]]`, plus any `CODEMAP_DB_<NAME>` (found automatically). `.env` is never read.
- Each check is a single `SELECT` on the migrations table in a read-only session, with a 5-second timeout. Passwords never appear in answers or errors.
- PostgreSQL (`postgres://user:pass@host:5432/db`), MySQL / MariaDB (`mysql://…`) and SQLite (`sqlite:///path`). TLS is not used: reach staging through a tunnel or VPN.
- A database whose rows mostly don't match the app's migration files is reported as `other_app` rather than listing false "pending" migrations.

### Schema from the database

When the migrations don't describe the database (an app with almost no migrations, or one that drifted), add `schema = "live"` to its `[[databases]]` entry. `get_schema`, `verify` and the column checks then use the database's own tables, columns, types, nullability, defaults, keys and foreign keys, read from `information_schema` (MySQL / MariaDB, PostgreSQL) or `pragma` (SQLite) in the same read-only session.
- Every column says where it comes from: `source: "live:local"` (or `dump:<name>`); no `source` means the migrations or the schema dump in `database/schema/`.
- When both sides know a table, differences are kept as `drift`: "no migration creates this column", "in migrations, not in live:local", "migrations make it nullable".
- The database is read again at most every 10 minutes. If it can't be reached, the migrations answer and `doctor` says why.
- Without a connection, point `dump` at a structure dump (`mysqldump --no-data`, `pg_dump --schema-only`) instead.

Pass the URLs and, when the database runs in Docker, its network:

```json
"args": ["run", "-i", "--rm", "--network", "myapp_default",
         "-e", "CODEMAP_DB_LOCAL", "-e", "CODEMAP_DB_STAGING",
         "-v", "${PWD}:/workspace:ro", "-v", "codemap-data:/data",
         "codemap:latest", "serve", "--project", "/workspace"],
"env": { "CODEMAP_DB_LOCAL": "postgres://user:pass@postgres:5432/app",
         "CODEMAP_DB_STAGING": "postgres://user:pass@host.docker.internal:15432/app" }
```

Exclude patterns follow `.gitignore` rules. A leading `/` anchors a pattern to the project root, so a source folder that happens to be called `vendor` or `public` is still indexed. `.env*`, `vendor/`, `storage/`, `bootstrap/cache/`, `node_modules/`, `*.d.ts` and minified `*.min.*` bundles are never indexed. Secrets are never read.

### Artisan mode

With `artisan = "on"` (or `"auto"` when `php` and `vendor/` exist), codemap runs `php artisan route:list --json` and uses it as the source of truth, keeping `file:line` from static analysis. Laravel's cache paths are pointed at a temporary directory, so this works on a read-only mount. Any failure or timeout falls back to static routes, with a warning.

**This boots your application, which means it executes project PHP code**, so it is off by default. In Docker it needs the PHP image: `docker build -t codemap-php:latest --target runtime-php .`

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
- Database columns (`orders.collected_at`) are found by name: in the table's models (`$fillable`, `$casts`, …), in files that use those models or name the table, in factories and in frontend code; other mentions are listed as possible.

**Swift, Kotlin, Java, Rust:** parsed with tree-sitter.
- There is no type checker: a member call (`model.refresh()`) is matched by name, and uses outside the member's own file are listed as possible. With an `index.scip` in the project root (see [Exact references](#exact-references-indexscip)) these are settled exactly.
- The Apple and Android SDKs, Swift packages and Rust dependencies are not indexed, so `verify` says a name like `URLSession` is not declared in the project.
- A few valid forms the Swift and Kotlin grammars misread (`@_silgen_name`, regex literals; Kotlin `var x by …; private set`, a modifier word used as a name) are skipped or rewritten before parsing, never reported as errors.
- API calls from mobile and Rust code are not linked to backend routes.

**Not yet:** embeddings and semantic search; linking API calls in Swift, Android and Rust code to backend routes.

## Exact references (`index.scip`)

codemap matches member calls by name, because it reads code without a type checker. Compilers and type checkers can write exact references in the SCIP format; when the project root has an `index.scip`, `find_usages` uses it for the symbol it resolved: "possible" uses the index confirms are listed, the others dropped, and references only the index knows are added. The answer says when files changed after the index was written.

Generate it with the project's own indexer, for example:

```bash
rust-analyzer scip .                       # Rust
npx @sourcegraph/scip-typescript index     # TypeScript / JavaScript
scip-php                                   # PHP (composer require --dev davidrjenni/scip-php)
scip-java index                            # Java / Kotlin (Gradle, Maven)
```

Add `index.scip` to `.gitignore`. Regenerating it (in CI, or a git hook) keeps answers exact; without it codemap works as before.

## Report cases

`fixtures/report-cases/` holds wrong answers codemap once gave, each as a made-up example (`project/`) and the check of the right answer (`case.json`). The test suite indexes every one and runs its check, so a fixed bug cannot come back. Turn a reproducing report into a case with `codemap reports --to-fixture <id>`.

## Troubleshooting

- **The index is empty or "no indexable files found"**: Docker Desktop only mounts shared host paths, and an unshared one (e.g. `/private/tmp`) appears as an empty folder. Use a path under your home folder.
- **`fatal: detected dubious ownership`**: the image already marks `/workspace` as a safe git directory. If you mount the project elsewhere, add `git config --system --add safe.directory <path>` to your own image.
- **`git_dirty` / slow refreshes on macOS**: codemap calls `git status` with `core.checkStat=minimal`, because inside a container the host's inode/device numbers don't match git's index. Without that, git re-reads every tracked file (seconds on large repositories).
- **Responses say `stale: true`**: re-indexing took more than 1 s (e.g. right after a large `git checkout`). The next call is usually fresh; meanwhile Claude should re-check files directly.
- **Routes look wrong**: run `codemap doctor`, which shows where routes were read from and why. If the app can boot, consider artisan mode.
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

Performance smoke tests (ignored by default): `docker compose run --rm dev cargo test --release -p codemap-php --test index -- --ignored --nocapture`. Measured: 2,000 classes / 82,000 symbols cold in about 1 s, no-op refresh about 10 ms.

### Layout

```
crates/codemap-core     store (SQLite + migrations), scanner, git, symbols + search, envelope, adapter trait
crates/codemap-php      tree-sitter-php parsing, name resolution, symbol extraction
crates/codemap-laravel  detection, routes (static + artisan), models, schema replay, Form Requests, doctor
crates/codemap-js       oxc JS/TS analysis, Vue SFC splitter, template tokenizer, literal reader
crates/codemap-vue      Vue/Nuxt detection, component naming and registry, stores, get_component, doctor
crates/codemap-swift    Swift / SwiftUI: symbols, views, screens, string catalogs (tree-sitter-swift)
crates/codemap-android  Kotlin / Java / Compose: symbols, composables, manifest, screens, strings.xml
crates/codemap-rust     Rust: symbols by Rust path, impls, binaries and clap subcommands
crates/codemap-mcp      MCP server (rmcp, stdio) and the tool implementations shared with `codemap query`
crates/codemap-cli      the `codemap` binary
fixtures/            hand-written projects used by the tests: Laravel, Vue 2, Vue 3, Nuxt 2, Nuxt 3, Next.js,
                     Swift, Android, Rust, API linking and payloads, schema dumps, route edge cases,
                     and report-cases/ (wrong answers codemap once gave)
```

Extractor versions (`PHP_EXTRACTOR_VERSION`, `ROUTES_EXTRACTOR_VERSION`, `DATA_EXTRACTOR_VERSION`, `DETECTION_VERSION`, `JS_EXTRACTOR_VERSION`, `REGISTRY_VERSION`, `VUE_DETECTION_VERSION`, `SWIFT_EXTRACTOR_VERSION`, `ANDROID_EXTRACTOR_VERSION`, `RUST_EXTRACTOR_VERSION`) make existing indexes re-extract after an upgrade. Bump the relevant one whenever extraction output changes.

## License

MIT — see [LICENSE](LICENSE).
