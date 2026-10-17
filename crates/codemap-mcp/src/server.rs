//! MCP server over stdio (rmcp). Before each tool call the index is brought up
//! to date within a time budget; past the budget the answer comes from the
//! previous index with `stale: true`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use codemap_core::store::meta_keys;
use codemap_core::{ScanOptions, Store};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Serialize;

use crate::tools::{
    Freshness, GetComponentParams, GetSymbolParams, ListPagesParams, ListRoutesParams,
    OverviewParams, Project, SchemaParams, SearchParams, ToolError,
};
use crate::verify::VerifyParams;

/// How long a call waits for re-indexing before answering from the old index.
pub const REFRESH_BUDGET: Duration = Duration::from_secs(1);

pub const INSTRUCTIONS: &str = "Facts about this project's code, routes, models and database \
schema, read from the source. Call project_overview at the start of a task. Call search_symbol \
before creating any class, method or helper and reuse what exists. Call get_schema before writing \
queries, migrations or model code; never assume column names. Prefer get_symbol over reading whole \
files. For Vue/Nuxt code, call get_component before using or changing a component and search_symbol \
for composables and stores. Call verify with every class, method, route, table and column your change relies on before finishing. After editing, call find_usages with git_changes: true (or changed_files) for one summary of what the change affects and the exact commands to run only the affected tests. If the project is a workspace (several apps), every tool takes an optional `app`; without it, all apps are searched. If a response has stale: true, re-check files directly. found: false means it does not exist \
in the project. If an answer turns out wrong after you checked the files, report it once with \
report_wrong_answer (pass its meta.call_id), then carry on.";

/// One indexed project (the root, or one app of a workspace).
struct App {
    project: Project,
    /// Answers queries; a separate connection so reads never wait for re-indexing.
    reader: Mutex<Store>,
    /// Re-indexes; held for the whole refresh.
    writer: Mutex<Store>,
}

/// `report_wrong_answer` input.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct ReportParams {
    /// `meta.call_id` of the answer that was wrong.
    pub call_id: String,
    /// said_missing | said_exists | wrong_detail | left_out | false_problem | wrong_command | cannot_answer
    pub kind: String,
    /// The exact thing that was wrong: `orders.status`, `GET /api/v1/malls`, `MallCard.vue`.
    pub item: String,
    /// What is actually true, in one sentence.
    pub right: String,
    /// Where it was proved: `file:line` entries, or a command and a short output line.
    pub evidence: Vec<String>,
    /// A check codemap can re-run: `{ "verify": { "columns": ["orders.x"] }, "is": "exists" }`
    /// or `{ "tool": "get_schema", "args": {...}, "contains": "text" }` (or `"lacks"`).
    #[serde(default)]
    pub expect: Option<serde_json::Value>,
    /// A made-up mini project giving the same wrong answer, with neutral names and none of the
    /// real code: `{ "files": { "path": "content" }, "call": { "tool": ..., "args": ... },
    /// "expect": <check of the right answer>, "wrong": "...", "right": "..." }`.
    #[serde(default)]
    pub example: Option<serde_json::Value>,
    /// Workspace app (default: the app of the call).
    #[serde(default)]
    pub app: Option<String>,
}

/// The person's answer to "send this report to the codemap repository?".
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SendChoice {
    /// Send the issue shown above to the codemap repository on GitHub.
    pub send: bool,
    /// Don't ask again for this project (this means don't send).
    #[serde(default)]
    pub dont_ask_again: bool,
    /// Optional: your edited issue text, to send instead of the text shown.
    #[serde(default)]
    pub edited_text: Option<String>,
}
rmcp::elicit_safe!(SendChoice);

/// One answer, kept so a wrong-answer report can point at it.
#[derive(Clone)]
struct Answer {
    id: String,
    tool: String,
    args: serde_json::Value,
    data: serde_json::Value,
    indexed_at: Option<String>,
    git_commit: Option<String>,
    git_dirty: bool,
    stale: bool,
}

/// How many answers are kept for reports.
const KEPT_ANSWERS: usize = 50;
/// An answer slower than this (after the first index) is reported.
const SLOW: Duration = Duration::from_secs(5);

