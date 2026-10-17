//! `codemap doctor`: how the project was understood, and what could not be.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use codemap_core::store::{IndexStats, WarningRow, meta_keys};
use codemap_core::symbols::{MemberLookup, find_member};
use codemap_core::{Config, Result, Store, git};
use serde::Serialize;

use crate::detect::Detection;
use crate::refresh::{stored_detection, stored_routes_state};
use crate::routes::{self, ActionKind, RouteSource, artisan};

#[derive(Debug, Clone, Serialize)]
pub struct GitStatus {
    pub available: bool,
    pub commit: Option<String>,
    pub dirty: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtisanStatus {
    pub mode: String,
    pub decision: String,
    pub php_binary: Option<String>,
    pub php_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteIssue {
    pub location: Option<String>,
    pub route: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteSummary {
    pub total: usize,
    pub by_source: BTreeMap<String, usize>,
    pub produced_by: Option<String>,
    pub sources: Vec<String>,
    pub files: Vec<String>,
    pub unresolved: Vec<RouteIssue>,
    /// Controller class or method that does not exist in project code.
    pub missing_code: Vec<RouteIssue>,
    /// Methods possibly inherited from vendor classes (not indexed).
    pub unverifiable: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub project: String,
    pub index_path: Option<String>,
    pub indexed_at: Option<String>,
    pub git: GitStatus,
    pub detection: Option<Detection>,
    pub artisan: ArtisanStatus,
    pub routes: RouteSummary,
    pub stats: IndexStats,
    pub warnings: Vec<WarningRow>,
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn php_version(bin: &Path) -> Option<String> {
    let out = Command::new(bin)
        .arg("-v")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(str::to_owned)
}

pub fn build(store: &Store, root: &Path, config: &Config, project: &str) -> Result<DoctorReport> {
    let info = git::info(root);
    let detection = stored_detection(store)?;
    let state = stored_routes_state(store)?;
    let php = artisan::php_binary();
    let decision = match artisan::decide(&config.laravel, root) {
        artisan::Decision::Skip(why) => why.to_owned(),
        artisan::Decision::Run => {
            "runs `route:list --json` (boots the app: executes project PHP code)".to_owned()
        }
    };

    let all = routes::all(store.conn())?;
    let mut by_source = BTreeMap::new();
    let mut unresolved = Vec::new();
    let mut missing_code = Vec::new();
    let mut unverifiable = 0;
    let project_namespaces: Vec<String> = detection
        .iter()
        .flat_map(|d| d.psr4.iter())
        .map(|r| format!("{}\\", r.namespace))
        .collect();
    for r in &all {
        let source = match r.source {
            RouteSource::Static => "static",
            RouteSource::Artisan => "artisan",
            RouteSource::Next => "next",
            RouteSource::Package => "package",
        };
        *by_source.entry(source.to_owned()).or_insert(0) += 1;
        let label = format!("{} {}", r.methods_str(), r.uri);
        if let Some(reason) = &r.unresolved_reason {
            unresolved.push(RouteIssue {
                location: r.location(),
                route: label.clone(),
                detail: reason.clone(),
            });
        }
        if r.action_kind != ActionKind::Controller {
            continue;
        }
        let (Some(class), Some(method)) = (&r.controller_fqn, &r.controller_method) else {
            continue;
        };
        let issue = match find_member(store.conn(), class, method)? {
            MemberLookup::Found(_) => None,
            MemberLookup::ClassMissing
                if project_namespaces
                    .iter()
                    .any(|ns| class.starts_with(ns.as_str())) =>
            {
                Some(format!("controller {class} is not in the project"))
            }
            MemberLookup::ClassMissing | MemberLookup::Unverifiable(_) => {
                unverifiable += 1;
                None
            }
            MemberLookup::MemberMissing => Some(format!("{class} has no method {method}()")),
        };
        if let Some(detail) = issue {
            missing_code.push(RouteIssue {
                location: r.location(),
                route: label,
                detail,
            });
        }
    }

    Ok(DoctorReport {
        project: project.to_owned(),
        index_path: store.path().map(|p| p.display().to_string()),
        indexed_at: store.get_meta(meta_keys::INDEXED_AT)?,
        git: GitStatus {
            available: git_available(),
            commit: info.commit,
            dirty: info.dirty,
        },
        detection,
        artisan: ArtisanStatus {
            mode: serde_json::to_value(config.laravel.artisan)?
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            decision,
            php_version: php.as_deref().and_then(php_version),
            php_binary: php.map(|p| p.display().to_string()),
        },
        routes: RouteSummary {
            total: all.len(),
            by_source,
            produced_by: state.as_ref().map(|s| s.route_source.clone()),
            sources: state
                .as_ref()
                .map(|s| s.sources.clone())
                .unwrap_or_default(),
            files: state.map(|s| s.files).unwrap_or_default(),
            unresolved,
            missing_code,
            unverifiable,
        },
        stats: store.stats()?,
        warnings: store.warnings()?,
    })
}

pub fn render_text(r: &DoctorReport) -> String {
    let mut s = String::new();
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    let _ = writeln!(s, "project      {}", r.project);
    if let Some(p) = &r.index_path {
        let _ = writeln!(s, "index        {p}");
    }
    let _ = writeln!(
        s,
        "indexed at   {}",
        r.indexed_at.as_deref().unwrap_or("never")
    );
    let _ = writeln!(
        s,
        "git          available: {}, commit: {}{}",
        yes_no(r.git.available),
        r.git
            .commit
            .as_deref()
            .map_or("n/a", |c| &c[..c.len().min(12)]),
        if r.git.dirty == Some(true) {
            " (dirty)"
        } else {
            ""
        }
    );

    match &r.detection {
        Some(d) if d.laravel => {
            let version = d.version.as_deref().unwrap_or("?");
            let _ = writeln!(
                s,
                "laravel      {version} (from {}{})",
                d.version_source.as_deref().unwrap_or("?"),
                if d.version_inferred { ", inferred" } else { "" }
            );
            let layout = serde_json::to_value(d.layout).ok();
            let _ = writeln!(
                s,
                "layout       {}",
                layout
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
            );
            for reason in &d.reasons {
                let _ = writeln!(s, "  - {reason}");
            }
            let _ = writeln!(
                s,
                "php          {}",
                d.php_constraint.as_deref().unwrap_or("not constrained")
            );
            if !d.psr4.is_empty() {
                let _ = writeln!(s, "psr-4");
                for root in &d.psr4 {
                    let _ = writeln!(
                        s,
                        "  {}\\ -> {}{}",
                        root.namespace,
                        root.path,
                        if root.dev { " (dev)" } else { "" }
                    );
                }
            }
        }
        Some(d) => {
            let _ = writeln!(s, "laravel      not detected");
            for reason in &d.reasons {
                let _ = writeln!(s, "  - {reason}");
            }
        }
        None => {
            let _ = writeln!(s, "laravel      not analysed yet (run `codemap scan`)");
        }
    }

    let _ = writeln!(
        s,
        "artisan      mode {}: {}; php binary: {}",
        r.artisan.mode,
        r.artisan.decision,
        r.artisan
            .php_version
            .as_deref()
            .or(r.artisan.php_binary.as_deref())
            .unwrap_or("not found")
    );

    let rt = &r.routes;
    let sources: Vec<String> = rt
        .by_source
        .iter()
        .map(|(k, v)| format!("{v} {k}"))
        .collect();
    let _ = writeln!(
        s,
        "routes       {} ({}){}",
        rt.total,
        if sources.is_empty() {
            "none".to_owned()
        } else {
            sources.join(", ")
        },
        rt.produced_by
            .as_deref()
            .map(|p| format!("; produced by {p}"))
            .unwrap_or_default()
    );
    for source in &rt.sources {
        let _ = writeln!(s, "  from {source}");
    }
    if !rt.files.is_empty() {
        let _ = writeln!(s, "  files read: {}", rt.files.join(", "));
    }
    let _ = writeln!(s, "unresolved   {}", rt.unresolved.len());
    for i in &rt.unresolved {
        let _ = writeln!(
            s,
            "  {} {}: {}",
            i.location.as_deref().unwrap_or("-"),
            i.route,
            i.detail
        );
    }
    let _ = writeln!(
        s,
        "missing code {}{}",
        rt.missing_code.len(),
        if rt.unverifiable > 0 {
            format!(
                " ({} more not verifiable: controller or method inherited from vendor)",
                rt.unverifiable
            )
        } else {
            String::new()
        }
    );
    for i in &rt.missing_code {
        let _ = writeln!(
            s,
            "  {} {}: {}",
            i.location.as_deref().unwrap_or("-"),
            i.route,
            i.detail
        );
    }

    let st = &r.stats;
    let symbols: u64 = st.symbols_by_kind.values().sum();
    let _ = writeln!(
        s,
        "index        {} files, {} symbols",
        st.files_total, symbols
    );
    let _ = writeln!(s, "warnings     {}", r.warnings.len());
    // Group identical messages per file: "path: message (lines 3, 9, 12)".
    let mut grouped: BTreeMap<(&str, &str, &str), Vec<Option<u32>>> = BTreeMap::new();
    for w in &r.warnings {
        grouped
            .entry((
                w.adapter.as_str(),
                w.path.as_deref().unwrap_or("-"),
                w.message.as_str(),
            ))
            .or_default()
            .push(w.line);
    }
    let mut current = "";
    for ((adapter, path, message), lines) in &grouped {
        if *adapter != current {
            let _ = writeln!(s, "  [{adapter}]");
            current = adapter;
        }
        let lines: Vec<String> = lines.iter().flatten().map(u32::to_string).collect();
        let at = match lines.len() {
            0 => (*path).to_owned(),
            1 => format!("{path}:{}", lines[0]),
            n => format!("{path} ({n}×, lines {})", lines.join(", ")),
        };
        let _ = writeln!(s, "  {at}: {message}");
    }
    s
}
