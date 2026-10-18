//! How codemap is used in a project: every tool call (which tool, the
//! arguments with code cut out, time taken, answer size, result), every index
//! run (why, full or not, files re-read, time taken) and every AI app session.
//! Kept in `usage.jsonl` next to each app's index for 90 days; it never
//! leaves this machine. `codemap usage` reads it together with the wrong-answer
//! reports and the AI mistakes log.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::Project;

const FILE: &str = "usage.jsonl";
/// Entries older than this are dropped.
pub const KEEP_DAYS: i64 = 90;
/// The log is trimmed to its newest half past this size.
const MAX_BYTES: u64 = 20 * 1024 * 1024;
/// A string argument longer than this is code or text, not a name: only its
/// length is kept.
const MAX_ARG: usize = 200;
/// Arguments that carry code or tool output, never names.
const CODE_ARGS: &[&str] = &[
    "snippet",
    "code",
    "source",
    "content",
    "type_errors",
    "diff",
];
/// A call repeated within this many seconds after a poor answer is a retry.
const RETRY_SECS: i64 = 120;
/// An answer slower than this is counted as slow.
const SLOW_MS: u64 = 5000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Entry {
    /// An AI app connected to the MCP server.
    Session {
        at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client: Option<String>,
        codemap: String,
    },
    /// One tool call.
    Call {
        at: String,
        tool: String,
        /// The arguments, with code and long text replaced by their length.
        args: Value,
        ms: u64,
        /// Size of the answer sent back.
        bytes: u64,
        /// `ok`, `not_found`, `invalid_args`, `error` or `crashed`.
        outcome: String,
        /// Answered from an index that was still being updated.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stale: bool,
        /// Files re-indexed just before answering.
        #[serde(default, skip_serializing_if = "is_zero")]
        reindexed: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// `mcp` (an AI app) or `cli` (`codemap query`).
        via: String,
    },
    /// One index run that read files.
    Index {
        at: String,
        /// `startup` (the server's first check), `call` (before a tool
        /// answered), `scan` (`codemap scan`), `cli` (another command) or
        /// `linked` (a frontend linked to this project read its routes).
        trigger: String,
        full: bool,
        ms: u64,
        walked: usize,
        reparsed: usize,
        removed: usize,
        #[serde(default = "yes", skip_serializing_if = "Clone::clone")]
        ok: bool,
    },
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn yes() -> bool {
    true
}

impl Entry {
    fn at(&self) -> &str {
        match self {
            Entry::Session { at, .. } | Entry::Call { at, .. } | Entry::Index { at, .. } => at,
        }
    }
}

pub fn log_path(project: &Project) -> PathBuf {
    log_path_for(&project.index_path)
}

/// The usage log next to an index file.
pub fn log_path_for(index: &Path) -> PathBuf {
    index.with_file_name(FILE)
}

/// Off with `CODEMAP_NO_USAGE_LOG=1`.
fn enabled() -> bool {
    !std::env::var("CODEMAP_NO_USAGE_LOG").is_ok_and(|v| v == "1")
}

/// Appends one entry; a failure is logged, never raised.
pub fn append(project: &Project, entry: &Entry) {
    append_at(&project.index_path, entry);
}

/// Appends one entry to the log next to `index`.
pub fn append_at(index: &Path, entry: &Entry) {
    if !enabled() {
        return;
    }
    let path = log_path_for(index);
    let write = || -> std::io::Result<()> {
        if path.metadata().is_ok_and(|m| m.len() > MAX_BYTES) {
            trim(&path, true)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
        writeln!(f, "{line}")
    };
    if let Err(e) = write() {
        tracing::debug!(error = %e, path = %path.display(), "usage log not written");
    }
}

/// Drops entries older than [`KEEP_DAYS`] (and, when `half`, the older half).
fn trim(path: &Path, half: bool) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let cutoff = codemap_core::time::days_ago_rfc3339(KEEP_DAYS);
    let mut keep: Vec<&str> = text
        .lines()
        .filter(|l| serde_json::from_str::<Entry>(l).is_ok_and(|e| e.at() >= cutoff.as_str()))
        .collect();
    if half {
        keep.drain(..keep.len() / 2);
    }
    let mut out = keep.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(path, out)
}

/// Drops entries older than 90 days; called when a server starts.
pub fn prune(project: &Project) {
    let path = log_path(project);
    if path.exists()
        && let Err(e) = trim(&path, false)
    {
        tracing::debug!(error = %e, "usage log not pruned");
    }
}

pub fn load(project: &Project) -> Vec<Entry> {
    std::fs::read_to_string(log_path(project))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn clear(project: &Project) -> std::io::Result<()> {
    match std::fs::remove_file(log_path(project)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The arguments as kept: names and options as given; code, tool output and
/// long text replaced by `<N chars>`.
pub fn redact(args: &Value) -> Value {
    fn walk(v: &Value, key: Option<&str>) -> Value {
        match v {
            Value::String(s)
                if key.is_some_and(|k| CODE_ARGS.contains(&k) || k.contains("snippet")) =>
            {
                json!(format!("<{} chars of code>", s.chars().count()))
            }
            Value::String(s) if s.chars().count() > MAX_ARG || s.contains('\n') => {
                json!(format!("<{} chars>", s.chars().count()))
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, key)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| (k.clone(), walk(x, Some(k.as_str()))))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    walk(args, None)
}

/// The outcome of an answer: `not_found` when it says something does not
/// exist, else `ok`.
pub fn outcome_of(data: &Value) -> &'static str {
    if data.get("found") == Some(&Value::Bool(false)) {
        "not_found"
    } else {
        "ok"
    }
}

/// Records one index run (only runs that read files, plus the first one).
pub fn record_index(
    project: &Project,
    trigger: &str,
    full: bool,
    started: std::time::Instant,
    report: Option<&crate::RefreshReport>,
) {
    record_index_at(&project.index_path, trigger, full, started, report);
}

/// The same, for the index file at `index` (a linked project's).
pub fn record_index_at(
    index: &Path,
    trigger: &str,
    full: bool,
    started: std::time::Instant,
    report: Option<&crate::RefreshReport>,
) {
    let (walked, reparsed, removed) = report
        .map(|r| (r.scan.walked, r.scan.reparsed, r.scan.removed))
        .unwrap_or_default();
    if report.is_some() && reparsed + removed == 0 && trigger != "startup" && !full {
        return;
    }
    append_at(
        index,
        &Entry::Index {
            at: codemap_core::time::now_rfc3339(),
            trigger: trigger.to_owned(),
            // Every file read (a first index) is a full run too.
            full: full || (walked > 0 && reparsed == walked),
            ms: started.elapsed().as_millis() as u64,
            walked,
            reparsed,
            removed,
            ok: report.is_some(),
        },
    );
}

// ---------------------------------------------------------------- report

#[derive(Default)]
struct ToolStats {
    calls: usize,
    ms: Vec<u64>,
    bytes: u64,
    outcomes: BTreeMap<String, usize>,
    stale: usize,
    slow: usize,
    retried: usize,
    reported_ai: usize,
    reported_auto: usize,
    misses: usize,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn seconds(at: &str) -> Option<i64> {
    at.parse::<jiff::Timestamp>().ok().map(|t| t.as_second())
}

/// The usage summary of one app over the last `days` days, as JSON.
pub fn summary(project: &Project, days: i64) -> Value {
    summary_in(project, days, &[])
}

/// The same, for one app of a workspace: `others` are the other apps, whose
/// calls count when the AI retried there (a call refused for a wrong app
/// name is kept in the first app's log).
pub fn summary_in(project: &Project, days: i64, others: &[Project]) -> Value {
    summary_with(project, &[], days, others)
}

/// Like [`summary_in`], also reading the logs of `also`: the same app
/// indexed another way (on its own, or as part of a workspace).
pub fn summary_with(project: &Project, also: &[Project], days: i64, others: &[Project]) -> Value {
    let since = codemap_core::time::days_ago_rfc3339(days);
    let mine = |p: &Project| {
        std::iter::once(p)
            .chain(also)
            .any(|x| x.index_path == p.index_path)
    };
    let other_calls: Vec<Entry> = others
        .iter()
        .filter(|o| !mine(o) && o.index_path != project.index_path)
        .flat_map(load)
        .filter(|e| matches!(e, Entry::Call { .. }) && e.at() >= since.as_str())
        .collect();
    let logs: Vec<&Project> = std::iter::once(project).chain(also).collect();
    let mut entries: Vec<Entry> = logs
        .iter()
        .flat_map(|p| load(p))
        .filter(|e| e.at() >= since.as_str())
        .collect();
    entries.sort_by(|a, b| a.at().cmp(b.at()));
    let all_reports: Vec<crate::reports::Report> = logs
        .iter()
        .flat_map(|p| crate::reports::load(p))
        .filter(|r| r.at.as_str() >= since.as_str())
        .collect();
    // A slow answer is not a wrong one: counted on its own.
    let slow_reports = all_reports.iter().filter(|r| r.kind == "slow").count();
    let reports: Vec<crate::reports::Report> = all_reports
        .into_iter()
        .filter(|r| r.kind != "slow")
        .collect();
    let misses: Vec<crate::misses::Miss> = logs
        .iter()
        .flat_map(|p| crate::misses::load(p))
        .filter(|m| m.at.as_str() >= since.as_str())
        .collect();

    let mut tools: BTreeMap<String, ToolStats> = BTreeMap::new();
    let mut by_day: BTreeMap<String, usize> = BTreeMap::new();
    let mut clients: BTreeMap<String, usize> = BTreeMap::new();
    let mut sessions = 0usize;
    let mut cli_calls = 0usize;
    let mut first: Option<&str> = None;
    let mut last: Option<&str> = None;
    // Calls in time order, for retries: (seconds, tool, args, poor answer, this app's).
    let mut seq: Vec<(i64, &str, &Value, bool, bool)> = Vec::new();
    for e in &other_calls {
        if let Entry::Call { at, tool, args, .. } = e
            && let Some(s) = seconds(at)
        {
            seq.push((s, tool, args, false, false));
        }
    }
    let mut index_runs: Vec<&Entry> = Vec::new();
    for e in &entries {
        first = Some(first.map_or(e.at(), |f| f.min(e.at())));
        last = Some(last.map_or(e.at(), |l| l.max(e.at())));
        match e {
            Entry::Session { client, .. } => {
                sessions += 1;
                *clients
                    .entry(client.clone().unwrap_or_else(|| "unknown".into()))
                    .or_default() += 1;
            }
            Entry::Call {
                at,
                tool,
                args,
                ms,
                bytes,
                outcome,
                stale,
                via,
                ..
            } => {
                let t = tools.entry(tool.clone()).or_default();
                t.calls += 1;
                t.ms.push(*ms);
                t.bytes += bytes;
                *t.outcomes.entry(outcome.clone()).or_default() += 1;
                t.stale += usize::from(*stale);
                t.slow += usize::from(*ms > SLOW_MS);
                *by_day
                    .entry(at.get(..10).unwrap_or(at).to_owned())
                    .or_default() += 1;
                if via == "cli" {
                    cli_calls += 1;
                }
                if let Some(s) = seconds(at) {
                    seq.push((s, tool, args, outcome != "ok", true));
                }
            }
            Entry::Index { .. } => index_runs.push(e),
        }
    }
    // A poor answer (not found, error, refused arguments) followed by the same
    // tool with other arguments soon after: the AI had to try again.
    seq.sort_by_key(|(s, ..)| *s);
    for (i, (s, tool, args, poor, own)) in seq.iter().enumerate() {
        if !poor || !own {
            continue;
        }
        // Times are whole seconds: a call in the same second may sort first.
        if seq.iter().enumerate().any(|(j, (s2, t2, a2, _, _))| {
            j != i && *s2 >= *s && s2 - s <= RETRY_SECS && t2 == tool && a2 != args
        }) && let Some(t) = tools.get_mut(*tool)
        {
            t.retried += 1;
        }
    }
    for r in &reports {
        let t = tools.entry(r.tool.clone()).or_default();
        if r.source == "ai" {
            t.reported_ai += 1;
        } else {
            t.reported_auto += 1;
        }
    }
    for m in &misses {
        tools.entry(m.tool.clone()).or_default().misses += 1;
    }

    let mut all_ms: Vec<u64> = tools.values().flat_map(|t| t.ms.iter().copied()).collect();
    all_ms.sort_unstable();
    let total_calls: usize = tools.values().map(|t| t.calls).sum();
    let tool_rows: Vec<Value> = tools
        .iter_mut()
        .map(|(name, t)| {
            t.ms.sort_unstable();
            let n = t.calls.max(1) as f64;
            let count = |k: &str| t.outcomes.get(k).copied().unwrap_or(0);
            let poor = count("not_found") + count("error") + count("crashed") + count("invalid_args");
            json!({
                "tool": name,
                "calls": t.calls,
                "avg_ms": if t.calls > 0 { (t.ms.iter().sum::<u64>() as f64 / n).round() as u64 } else { 0 },
                "p95_ms": percentile(&t.ms, 0.95),
                "max_ms": t.ms.last().copied().unwrap_or(0),
                "avg_kb": if t.calls > 0 { ((t.bytes as f64 / n) / 1024.0 * 10.0).round() / 10.0 } else { 0.0 },
                "ok": count("ok"),
                "not_found": count("not_found"),
                "invalid_args": count("invalid_args"),
                "errors": count("error") + count("crashed"),
                "stale": t.stale,
                "slow": t.slow,
                "retried": t.retried,
                "poor_answer_rate": if t.calls > 0 { ((poor as f64 / n) * 1000.0).round() / 10.0 } else { 0.0 },
                "wrong_answers_reported_by_ai": t.reported_ai,
                "wrong_answers_found_by_codemap": t.reported_auto,
                "names_ai_asked_that_do_not_exist": t.misses,
            })
        })
        .filter(|v| v["calls"].as_u64() > Some(0) || v["wrong_answers_reported_by_ai"].as_u64() > Some(0) || v["names_ai_asked_that_do_not_exist"].as_u64() > Some(0))
        .collect();

    // Index runs.
    let mut idx_ms: Vec<u64> = Vec::new();
    let mut triggers: BTreeMap<String, usize> = BTreeMap::new();
    let (mut full_runs, mut failed, mut files) = (0usize, 0usize, 0usize);
    let mut slowest: Option<&Entry> = None;
    let mut last_full: Option<&Entry> = None;
    for e in &index_runs {
        if let Entry::Index {
            trigger,
            full,
            ms,
            reparsed,
            removed,
            ok,
            ..
        } = e
        {
            idx_ms.push(*ms);
            *triggers.entry(trigger.clone()).or_default() += 1;
            full_runs += usize::from(*full);
            failed += usize::from(!ok);
            files += reparsed + removed;
            if slowest.is_none_or(|s| matches!(s, Entry::Index { ms: m, .. } if ms > m)) {
                slowest = Some(e);
            }
            if *full {
                last_full = Some(e);
            }
        }
    }
    idx_ms.sort_unstable();
    let index_line = |e: Option<&&Entry>| -> Value {
        match e {
            Some(Entry::Index {
                at,
                trigger,
                full,
                ms,
                walked,
                reparsed,
                ..
            }) => {
                json!({ "at": at, "trigger": trigger, "full": full, "ms": ms, "files_walked": walked, "files_read": reparsed })
            }
            _ => Value::Null,
        }
    };

    let open: Vec<Value> = {
        let mut latest: BTreeMap<&str, &crate::reports::Report> = BTreeMap::new();
        for r in &reports {
            latest.insert(r.id.as_str(), r);
        }
        latest
            .values()
            .filter(|r| r.status != "closed")
            .map(|r| json!({ "id": r.id, "tool": r.tool, "kind": r.kind, "item": r.item, "source": r.source, "at": r.at }))
            .collect()
    };
    let closed = {
        let mut ids: BTreeMap<&str, &str> = BTreeMap::new();
        for r in &reports {
            ids.insert(r.id.as_str(), r.status.as_str());
        }
        ids.values().filter(|s| **s == "closed").count()
    };
    let distinct_reports = open.len() + closed;

    let mut top_misses: BTreeMap<String, usize> = BTreeMap::new();
    for m in &misses {
        *top_misses
            .entry(format!("{} {}", m.kind, m.item))
            .or_default() += 1;
    }
    let mut top_misses: Vec<(String, usize)> = top_misses.into_iter().collect();
    top_misses.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top_misses.truncate(10);

    json!({
        "app": project.app,
        "period": { "days": days, "from": first, "to": last },
        "sessions": { "count": sessions, "clients": clients },
        "calls": {
            "total": total_calls,
            "from_cli": cli_calls,
            "avg_ms": if all_ms.is_empty() { 0 } else { (all_ms.iter().sum::<u64>() as f64 / all_ms.len() as f64).round() as u64 },
            "p95_ms": percentile(&all_ms, 0.95),
            "per_day": by_day,
        },
        "tools": tool_rows,
        "accuracy": {
            "note": "codemap cannot see whether an answer was right; these are the signals it has: wrong answers the AI reported (report_wrong_answer) or codemap caught itself (a contradiction, a failure, a slow answer), answers that found nothing, refused arguments, and calls the AI had to repeat with other arguments within 2 minutes.",
            "wrong_answers": { "total": distinct_reports, "open": open.len(), "closed": closed },
            "slow_answers_reported": slow_reports,
            "open_reports": open,
            "names_ai_asked_that_do_not_exist": misses.len(),
            "top_missing_names": top_misses.iter().map(|(k, n)| json!({ "name": k, "times": n })).collect::<Vec<_>>(),
        },
        "indexing": {
            "runs": index_runs.len(),
            "full_runs": full_runs,
            "failed_runs": failed,
            "by_trigger": triggers,
            "files_read": files,
            "total_ms": idx_ms.iter().sum::<u64>(),
            "avg_ms": if idx_ms.is_empty() { 0 } else { (idx_ms.iter().sum::<u64>() as f64 / idx_ms.len() as f64).round() as u64 },
            "p95_ms": percentile(&idx_ms, 0.95),
            "slowest": index_line(slowest.as_ref()),
            "last_full": index_line(last_full.as_ref()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_and_long_text_are_cut_from_arguments() {
        let args = json!({
            "query": "OrderService",
            "php_snippet": "<?php echo 1;",
            "claims": { "columns": ["orders.total"] },
            "note": "a\nb",
            "long": "x".repeat(300),
        });
        let r = redact(&args);
        assert_eq!(r["query"], "OrderService");
        assert_eq!(r["php_snippet"], "<13 chars of code>");
        assert_eq!(r["claims"]["columns"][0], "orders.total");
        assert_eq!(r["note"], "<3 chars>");
        assert_eq!(r["long"], "<300 chars>");
    }

    #[test]
    fn summary_counts_outcomes_retries_and_index_runs() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
            config: codemap_core::Config::default(),
            id: "t".into(),
            index_path: dir.path().join("index.db"),
            app: None,
        };
        let now = jiff::Timestamp::now().as_second();
        let at = |s: i64| {
            jiff::Timestamp::from_second(now - 600 + s)
                .unwrap()
                .to_string()
        };
        let call = |s: i64, tool: &str, q: &str, outcome: &str, ms: u64| Entry::Call {
            at: at(s),
            tool: tool.into(),
            args: json!({ "query": q }),
            ms,
            bytes: 2048,
            outcome: outcome.into(),
            stale: false,
            reindexed: 0,
            call_id: None,
            via: "mcp".into(),
        };
        append(
            &project,
            &Entry::Session {
                at: at(0),
                client: Some("claude-code 2.1".into()),
                codemap: "x".into(),
            },
        );
        append(&project, &call(1, "search_symbol", "Ordr", "not_found", 10));
        append(&project, &call(20, "search_symbol", "Order", "ok", 30));
        append(&project, &call(30, "get_schema", "orders", "ok", 50));
        append(
            &project,
            &call(40, "get_schema", "orders", "invalid_args", 5),
        );
        // An old entry is ignored (and pruned).
        let mut old = call(0, "verify", "x", "ok", 1);
        if let Entry::Call { at, .. } = &mut old {
            *at = jiff::Timestamp::from_second(now - 100 * 86_400)
                .unwrap()
                .to_string();
        }
        append(&project, &old);
        append(
            &project,
            &Entry::Index {
                at: at(0),
                trigger: "startup".into(),
                full: false,
                ms: 900,
                walked: 50,
                reparsed: 50,
                removed: 0,
                ok: true,
            },
        );
        append(
            &project,
            &Entry::Index {
                at: at(25),
                trigger: "call".into(),
                full: false,
                ms: 40,
                walked: 50,
                reparsed: 1,
                removed: 0,
                ok: true,
            },
        );
        let u = summary(&project, 90);
        assert_eq!(u["calls"]["total"], 4, "{u}");
        assert_eq!(u["sessions"]["clients"]["claude-code 2.1"], 1);
        let tool = |n: &str| {
            u["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["tool"] == n)
                .cloned()
                .unwrap()
        };
        let s = tool("search_symbol");
        assert_eq!(s["not_found"], 1);
        assert_eq!(s["retried"], 1, "{s}");
        // The refused call was not followed by different arguments: no retry.
        assert_eq!(tool("get_schema")["invalid_args"], 1);
        assert_eq!(tool("get_schema")["retried"], 0);
        assert_eq!(u["indexing"]["runs"], 2);
        assert_eq!(u["indexing"]["slowest"]["ms"], 900);
        assert_eq!(u["indexing"]["files_read"], 51);
        prune(&project);
        assert!(
            !std::fs::read_to_string(log_path(&project))
                .unwrap()
                .contains("\"verify\"")
        );
    }
}