struct Shared {
    apps: Vec<App>,
    /// The last answers, newest last.
    answers: Mutex<VecDeque<Answer>>,
    next_call: AtomicU64,
    /// Bumped whenever re-indexing changed files: answers from the same
    /// generation saw the same code.
    generation: AtomicU64,
    /// Names answered "not found": `kind/item` → (call id, generation).
    said_missing: Mutex<HashMap<String, (String, u64)>>,
    /// What the AI app said about itself when it connected.
    client: Mutex<Option<rmcp::model::InitializeRequestParams>>,
    last_check: Mutex<Option<Instant>>,
    interval: Duration,
    /// Held while a refresh runs, so a call never answers "fresh" from an
    /// index that is still being updated (e.g. by the start-up warm-up).
    gate: tokio::sync::Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
pub struct CodemapServer {
    shared: Arc<Shared>,
    tool_router: ToolRouter<Self>,
}

impl CodemapServer {
    pub fn open(projects: Vec<Project>) -> anyhow::Result<Self> {
        anyhow::ensure!(!projects.is_empty(), "nothing to index");
        let interval = Duration::from_millis(
            projects
                .iter()
                .map(|p| p.config.freshness.check_interval_ms)
                .min()
                .unwrap_or(2000),
        );
        let mut apps = Vec::new();
        for project in projects {
            let writer = Store::open(&project.index_path)?;
            writer.set_meta(meta_keys::PROJECT_ID, &project.id)?;
            let reader = Store::open(&project.index_path)?;
            apps.push(App {
                project,
                reader: Mutex::new(reader),
                writer: Mutex::new(writer),
            });
        }
        Ok(Self {
            shared: Arc::new(Shared {
                apps,
                answers: Mutex::new(VecDeque::new()),
                next_call: AtomicU64::new(1),
                generation: AtomicU64::new(0),
                said_missing: Mutex::new(HashMap::new()),
                client: Mutex::new(None),
                last_check: Mutex::new(None),
                interval,
                gate: tokio::sync::Mutex::new(()),
            }),
            tool_router: Self::tool_router() + Self::report_router(),
        })
    }

    /// Re-indexes changed files if the last check is older than the interval,
    /// or waits for a refresh that is already running.
    pub async fn freshness(&self) -> Freshness {
        let never_indexed = self.shared.apps.iter().any(|a| {
            lock(&a.reader)
                .get_meta(meta_keys::INDEXED_AT)
                .ok()
                .flatten()
                .is_none()
        });
        let due = {
            let mut last = lock(&self.shared.last_check);
            let due = never_indexed || last.is_none_or(|t| t.elapsed() >= self.shared.interval);
            if due {
                *last = Some(Instant::now());
            }
            due
        };
        if !due {
            // Checked recently, but a refresh started earlier may still be running.
            if self.shared.gate.try_lock().is_ok() {
                return Freshness::default();
            }
            return match tokio::time::timeout(REFRESH_BUDGET, self.shared.gate.lock()).await {
                Ok(_) => Freshness::default(),
                Err(_) => stale(format!(
                    "re-indexing is still running after {} ms; answering from the previous index",
                    REFRESH_BUDGET.as_millis()
                )),
            };
        }

        let shared = self.shared.clone();
        let task = tokio::spawn(async move {
            let _gate = shared.gate.lock().await;
            let worker = shared.clone();
            tokio::task::spawn_blocking(move || {
                // Backends first: frontends read their routes when linking.
                let mut total = 0;
                for app in &worker.apps {
                    let mut store = lock(&app.writer);
                    let r = crate::refresh::refresh(
                        &mut store,
                        &app.project.root,
                        &app.project.config,
                        ScanOptions::default(),
                    )?;
                    total += r.reindexed_files();
                }
                Ok::<usize, codemap_core::Error>(total)
            })
            .await
        });
        // The very first index must be complete: an empty index would answer
        // "not found" for code that exists.
        let outcome = if never_indexed {
            Ok(task.await)
        } else {
            tokio::time::timeout(REFRESH_BUDGET, task).await
        };
        match outcome {
            Ok(Ok(Ok(Ok(reindexed)))) => Freshness {
                reindexed_files: reindexed,
                ..Freshness::default()
            },
            Ok(Ok(Ok(Err(e)))) => stale(format!(
                "re-index failed ({e}); answering from the previous index"
            )),
            Ok(Ok(Err(e)) | Err(e)) => stale(format!(
                "re-index crashed ({e}); answering from the previous index"
            )),
            Err(_) => stale(format!(
                "re-indexing takes longer than {} ms; answering from the previous index",
                REFRESH_BUDGET.as_millis()
            )),
        }
    }

