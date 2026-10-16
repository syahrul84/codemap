//! MCP server over stdio (rmcp). Before each tool call the index is brought up
//! to date within a time budget; past the budget the answer comes from the
//! previous index with `stale: true`.

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
in the project.";

/// One indexed project (the root, or one app of a workspace).
struct App {
    project: Project,
    /// Answers queries; a separate connection so reads never wait for re-indexing.
    reader: Mutex<Store>,
    /// Re-indexes; held for the whole refresh.
    writer: Mutex<Store>,
}

struct Shared {
    apps: Vec<App>,
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
                last_check: Mutex::new(None),
                interval,
                gate: tokio::sync::Mutex::new(()),
            }),
            tool_router: Self::tool_router(),
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
        let shared = self.shared.clone();
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
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        match result {
            Ok(envelope) => {
                let text = serde_json::to_string(&envelope)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(ToolError::InvalidParams(m)) => Err(ErrorData::invalid_params(m, None)),
            Err(e) => Err(ErrorData::internal_error(e.to_string(), None)),
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
        description = "Use before finalising code to check that everything it relies on exists. Pass `claims` (classes, methods as Class::method, functions, routes by name or 'METHOD uri', tables, columns as table.column, relations as Model::relation, Vue components by name or tag, endpoints the frontend calls as 'METHOD /path', translation keys as i18n_keys; `i18n_keys: ['*']` lists every problem text) and/or a `php_snippet` to scan. Each item is exists, not_found (with suggestions), external (vendor/PHP built-in) or unresolved. Fix every not_found. Before a release, pass `release_check: true` (no claims) for one list of problems: missing endpoints, fields not returned, translation gaps, unused code added on the branch, migrations not run, config / env keys, settings without defaults, copy rules; `clean: true` when there are none. A problem that is on purpose is silenced with a `codemap-ignore` comment on (or above) its line, or `[release] ignore` in .codemap.toml."
    )]
    pub async fn verify(
        &self,
        params: Parameters<VerifyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("verify", params.0).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for CodemapServer {
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
