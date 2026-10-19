//! `codemap` — index a Laravel project and serve the facts over MCP.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use codemap_core::{Config, ScanOptions, Store, paths};
use codemap_mcp::RefreshReport;
use serde::Serialize;

mod update;

#[derive(Debug, Parser)]
#[command(
    name = "codemap",
    version = codemap_core::VERSION,
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
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
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
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Call an MCP tool from the command line (for debugging).
    Query {
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Tool name, e.g. `search_symbol`.
        tool: String,
        /// Tool arguments as a JSON object.
        args: Option<String>,
    },
    /// Show counts, timings and index size.
    Stats {
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Print stats as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Write the code map: one HTML page with screens, API calls, endpoints,
    /// controllers, models, tables and their problems.
    Map {
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Where to write the page (in Docker, a folder mounted writable).
        #[arg(long, default_value = "codemap.html")]
        out: PathBuf,
    },
    /// How codemap is used in this project: tool calls (time taken, answers
    /// that found nothing, errors, retries, wrong answers reported), AI app
    /// sessions and index runs. Kept for 90 days on this machine.
    Usage {
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Print as JSON (every number, per tool and per day).
        #[arg(long)]
        json: bool,
        /// Only the last N days (at most 90).
        #[arg(long, default_value_t = 90)]
        days: i64,
        /// Empty the usage log.
        #[arg(long)]
        clear: bool,
    },
    /// Names the AI asked for that do not exist, most frequent first, with
    /// the real name and what to do about it.
    Misses {
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Print as JSON.
        #[arg(long)]
        json: bool,
        /// Empty the log.
        #[arg(long)]
        clear: bool,
    },
    /// Answers codemap got wrong (reported by the AI or found by codemap),
    /// re-checked; export them as test cases or send one to the codemap repo.
    Reports {
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Print as JSON.
        #[arg(long)]
        json: bool,
        /// Also list closed reports.
        #[arg(long)]
        all: bool,
        /// Write one folder per open report into this folder.
        #[arg(long)]
        export: Option<PathBuf>,
        /// Close one report by hand (not a real bug).
        #[arg(long)]
        close: Option<String>,
        /// Send one report (with a reproducing made-up example) to the codemap repository, after asking.
        #[arg(long)]
        send: Option<String>,
        /// Empty the log.
        #[arg(long)]
        clear: bool,
    },
    /// Install the latest release of codemap (checked against its SHA256SUMS).
    Update {
        /// Only say whether a newer version exists.
        #[arg(long)]
        check: bool,
        /// Install the latest release even if it is not newer (a repair).
        #[arg(long)]
        force: bool,
    },
    /// Save the current API findings so later runs (`check api`,
    /// `api_usage new_only`) report only new ones.
    Baseline {
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Write here instead of the index folder; commit it as
        /// `.codemap-baseline.json` in the repository root for CI.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// A gate for CI and pre-commit: print new findings and exit 1 when a new
    /// call has no route or uses the wrong method.
    Check {
        /// What to check (`api`).
        #[arg(value_parser = ["api"])]
        what: String,
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Only what this branch changes (plus uncommitted changes).
        #[arg(long)]
        changed: bool,
        /// The branch to compare with (default: origin/HEAD, main, master, develop).
        #[arg(long)]
        base: Option<String>,
        /// The baseline file (default: the index folder, then `.codemap-baseline.json`).
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Print as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Explain detection, warnings and unresolved items.
    Doctor {
        /// The project folder (default: the current folder).
        #[arg(long, default_value = ".")]
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
    codemap_core::init_threads();
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
        Command::Reports {
            project,
            json,
            all,
            export,
            close,
            send,
            clear,
        } => cmd_reports(
            &cli,
            project,
            *json,
            *all,
            export.as_deref(),
            close.as_deref(),
            send.as_deref(),
            *clear,
        ),
        Command::Update { check, force } => {
            let data_dir = paths::resolve_data_dir(cli.data_dir.clone())?;
            update::run(&data_dir, *check, *force)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Misses {
            project,
            json,
            clear,
        } => cmd_misses(&cli, project, *json, *clear),
        Command::Baseline { project, out } => cmd_baseline(&cli, project, out.as_deref()),
        Command::Usage {
            project,
            json,
            days,
            clear,
        } => cmd_usage(&cli, project, *json, *days, *clear),
        Command::Check {
            project,
            changed,
            base,
            baseline,
            json,
            ..
        } => cmd_check(
            &cli,
            project,
            *changed,
            base.as_deref(),
            baseline.as_deref(),
            *json,
        ),
    }
}

fn cmd_usage(
    cli: &Cli,
    project: &Path,
    json: bool,
    days: i64,
    clear: bool,
) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    let days = days.clamp(1, codemap_mcp::usage::KEEP_DAYS);
    let data_dir = paths::resolve_data_dir(cli.data_dir.clone())?;
    let mcp: Vec<codemap_mcp::Project> = projects.iter().map(Project::mcp).collect();
    // An app writes its log under the index it was served with: on its own
    // (`serve --project backend`) or as part of the workspace. Both count.
    let aliases: Vec<Vec<codemap_mcp::Project>> =
        mcp.iter().map(|p| other_indexes(p, &data_dir)).collect();
    if clear {
        for p in mcp.iter().chain(aliases.iter().flatten()) {
            codemap_mcp::usage::clear(p)?;
        }
        println!("usage log cleared");
        return Ok(ExitCode::SUCCESS);
    }
    let all: Vec<serde_json::Value> = mcp
        .iter()
        .zip(&aliases)
        .map(|(p, also)| codemap_mcp::usage::summary_with(p, also, days, &mcp))
        .collect();
    if json {
        let out = if all.len() == 1 {
            all[0].clone()
        } else {
            serde_json::json!({ "apps": all })
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(ExitCode::SUCCESS);
    }
    if all.len() > 1 {
        let n = |v: &serde_json::Value| v.as_u64().unwrap_or(0);
        let per_app: Vec<String> = projects
            .iter()
            .zip(&all)
            .map(|(p, u)| {
                format!(
                    "{} {}",
                    p.app.as_deref().unwrap_or("."),
                    n(&u["calls"]["total"])
                )
            })
            .collect();
        // Every app records every session of the workspace's server.
        let sessions = all
            .iter()
            .map(|u| n(&u["sessions"]["count"]))
            .max()
            .unwrap_or(0);
        println!(
            "workspace: {} tool calls in {} {} ({}); per app below",
            all.iter().map(|u| n(&u["calls"]["total"])).sum::<u64>(),
            sessions,
            if sessions == 1 { "session" } else { "sessions" },
            per_app.join(", ")
        );
    }
    for (p, u) in projects.iter().zip(&all) {
        header(&projects, p);
        print_usage(u);
    }
    Ok(ExitCode::SUCCESS)
}

/// The same app's other indexes: a workspace app indexed on its own, or an
/// app indexed as part of the workspace around it. Only those that exist.
fn other_indexes(p: &codemap_mcp::Project, data_dir: &Path) -> Vec<codemap_mcp::Project> {
    let mut ids: Vec<String> = Vec::new();
    if p.app.is_some() {
        if let Ok(id) = paths::resolve_project_id(None, &p.root) {
            ids.push(id);
        }
    } else if let (Some(parent), Some(dir)) = (p.root.parent(), p.root.file_name()) {
        let dir: String = dir
            .to_string_lossy()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        if let Ok(ws) = paths::resolve_project_id(None, parent) {
            ids.push(format!("{ws}-{dir}"));
        }
    }
    ids.into_iter()
        .filter(|id| *id != p.id)
        .map(|id| codemap_mcp::Project {
            index_path: paths::index_path(data_dir, &id),
            id,
            ..p.clone()
        })
        .filter(|x| x.index_path.parent().is_some_and(Path::is_dir))
        .collect()
}

fn print_usage(u: &serde_json::Value) {
    let n = |v: &serde_json::Value| v.as_u64().unwrap_or(0);
    let period = &u["period"];
    let calls = &u["calls"];
    if n(&calls["total"]) == 0 && n(&u["indexing"]["runs"]) == 0 && n(&u["sessions"]["count"]) == 0
    {
        println!(
            "no usage recorded in the last {} days (the log starts when the MCP server or `codemap query` is used)",
            n(&period["days"])
        );
        return;
    }
    println!(
        "last {} days ({} to {})",
        n(&period["days"]),
        period["from"]
            .as_str()
            .unwrap_or("?")
            .get(..10)
            .unwrap_or("?"),
        period["to"]
            .as_str()
            .unwrap_or("?")
            .get(..10)
            .unwrap_or("?")
    );
    let clients: Vec<String> = u["sessions"]["clients"]
        .as_object()
        .map(|o| o.iter().map(|(k, v)| format!("{k} ×{}", n(v))).collect())
        .unwrap_or_default();
    println!(
        "sessions     {}{}",
        n(&u["sessions"]["count"]),
        if clients.is_empty() {
            String::new()
        } else {
            format!(" ({})", clients.join(", "))
        }
    );
    println!(
        "tool calls   {} (avg {} ms, 95% under {} ms){}",
        n(&calls["total"]),
        n(&calls["avg_ms"]),
        n(&calls["p95_ms"]),
        if n(&calls["from_cli"]) > 0 {
            format!("; {} from the CLI", n(&calls["from_cli"]))
        } else {
            String::new()
        }
    );
    if let Some(rows) = u["tools"].as_array().filter(|r| !r.is_empty()) {
        println!();
        println!(
            "  {:<20} {:>6} {:>7} {:>7} {:>5} {:>9} {:>7} {:>7} {:>7} {:>9} {:>8}",
            "tool",
            "calls",
            "avg ms",
            "p95 ms",
            "slow",
            "not found",
            "refused",
            "errors",
            "retried",
            "reported",
            "missing"
        );
        for r in rows {
            println!(
                "  {:<20} {:>6} {:>7} {:>7} {:>5} {:>9} {:>7} {:>7} {:>7} {:>9} {:>8}",
                r["tool"].as_str().unwrap_or("?"),
                n(&r["calls"]),
                n(&r["avg_ms"]),
                n(&r["p95_ms"]),
                n(&r["slow"]),
                n(&r["not_found"]),
                n(&r["invalid_args"]),
                n(&r["errors"]),
                n(&r["retried"]),
                format!(
                    "{}/{}",
                    n(&r["wrong_answers_reported_by_ai"]),
                    n(&r["wrong_answers_found_by_codemap"])
                ),
                n(&r["names_ai_asked_that_do_not_exist"]),
            );
        }
        println!(
            "  (slow: over 5 s; refused: arguments the tool did not accept; retried: the AI asked again with other arguments within 2 minutes;"
        );
        println!(
            "   reported: wrong answers reported by the AI / found by codemap; missing: names the AI asked for that do not exist)"
        );
    }
    let acc = &u["accuracy"];
    let w = &acc["wrong_answers"];
    println!();
    println!(
        "wrong answers {} ({} open, {} closed); names the AI asked for that do not exist: {}; slow answers codemap reported: {}",
        n(&w["total"]),
        n(&w["open"]),
        n(&w["closed"]),
        n(&acc["names_ai_asked_that_do_not_exist"]),
        n(&acc["slow_answers_reported"])
    );
    for r in acc["open_reports"]
        .as_array()
        .into_iter()
        .flatten()
        .take(10)
    {
        println!(
            "  open {} {} {}: {}",
            r["id"].as_str().unwrap_or(""),
            r["tool"].as_str().unwrap_or(""),
            r["kind"].as_str().unwrap_or(""),
            r["item"].as_str().unwrap_or("")
        );
    }
    let top: Vec<String> = acc["top_missing_names"]
        .as_array()
        .into_iter()
        .flatten()
        .take(5)
        .map(|m| format!("{} ×{}", m["name"].as_str().unwrap_or(""), n(&m["times"])))
        .collect();
    if !top.is_empty() {
        println!("  most asked for: {}", top.join(", "));
    }
    println!(
        "  codemap cannot see whether an answer was right; these are the signals it has (see `codemap reports`, `codemap misses`)."
    );
    let ix = &u["indexing"];
    println!();
    let triggers: Vec<String> = ix["by_trigger"]
        .as_object()
        .map(|o| o.iter().map(|(k, v)| format!("{k} {}", n(v))).collect())
        .unwrap_or_default();
    println!(
        "indexing     {} index {} ({} full{}), {} files read, {} ms in total, avg {} ms",
        n(&ix["runs"]),
        if n(&ix["runs"]) == 1 { "run" } else { "runs" },
        n(&ix["full_runs"]),
        if n(&ix["failed_runs"]) > 0 {
            format!(", {} failed", n(&ix["failed_runs"]))
        } else {
            String::new()
        },
        n(&ix["files_read"]),
        n(&ix["total_ms"]),
        n(&ix["avg_ms"])
    );
    if !triggers.is_empty() {
        println!("  why: {}", triggers.join(", "));
    }
    for (label, key) in [("slowest", "slowest"), ("last full", "last_full")] {
        let e = &ix[key];
        if e.is_object() {
            println!(
                "  {label}: {} ms on {} ({}, {} files read of {})",
                n(&e["ms"]),
                e["at"]
                    .as_str()
                    .unwrap_or("")
                    .replace('T', " ")
                    .trim_end_matches('Z'),
                e["trigger"].as_str().unwrap_or(""),
                n(&e["files_read"]),
                n(&e["files_walked"])
            );
        }
    }
}

/// Every full `api_usage` row of one app (all pages).
fn api_rows(
    store: &Store,
    project: &codemap_mcp::Project,
    changed: bool,
    base: Option<&str>,
) -> anyhow::Result<(Vec<serde_json::Value>, serde_json::Value)> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let out = codemap_mcp::api::api_usage(
            store,
            project,
            codemap_mcp::api::ApiUsageParams {
                git_changes: changed,
                base: base.map(str::to_owned),
                limit: Some(100),
                cursor: cursor.take(),
                detail: codemap_mcp::tools::DetailParam::Full,
                ..Default::default()
            },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        rows.extend(out.data["calls"].as_array().cloned().unwrap_or_default());
        match out.next_cursor {
            Some(c) => cursor = Some(c),
            None => return Ok((rows, out.data)),
        }
    }
}

fn cmd_baseline(cli: &Cli, project: &Path, out: Option<&Path>) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    let mut findings = serde_json::Map::new();
    let mut written = Vec::new();
    for p in &projects {
        let (store, _) = p.refresh(false)?;
        let mcp = p.mcp();
        let (rows, _) = api_rows(&store, &mcp, false, None)?;
        let mut keys: Vec<String> = rows.iter().flat_map(codemap_mcp::api::row_keys).collect();
        keys.sort();
        keys.dedup();
        let app = p.app.clone().unwrap_or_default();
        println!(
            "{}: {} findings saved",
            if app.is_empty() { "project" } else { &app },
            keys.len()
        );
        if out.is_none() {
            let path = mcp
                .index_path
                .with_file_name(codemap_mcp::api::BASELINE_FILE);
            let one = serde_json::json!({ "version": 1, "codemap": codemap_core::VERSION, "findings": { app: keys } });
            std::fs::write(&path, serde_json::to_string_pretty(&one)? + "\n")
                .with_context(|| format!("writing {}", path.display()))?;
            written.push(path);
        } else {
            findings.insert(app, serde_json::json!(keys));
        }
    }
    if let Some(path) = out {
        let all = serde_json::json!({ "version": 1, "codemap": codemap_core::VERSION, "findings": findings });
        std::fs::write(path, serde_json::to_string_pretty(&all)? + "\n")
            .with_context(|| format!("writing {}", path.display()))?;
        written.push(path.to_path_buf());
    }
    for w in written {
        println!("wrote {}", w.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_check(
    cli: &Cli,
    project: &Path,
    changed: bool,
    base: Option<&str>,
    baseline: Option<&Path>,
    json: bool,
) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    let mut report = Vec::new();
    let mut failing = 0usize;
    let mut warnings = 0usize;
    for p in &projects {
        let (store, _) = p.refresh(false)?;
        let mcp = p.mcp();
        let known = match baseline {
            Some(path) => Some(
                codemap_mcp::api::load_baseline(path, p.app.as_deref())
                    .with_context(|| format!("no baseline for this app in {}", path.display()))?,
            ),
            None => codemap_mcp::api::baseline_paths(&mcp)
                .iter()
                .find_map(|path| codemap_mcp::api::load_baseline(path, p.app.as_deref())),
        };
        let (rows, data) = api_rows(&store, &mcp, changed, base)?;
        for row in rows {
            let keys = codemap_mcp::api::row_keys(&row);
            if keys.is_empty()
                || known
                    .as_ref()
                    .is_some_and(|k| keys.iter().all(|x| k.contains(x)))
            {
                continue;
            }
            let status = row["status"].as_str().unwrap_or_default();
            let fails = matches!(status, "missing" | "method_mismatch");
            if fails {
                failing += 1;
            } else {
                warnings += 1;
            }
            let mut row = row;
            row["app"] = serde_json::json!(p.app);
            row["fails"] = serde_json::json!(fails);
            report.push(row);
        }
        if !json && let Some(sc) = data.get("scope") {
            eprintln!(
                "{}compared with {} ({} changed files)",
                p.app
                    .as_deref()
                    .map(|a| format!("{a}: "))
                    .unwrap_or_default(),
                sc["compared_with"].as_str().unwrap_or("?"),
                sc["changed_files"]
            );
        }
        if !json && known.is_none() {
            eprintln!(
                "note: no baseline saved; every finding counts as new (run `codemap baseline`)"
            );
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "failing": failing, "warnings": warnings, "findings": report
            }))?
        );
    } else {
        for r in &report {
            let s = r["status"].as_str().unwrap_or_default();
            let what = match s {
                "missing" => "no route".to_owned(),
                "method_mismatch" => format!("wrong method; route is {}", r["path_exists_as"]),
                "unresolved" => format!("unclear: {}", r["why"].as_str().unwrap_or_default()),
                _ => {
                    let list = |k: &str| -> String {
                        let mut v: Vec<&str> = r[k]
                            .as_array()
                            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                            .unwrap_or_default();
                        v.dedup();
                        v.join(", ")
                    };
                    let mut parts = Vec::new();
                    for (k, text) in [
                        (
                            "read_but_not_returned",
                            "reads {} but the endpoint does not return it",
                        ),
                        (
                            "typed_but_not_returned",
                            "its type requires {} but the endpoint does not send it",
                        ),
                        ("missing_required", "does not send required {}"),
                        (
                            "unknown_field",
                            "sends {}, which the action does not accept",
                        ),
                        ("possible_rename", "possibly renamed (sent → required): {}"),
                    ] {
                        let l = list(k);
                        if !l.is_empty() {
                            parts.push(text.replace("{}", &l));
                        }
                    }
                    parts.join("; ")
                }
            };
            println!(
                "{} {}  {}  → {what}",
                if r["fails"] == true { "FAIL" } else { "warn" },
                r["at"].as_str().unwrap_or_default(),
                r["call"].as_str().unwrap_or_default(),
            );
        }
        println!("{failing} failing, {warnings} warnings (new findings only)");
    }
    Ok(if failing > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
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
        self.refresh_as(full, "cli")
    }

    /// Refreshes the index and writes the run to the usage log.
    fn refresh_as(&self, full: bool, trigger: &str) -> anyhow::Result<(Store, RefreshReport)> {
        let mut store = Store::open(&self.index_path)?;
        store.set_meta(codemap_core::store::meta_keys::PROJECT_ID, &self.id)?;
        let started = std::time::Instant::now();
        let report =
            codemap_mcp::refresh(&mut store, &self.root, &self.config, ScanOptions { full });
        codemap_mcp::usage::record_index(&self.mcp(), trigger, full, started, report.as_ref().ok());
        Ok((store, report?))
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
    let (_, report) = p.refresh_as(full, "scan")?;
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
            if !report.vue.detected
                && !report.swift.detected
                && !report.android.detected
                && !report.rust.detected
            {
                println!(
                    "no Laravel, Vue, Swift, Android or Rust project detected (see `codemap doctor`)"
                );
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
        let sw = &report.swift;
        if sw.detected {
            println!(
                "swift: parsed {} files in {} ms ({} symbols); {} views, {} screens{}",
                sw.parsed,
                sw.duration_ms,
                sw.symbols,
                sw.views,
                sw.screens,
                if sw.parsed == 0 { " (unchanged)" } else { "" }
            );
        }
        let an = &report.android;
        if an.detected {
            println!(
                "android: parsed {} files in {} ms ({} symbols); {} composables, {} screens{}",
                an.parsed,
                an.duration_ms,
                an.symbols,
                an.composables,
                an.screens,
                if an.parsed == 0 { " (unchanged)" } else { "" }
            );
        }
        let rs = &report.rust;
        if rs.detected {
            println!(
                "rust: parsed {} files in {} ms ({} symbols); {} crates, {} binaries and commands{}",
                rs.parsed,
                rs.duration_ms,
                rs.symbols,
                rs.crates,
                rs.entry_points,
                if rs.parsed == 0 { " (unchanged)" } else { "" }
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
    let started = std::time::Instant::now();
    let result = codemap_mcp::workspace::call(&apps, tool, args.clone(), fresh);
    let target = args["app"]
        .as_str()
        .and_then(|a| mcp.iter().find(|p| p.app.as_deref() == Some(a)))
        .unwrap_or(&mcp[0]);
    let (text, outcome) = match &result {
        Ok(env) => (
            serde_json::to_string_pretty(env)?,
            codemap_mcp::usage::outcome_of(&env.data),
        ),
        Err(codemap_mcp::ToolError::InvalidParams(m)) => (m.clone(), "invalid_args"),
        Err(e) => (e.to_string(), "error"),
    };
    codemap_mcp::usage::append(
        target,
        &codemap_mcp::usage::Entry::Call {
            at: codemap_core::time::now_rfc3339(),
            tool: tool.to_owned(),
            args: codemap_mcp::usage::redact(&args),
            ms: started.elapsed().as_millis() as u64,
            bytes: text.len() as u64,
            outcome: outcome.to_owned(),
            stale: false,
            reindexed: 0,
            call_id: None,
            via: "cli".into(),
        },
    );
    result?;
    println!("{text}");
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

#[allow(clippy::too_many_arguments)]
fn cmd_reports(
    cli: &Cli,
    project: &Path,
    json: bool,
    all: bool,
    export: Option<&Path>,
    close: Option<&str>,
    send: Option<&str>,
    clear: bool,
) -> anyhow::Result<ExitCode> {
    use codemap_mcp::reports;
    let projects = Project::resolve(cli, project)?;
    let mcp: Vec<codemap_mcp::Project> = projects.iter().map(Project::mcp).collect();
    if clear {
        for p in &mcp {
            reports::clear(p)?;
        }
        eprintln!("the wrong-answer reports are cleared");
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(id) = close {
        let mut done = false;
        for p in &mcp {
            done |= reports::close(p, id)?;
        }
        anyhow::ensure!(done, "no report {id}");
        eprintln!("closed {id}");
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(id) = send {
        let p = mcp
            .iter()
            .find(|p| reports::load(p).iter().any(|r| r.id == id))
            .with_context(|| format!("no report {id}"))?;
        if let Some(why) = reports::sending_off(p) {
            anyhow::bail!("sending is off: {why}");
        }
        // Checked on this build first: a fixed report is closed, not sent.
        let mut stores = Vec::new();
        for pr in &projects {
            let (store, _) = pr.refresh(false)?;
            stores.push(store);
        }
        let apps: Vec<(&Store, &codemap_mcp::Project)> = stores.iter().zip(&mcp).collect();
        let r = match reports::prepare_send(&apps, p, id) {
            Ok(r) => r,
            Err(why) => {
                println!("{why}");
                return Ok(ExitCode::SUCCESS);
            }
        };
        let (title, body) = reports::issue(p, &r)
            .context("only a report with a made-up example that reproduces can be sent")?;
        println!(
            "This goes to https://github.com/{}/issues (only the made-up example, never your code):\n\nTitle: {title}\n\n{body}",
            codemap_core::updates::REPO
        );
        print!("Send? y/n ");
        use std::io::Write as _;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") && !answer.trim().eq_ignore_ascii_case("yes") {
            println!("Not sent.");
            return Ok(ExitCode::SUCCESS);
        }
        match reports::send(p, &r, None) {
            Ok(sent) => {
                reports::set_issue(p, &r.id, &sent.url);
                if sent.comment {
                    println!(
                        "Added to the open issue {} as a comment (the same bug was already reported; no new issue).",
                        sent.url
                    );
                } else {
                    println!("Sent: {}", sent.url);
                }
            }
            Err(e) => println!("{e}"),
        }
        return Ok(ExitCode::SUCCESS);
    }
    let mut stores = Vec::new();
    for p in &projects {
        let (store, _) = p.refresh(false)?;
        stores.push(store);
    }
    let apps: Vec<(&Store, &codemap_mcp::Project)> = stores.iter().zip(&mcp).collect();
    if let Some(dir) = export {
        let mut n = 0;
        for (store, p) in &apps {
            n += reports::export(store, p, dir)?;
        }
        eprintln!("exported {n} open reports to {}", dir.display());
        return Ok(ExitCode::SUCCESS);
    }
    let mut rows = Vec::new();
    for (_, p) in &apps {
        for (r, note) in reports::recheck(&apps, p) {
            if all || r.status == "open" {
                rows.push((r, note));
            }
        }
    }
    rows.sort_by(|a, b| {
        (
            a.0.status != "open",
            &a.0.tool,
            &a.0.kind,
            std::cmp::Reverse(a.0.times),
        )
            .cmp(&(
                b.0.status != "open",
                &b.0.tool,
                &b.0.kind,
                std::cmp::Reverse(b.0.times),
            ))
    });
    if json {
        let list: Vec<serde_json::Value> = rows
            .iter()
            .map(|(r, note)| {
                let mut v = serde_json::to_value(r).unwrap_or_default();
                if let Some(n) = note {
                    v["note"] = serde_json::json!(n);
                }
                v
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(ExitCode::SUCCESS);
    }
    if rows.is_empty() {
        println!("No open wrong-answer reports.");
        return Ok(ExitCode::SUCCESS);
    }
    for (r, note) in &rows {
        let app = r
            .app
            .as_deref()
            .map(|a| format!("[{a}] "))
            .unwrap_or_default();
        let mut tags = vec![r.source.clone()];
        if let Some(n) = note {
            tags.push((*n).to_owned());
        }
        if let Some(st) = &r.example_status {
            // A closed report's example is history: say when it reproduced.
            if r.status == "closed" && st == "reproduces" {
                let on = r.reproduced_on.as_deref().unwrap_or(&r.codemap);
                tags.push(format!("example reproduced on {on}"));
            } else {
                tags.push(format!("example {st}"));
            }
        }
        if let Some(i) = &r.issue {
            tags.push(format!("sent {i}"));
        }
        if r.status == "closed" {
            tags.push(format!(
                "closed in {}",
                r.closed_in.as_deref().unwrap_or("?")
            ));
        }
        println!(
            "{} {app}{} · {} · `{}` ({}×, seen on {}) [{}]",
            r.id,
            r.tool,
            r.kind,
            r.item,
            r.times,
            r.codemap,
            tags.join(", ")
        );
        println!("    right: {}", r.right);
        if !r.evidence.is_empty() {
            println!("    evidence: {}", r.evidence.join(", "));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_serve(cli: &Cli, project: &Path) -> anyhow::Result<ExitCode> {
    let projects = Project::resolve(cli, project)?;
    // Once a day, in the background: is there a newer release?
    if let Ok(data_dir) = paths::resolve_data_dir(cli.data_dir.clone()) {
        update::clean_old_copy();
        update::check_in_background(data_dir);
    }
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
    if let Ok(data_dir) = paths::resolve_data_dir(cli.data_dir.clone()) {
        update::check_if_due(&data_dir, 5);
        if !json && let Some(n) = codemap_core::updates::notice(&data_dir) {
            println!("{n}\n");
        }
    }
    if !json {
        let mcp: Vec<codemap_mcp::Project> = projects.iter().map(Project::mcp).collect();
        let refs: Vec<&codemap_mcp::Project> = mcp.iter().collect();
        if let Some(line) = codemap_mcp::reports::summary_line(&refs) {
            println!("{line}\n");
        }
    }
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
    fn project_defaults_to_the_current_folder() {
        let cli = Cli::try_parse_from(["codemap", "scan"]).unwrap();
        assert!(
            matches!(cli.command, Command::Scan { ref project, .. } if project == &PathBuf::from("."))
        );
    }
}