    async fn run<P: Serialize>(
        &self,
        name: &'static str,
        params: P,
    ) -> Result<CallToolResult, ErrorData> {
        let args = serde_json::to_value(params)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let fresh = self.freshness().await;
        if fresh.reindexed_files > 0 {
            self.shared.generation.fetch_add(1, Ordering::SeqCst);
        }
        let started = Instant::now();
        let shared = self.shared.clone();
        let call_args = args.clone();
        let (reindexed, was_stale) = (fresh.reindexed_files, fresh.stale);
        let result = tokio::task::spawn_blocking(move || {
            let guards: Vec<_> = shared.apps.iter().map(|a| lock(&a.reader)).collect();
            let apps: Vec<(&Store, &Project)> = guards
                .iter()
                .zip(&shared.apps)
                .map(|(g, a)| (&**g, &a.project))
                .collect();
            let asked = args.clone();
            let result = crate::workspace::call(&apps, name, args, fresh);
            // Names the AI asked for that do not exist go to its mistakes log.
            if let Ok(env) = &result {
                let projects: Vec<&Project> = shared.apps.iter().map(|a| &a.project).collect();
                crate::misses::record(&projects, name, &asked, &env.data);
            }
            result
        })
        .await;
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                // A panic inside the tool: report it, then answer with the error.
                self.auto_report(
                    name,
                    &call_args,
                    "tool_failure",
                    name,
                    &format!("the tool crashed: {e}"),
                    serde_json::Value::Null,
                );
                return Err(ErrorData::internal_error(e.to_string(), None));
            }
        };
        match result {
            Ok(mut envelope) => {
                let id = format!("c{}", self.shared.next_call.fetch_add(1, Ordering::SeqCst));
                envelope.meta.call_id = Some(id.clone());
                self.after_answer(
                    &id,
                    name,
                    &call_args,
                    &envelope,
                    started.elapsed(),
                    reindexed,
                    was_stale,
                );
                let text = serde_json::to_string(&envelope)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(ToolError::InvalidParams(m)) => Err(ErrorData::invalid_params(m, None)),
            Err(e) => {
                self.auto_report(
                    name,
                    &call_args,
                    "tool_failure",
                    name,
                    &format!("the tool failed: {e}"),
                    serde_json::Value::Null,
                );
                Err(ErrorData::internal_error(e.to_string(), None))
            }
        }
    }

    /// The project an app name (or the first app) belongs to.
    fn project_for(&self, app: Option<&str>) -> &Project {
        let apps = &self.shared.apps;
        &app.and_then(|a| apps.iter().find(|x| x.project.app.as_deref() == Some(a)))
            .unwrap_or(&apps[0])
            .project
    }

    /// Records a wrong answer codemap can tell by itself.
    fn auto_report(
        &self,
        tool: &str,
        args: &serde_json::Value,
        kind: &str,
        item: &str,
        right: &str,
        answer: serde_json::Value,
    ) {
        let app = args["app"].as_str().or_else(|| answer["app"].as_str());
        let project = self.project_for(app);
        let r = crate::reports::Report {
            id: crate::reports::id_for(tool, args, kind, item),
            at: jiff::Timestamp::now().to_string(),
            source: "auto".into(),
            app: app.map(str::to_owned),
            tool: tool.into(),
            args: args.clone(),
            kind: kind.into(),
            item: item.into(),
            answer,
            right: right.into(),
            ..Default::default()
        };
        if let Err(e) = crate::reports::add(project, r) {
            tracing::warn!("cannot save a wrong-answer report: {e}");
        }
    }

