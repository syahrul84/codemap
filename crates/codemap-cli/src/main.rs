//! `codemap` — index a Laravel project and serve the facts over MCP.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use codemap_core::{Config, ScanOptions, Store, paths};
use codemap_mcp::RefreshReport;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "codemap",
    version,
    about = "Project index for Laravel projects, served over MCP"
)]
struct Cli {
    /// Stable id for the project's index (default: derived from the project).
    #[arg(long, global = true, env = "CODEMAP_PROJECT_ID")]
    project_id: Option<String>,

    /// Directory holding the indexes (default: /data in Docker, cache dir otherwise).
    #[arg(long, global = true, env = "CODEMAP_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Display name of the project (default: .codemap.toml, composer.json, git
    /// repository, then the folder name, which is `workspace` in Docker).
    #[arg(long, global = true, env = "CODEMAP_PROJECT_NAME")]
    name: Option<String>,

    /// Linked project, `name=path` (repeatable; env CODEMAP_LINKS, comma-separated),
    /// e.g. the Laravel backend of a Vue / Nuxt frontend.
    #[arg(
        long = "link",
        global = true,
        env = "CODEMAP_LINKS",
        value_delimiter = ','
    )]
    links: Vec<String>,

    /// Log filter, e.g. `info` or `codemap_core=debug`. Logs always go to stderr.
    #[arg(long, global = true, env = "CODEMAP_LOG", default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build or update the index, then print a summary.
    Scan {
        #[arg(long)]
        project: PathBuf,
        /// Ignore the existing index and re-parse every file.
        #[arg(long)]
        full: bool,
        /// Print the scan report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run the MCP server on stdio.
    Serve {
        #[arg(long)]
        project: PathBuf,
    },
    /// Call an MCP tool from the command line (for debugging).
    Query {
        #[arg(long)]
        project: PathBuf,
        /// Tool name, e.g. `search_symbol`.
        tool: String,
        /// Tool arguments as a JSON object.
        args: Option<String>,
    },
    /// Show counts, timings and index size.
    Stats {
        #[arg(long)]
        project: PathBuf,
        /// Print stats as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Write the code map: one HTML page with screens, API calls, endpoints,
    /// controllers, models, tables and their problems.
    Map {
        #[arg(long)]
        project: PathBuf,
        /// Where to write the page (in Docker, a folder mounted writable).
        #[arg(long, default_value = "codemap.html")]
        out: PathBuf,
    },
    /// Names the AI asked for that do not exist, most frequent first, with
    /// the real name and what to do about it.
    Misses {
        #[arg(long)]
        project: PathBuf,
        /// Print as JSON.
        #[arg(long)]
        json: bool,
        /// Empty the log.
        #[arg(long)]
        clear: bool,
    },
    /// Explain detection, warnings and unresolved items.
    Doctor {
        #[arg(long)]
        project: PathBuf,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// The tool was called pidx before: its `PIDX_*` variables still work.
fn legacy_env(cli: &mut Cli) {
    let old = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    if cli.project_id.is_none() {
        cli.project_id = old("PIDX_PROJECT_ID");
    }
    if cli.data_dir.is_none() {
        cli.data_dir = old("PIDX_DATA_DIR").map(PathBuf::from);
    }
    if cli.name.is_none() {
        cli.name = old("PIDX_PROJECT_NAME");
    }
    if cli.links.is_empty()
        && let Some(l) = old("PIDX_LINKS")
    {
        cli.links = l.split(',').map(str::to_owned).collect();
    }
}

fn main() -> ExitCode {
    let mut cli = Cli::parse();
    legacy_env(&mut cli);
    if let Err(err) = codemap_core::logging::init(&cli.log_level) {
        eprintln!("codemap: {err}");
        return ExitCode::FAILURE;
    }
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    tracing::debug!("starting codemap {}", codemap_core::VERSION);
    match &cli.command {
        Command::Scan {
            project,
            full,
            json,
        } => cmd_scan(&cli, project, *full, *json),
        Command::Stats { project, json } => cmd_stats(&cli, project, *json),
        Command::Serve { project } => cmd_serve(&cli, project),
        Command::Query {
            project,
            tool,
            args,
        } => cmd_query(&cli, project, tool, args.as_deref()),
        Command::Doctor { project, json } => cmd_doctor(&cli, project, *json),
        Command::Map { project, out } => cmd_map(&cli, project, out),
        Command::Misses {
            project,
            json,
            clear,
        } => cmd_misses(&cli, project, *json, *clear),
    }
}

/// Resolved locations for one project.
struct Project {
    root: PathBuf,
    config: Config,
    id: String,
    index_path: PathBuf,
    /// Workspace app name.
    app: Option<String>,
}

impl Project {
    fn mcp(&self) -> codemap_mcp::Project {
        codemap_mcp::Project {
            root: self.root.clone(),
            config: self.config.clone(),
            id: self.id.clone(),
            index_path: self.index_path.clone(),
            app: self.app.clone(),
        }
    }

    fn name(&self, store: &Store) -> String {
        self.mcp().name(store)
    }

    /// Opens the index and brings it up to date.
    fn refresh(&self, full: bool) -> anyhow::Result<(Store, RefreshReport)> {
        let mut store = Store::open(&self.index_path)?;
        store.set_meta(codemap_core::store::meta_keys::PROJECT_ID, &self.id)?;
        let report =
            codemap_mcp::refresh(&mut store, &self.root, &self.config, ScanOptions { full })?;
        Ok((store, report))
    }
}

impl Project {
    /// The project, or one per app when the folder is a workspace.
    fn resolve(cli: &Cli, project: &Path) -> anyhow::Result<Vec<Self>> {
        let root = project
            .canonicalize()
            .with_context(|| format!("project path {} not found", project.display()))?;
        anyhow::ensure!(
            root.is_dir(),
            "project path {} is not a directory",
            root.display()
        );
        let mut config = Config::load(&root)?;
        if let Some(n) = cli.name.as_deref().filter(|n| !n.trim().is_empty()) {
            config.project.name = Some(n.trim().to_owned());
        }
        for link in &cli.links {
            let (name, path) = link
                .split_once('=')
                .with_context(|| format!("--link expects name=path, got `{link}`"))?;
            config.links.retain(|l| l.name != name);
            config.links.push(codemap_core::config::LinkConfig {
                name: name.trim().to_owned(),
                path: PathBuf::from(path.trim()),
                kind: None,
                api_base: None,
                index_path: None,
            });
        }
        let id = paths::resolve_project_id(cli.project_id.as_deref(), &root)?;
        let data_dir = paths::resolve_data_dir(cli.data_dir.clone())?;
        let projects = codemap_mcp::workspace::projects(&root, config, &data_dir, &id)?;
        for p in &projects {
            tracing::debug!(id = p.id, index = %p.index_path.display(), app = ?p.app, "resolved project");
        }
        Ok(projects
            .into_iter()
            .map(|p| Self {
                root: p.root,
                config: p.config,
                id: p.id,
                index_path: p.index_path,
                app: p.app,
            })
            .collect())
    }
}

fn header(projects: &[Project], p: &Project) {
    if projects.len() > 1 {
        println!("\n== {}", p.app.as_deref().unwrap_or("."));
    }
}

fn cmd_scan(cli: &Cli, project: &Path, full: bool, json: bool) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    for p in &projects {
        if !json {
            header(&projects, p);
        }
        scan_one(p, full, json)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn scan_one(p: &Project, full: bool, json: bool) -> anyhow::Result<ExitCode> {
    let (_, report) = p.refresh(full)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let (s, php) = (&report.scan, &report.php);
        println!(
            "scanned {} files in {} ms: {} added, {} modified, {} removed, {} touched, \
             {} unchanged ({} re-parsed); {} warnings",
            s.walked,
            s.duration_ms,
            s.added,
            s.modified,
            s.removed,
            s.touched,
            s.unchanged,
            s.reparsed,
            s.warnings,
        );
        println!(
            "parsed {} PHP files in {} ms: {} symbols, {} warnings",
            php.parsed, php.duration_ms, php.symbols, php.warnings
        );
        let l = &report.laravel;
        if l.detected {
            let r = &l.routes;
            println!(
                "laravel {} ({}): {} routes, {} unresolved{}",
                l.version.as_deref().unwrap_or("?"),
                l.layout
                    .and_then(|x| serde_json::to_value(x).ok())
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                r.items,
                r.unresolved,
                if r.ran {
                    format!(
                        " (re-extracted in {} ms, {})",
                        r.duration_ms,
                        l.route_source.as_deref().unwrap_or("static")
                    )
                } else {
                    " (unchanged)".into()
                }
            );
        } else {
            if !report.vue.detected {
                println!("no Laravel or Vue project detected (see `codemap doctor`)");
            }
        }
        let v = &report.vue;
        if v.detected {
            println!(
                "{}: parsed {} JS/TS/Vue files in {} ms ({} symbols, {} warnings); {} components{}",
                match (&v.nuxt, &v.vue) {
                    (Some(n), _) => format!("nuxt {n}"),
                    (None, Some(x)) => format!("vue {x}"),
                    _ => "vue".to_owned(),
                },
                v.js.parsed,
                v.js.duration_ms,
                v.js.symbols,
                v.js.warnings,
                v.components,
                if v.registry_ran { "" } else { " (unchanged)" }
            );
        }
        let d = &report.laravel.data;
        if l.detected {
            println!(
                "laravel data: {} models, {} tables ({} migrations replayed), {} form requests{}",
                d.models,
                d.tables,
                d.migrations_replayed,
                d.form_requests,
                if d.ran { "" } else { " (unchanged)" }
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_query(
    cli: &Cli,
    project: &Path,
    tool: &str,
    args: Option<&str>,
) -> anyhow::Result<ExitCode> {
    if !codemap_mcp::TOOL_NAMES.contains(&tool) {
        anyhow::bail!(
            "unknown tool `{tool}`; available: {}",
            codemap_mcp::TOOL_NAMES.join(", ")
        );
    }
    let args: serde_json::Value = match args {
        Some(text) => serde_json::from_str(text).context("tool arguments must be a JSON object")?,
        None => serde_json::json!({}),
    };
    let projects = Project::resolve(cli, project)?;
    let mut stores = Vec::new();
    let mut reindexed = 0;
    for p in &projects {
        let (store, report) = p.refresh(false)?;
        reindexed += report.reindexed_files();
        stores.push(store);
    }
    let fresh = codemap_mcp::Freshness {
        reindexed_files: reindexed,
        ..codemap_mcp::Freshness::default()
    };
    let mcp: Vec<codemap_mcp::Project> = projects.iter().map(Project::mcp).collect();
    let apps: Vec<(&Store, &codemap_mcp::Project)> = stores.iter().zip(&mcp).collect();
    let envelope = codemap_mcp::workspace::call(&apps, tool, args, fresh)?;
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(ExitCode::SUCCESS)
}

fn cmd_map(cli: &Cli, project: &Path, out: &Path) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    let mut stores = Vec::new();
    for p in &projects {
        let (store, _) = p.refresh(false)?;
        stores.push(store);
    }
    let mcp: Vec<codemap_mcp::Project> = projects.iter().map(Project::mcp).collect();
    let apps: Vec<(&Store, &codemap_mcp::Project)> = stores.iter().zip(&mcp).collect();
    let title = cli.name.clone().unwrap_or_else(|| mcp[0].name(&stores[0]));
    let mut data = codemap_mcp::map::data(&apps, &title)?;
    codemap_mcp::map::prune(&mut data);
    let page = codemap_mcp::map::html(&data);
    if let Err(e) = std::fs::write(out, &page) {
        let dir = out
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let why = match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                "permission denied. In Docker the page is written by the container's user, which needs a folder it may write to: mount one with `-v \"$PWD/codemap-out\":/out` and pass `--out /out/codemap.html`. On Linux, also add `--user \"$(id -u):$(id -g)\"` so the folder's owner matches; the project itself stays read-only.".to_owned()
            }
            std::io::ErrorKind::NotFound => format!(
                "the folder {} does not exist (in Docker: it is not mounted). Mount a writable folder with `-v \"$PWD/codemap-out\":/out` and pass `--out /out/codemap.html`.",
                dir.display()
            ),
            std::io::ErrorKind::ReadOnlyFilesystem => "the folder is mounted read-only (the project mount is `:ro`). Mount a separate writable folder with `-v \"$PWD/codemap-out\":/out` and pass `--out /out/codemap.html`.".to_owned(),
            _ => e.to_string(),
        };
        anyhow::bail!("cannot write {}: {why}", out.display());
    }
    let count = |k: &str| {
        data["nodes"].as_array().map_or(0, |n| {
            n.iter()
                .filter(|n| n["kind"] == k && n["stub"] != true)
                .count()
        })
    };
    eprintln!(
        "wrote {} ({} KB): {} screens, {} frontend files, {} endpoints, {} controllers, {} models, {} tables",
        out.display(),
        page.len() / 1024,
        count("screen"),
        count("frontend"),
        count("endpoint"),
        count("controller"),
        count("model"),
        count("table"),
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_misses(cli: &Cli, project: &Path, json: bool, clear: bool) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    let mcp: Vec<codemap_mcp::Project> = projects.iter().map(Project::mcp).collect();
    if clear {
        for p in &mcp {
            codemap_mcp::misses::clear(p)?;
        }
        eprintln!("the AI mistakes log is empty");
        return Ok(ExitCode::SUCCESS);
    }
    let mut stores = Vec::new();
    for p in &projects {
        let (store, _) = p.refresh(false)?;
        stores.push(store);
    }
    let apps: Vec<(&Store, &codemap_mcp::Project)> = stores.iter().zip(&mcp).collect();
    let list = codemap_mcp::misses::report(&apps, 200);
    if json {
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(ExitCode::SUCCESS);
    }
    if list.is_empty() {
        println!("No AI mistakes logged (or every name asked for exists now).");
        return Ok(ExitCode::SUCCESS);
    }
    for m in &list {
        let s = |k: &str| m[k].as_str().unwrap_or_default().to_owned();
        let app = m["app"]
            .as_str()
            .map(|a| format!("[{a}] "))
            .unwrap_or_default();
        println!(
            "{app}{}× {} `{}`{}",
            m["times"],
            s("kind"),
            s("item"),
            m["closest"]
                .as_str()
                .map(|c| format!("  → `{c}`"))
                .unwrap_or_default()
        );
        println!("    {}", s("idea"));
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_serve(cli: &Cli, project: &Path) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    runtime.block_on(codemap_mcp::serve(
        projects.iter().map(Project::mcp).collect(),
    ))?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_doctor(cli: &Cli, project: &Path, json: bool) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    for p in &projects {
        if !json {
            header(&projects, p);
        }
        doctor_one(p, json)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn doctor_one(p: &Project, json: bool) -> anyhow::Result<ExitCode> {
    let (store, _) = p.refresh(false)?;
    let report = codemap_laravel::doctor::build(&store, &p.root, &p.config, &p.name(&store))?;
    let vue = codemap_vue::doctor::build(&store)?;
    if json {
        let mut v = serde_json::to_value(&report)?;
        v["vue"] = serde_json::to_value(&vue)?;
        v["i18n"] = codemap_mcp::i18n::health_json(store.conn(), &p.config.i18n.locales)?;
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        print!("{}", codemap_laravel::doctor::render_text(&report));
        if let Some(vue) = &vue {
            print!("{}", codemap_vue::doctor::render_text(vue));
            print!("{}", codemap_mcp::api::doctor_text(&store, &p.mcp())?);
        }
        print!(
            "{}",
            codemap_mcp::i18n::doctor_text(store.conn(), &p.config.i18n.locales)?
        );
        let files: Vec<String> = store.list_files()?.into_iter().map(|f| f.path).collect();
        let m =
            codemap_laravel::migration_status::status(&p.root, &files, &p.config.databases, false);
        if m["configured"] == true && m["migration_files"].as_u64().unwrap_or(0) > 0 {
            println!("\nmigrations ({} files)", m["migration_files"]);
            for d in m["databases"].as_array().into_iter().flatten() {
                let name = d["name"].as_str().unwrap_or("?");
                let target = d["database"].as_str().unwrap_or("");
                match d["status"].as_str().unwrap_or("") {
                    "pending" => {
                        println!("  {name} ({target}): {} pending", d["pending_total"]);
                        for x in d["pending"].as_array().into_iter().flatten() {
                            println!("    {}", x.as_str().unwrap_or(""));
                        }
                    }
                    "up_to_date" => println!("  {name} ({target}): up to date"),
                    _ => println!(
                        "  {name}: {} ({})",
                        d["status"].as_str().unwrap_or(""),
                        d["detail"].as_str().unwrap_or("")
                    ),
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Serialize)]
struct StatsOutput {
    project_id: String,
    index_path: PathBuf,
    index_bytes: u64,
    #[serde(flatten)]
    index: codemap_core::store::IndexStats,
}

fn cmd_stats(cli: &Cli, project: &Path, json: bool) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    let mut code = ExitCode::SUCCESS;
    for p in projects.iter() {
        if !json {
            header(&projects, p);
        }
        if stats_one(p, json)? != ExitCode::SUCCESS {
            code = ExitCode::FAILURE;
        }
    }
    Ok(code)
}

fn stats_one(p: &Project, json: bool) -> anyhow::Result<ExitCode> {
    if !p.index_path.exists() {
        eprintln!(
            "no index yet for {} (expected {}); run `codemap scan` first",
            p.root.display(),
            p.index_path.display()
        );
        return Ok(ExitCode::FAILURE);
    }
    let store = Store::open(&p.index_path)?;
    let out = StatsOutput {
        index_bytes: index_bytes(&p.index_path),
        project_id: p.id.clone(),
        index_path: p.index_path.clone(),
        index: store.stats()?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(ExitCode::SUCCESS);
    }
    let s = &out.index;
    println!("project id   {}", out.project_id);
    println!(
        "index        {} ({} bytes)",
        out.index_path.display(),
        out.index_bytes
    );
    println!("schema       v{}", s.schema_version);
    println!(
        "indexed at   {}",
        s.indexed_at.as_deref().unwrap_or("never")
    );
    println!(
        "git          {}{}",
        s.git_commit
            .as_deref()
            .map_or("n/a", |c| &c[..c.len().min(12)]),
        if s.git_dirty == Some(true) {
            " (dirty)"
        } else {
            ""
        }
    );
    println!("files        {} ({} bytes)", s.files_total, s.source_bytes);
    for (lang, count) in &s.files_by_lang {
        println!("  {lang:<10} {count}");
    }
    let symbol_total: u64 = s.symbols_by_kind.values().sum();
    println!("symbols      {symbol_total}");
    for (kind, count) in &s.symbols_by_kind {
        println!("  {kind:<10} {count}");
    }
    println!("warnings     {}", s.warnings);
    if let Some(last) = &s.last_scan {
        println!(
            "last scan    {} ms, {} walked, {} re-parsed, {} removed",
            last.duration_ms, last.walked, last.reparsed, last.removed
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Size of the index including its WAL and shared-memory files.
fn index_bytes(path: &Path) -> u64 {
    ["", "-wal", "-shm"]
        .iter()
        .filter_map(|suffix| {
            let mut p = path.as_os_str().to_owned();
            p.push(suffix);
            std::fs::metadata(p).ok()
        })
        .map(|m| m.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_global_flags_after_subcommand() {
        let cli = Cli::try_parse_from([
            "codemap",
            "scan",
            "--project",
            "/workspace",
            "--full",
            "--project-id",
            "abc123",
            "--data-dir",
            "/data",
            "--log-level",
            "debug",
        ])
        .unwrap();
        assert_eq!(cli.project_id.as_deref(), Some("abc123"));
        assert_eq!(cli.data_dir, Some(PathBuf::from("/data")));
        assert_eq!(cli.log_level, "debug");
        assert!(matches!(
            cli.command,
            Command::Scan { ref project, full: true, json: false } if project == &PathBuf::from("/workspace")
        ));
    }

    #[test]
    fn parses_every_subcommand() {
        for args in [
            &["codemap", "serve", "--project", "/w"][..],
            &[
                "codemap",
                "query",
                "--project",
                "/w",
                "search_symbol",
                r#"{"query":"Order"}"#,
            ],
            &["codemap", "stats", "--project", "/w", "--json"],
            &["codemap", "scan", "--project", "/w", "--json"],
            &["codemap", "doctor", "--project", "/w"],
        ] {
            Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
    }

    #[test]
    fn project_is_required() {
        assert!(Cli::try_parse_from(["codemap", "scan"]).is_err());
    }
}
