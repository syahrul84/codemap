//! The tools, independent of transport: the MCP server and `codemap query` both
//! call [`call`], so they always answer the same way.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use codemap_core::envelope::{Detail, Envelope, Meta};
use codemap_core::store::meta_keys;
use codemap_core::symbols::{self, SearchInput, SymbolKind, SymbolRecord};
use codemap_core::{Config, Store};
use codemap_laravel::detect::Detection;
use codemap_laravel::refresh::{stored_detection, stored_routes_state};
use codemap_laravel::routes::{self, ListInput};
use codemap_laravel::{models, requests, schema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Every tool, in the order they are listed.
pub const TOOL_NAMES: &[&str] = &[
    "project_overview",
    "search_symbol",
    "get_symbol",
    "get_component",
    "list_pages",
    "list_routes",
    "api_usage",
    "find_usages",
    "get_schema",
    "verify",
];

/// One indexed project.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: Config,
    pub id: String,
    pub index_path: PathBuf,
    /// The app's folder name when this project is one app of a workspace.
    pub app: Option<String>,
}

impl Project {
    /// `.codemap.toml` name, else composer.json `name` (unless it is the
    /// untouched `laravel/laravel` skeleton name), else the git repository
    /// name, else the folder name (just `workspace` inside Docker).
    pub fn name(&self, store: &Store) -> String {
        let meta = |key: &str| store.get_meta(key).ok().flatten();
        self.config
            .project
            .name
            .clone()
            .or_else(|| meta(meta_keys::COMPOSER_NAME).filter(|n| n != "laravel/laravel"))
            .or_else(|| meta(meta_keys::GIT_REPO))
            .or_else(|| apple_project_name(&self.root))
            .unwrap_or_else(|| {
                self.root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
    }
}

/// `CleanUp` from `Package.swift` (`name: "CleanUp"`) or `CleanUp.xcodeproj`,
/// or a Gradle build's root project name.
fn apple_project_name(root: &std::path::Path) -> Option<String> {
    // Gradle: `rootProject.name = "mySolat"`.
    for f in ["settings.gradle.kts", "settings.gradle"] {
        if let Ok(src) = std::fs::read_to_string(root.join(f))
            && let Some(rest) = src.split("rootProject.name").nth(1)
        {
            let rest = rest.trim_start().trim_start_matches('=').trim_start();
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            return rest[1..].split(quote).next().map(str::to_owned);
        }
    }
    if let Ok(src) = std::fs::read_to_string(root.join("Package.swift")) {
        let after = src.split("Package(").nth(1)?;
        let name = after
            .split_once("name:")?
            .1
            .trim_start()
            .strip_prefix('"')?;
        return name.split('"').next().map(str::to_owned);
    }
    std::fs::read_dir(root).ok()?.flatten().find_map(|e| {
        let n = e.file_name().to_string_lossy().into_owned();
        n.strip_suffix(".xcworkspace")
            .or_else(|| n.strip_suffix(".xcodeproj"))
            .map(str::to_owned)
    })
}

/// Freshness of the index for this call.
#[derive(Debug, Clone, Default)]
pub struct Freshness {
    pub reindexed_files: usize,
    pub stale: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum ToolError {
    UnknownTool(String),
    InvalidParams(String),
    Internal(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool(t) => write!(
                f,
                "unknown tool `{t}`; available: {}",
                TOOL_NAMES.join(", ")
            ),
            Self::InvalidParams(m) => write!(f, "invalid arguments: {m}"),
            Self::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<rusqlite::Error> for ToolError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<codemap_core::Error> for ToolError {
    fn from(e: codemap_core::Error) -> Self {
        Self::Internal(error_chain(&e))
    }
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut msg = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    msg
}

type ToolResult<T> = Result<T, ToolError>;

/// The `data` of a response plus pagination.
pub struct Output {
    pub data: Value,
    pub next_cursor: Option<String>,
    pub truncated: bool,
}

impl From<Value> for Output {
    fn from(data: Value) -> Self {
        Self {
            data,
            next_cursor: None,
            truncated: false,
        }
    }
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> ToolResult<T> {
    let args = if args.is_null() { json!({}) } else { args };
    serde_json::from_value(args).map_err(|e| ToolError::InvalidParams(e.to_string()))
}

/// Runs `name` with JSON `args` and wraps the result in the response envelope.
pub fn call(
    store: &Store,
    project: &Project,
    name: &str,
    args: Value,
    fresh: Freshness,
) -> ToolResult<Envelope<Value>> {
    let out: Output = match name {
        "project_overview" => project_overview(store, project, parse(args)?)?.into(),
        "search_symbol" => {
            let p: SearchParams = parse(args)?;
            let r = symbols::search(store.conn(), &p.into())?;
            Output {
                data: r.data,
                next_cursor: r.next_cursor,
                truncated: r.truncated,
            }
        }
        "get_symbol" => get_symbol(store, project, parse(args)?)?.into(),
        "get_component" => {
            let p: GetComponentParams = parse(args)?;
            if let Some(v) = codemap_swift::view::get_view(store, &p.name)? {
                v.into()
            } else if let Some(v) = codemap_android::view::get_composable(store, &p.name)? {
                v.into()
            } else {
                let major = codemap_vue::stored_detection(store)?.and_then(|d| d.vue_major);
                let mut v =
                    codemap_vue::component::get_component(store, &p.name, p.detail.into(), major)?;
                if let Some(file) = v["file"].as_str().map(str::to_owned) {
                    let keys =
                        crate::i18n::file_keys(store.conn(), &project.config.i18n.locales, &file)?;
                    if !keys.is_null() {
                        v["i18n"] = keys;
                    }
                }
                v.into()
            }
        }
        "list_pages" => {
            let p: ListPagesParams = parse(args)?;
            let with_permissions = p.permissions;
            let mut r = codemap_vue::pages::list(store, &p.into())?;
            if with_permissions {
                let perms = admin_permissions(store, &project.root, &r.data)?;
                if !perms.is_empty() {
                    r.data["admin_permissions"] = Value::Object(perms);
                }
            }
            Output {
                data: r.data,
                next_cursor: r.next_cursor,
                truncated: r.truncated,
            }
        }
        "list_routes" => {
            let p: ListRoutesParams = parse(args)?;
            let full = matches!(p.detail, DetailParam::Full);
            let with_permissions = p.permissions;
            let conn = store.conn();
            let root = project.root.clone();
            let mut r = routes::list_with(conn, &p.into(), |route, v| {
                if !with_permissions {
                    return;
                }
                let perms = codemap_laravel::permissions::for_route(conn, &root, route);
                if v.is_string() {
                    let line = format!(
                        "{}  · {}",
                        v.as_str().unwrap_or_default(),
                        codemap_laravel::permissions::summary(&perms)
                    );
                    *v = json!(line);
                } else {
                    v["permissions"] = perms;
                }
            })?;
            if full && let Some(list) = r.data["routes"].as_array_mut() {
                for route in list {
                    if let (Some(c), Some(m)) = (
                        route["controller_fqn"].as_str(),
                        route["controller_method"].as_str(),
                    ) {
                        route["returns"] =
                            json!(returns_summary(store.conn(), &project.root, c, m));
                    }
                }
            }
            Output {
                data: r.data,
                next_cursor: r.next_cursor,
                truncated: r.truncated,
            }
        }
        "api_usage" => {
            let r = crate::api::api_usage(store, project, parse(args)?)?;
            Output {
                data: r.data,
                next_cursor: r.next_cursor,
                truncated: r.truncated,
            }
        }
        "find_usages" => {
            let r = crate::usages::find_usages(store, project, parse(args)?)?;
            Output {
                data: r.data,
                next_cursor: r.next_cursor,
                truncated: r.truncated,
            }
        }
        "get_schema" => {
            let p: SchemaParams = parse(args)?;
            if p.background {
                codemap_laravel::background::describe(
                    store,
                    &project.root,
                    p.key_prefix.as_deref(),
                    p.detail.into(),
                )?
                .into()
            } else if p.cache {
                codemap_laravel::cache_map::describe(
                    store,
                    p.key_prefix.as_deref(),
                    p.detail.into(),
                )?
                .into()
            } else if p.migrations {
                let files: Vec<String> = store.list_files()?.into_iter().map(|f| f.path).collect();
                Output {
                    data: codemap_laravel::migration_status::status(
                        &project.root,
                        &files,
                        &project.config.databases,
                        matches!(p.detail, DetailParam::Full),
                    ),
                    next_cursor: None,
                    truncated: false,
                }
            } else if p.settings {
                codemap_laravel::settings::describe(
                    store,
                    &project.root,
                    p.key_prefix.as_deref(),
                    p.detail.into(),
                )?
                .into()
            } else {
                schema::describe(store.conn(), p.table.as_deref(), p.detail.into())?.into()
            }
        }
        "verify" => crate::verify::verify(store, project, parse(args)?)?.into(),
        other => return Err(ToolError::UnknownTool(other.to_owned())),
    };
    let mut out = out;
    not_scanned(project, &mut out.data);
    let mut meta = Meta::from_store(store, &project.name(store), fresh.reindexed_files)?;
    meta.stale = fresh.stale;
    meta.warnings = fresh.warnings;
    meta.next_cursor = out.next_cursor;
    meta.truncated = out.truncated;
    Ok(Envelope::new(out.data, meta))
}

/// "Not found" is only as good as what was read: name the folders with code
/// that the scan skips, so a miss there is not taken as "does not exist".
fn not_scanned(project: &Project, data: &mut Value) {
    let missed = data["found"] == json!(false)
        || data["results"]
            .as_array()
            .is_some_and(|r| r.iter().any(|x| x["status"] == json!("not_found")));
    if !missed {
        return;
    }
    let folders = codemap_core::scan::unscanned_folders(&project.root, &project.config);
    if folders.is_empty() {
        return;
    }
    data["not_scanned"] = json!(folders);
    data["not_scanned_note"] = json!(format!(
        "These folders hold code that is not indexed (outside [scan] include), so a name defined there would show as not found. If they belong to the app, add them to [scan] include in {}.",
        codemap_core::config::CONFIG_FILE
    ));
}

// ---------------------------------------------------------------- params

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DetailParam {
    /// Compact (default).
    #[default]
    Brief,
    /// Everything known.
    Full,
}

impl From<DetailParam> for Detail {
    fn from(d: DetailParam) -> Self {
        match d {
            DetailParam::Brief => Detail::Brief,
            DetailParam::Full => Detail::Full,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverviewParams {
    /// Workspace app to look at (`backend`, `consumer`, …). Omit to search every app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// `brief` (default) or `full`.
    #[serde(default)]
    pub detail: DetailParam,
}

/// A flag given as `true`, `"true"`, `1` or `"yes"`: AI apps sometimes send
/// booleans as strings.
pub fn lenient_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    use serde::de::Error as _;
    let v = serde_json::Value::deserialize(d)?;
    match &v {
        serde_json::Value::Bool(b) => Ok(*b),
        serde_json::Value::Null => Ok(false),
        serde_json::Value::Number(n) => Ok(n.as_f64().is_some_and(|x| x != 0.0)),
        serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" | "on" => Ok(true),
            "false" | "no" | "0" | "off" | "" => Ok(false),
            other => Err(D::Error::custom(format!(
                "expected true or false, got `{other}`"
            ))),
        },
        _ => Err(D::Error::custom("expected true or false")),
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchParams {
    /// Workspace app to look at (`backend`, `consumer`, …). Omit to search every app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Short name, FQN, `Class::method`, or part of a name.
    #[serde(
        alias = "name",
        alias = "q",
        alias = "search",
        alias = "target",
        alias = "symbol"
    )]
    pub query: String,
    /// Only this kind: class, interface, trait, enum, method, function, property, const, enum_case,
    /// component, composable, store.
    #[serde(default)]
    pub kind: Option<String>,
    /// Only symbols under this namespace, e.g. `App\Services`.
    #[serde(default, alias = "namespace")]
    pub namespace_prefix: Option<String>,
    /// Max results (default 20, max 100).
    #[serde(default)]
    pub limit: Option<usize>,
    /// `next_cursor` from a previous response.
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: DetailParam,
}

impl From<SearchParams> for SearchInput {
    fn from(p: SearchParams) -> Self {
        SearchInput {
            query: p.query,
            kind: p.kind.as_deref().and_then(SymbolKind::parse),
            namespace_prefix: p.namespace_prefix,
            limit: p.limit,
            cursor: p.cursor,
            detail: p.detail.into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetSymbolParams {
    /// Workspace app to look at (`backend`, `consumer`, …). Omit to search every app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Short name, FQN, or `Class::method` / `Class::$property` / `Class::CONST`.
    #[serde(
        alias = "symbol",
        alias = "fqn",
        alias = "query",
        alias = "target",
        alias = "class"
    )]
    pub name: String,
    /// Include the source lines (default false).
    #[serde(default, deserialize_with = "lenient_bool")]
    pub include_source: bool,
    /// Max source lines when `include_source` (default 80).
    #[serde(default)]
    pub max_lines: Option<usize>,
    #[serde(default)]
    pub detail: DetailParam,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetComponentParams {
    /// Workspace app to look at (`backend`, `consumer`, …). Omit to search every app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Component name (`OrderCard`), tag (`order-card`), or file path (`src/components/OrderCard.vue`).
    #[serde(
        alias = "component",
        alias = "file",
        alias = "path",
        alias = "query",
        alias = "target"
    )]
    pub name: String,
    #[serde(default)]
    pub detail: DetailParam,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListPagesParams {
    /// Workspace app to look at (`backend`, `consumer`, …). Omit to search every app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Substring of the page URL, e.g. `orders` or `:id`.
    #[serde(
        default,
        alias = "path",
        alias = "uri",
        alias = "uri_contains",
        alias = "url"
    )]
    pub path_contains: Option<String>,
    /// Route name prefix, e.g. `admin.`.
    #[serde(default)]
    pub name_prefix: Option<String>,
    /// Component file (or part of it) rendered by the page.
    #[serde(default, alias = "component")]
    pub file: Option<String>,
    /// Admin (Filament) pages: who may open them (the model's policy
    /// methods, canAccess / canViewAny overrides).
    #[serde(default, deserialize_with = "lenient_bool")]
    pub permissions: bool,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: DetailParam,
}

impl From<ListPagesParams> for codemap_vue::pages::PageFilter {
    fn from(p: ListPagesParams) -> Self {
        Self {
            path_contains: p.path_contains,
            name_prefix: p.name_prefix,
            file: p.file,
            limit: p.limit,
            cursor: p.cursor,
            detail: p.detail.into(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListRoutesParams {
    /// Workspace app to look at (`backend`, `consumer`, …). Omit to search every app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Substring of the URI, e.g. `orders`.
    #[serde(
        default,
        alias = "uri",
        alias = "path",
        alias = "path_contains",
        alias = "url"
    )]
    pub uri_contains: Option<String>,
    /// Route name prefix, e.g. `orders.`.
    #[serde(default)]
    pub name_prefix: Option<String>,
    /// HTTP method, e.g. `POST`.
    #[serde(default, alias = "verb")]
    pub method: Option<String>,
    /// Controller FQN or short name.
    #[serde(default)]
    pub controller: Option<String>,
    /// Middleware name, e.g. `auth` (also matches `auth:sanctum`).
    #[serde(default)]
    pub middleware: Option<String>,
    /// Who may call each route: access middleware, checks in the action
    /// (`authorize`, `Gate`, `can`), the Form Request's `authorize()`, and the
    /// policy method; routes with none are marked public.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub permissions: bool,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: DetailParam,
}

impl From<ListRoutesParams> for ListInput {
    fn from(p: ListRoutesParams) -> Self {
        ListInput {
            uri_contains: p.uri_contains,
            name_prefix: p.name_prefix,
            method: p.method,
            controller: p.controller,
            middleware: p.middleware,
            limit: p.limit,
            cursor: p.cursor,
            detail: p.detail.into(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchemaParams {
    /// Workspace app to look at (`backend`, `consumer`, …). Omit to search every app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Table name. Omit to list all tables.
    #[serde(default, alias = "name", alias = "table_name")]
    pub table: Option<String>,
    /// Settings instead of tables: key/value settings read and written in code
    /// (with defaults and the admin page that edits them) and config keys
    /// backed by env() (with their defaults). `.env` values are never read.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub settings: bool,
    /// With `settings`: only keys starting with this, e.g. `popularity.` or `app.`.
    #[serde(default)]
    pub key_prefix: Option<String>,
    /// Migrations not yet applied to the configured databases (local, staging):
    /// the migration files compared with each database's `migrations` table,
    /// read-only. Databases come from `.codemap.toml` `[[databases]]` / `CODEMAP_DB_*`.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub migrations: bool,
    /// The cache map: every cache key (or pattern) with where it is written
    /// and its TTL, where it is read and where it is cleared. Narrow with `key_prefix`.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub cache: bool,
    /// Background work: the schedule, queued jobs and who dispatches them,
    /// events with their listeners and who fires them, notifications and mails
    /// and who sends them. `key_prefix` filters by name.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub background: bool,
    #[serde(default)]
    pub detail: DetailParam,
}

/// Permissions of the Filament pages in a `list_pages` answer, by URL.
fn admin_permissions(
    store: &Store,
    root: &std::path::Path,
    data: &Value,
) -> ToolResult<serde_json::Map<String, Value>> {
    let conn = store.conn();
    let shown: Vec<String> = data["pages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            p["path"].as_str().map(str::to_owned).or_else(|| {
                p.as_str()
                    .and_then(|s| s.split_whitespace().next())
                    .map(str::to_owned)
            })
        })
        .collect();
    let all = symbols::all(conn)?;
    let mut out = serde_json::Map::new();
    let mut stmt = conn.prepare(
        "SELECT route_path, file, meta FROM pages WHERE source = 'filament' AND file IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let rows: Vec<(String, String, Option<String>)> = rows.collect::<rusqlite::Result<_>>()?;
    // A resource's sub-pages (`/create`, `/{record}/edit`) share its index page's model.
    let model_meta = |path: &str| -> Option<String> {
        rows.iter()
            .filter(|(p, _, m)| {
                path.starts_with(p.as_str())
                    && m.as_deref().is_some_and(|m| m.contains(" · model "))
            })
            .max_by_key(|(p, _, _)| p.len())
            .and_then(|(_, _, m)| m.clone())
    };
    for (path, file, meta) in rows.iter().cloned() {
        if !shown.contains(&path) || out.len() >= 60 {
            continue;
        }
        let meta = meta
            .filter(|m| m.contains(" · model "))
            .or_else(|| model_meta(&path));
        let Some(class) = all
            .iter()
            .find(|s| s.file == file && s.kind.is_class_like())
            .map(|s| s.fqn.clone())
        else {
            continue;
        };
        // `… · model Merchant · …` → App\Models\Merchant.
        let model = meta.as_deref().and_then(|m| {
            let short = m
                .split(" · ")
                .find_map(|p| p.strip_prefix("model "))?
                .trim();
            all.iter()
                .find(|s| {
                    s.kind.is_class_like()
                        && s.fqn.rsplit('\\').next() == Some(short)
                        && s.fqn.contains("\\Models\\")
                })
                .map(|s| s.fqn.clone())
        });
        out.insert(
            path,
            codemap_laravel::permissions::for_admin_page(conn, root, &class, model.as_deref()),
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------- project_overview

fn count(store: &Store, sql: &str) -> ToolResult<u64> {
    store
        .conn()
        .query_row(sql, [], |r| r.get(0))
        .map_err(|e| ToolError::Internal(e.to_string()))
}

fn project_overview(store: &Store, project: &Project, p: OverviewParams) -> ToolResult<Value> {
    let detail: Detail = p.detail.into();
    let stats = store.stats()?;
    let detection: Option<Detection> = stored_detection(store)?;
    let routes_state = stored_routes_state(store)?;
    let kinds = &stats.symbols_by_kind;
    let k = |name: &str| kinds.get(name).copied().unwrap_or(0);

    let counts = json!({
        "php_files": stats.files_by_lang.get("php").copied().unwrap_or(0),
        "classes": k("class"),
        "interfaces": k("interface"),
        "traits": k("trait"),
        "enums": k("enum"),
        "functions": k("function"),
        "routes": count(store, "SELECT COUNT(*) FROM routes")?,
        "models": count(store, "SELECT COUNT(*) FROM models")?,
        "tables": count(store, "SELECT COUNT(*) FROM db_tables WHERE dropped = 0")?,
        "form_requests": count(store, "SELECT COUNT(*) FROM form_requests")?,
    });

    // Top namespaces by number of class-likes: the project's "modules".
    let mut by_ns: BTreeMap<String, u64> = BTreeMap::new();
    {
        let mut stmt = store
            .conn()
            .prepare("SELECT fqn FROM symbols WHERE kind IN ('class','interface','trait','enum')")
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        for fqn in rows.flatten() {
            let ns = fqn.rsplit_once('\\').map_or("(global)", |(ns, _)| ns);
            *by_ns.entry(ns.to_owned()).or_insert(0) += 1;
        }
    }
    let mut modules: Vec<(String, u64)> = by_ns.into_iter().collect();
    modules.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let module_limit = if detail == Detail::Full { 50 } else { 15 };
    let modules: Vec<Value> = modules
        .iter()
        .take(module_limit)
        .map(|(ns, n)| json!({ "namespace": ns, "classes": n }))
        .collect();

    // Key directories: code files per first two path segments.
    let mut dirs: BTreeMap<String, u64> = BTreeMap::new();
    for f in store.list_files()? {
        if !matches!(f.lang.as_str(), "php" | "vue" | "js" | "ts") {
            continue;
        }
        let parts: Vec<&str> = f.path.split('/').collect();
        if parts.len() > 1 {
            let dir = parts[..parts.len().min(3) - 1].join("/");
            *dirs.entry(dir).or_insert(0) += 1;
        }
    }
    let mut dirs: Vec<(String, u64)> = dirs.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let dirs: Vec<Value> = dirs
        .iter()
        .take(if detail == Detail::Full { 40 } else { 12 })
        .map(|(d, n)| json!({ "dir": d, "files": n }))
        .collect();

    let models: Vec<Value> = models::load(store.conn())?
        .iter()
        .map(|m| {
            if detail == Detail::Full {
                json!({ "model": m.fqn, "table": m.table, "table_inferred": m.table_inferred })
            } else {
                json!(format!(
                    "{} → {}",
                    m.fqn.rsplit('\\').next().unwrap_or(&m.fqn),
                    m.table
                ))
            }
        })
        .collect();

    let unresolved_routes = count(
        store,
        "SELECT COUNT(*) FROM routes WHERE unresolved_reason IS NOT NULL",
    )?;
    let mut v = json!({
        "name": project.name(store),
        "framework": detection.as_ref().filter(|d| d.laravel).map(|d| json!({
            "laravel": d.version,
            "version_inferred": d.version_inferred,
            "layout": d.layout,
        })),
        "php": detection.as_ref().and_then(|d| d.php_constraint.clone()),
        "psr4": detection.as_ref().map(|d| d.psr4.iter().map(|r| json!(format!("{}\\ → {}", r.namespace, r.path))).collect::<Vec<_>>()),
        "counts": counts,
        "route_files": routes_state.as_ref().map(|s| s.files.iter().filter(|f| f.starts_with("routes/")).cloned().collect::<Vec<_>>()),
        "key_dirs": dirs,
        "modules": modules,
        "models": if detail == Detail::Full { json!(models) } else { Value::Null },
        "frontend": frontend_overview(store, detail)?,
        "swift": codemap_swift::view::overview(store, &project.root)?,
        "android": codemap_android::view::overview(store)?,
        "rust": codemap_rust::overview(store, &project.root)?,
        "i18n": crate::i18n::overview(store.conn(), &project.config.i18n.locales)?,
        "health": {
            "warnings": stats.warnings,
            "unresolved_routes": unresolved_routes,
        },
        "next": "search_symbol before writing new code; get_schema before queries or migrations (settings: true for app settings, migrations: true for unapplied migrations); get_component before using or changing a component; after editing, find_usages git_changes: true for the tests to run; verify before finishing",
    });
    // A newer codemap release (from the last daily check; no network here).
    if let Some(data_dir) = project
        .index_path
        .parent()
        .and_then(std::path::Path::parent)
        && let Some(n) = codemap_core::updates::notice(data_dir)
    {
        v["update"] = json!(n);
    }
    // Answers codemap got wrong, waiting for a fix.
    if let Some(line) = crate::reports::summary_line(&[project]) {
        v["wrong_answer_reports"] = json!(line);
    }
    // Names the AI got wrong before in this project, so it avoids them now.
    let mistaken = crate::misses::often_mistaken(store, project);
    if !mistaken.is_empty() {
        v["often_mistaken"] = json!(mistaken);
    }
    for key in [
        "frontend", "swift", "android", "rust", "i18n", "models", "key_dirs",
    ] {
        if v[key].is_null() || v[key].as_array().is_some_and(|a| a.is_empty()) {
            v.as_object_mut().map(|o| o.remove(key));
        }
    }
    // A frontend-only project: leave out the empty PHP / Laravel sections.
    let laravel = detection.as_ref().is_some_and(|d| d.laravel);
    // PHP files without a `composer.json` (test fixtures in a Rust or Swift
    // repository) are not a PHP app either.
    let php_app = v["counts"]["php_files"] != 0 && project.root.join("composer.json").is_file();
    if !laravel && !php_app {
        if let Some(list) = v["i18n"].as_array_mut() {
            list.retain(|c| c["source"] != "laravel");
            if list.is_empty() {
                v["i18n"] = Value::Null;
            }
        }
        if v["i18n"].is_null() {
            v.as_object_mut().map(|o| o.remove("i18n"));
        }
        if let Some(o) = v.as_object_mut() {
            for key in [
                "framework",
                "php",
                "psr4",
                "counts",
                "route_files",
                "modules",
                "models",
            ] {
                o.remove(key);
            }
        }
        if unresolved_routes == 0 {
            v["health"]
                .as_object_mut()
                .map(|h| h.remove("unresolved_routes"));
        }
        v["next"] = json!(
            if !v["rust"].is_null() && v["swift"].is_null() && v["android"].is_null() {
                "search_symbol before writing new code; get_symbol for a type's fields and methods (Type::method); list_pages for binaries and their subcommands; find_usages before renaming or removing (tests listed apart); verify before finishing"
            } else if v["swift"].is_null() && v["android"].is_null() {
                "search_symbol before writing new code; get_component before using or changing a component; verify before finishing"
            } else {
                "search_symbol before writing new code; get_component before using or changing a SwiftUI view or composable; list_pages for the app's screens (windows, activities, tabs, navigation, sheets, dialogs); find_usages before renaming or removing; verify before finishing"
            }
        );
    }
    if detail == Detail::Full
        && let Some(d) = &detection
    {
        v["detection_reasons"] = json!(d.reasons);
        v["route_sources"] = json!(routes_state.map(|s| s.sources));
    }
    Ok(v)
}

/// Vue / Nuxt summary, or null when the project has no frontend.
fn frontend_overview(store: &Store, detail: Detail) -> ToolResult<Value> {
    let Some(d) = codemap_vue::stored_detection(store)? else {
        return Ok(Value::Null);
    };
    if !d.vue {
        return Ok(Value::Null);
    }
    let framework = match (&d.nuxt, &d.vue_package) {
        _ if d.next.is_some() => format!(
            "Next.js {}",
            d.next
                .as_ref()
                .map(|n| n.package.display())
                .unwrap_or_default()
        ),
        _ if d.react.is_some() => format!(
            "React {}",
            d.react.as_ref().map(|r| r.display()).unwrap_or_default()
        ),
        (Some(n), _) => format!("Nuxt {}", n.package.display()),
        (None, Some(v)) => format!("Vue {}", v.display()),
        (None, None) => format!(
            "Vue {}",
            d.vue_major.map(|m| m.to_string()).unwrap_or_default()
        ),
    };
    let kinds = |kind: &str| -> ToolResult<u64> {
        store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM js_modules WHERE kind = ?1",
                [kind],
                |r| r.get(0),
            )
            .map_err(|e| ToolError::Internal(e.to_string()))
    };
    let stats = store.stats()?;
    let lang = |l: &str| stats.files_by_lang.get(l).copied().unwrap_or(0);
    let sym = |k: &str| stats.symbols_by_kind.get(k).copied().unwrap_or(0);
    let mut v = json!({
        "framework": framework,
        "vue": d.vue_major,
        "build": d.build,
        "src_dir": if d.src_dir.is_empty() { "." } else { d.src_dir.as_str() },
        "typescript": d.typescript,
        "libraries": d.libraries.iter().map(|p| format!("{} {}", p.name, p.display())).collect::<Vec<_>>(),
        "ui": d.ui.iter().map(|p| format!("{} {}", p.name, p.display())).collect::<Vec<_>>(),
        "counts": {
            "vue_files": lang("vue"),
            "js_ts_files": lang("js") + lang("ts"),
            "components": kinds("component")?,
            "pages": kinds("page")?,
            "layouts": kinds("layout")?,
            "composables": sym("composable"),
            "stores": sym("store"),
        },
        "unresolved_component_tags": count(store, "SELECT COUNT(DISTINCT tag) FROM component_uses WHERE via = 'unresolved'")?,
    });
    if detail == Detail::Full {
        v["aliases"] = json!(
            d.aliases
                .iter()
                .map(|(a, p)| format!("{a} → {}", if p.is_empty() { "." } else { p }))
                .collect::<Vec<_>>()
        );
        v["component_dirs"] = json!(d.component_dirs);
        v["detection_reasons"] = json!(d.reasons);
    }
    Ok(v)
}

// ---------------------------------------------------------------- get_symbol

/// Several matches: keep those spelled exactly as asked (`Store`, not the
/// `store` functions), and when a field and a method share one name (Rust
/// allows it), the method.
fn narrow(found: &mut Vec<SymbolRecord>, query: &str) {
    if found.len() < 2 {
        return;
    }
    let q = query.trim().trim_start_matches('\\');
    let short = q.rsplit([':', '\\', '.']).next().unwrap_or(q);
    if found.iter().any(|s| s.name == short) {
        found.retain(|s| s.name == short);
    }
    let same_fqn = found.iter().all(|s| s.fqn == found[0].fqn);
    if same_fqn && found.iter().any(|s| s.kind != SymbolKind::Property) {
        found.retain(|s| s.kind != SymbolKind::Property);
    }
}

fn candidates(store: &Store, name: &str) -> ToolResult<Vec<SymbolRecord>> {
    let name = name.trim().trim_start_matches('\\');
    let exact = symbols::by_fqn(store.conn(), name)?;
    if !exact.is_empty() || name.contains('\\') {
        return Ok(exact);
    }
    // `ShortClass::member`: resolve the class by short name, then the member.
    if let Some((class, member)) = name.split_once("::") {
        let mut out = Vec::new();
        for c in symbols::all_named(store.conn(), class)? {
            if c.kind.is_class_like() {
                out.extend(symbols::by_fqn(
                    store.conn(),
                    &format!("{}::{member}", c.fqn),
                )?);
            }
        }
        return Ok(out);
    }
    // Short name: class-likes, functions, components, composables and stores
    // (members need `Class::member`).
    let all = symbols::all_named(store.conn(), name)?;
    Ok(all
        .into_iter()
        .filter(|s| {
            s.kind.is_class_like()
                || matches!(
                    s.kind,
                    SymbolKind::Function
                        | SymbolKind::Component
                        | SymbolKind::Composable
                        | SymbolKind::Store
                )
        })
        .collect())
}

fn get_symbol(store: &Store, project: &Project, p: GetSymbolParams) -> ToolResult<Value> {
    let detail: Detail = p.detail.into();
    let mut found = candidates(store, &p.name)?;
    if found.is_empty() {
        let s = symbols::search(
            store.conn(),
            &SearchInput {
                query: p.name.clone(),
                limit: Some(5),
                ..SearchInput::default()
            },
        )?;
        let suggestions = if s.data["found"] == true {
            s.data["matches"].clone()
        } else {
            s.data["suggestions"].clone()
        };
        let hint = suggestions
            .get(0)
            .and_then(|m| m["fqn"].as_str())
            .map_or_else(
                || format!("No symbol named {}.", p.name),
                |f| format!("Did you mean {f}?"),
            );
        return Ok(
            json!({ "found": false, "query": p.name, "suggestions": suggestions, "hint": hint }),
        );
    }
    narrow(&mut found, &p.name);
    if found.len() > 1 {
        let list: Vec<Value> = found
            .iter()
            .map(|s| s.to_json(Detail::Brief, None))
            .collect();
        return Ok(json!({
            "found": true,
            "ambiguous": true,
            "candidates": list,
            "hint": "Several symbols share this name; call get_symbol with one of these FQNs.",
        }));
    }
    let sym = found.remove(0);
    let mut v = sym.to_json(Detail::Full, None);
    v["found"] = json!(true);
    let conn = store.conn();

    if sym.kind.is_class_like() {
        let members = symbols::members(conn, &sym.fqn)?;
        // Brief: methods the model block already describes (relations,
        // accessors, scopes, casts()) are not repeated.
        let covered: std::collections::HashSet<String> = if detail == Detail::Brief {
            models::load(conn)?
                .into_iter()
                .find(|m| m.fqn.eq_ignore_ascii_case(&sym.fqn))
                .map(|m| {
                    m.relations
                        .iter()
                        .map(|r| r.method.clone())
                        .chain(m.accessors.iter().map(|a| a.method.clone()))
                        .chain(m.scopes.iter().map(|s| s.method.clone()))
                        .chain(["casts".to_owned()])
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Default::default()
        };
        let list: Vec<Value> = members
            .iter()
            .filter(|m| detail == Detail::Full || m.visibility.as_deref() != Some("private"))
            .filter(|m| !covered.contains(&m.name))
            .map(|m| {
                if detail == Detail::Full {
                    m.to_json(Detail::Full, None)
                } else {
                    json!(format!("{}  :{}", m.signature, m.line))
                }
            })
            .collect();
        if detail == Detail::Brief {
            // Names only (one line); signatures and lines with detail: full.
            let names: Vec<String> = members
                .iter()
                .filter(|m| m.visibility.as_deref() != Some("private"))
                .filter(|m| !covered.contains(&m.name))
                .map(|m| {
                    if m.kind == SymbolKind::Method {
                        format!("{}()", m.name)
                    } else {
                        m.name.clone()
                    }
                })
                .collect();
            if !names.is_empty() {
                v["members_total"] = json!(names.len());
                let shown: Vec<&String> = names.iter().take(60).collect();
                v["members"] = json!(
                    shown
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                v["members_hint"] =
                    json!("Names only; pass detail: full for signatures and lines.");
            }
            drop(list);
        } else {
            v["members"] = json!(list);
        }
        let subclasses: Vec<String> = symbols::descendants(conn, &[&sym.fqn])?
            .into_iter()
            .map(|c| c.fqn)
            .collect();
        if !subclasses.is_empty() {
            v["subclasses"] = json!(subclasses);
        }
        enrich_class(store, &project.root, &sym, &mut v, detail)?;
    } else if sym.kind == SymbolKind::Component {
        let major = codemap_vue::stored_detection(store)?.and_then(|d| d.vue_major);
        let mut c = codemap_vue::component::get_component(store, &sym.file, Detail::Brief, major)?;
        if let Some(o) = c.as_object_mut() {
            for k in ["found", "name", "file"] {
                o.remove(k);
            }
        }
        v["component"] = c;
    } else if let Some(parent) = &sym.parent_fqn {
        enrich_member(store, &project.root, parent, &sym, &mut v)?;
    }

    if p.include_source {
        let max = p.max_lines.unwrap_or(80).clamp(1, 500);
        v["source"] = json!(source_lines(
            &project.root,
            &sym.file,
            sym.line,
            sym.end_line,
            max
        ));
    }
    Ok(v)
}

fn enrich_class(
    store: &Store,
    root: &Path,
    sym: &SymbolRecord,
    v: &mut Value,
    detail: Detail,
) -> ToolResult<()> {
    let conn = store.conn();
    if let Some(m) = models::load(conn)?
        .into_iter()
        .find(|m| m.fqn.eq_ignore_ascii_case(&sym.fqn))
    {
        let columns = schema::load_columns(conn, &m.table)?;
        if detail == Detail::Brief {
            let short = |f: &str| f.rsplit('\\').next().unwrap_or(f).to_owned();
            let mut model = json!({
                "table": m.table,
                "columns": columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                "fillable": m.fillable,
                "guarded": m.guarded,
                "hidden": m.hidden,
                "casts": m.casts.map(|c| c.into_iter().collect::<BTreeMap<_, _>>()),
                "appends": m.appends,
                "traits": m.traits.iter().map(|t| short(t)).collect::<Vec<_>>(),
                "relations": m.relations.iter().take(15).map(|r| format!(
                    "{}: {} {}{} :{}",
                    r.method,
                    r.r#type,
                    r.related.as_deref().map(short).unwrap_or_else(|| "?".into()),
                    r.foreign_key.as_ref().map(|k| format!(" ({k})")).unwrap_or_default(),
                    r.line
                )).collect::<Vec<_>>(),
                "accessors": m.accessors.iter().map(|a| a.attribute.clone()).collect::<Vec<_>>(),
                "scopes": m.scopes.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                "notes": m.notes,
            });
            if columns.is_empty() {
                model["table_exists"] = json!(false);
            }
            if m.relations.len() > 15 {
                model["relations_total"] = json!(m.relations.len());
            }
            if m.table_inferred {
                model["table_inferred"] = json!(true);
            }
            if let Some(o) = model.as_object_mut() {
                o.retain(|_, v| {
                    !(v.is_null()
                        || v.as_array().is_some_and(|a| a.is_empty())
                        || v.as_object().is_some_and(|x| x.is_empty()))
                });
            }
            v["model"] = model;
        } else {
            v["model"] = json!({
                "table": m.table,
                "table_inferred": m.table_inferred,
                "table_exists": !columns.is_empty(),
                "columns": columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                "connection": m.connection,
                "primary_key": m.primary_key,
                "fillable": m.fillable,
                "guarded": m.guarded,
                "hidden": m.hidden,
                "casts": m.casts.map(|c| c.into_iter().collect::<BTreeMap<_, _>>()),
                "appends": m.appends,
                "traits": m.traits,
                "relations": m.relations,
                "accessors": m.accessors,
                "scopes": m.scopes,
                "notes": m.notes,
            });
        }
    }
    let class_routes: Vec<Value> = routes::all(conn)?
        .into_iter()
        .filter(|r| {
            r.controller_fqn
                .as_deref()
                .is_some_and(|c| c.eq_ignore_ascii_case(&sym.fqn))
        })
        .map(|r| {
            json!({
                "methods": r.methods_str(),
                "uri": r.uri,
                "name": r.name,
                "action": r.controller_method,
                "file": r.location(),
                "returns": r.controller_method.as_deref().map(|m| returns_summary(conn, root, &sym.fqn, m)),
            })
        })
        .collect();
    if !class_routes.is_empty() {
        v["routes"] = json!(class_routes);
        let uses = requests::used_by_controller(conn, &sym.fqn)?;
        if !uses.is_empty() {
            v["form_requests"] = json!(uses);
        }
    }
    if let Some(r) = requests::load(conn, Some(&sym.fqn))?.into_iter().next() {
        let rules: BTreeMap<String, Vec<String>> =
            r.rules.into_iter().map(|f| (f.field, f.rules)).collect();
        v["form_request"] = json!({ "rules": rules, "authorize": r.authorize, "unresolved_reason": r.unresolved_reason });
    }
    if detail == Detail::Brief {
        // Keep brief responses small: members are already listed as signatures.
        v.as_object_mut().map(|o| o.remove("meta"));
    }
    Ok(())
}

/// `resource MallResource → id, slug, title?` — what an action returns, in one line.
pub fn returns_summary(
    conn: &rusqlite::Connection,
    root: &Path,
    class: &str,
    method: &str,
) -> String {
    let s = codemap_laravel::responses::for_action(conn, root, class, method);
    if s.kind == "unknown" {
        return format!("unknown ({})", s.notes.join("; "));
    }
    let mut out = s.kind.clone();
    if let Some(r) = &s.resource {
        out.push(' ');
        out.push_str(r.rsplit('\\').next().unwrap_or(r));
    }
    if !s.fields.is_empty() {
        let fields: Vec<String> = s
            .fields
            .iter()
            .map(|f| match &f.children {
                Some(c) => format!(
                    "{}{}{{{}}}",
                    f.name,
                    if f.conditional { "?" } else { "" },
                    c.join(", ")
                ),
                None => format!("{}{}", f.name, if f.conditional { "?" } else { "" }),
            })
            .collect();
        out.push_str(if s.wrapped_in_data {
            " → data: "
        } else {
            " → "
        });
        out.push_str(&fields.join(", "));
    }
    if !s.complete {
        out.push_str(" (partial)");
    }
    out
}

fn enrich_member(
    store: &Store,
    root: &Path,
    class: &str,
    sym: &SymbolRecord,
    v: &mut Value,
) -> ToolResult<()> {
    let conn = store.conn();
    if sym.kind == SymbolKind::Method {
        let method_routes: Vec<Value> = routes::all(conn)?
            .into_iter()
            .filter(|r| {
                r.controller_fqn.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(class))
                    && r.controller_method.as_deref() == Some(sym.name.as_str())
            })
            .map(|r| json!({ "methods": r.methods_str(), "uri": r.uri, "name": r.name, "file": r.location() }))
            .collect();
        if !method_routes.is_empty() {
            v["routes"] = json!(method_routes);
            v["returns"] = json!(returns_summary(conn, root, class, &sym.name));
        }
        let uses: Vec<_> = requests::used_by_controller(conn, class)?
            .into_iter()
            .filter(|u| u.method == sym.name)
            .collect();
        if let Some(u) = uses.first()
            && let Some(r) = requests::load(conn, Some(&u.request))?.into_iter().next()
        {
            let rules: BTreeMap<String, Vec<String>> =
                r.rules.into_iter().map(|f| (f.field, f.rules)).collect();
            v["form_request"] = json!({ "class": u.request, "param": u.param, "rules": rules });
        }
        if let Some(m) = models::load(conn)?
            .into_iter()
            .find(|m| m.fqn.eq_ignore_ascii_case(class))
            && let Some(rel) = m.relations.iter().find(|r| r.method == sym.name)
        {
            v["relation"] = json!(rel);
        }
    }
    Ok(())
}

fn source_lines(root: &Path, file: &str, start: u32, end: u32, max: usize) -> Value {
    let Ok(text) = std::fs::read_to_string(root.join(file)) else {
        return json!({ "error": format!("cannot read {file}") });
    };
    let lines: Vec<&str> = text.lines().collect();
    let from = start.saturating_sub(1) as usize;
    let to = (end as usize).min(lines.len());
    let take = (to.saturating_sub(from)).min(max);
    json!({
        "from_line": start,
        "text": lines[from..from + take].join("\n"),
        "truncated": to - from > take,
    })
}