    /// Keeps the answer for reports, and records the ones that prove
    /// themselves wrong: contradicting an earlier "not found" with no file
    /// changed in between, or very slow.
    #[allow(clippy::too_many_arguments)]
    fn after_answer(
        &self,
        id: &str,
        tool: &str,
        args: &serde_json::Value,
        env: &codemap_core::envelope::Envelope<serde_json::Value>,
        took: Duration,
        reindexed: usize,
        stale: bool,
    ) {
        let generation = self.shared.generation.load(Ordering::SeqCst);
        {
            let mut answers = lock(&self.shared.answers);
            answers.push_back(Answer {
                id: id.to_owned(),
                tool: tool.to_owned(),
                args: args.clone(),
                data: env.data.clone(),
                indexed_at: env.meta.indexed_at.clone(),
                git_commit: env.meta.git_commit.clone(),
                git_dirty: env.meta.git_dirty == Some(true),
                stale: env.meta.stale,
            });
            while answers.len() > KEPT_ANSWERS {
                answers.pop_front();
            }
        }
        if stale {
            return;
        }
        let key = |k: &str, i: &str| format!("{k}/{}", i.to_lowercase());
        // Found now, but said missing earlier on the same code.
        for (kind, item) in crate::misses::found_items(tool, args, &env.data) {
            let earlier = lock(&self.shared.said_missing).remove(&key(&kind, &item));
            if let Some((earlier_id, g)) = earlier
                && g == generation
            {
                let before = lock(&self.shared.answers)
                    .iter()
                    .find(|a| a.id == earlier_id)
                    .cloned();
                if let Some(a) = before {
                    self.auto_report(
                        &a.tool,
                        &a.args,
                        "said_missing",
                        &item,
                        &format!("call {id} ({tool}) found it, with no file changed since call {earlier_id}"),
                        a.data,
                    );
                }
            }
        }
        for m in crate::misses::found_in(tool, args, &env.data) {
            lock(&self.shared.said_missing)
                .entry(key(&m.kind, &m.item))
                .or_insert((id.to_owned(), generation));
        }
        if took > SLOW && reindexed == 0 {
            self.auto_report(
                tool,
                args,
                "slow",
                tool,
                &format!(
                    "answered in {:.1} s with nothing to re-index",
                    took.as_secs_f64()
                ),
                serde_json::Value::Null,
            );
        }
    }
}

fn stale(warning: String) -> Freshness {
    tracing::warn!("{warning}");
    Freshness {
        reindexed_files: 0,
        stale: true,
        warnings: vec![warning],
    }
}

