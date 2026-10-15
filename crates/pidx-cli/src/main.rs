//! `pidx` — index a Laravel project and serve the facts over MCP.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use pidx_core::{Config, ScanOptions, Store, paths};
use pidx_mcp::RefreshReport;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "pidx",
    version,
    about = "Project index for Laravel projects, served over MCP"
)]
struct Cli {
    /// Stable id for the project's index (default: derived from the project).
    #[arg(long, global = true, env = "PIDX_PROJECT_ID")]
    project_id: Option<String>,

    /// Directory holding the indexes (default: /data in Docker, cache dir otherwise).
    #[arg(long, global = true, env = "PIDX_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Display name of the project (default: .pidx.toml, composer.json, git
    /// repository, then the folder name, which is `workspace` in Docker).
    #[arg(long, global = true, env = "PIDX_PROJECT_NAME")]
    name: Option<String>,

    /// Linked project, `name=path` (repeatable; env PIDX_LINKS, comma-separated),
    /// e.g. the Laravel backend of a Vue / Nuxt frontend.
    #[arg(
        long = "link",
        global = true,
        env = "PIDX_LINKS",
        value_delimiter = ','
    )]
    links: Vec<String>,

    /// Log filter, e.g. `info` or `pidx_core=debug`. Logs always go to stderr.
    #[arg(long, global = true, env = "PIDX_LOG", default_value = "info")]
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
    /// Explain detection, warnings and unresolved items.
    Doctor {
        #[arg(long)]
        project: PathBuf,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(err) = pidx_core::logging::init(&cli.log_level) {
        eprintln!("pidx: {err}");
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
    tracing::debug!("starting pidx {}", pidx_core::VERSION);
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
    fn mcp(&self) -> pidx_mcp::Project {
        pidx_mcp::Project {
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
        store.set_meta(pidx_core::store::meta_keys::PROJECT_ID, &self.id)?;
        let report = pidx_mcp::refresh(&mut store, &self.root, &self.config, ScanOptions { full })?;
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
            config.links.push(pidx_core::config::LinkConfig {
                name: name.trim().to_owned(),
                path: PathBuf::from(path.trim()),
                kind: None,
                api_base: None,
                index_path: None,
            });
        }
        let id = paths::resolve_project_id(cli.project_id.as_deref(), &root)?;
        let data_dir = paths::resolve_data_dir(cli.data_dir.clone())?;
        let projects = pidx_mcp::workspace::projects(&root, config, &data_dir, &id)?;
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
                println!("no Laravel or Vue project detected (see `pidx doctor`)");
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
    if !pidx_mcp::TOOL_NAMES.contains(&tool) {
        anyhow::bail!(
            "unknown tool `{tool}`; available: {}",
            pidx_mcp::TOOL_NAMES.join(", ")
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
    let fresh = pidx_mcp::Freshness {
        reindexed_files: reindexed,
        ..pidx_mcp::Freshness::default()
    };
    let mcp: Vec<pidx_mcp::Project> = projects.iter().map(Project::mcp).collect();
    let apps: Vec<(&Store, &pidx_mcp::Project)> = stores.iter().zip(&mcp).collect();
    let envelope = pidx_mcp::workspace::call(&apps, tool, args, fresh)?;
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(ExitCode::SUCCESS)
}

fn cmd_serve(cli: &Cli, project: &Path) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    runtime.block_on(pidx_mcp::serve(projects.iter().map(Project::mcp).collect()))?;
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
    let report = pidx_laravel::doctor::build(&store, &p.root, &p.config, &p.name(&store))?;
    let vue = pidx_vue::doctor::build(&store)?;
    if json {
        let mut v = serde_json::to_value(&report)?;
        v["vue"] = serde_json::to_value(&vue)?;
        v["i18n"] = pidx_mcp::i18n::health_json(store.conn())?;
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        print!("{}", pidx_laravel::doctor::render_text(&report));
        if let Some(vue) = &vue {
            print!("{}", pidx_vue::doctor::render_text(vue));
            print!("{}", pidx_mcp::api::doctor_text(&store, &p.mcp())?);
        }
        print!("{}", pidx_mcp::i18n::doctor_text(store.conn())?);
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Serialize)]
struct StatsOutput {
    project_id: String,
    index_path: PathBuf,
    index_bytes: u64,
    #[serde(flatten)]
    index: pidx_core::store::IndexStats,
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
            "no index yet for {} (expected {}); run `pidx scan` first",
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
            "pidx",
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
            &["pidx", "serve", "--project", "/w"][..],
            &[
                "pidx",
                "query",
                "--project",
                "/w",
                "search_symbol",
                r#"{"query":"Order"}"#,
            ],
            &["pidx", "stats", "--project", "/w", "--json"],
            &["pidx", "scan", "--project", "/w", "--json"],
            &["pidx", "doctor", "--project", "/w"],
        ] {
            Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
    }

    #[test]
    fn project_is_required() {
        assert!(Cli::try_parse_from(["pidx", "scan"]).is_err());
    }
}
