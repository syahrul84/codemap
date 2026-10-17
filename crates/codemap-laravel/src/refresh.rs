//! The one refresh path shared by the CLI and the MCP server:
//! scan → PHP symbols → Laravel detection → routes.

use std::path::Path;
use std::time::Instant;

use codemap_core::adapter::{ExtractContext, ExtractReport, FrameworkAdapter};
use codemap_core::store::{get_meta, meta_keys, set_meta, set_meta_opt};
use codemap_core::{Config, Result, ScanOptions, ScanReport, Store};
use codemap_php::PhpReport;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::detect::{Detection, Layout, detect};
use crate::routes::{self, artisan, sources};

/// Bump when route extraction output changes; routes are then rebuilt.
pub const ROUTES_EXTRACTOR_VERSION: i64 = 4;

/// Bump when model / schema / Form Request extraction output changes.
pub const DATA_EXTRACTOR_VERSION: i64 = 9;

/// Bump when detection output changes; detection then re-runs once.
pub const DETECTION_VERSION: i64 = 2;

/// `warnings.adapter` for models, schema and Form Request warnings.
pub const DATA_ADAPTER: &str = "laravel-data";

/// `warnings.adapter` for Laravel warnings.
pub const ADAPTER: &str = "laravel";

/// When the live schema was last read (unix seconds).
const LIVE_SCHEMA_AT: &str = "live_schema_at";
/// A live database is re-read at most this often (it changes without any
/// file changing).
const LIVE_SCHEMA_TTL_SECS: u64 = 600;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaravelReport {
    pub detected: bool,
    pub version: Option<String>,
    pub layout: Option<Layout>,
    pub routes: ExtractReport,
    /// Models, schema (tables) and Form Requests.
    pub data: DataReport,
    /// `static`, `artisan`, or `static (artisan failed)`.
    pub route_source: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshReport {
    pub scan: ScanReport,
    pub php: PhpReport,
    pub laravel: LaravelReport,
}

impl RefreshReport {
    pub fn reindexed_files(&self) -> usize {
        self.scan.reparsed + self.scan.removed
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataReport {
    pub ran: bool,
    pub models: usize,
    pub tables: usize,
    pub form_requests: usize,
    pub migrations_replayed: usize,
    pub warnings: usize,
    pub duration_ms: u64,
}

/// What the stored routes were built from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutesState {
    pub version: i64,
    pub config_hash: String,
    pub fingerprint: String,
    pub files: Vec<String>,
    pub sources: Vec<String>,
    pub route_source: String,
}

pub struct LaravelAdapter;

impl FrameworkAdapter for LaravelAdapter {
    type Detection = Detection;

    fn id(&self) -> &'static str {
        ADAPTER
    }

    fn detect(&self, root: &Path, store: &Store) -> Result<Option<Detection>> {
        let d = detect(root, store)?;
        Ok(d.laravel.then_some(d))
    }

    fn extract(
        &self,
        ctx: &ExtractContext<'_>,
        detection: &Detection,
        store: &mut Store,
    ) -> Result<ExtractReport> {
        let started = Instant::now();
        let state: Option<RoutesState> = get_meta(store.conn(), meta_keys::ROUTES_STATE)?
            .and_then(|j| serde_json::from_str(&j).ok());
        let config_hash = blake3::hash(serde_json::to_string(&ctx.config.laravel)?.as_bytes())
            .to_hex()[..16]
            .to_owned();
        let fingerprint = detection.routing_fingerprint();
        let relevant = |p: &String| {
            p.starts_with("routes/")
                || p == "bootstrap/app.php"
                || p == "composer.json"
                || p == "composer.lock"
                || p.starts_with("app/Providers/")
                || p.starts_with("config/")
                || state.as_ref().is_some_and(|s| s.files.contains(p))
        };
        let needed = ctx.full
            || match &state {
                None => true,
                Some(s) => {
                    s.version != ROUTES_EXTRACTOR_VERSION
                        || s.config_hash != config_hash
                        || s.fingerprint != fingerprint
                        || ctx.scan.changed_paths.iter().any(relevant)
                        || ctx.scan.removed_paths.iter().any(relevant)
                }
            };
        if !needed {
            let (items, unresolved) = route_counts(store)?;
            return Ok(ExtractReport {
                ran: false,
                items,
                unresolved,
                ..ExtractReport::default()
            });
        }

        let extracted = sources::extract_static(ctx.root, detection);
        let mut warnings: Vec<(Option<String>, Option<u32>, String)> = extracted
            .warnings
            .iter()
            .map(|w| (w.file.clone(), w.line, w.message.clone()))
            .collect();
        warnings.extend(detection.warnings.iter().map(|w| (None, None, w.clone())));

        let mut notes = Vec::new();
        let (route_list, route_source) = match artisan::decide(&ctx.config.laravel, ctx.root) {
            artisan::Decision::Skip(why) => {
                notes.push(why.to_owned());
                (extracted.routes.clone(), "static".to_owned())
            }
            artisan::Decision::Run => match artisan::run(ctx.root, &ctx.config.laravel) {
                Ok(list) => {
                    let (merged, unmatched) = artisan::merge(&extracted.routes, list);
                    if unmatched > 0 {
                        warnings.push((
                            None,
                            None,
                            format!(
                                "{unmatched} statically found route(s) were not reported by artisan \
                                 route:list (conditional, overridden or misread)"
                            ),
                        ));
                    }
                    (merged, "artisan".to_owned())
                }
                Err(e) => {
                    warnings.push((
                        None,
                        None,
                        format!("artisan route:list failed ({e}); using static routes"),
                    ));
                    (
                        extracted.routes.clone(),
                        "static (artisan failed)".to_owned(),
                    )
                }
            },
        };

        // Routes packages register themselves (artisan already lists them).
        let mut route_list = route_list;
        if route_source.starts_with("static") {
            for r in routes::packages::routes(ctx.root) {
                if !route_list
                    .iter()
                    .any(|x| x.uri == r.uri && x.methods_str() == r.methods_str())
                {
                    route_list.push(r);
                }
            }
        }
        let unresolved = route_list
            .iter()
            .filter(|r| r.unresolved_reason.is_some())
            .count();
        let new_state = RoutesState {
            version: ROUTES_EXTRACTOR_VERSION,
            config_hash,
            fingerprint,
            files: extracted.files.iter().cloned().collect(),
            sources: extracted.sources.clone(),
            route_source: route_source.clone(),
        };

        let tx = store.conn_mut().transaction()?;
        routes::replace_all(&tx, &route_list)?;
        tx.execute("DELETE FROM warnings WHERE adapter = ?1", [ADAPTER])?;
        for (file, line, message) in &warnings {
            tx.execute(
                "INSERT INTO warnings (file_id, path, line, adapter, message)
                 VALUES ((SELECT id FROM files WHERE path = ?1), ?1, ?2, ?3, ?4)",
                params![file, line, ADAPTER, message],
            )?;
        }
        set_meta(
            &tx,
            meta_keys::ROUTES_STATE,
            &serde_json::to_string(&new_state)?,
        )?;
        tx.commit()?;

        notes.insert(0, format!("routes from {route_source}"));
        Ok(ExtractReport {
            ran: true,
            items: route_list.len(),
            unresolved,
            warnings: warnings.len(),
            duration_ms: started.elapsed().as_millis() as u64,
            notes,
        })
    }
}

fn route_counts(store: &Store) -> Result<(usize, usize)> {
    Ok(store.conn().query_row(
        "SELECT COUNT(*), COUNT(unresolved_reason) FROM routes",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Stored detection, if any.
pub fn stored_detection(store: &Store) -> Result<Option<Detection>> {
    Ok(get_meta(store.conn(), meta_keys::DETECTION)?.and_then(|j| serde_json::from_str(&j).ok()))
}

pub fn stored_routes_state(store: &Store) -> Result<Option<RoutesState>> {
    Ok(
        get_meta(store.conn(), meta_keys::ROUTES_STATE)?
            .and_then(|j| serde_json::from_str(&j).ok()),
    )
}

fn save_detection(store: &Store, root: &Path, d: &Detection) -> Result<()> {
    let conn = store.conn();
    // `git@github.com:acme/shop-api.git` → `shop-api`: a readable project name in Docker.
    let repo = codemap_core::git::remote_url(root).and_then(|url| {
        url.trim_end_matches('/')
            .trim_end_matches(".git")
            .rsplit(['/', ':'])
            .next()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
    });
    set_meta_opt(conn, meta_keys::GIT_REPO, repo.as_deref())?;
    set_meta(
        conn,
        meta_keys::DETECTION_VERSION,
        &DETECTION_VERSION.to_string(),
    )?;
    set_meta(conn, meta_keys::DETECTION, &serde_json::to_string(d)?)?;
    set_meta_opt(conn, meta_keys::COMPOSER_NAME, d.composer_name.as_deref())?;
    set_meta_opt(conn, meta_keys::LARAVEL_VERSION, d.version.as_deref())?;
    set_meta_opt(conn, meta_keys::PHP_CONSTRAINT, d.php_constraint.as_deref())?;
    let layout = serde_json::to_value(d.layout)?;
    set_meta_opt(
        conn,
        meta_keys::LARAVEL_LAYOUT,
        d.laravel.then(|| layout.as_str().unwrap_or_default()),
    )?;
    Ok(())
}

/// Brings the whole index up to date. Cheap when nothing changed.
pub fn refresh(
    store: &mut Store,
    root: &Path,
    config: &Config,
    opts: ScanOptions,
) -> Result<RefreshReport> {
    let base = codemap_php::refresh(store, root, config, opts)?;
    let changed = opts.full || base.scan.reparsed + base.scan.removed > 0;
    let detection_current = get_meta(store.conn(), meta_keys::DETECTION_VERSION)?.as_deref()
        == Some(DETECTION_VERSION.to_string().as_str());
    let detection = match stored_detection(store)? {
        Some(d) if !changed && detection_current => d,
        _ => {
            let d = detect(root, store)?;
            save_detection(store, root, &d)?;
            d
        }
    };

    let mut laravel = LaravelReport {
        detected: detection.laravel,
        version: detection.version.clone(),
        layout: detection.laravel.then_some(detection.layout),
        ..LaravelReport::default()
    };
    if detection.laravel {
        let ctx = ExtractContext {
            root,
            config,
            scan: &base.scan,
            full: opts.full,
        };
        laravel.routes = LaravelAdapter.extract(&ctx, &detection, store)?;
        laravel.route_source = stored_routes_state(store)?.map(|s| s.route_source);
        let data_changed = opts.full
            || data_relevant(store, &base.scan)?
            || other_schema_due(store, config, &base.scan)?;
        laravel.data = extract_data(store, root, config, &detection, data_changed)?;
    } else if store.conn().query_row(
        "SELECT COUNT(*) FROM routes WHERE source != 'next'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0
    {
        // The project stopped being a Laravel app: drop stale Laravel facts.
        let conn = store.conn();
        for sql in [
            "DELETE FROM routes WHERE source != 'next'",
            "DELETE FROM models",
            "DELETE FROM model_relations",
            "DELETE FROM model_accessors",
            "DELETE FROM model_scopes",
            "DELETE FROM db_tables",
            "DELETE FROM db_columns",
            "DELETE FROM form_requests",
        ] {
            conn.execute(sql, [])?;
        }
        conn.execute(
            "DELETE FROM warnings WHERE adapter IN (?1, ?2)",
            [ADAPTER, DATA_ADAPTER],
        )?;
        conn.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            [meta_keys::ROUTES_STATE, meta_keys::DATA_STATE],
        )?;
    }
    crate::lang::refresh(store, root, &base.scan, opts.full)?;
    if detection.laravel {
        crate::filament::refresh(store, root, &base.scan, opts.full)?;
    }
    Ok(RefreshReport {
        scan: base.scan,
        php: base.php,
        laravel,
    })
}

/// Whether a change can affect models, the schema or Form Requests: anything
/// under `database/` or `config/`, composer files, or a file that declares (or
/// declared) a model or Form Request class.
fn data_relevant(store: &Store, scan: &ScanReport) -> Result<bool> {
    let paths: Vec<&String> = scan
        .changed_paths
        .iter()
        .chain(&scan.removed_paths)
        .collect();
    if paths.is_empty() {
        return Ok(false);
    }
    if paths.iter().any(|p| {
        p.starts_with("database/")
            || p.starts_with("config/")
            || p.as_str() == "composer.json"
            || p.as_str() == "composer.lock"
    }) {
        tracing::debug!("data re-extraction: database/, config/ or composer file changed");
        return Ok(true);
    }
    let conn = store.conn();
    let mut known = conn.prepare_cached(
        "SELECT 1 FROM models WHERE file = ?1 UNION ALL SELECT 1 FROM form_requests WHERE file = ?1 LIMIT 1",
    )?;
    for p in &paths {
        if known.exists([p.as_str()])? {
            tracing::debug!(path = %p, "data re-extraction: file holds a model or form request");
            return Ok(true);
        }
    }
    // A new or changed file that now declares a model / Form Request.
    let bases = [
        crate::models::ELOQUENT_MODEL,
        crate::models::AUTH_USER,
        crate::models::PIVOT,
        crate::models::MORPH_PIVOT,
        crate::requests::FORM_REQUEST,
    ];
    let relevant: std::collections::HashSet<String> =
        codemap_core::symbols::descendants(conn, &bases)?
            .into_iter()
            .map(|c| c.file)
            .collect();
    let hit = paths.iter().find(|p| relevant.contains(p.as_str()));
    if let Some(p) = hit {
        tracing::debug!(path = %p, "data re-extraction: file now declares a model or form request");
    }
    Ok(hit.is_some())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The `[[databases]]` entry whose schema is used besides the migrations.
fn schema_database<'c>(
    config: &'c Config,
    root: &Path,
) -> Option<&'c codemap_core::config::DatabaseConfig> {
    let folder = root.file_name().and_then(|n| n.to_str()).unwrap_or("");
    config.databases.iter().find(|d| {
        (d.dump.is_some() || d.schema.as_deref() == Some("live"))
            && d.app.as_deref().is_none_or(|a| a == folder)
    })
}

/// A live schema not read for a while, or a changed dump file.
fn other_schema_due(store: &Store, config: &Config, scan: &ScanReport) -> Result<bool> {
    for d in &config.databases {
        if let Some(dump) = d.dump.as_deref().and_then(Path::to_str)
            && scan.changed_paths.iter().any(|p| p == dump)
        {
            return Ok(true);
        }
        if d.schema.as_deref() == Some("live") && d.dump.is_none() {
            let last: u64 = get_meta(store.conn(), LIVE_SCHEMA_AT)?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            return Ok(now_secs().saturating_sub(last) >= LIVE_SCHEMA_TTL_SECS);
        }
    }
    Ok(false)
}

/// Puts the configured database's tables (live or from a dump) over the
/// replayed schema. Problems become warnings: the migrations still answer.
fn apply_other_schema(root: &Path, config: &Config, schema: &mut crate::schema::Schema) -> bool {
    let Some(db) = schema_database(config, root) else {
        return false;
    };
    let warn = |schema: &mut crate::schema::Schema, file: Option<String>, msg: String| {
        schema.warnings.push(crate::routes::eval::Warning {
            file,
            line: None,
            message: msg,
        });
    };
    if let Some(dump) = &db.dump {
        let rel = dump.to_string_lossy().into_owned();
        match std::fs::read_to_string(root.join(dump)) {
            Ok(sql) => {
                let parsed = crate::schema::dump::parse(&sql, &rel);
                schema.warnings.extend(parsed.warnings);
                crate::schema::live::merge(schema, parsed.tables, &format!("dump:{}", db.name));
            }
            Err(e) => warn(
                schema,
                Some(rel),
                format!("schema dump of `{}` not read: {e}", db.name),
            ),
        }
        return false;
    }
    let url = std::env::var(&db.env).ok().filter(|v| !v.is_empty());
    let Some(url) = url else {
        warn(
            schema,
            None,
            format!(
                "`{}` has schema = \"live\" but the environment variable {} is not set; get_schema shows the migrations only",
                db.name, db.env
            ),
        );
        return true;
    };
    match crate::schema::live::read(&url) {
        Ok(tables) => {
            tracing::info!(database = %db.name, tables = tables.len(), "live schema read");
            crate::schema::live::merge(schema, tables, &format!("live:{}", db.name));
        }
        Err(e) => warn(
            schema,
            None,
            format!(
                "live schema of `{}` not read: {e}; get_schema shows the migrations only",
                db.name
            ),
        ),
    }
    true
}

/// Models, schema replay and Form Requests. They read many files but are
/// cheap, so they are rebuilt whenever any file changed.
fn extract_data(
    store: &mut Store,
    root: &Path,
    config: &Config,
    detection: &Detection,
    changed: bool,
) -> Result<DataReport> {
    let state = get_meta(store.conn(), meta_keys::DATA_STATE)?;
    let version = DATA_EXTRACTOR_VERSION.to_string();
    if !changed && state.as_deref() == Some(version.as_str()) {
        let count =
            |sql: &str| -> Result<usize> { Ok(store.conn().query_row(sql, [], |r| r.get(0))?) };
        return Ok(DataReport {
            ran: false,
            models: count("SELECT COUNT(*) FROM models")?,
            tables: count("SELECT COUNT(*) FROM db_tables WHERE dropped = 0")?,
            form_requests: count("SELECT COUNT(*) FROM form_requests")?,
            ..DataReport::default()
        });
    }
    let started = Instant::now();
    let models = crate::models::extract(store.conn(), root)?;
    let mut schema = crate::schema::replay(
        root,
        &crate::schema::ReplayContext {
            models: &models,
            laravel_major: detection.major,
        },
    );
    let live = apply_other_schema(root, config, &mut schema);
    let requests = crate::requests::extract(store.conn(), root)?;

    let tx = store.conn_mut().transaction()?;
    crate::models::store(&tx, &models)?;
    crate::schema::store(&tx, &schema)?;
    crate::requests::store(&tx, &requests)?;
    tx.execute("DELETE FROM warnings WHERE adapter = ?1", [DATA_ADAPTER])?;
    for w in &schema.warnings {
        tx.execute(
            "INSERT INTO warnings (file_id, path, line, adapter, message)
             VALUES ((SELECT id FROM files WHERE path = ?1), ?1, ?2, ?3, ?4)",
            params![w.file, w.line, DATA_ADAPTER, w.message],
        )?;
    }
    set_meta(&tx, meta_keys::DATA_STATE, &version)?;
    if live {
        set_meta(&tx, LIVE_SCHEMA_AT, &now_secs().to_string())?;
    }
    tx.commit()?;

    Ok(DataReport {
        ran: true,
        models: models.len(),
        tables: schema.tables.iter().filter(|t| !t.dropped).count(),
        form_requests: requests.len(),
        migrations_replayed: schema.replayed.len(),
        warnings: schema.warnings.len(),
        duration_ms: started.elapsed().as_millis() as u64,
    })
}