#[tool_router(router = tool_router)]
impl CodemapServer {
    #[tool(
        name = "project_overview",
        description = "Use first in every session to understand the project before reading files: framework version and layout, PHP constraint, PSR-4 roots, counts, route files, key directories, main namespaces, models, index health."
    )]
    pub async fn project_overview(
        &self,
        params: Parameters<OverviewParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("project_overview", params.0).await
    }

    #[tool(
        name = "search_symbol",
        description = "Use before writing any new class, method or helper, to find existing code to reuse, or to locate code. Matches short names, FQNs, Class::method and partial names. Returns found:false with suggestions when nothing exists."
    )]
    pub async fn search_symbol(
        &self,
        params: Parameters<SearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("search_symbol", params.0).await
    }

    #[tool(
        name = "get_symbol",
        description = "Use instead of reading a whole file when you need one class or method: signature, location, members, parents. Models include table, fillable, casts, relations, accessors, scopes; controllers their routes and Form Requests; Form Requests their rules. Ambiguous short names return candidates."
    )]
    pub async fn get_symbol(
        &self,
        params: Parameters<GetSymbolParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("get_symbol", params.0).await
    }

    #[tool(
        name = "get_component",
        description = "Use before using, changing or creating a Vue component or Nuxt page: props (type, required, default), emits, v-model, slots, which components it renders (resolved to files or libraries), who renders it, stores and composables it uses. Accepts a name (OrderCard), tag (order-card) or file path."
    )]
    pub async fn get_component(
        &self,
        params: Parameters<GetComponentParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("get_component", params.0).await
    }

    #[tool(
        name = "list_pages",
        description = "Use to find a screen and the code behind it: Nuxt pages (file-based, incl. layout and middleware), vue-router routes, and Filament admin pages (admin URL → page class, resource, model, form fields, table columns), each with its URL, route name, the component file it renders, and where it is defined. Filters: path_contains, name_prefix, file. For backend endpoints use list_routes. Pass `permissions: true` for who may open admin pages (policy methods, canAccess)."
    )]
    pub async fn list_pages(
        &self,
        params: Parameters<ListPagesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("list_pages", params.0).await
    }

    #[tool(
        name = "list_routes",
        description = "Use to find Laravel HTTP endpoints or which controller handles a URI. Filters: uri_contains, name_prefix, method, controller, middleware. Each route has methods, uri, name, action, middleware, file:line. Pass `permissions: true` for who may call each route (middleware, checks in the action, Form Request authorize(), policy)."
    )]
    pub async fn list_routes(
        &self,
        params: Parameters<ListRoutesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("list_routes", params.0).await
    }

    #[tool(
        name = "api_usage",
        description = "Use to link the Vue / Nuxt frontend to the Laravel backend: which route each HTTP call hits, calls to endpoints that do not exist (or use the wrong method), and fields the frontend reads that the endpoint does not return. Pass `endpoint` (e.g. `GET api/v1/malls`) for its callers and response fields, `file` for one screen's calls, or `status: missing|fields|unresolved`. Calls whose URL could not be tied to one route are listed with file:line and why. Separate backend repos are linked with --link / .codemap.toml [[links]]."
    )]
    pub async fn api_usage(
        &self,
        params: Parameters<crate::api::ApiUsageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("api_usage", params.0).await
    }

    #[tool(
        name = "find_usages",
        description = "Two uses. (1) Change impact: pass `git_changes: true` (uncommitted changes) or `changed_files` and no target, for one summary of what the change affects (screens, routes, other code) and the exact commands to run only the affected tests. (2) Use before changing, renaming or removing anything: every place it is used, with file:line, and the tests that touch it listed separately (so you know which tests to run). Works for classes, Class::method, functions, route names, Vue components, composables, stores, translation keys, endpoints ('GET api/v1/orders') and files."
    )]
    pub async fn find_usages(
        &self,
        params: Parameters<crate::usages::FindUsagesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("find_usages", params.0).await
    }

    #[tool(
        name = "get_schema",
        description = "Use before writing any query, migration or model change; never assume column names. Without `table`: all tables with column counts and models. With `table`: columns (type, nullable, default, keys, foreign keys), mapped models, and the migration that created/changed them. With `settings: true`: app settings and their defaults (key/value settings read in code with the admin page that edits them, and config keys backed by env()). With `migrations: true`: migrations not yet applied to the configured databases (local, staging), read-only. With `cache: true`: every cache key with where it is written (TTL), read and cleared. With `background: true`: the schedule, queued jobs, events → listeners, and who sends each notification and mail."
    )]
    pub async fn get_schema(
        &self,
        params: Parameters<SchemaParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("get_schema", params.0).await
    }

    #[tool(
        name = "verify",
        description = "Use before finalising code to check that everything it relies on exists. Pass `claims` (classes, methods as Class::method, functions, routes by name or 'METHOD uri', tables, columns as table.column, relations as Model::relation, Vue components by name or tag, endpoints the frontend calls as 'METHOD /path', translation keys as i18n_keys; `i18n_keys: ['*']` lists every problem text) and/or a `php_snippet` to scan. Each item is exists, not_found (with suggestions), external (vendor/PHP built-in) or unresolved. Fix every not_found. Before a release, pass `release_check: true` (no claims) for one list of problems: missing endpoints, fields not returned, translation gaps, unused code added on the branch, migrations not run, config / env keys, settings without defaults, copy rules; `clean: true` when there are none. A problem that is on purpose is silenced with a `codemap-ignore` comment on (or above) its line, or `[release] ignore` in .codemap.toml. The release check also lists `release_steps` (what the server needs: migrate, composer install, caches, queue restart, rebuilds, new env variables) and the project's file `rules` the change touches. After running the project's type check (vue-tsc, nuxi typecheck, tsc, PHPStan --error-format=raw), pass its output as `type_errors` (with `app` in a workspace): only errors that are new since the saved starting point come back."
    )]
    pub async fn verify(
        &self,
        params: Parameters<VerifyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("verify", params.0).await
    }
}

#[tool_router(router = report_router)]
impl CodemapServer {
    #[tool(
        name = "report_wrong_answer",
        description = "Use when a codemap answer turned out wrong after you checked the files or the database: report it once with that answer's `meta.call_id`, then carry on. Give `kind`, the exact `item`, what is `right`, and `evidence` (file:line). Add an `expect` check codemap can re-run, and when you can a made-up `example`: the smallest project giving the same wrong answer, with neutral names and none of the real code (codemap checks it reproduces and contains nothing from this project; fix and resend if it says otherwise). Never ask the person to approve sending; codemap asks them itself. Do not report found: false when you were checking that something does not exist yet, answers marked stale, or your own wrong question."
    )]
    pub async fn report_wrong_answer(
        &self,
        params: Parameters<ReportParams>,
        peer: rmcp::Peer<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        if !crate::reports::KINDS.contains(&p.kind.as_str()) {
            return Err(ErrorData::invalid_params(
                format!("kind must be one of: {}", crate::reports::KINDS.join(", ")),
                None,
            ));
        }
        let Some(answer) = lock(&self.shared.answers)
            .iter()
            .find(|a| a.id == p.call_id)
            .cloned()
        else {
            return Err(ErrorData::invalid_params(
                format!(
                    "unknown or expired call_id `{}` (the last {KEPT_ANSWERS} answers are kept): call the tool again and report that response",
                    p.call_id
                ),
                None,
            ));
        };
        let app = p
            .app
            .clone()
            .or_else(|| answer.args["app"].as_str().map(str::to_owned))
            .or_else(|| answer.data["app"].as_str().map(str::to_owned));
        let shared = self.shared.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let guards: Vec<_> = shared.apps.iter().map(|a| lock(&a.reader)).collect();
            let apps: Vec<(&Store, &Project)> = guards
                .iter()
                .zip(&shared.apps)
                .map(|(g, a)| (&**g, &a.project))
                .collect();
            let project = app
                .as_deref()
                .and_then(|a| {
                    shared
                        .apps
                        .iter()
                        .find(|x| x.project.app.as_deref() == Some(a))
                })
                .unwrap_or(&shared.apps[0])
                .project
                .clone();
            let proof = p
                .example
                .as_ref()
                .map(|ex| crate::reports::prove(&apps, ex));
            let r = crate::reports::Report {
                id: crate::reports::id_for(&answer.tool, &answer.args, &p.kind, &p.item),
                at: jiff::Timestamp::now().to_string(),
                source: "ai".into(),
                app: app.clone(),
                tool: answer.tool.clone(),
                args: answer.args.clone(),
                kind: p.kind.clone(),
                item: p.item.clone(),
                answer: answer.data.clone(),
                right: p.right.clone(),
                evidence: p.evidence.clone(),
                expect: p.expect.clone(),
                example: p.example.clone(),
                example_status: proof.as_ref().map(|x| x.status.clone()),
                example_hits: proof.as_ref().map(|x| x.hits.clone()).unwrap_or_default(),
                example_answer: proof.as_ref().and_then(|x| x.answer.clone()),
                reproduced_on: proof
                    .as_ref()
                    .filter(|x| x.status == "reproduces")
                    .map(|_| codemap_core::VERSION.to_owned()),
                git_commit: answer.git_commit.clone(),
                git_dirty: answer.git_dirty,
                indexed_at: answer.indexed_at.clone(),
                stale: answer.stale,
                last_call: Some(p.call_id.clone()),
                ..Default::default()
            };
            crate::reports::add(&project, r).map(|(r, dup)| (r, dup, project))
        })
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let (report, duplicate, project) = match saved {
            Ok(x) => x,
            Err(e) => {
                // Reporting must never break a session.
                tracing::warn!("cannot save a wrong-answer report: {e}");
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::json!({ "data": { "status": "not saved", "why": e.to_string() } })
                        .to_string(),
                )]));
            }
        };
        let mut data = serde_json::json!({
            "report_id": report.id,
            "status": if duplicate { "updated" } else { "saved" },
            "duplicate_of": if duplicate { serde_json::json!(report.id) } else { serde_json::Value::Null },
            "times": report.times,
        });
        if let Some(st) = &report.example_status {
            data["example"] = serde_json::json!({ "status": st, "problems": report.example_hits });
        }
        data["send"] = serde_json::json!(self.offer_send(&peer, &project, &report).await);
        let text = serde_json::json!({ "data": data }).to_string();
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

impl CodemapServer {
    /// Asks the person (never the AI) whether to send a report with a proved
    /// example to the codemap repository. Returns what happened.
    async fn offer_send(
        &self,
        peer: &rmcp::Peer<rmcp::RoleServer>,
        project: &Project,
        report: &crate::reports::Report,
    ) -> String {
        if report.example_status.as_deref() != Some("reproduces") {
            return "not offered: only a report with a made-up example that reproduces can be sent"
                .into();
        }
        if let Some(why) = crate::reports::sending_off(project) {
            return format!("not offered: {why}");
        }
        let Some((title, body)) = crate::reports::issue(project, report) else {
            return "not offered".into();
        };
        // The app's handshake, as recorded when it connected (the peer
        // handed to a tool may not carry it).
        let hello = lock(&self.shared.client).clone();
        if peer.peer_info().is_none()
            && let Some(h) = &hello
        {
            peer.set_peer_info(h.clone());
        }
        if !peer
            .supported_elicitation_modes()
            .contains(&rmcp::service::ElicitationMode::Form)
        {
            let client = hello
                .as_ref()
                .map(|i| format!("{} {}", i.client_info.name, i.client_info.version))
                .unwrap_or_else(|| "this app".into());
            return format!(
                "not asked: send it with `codemap reports --send {}` in a terminal ({client} did not offer question forms (MCP elicitation) when it connected, so codemap cannot ask the person here)",
                report.id
            );
        }
        let message = format!(
            "codemap found a bug in its own answer and can send it to the codemap repository on GitHub, so it gets fixed. Only this made-up example is sent, never your code.\n\nTitle: {title}\n\n{body}"
        );
        let choice = peer
            .elicit_with_timeout::<SendChoice>(message, Some(Duration::from_secs(600)))
            .await;
        match choice {
            Ok(Some(c)) if c.send => {
                let project = project.clone();
                let report = report.clone();
                let edited = c.edited_text.filter(|t| !t.trim().is_empty());
                let sent = tokio::task::spawn_blocking(move || {
                    let r = crate::reports::send(&project, &report, edited.as_deref());
                    if let Ok(s) = &r {
                        crate::reports::set_issue(&project, &report.id, &s.url);
                    }
                    r
                })
                .await;
                match sent {
                    Ok(Ok(s)) if s.comment => format!(
                        "added to the open issue {} as a comment (no new issue)",
                        s.url
                    ),
                    Ok(Ok(s)) => format!("sent: {}", s.url),
                    Ok(Err(e)) => format!("not sent: {e}"),
                    Err(e) => format!("not sent: {e}"),
                }
            }
            Ok(Some(c)) => {
                if c.dont_ask_again {
                    crate::reports::never_ask_again(project);
                }
                "the person chose not to send it".into()
            }
            Ok(None) | Err(_) => "not sent (no answer from the person)".into(),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for CodemapServer {
    async fn initialize(
        &self,
        request: rmcp::model::InitializeRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::InitializeResult, ErrorData> {
        tracing::info!(
            client = %request.client_info.name,
            version = %request.client_info.version,
            forms = request.capabilities.elicitation.is_some(),
            "AI app connected"
        );
        *lock(&self.shared.client) = Some(request.clone());
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("codemap", codemap_core::VERSION))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Serves MCP on stdin/stdout until the client disconnects.
pub async fn serve(projects: Vec<Project>) -> anyhow::Result<()> {
    let server = CodemapServer::open(projects)?;
    // Warm the index in the background so the first call is quick.
    let warm = server.clone();
    tokio::spawn(async move {
        warm.freshness().await;
    });
    let service = server.serve(rmcp::transport::stdio()).await?;
    tracing::info!("MCP server ready on stdio");
    service.waiting().await?;
    Ok(())
}
