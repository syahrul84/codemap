//! Wrong-answer reports: when a codemap answer turns out to be wrong, the AI
//! reports it (`report_wrong_answer`, pointing at the answer by its
//! `call_id`), and some wrong answers are recorded on their own (two answers
//! that contradict each other with no file changed, a tool failure, a very
//! slow answer). Reports stay in `reports.jsonl` next to each app's index.
//! `codemap reports` lists them, re-checks them and exports them as test
//! cases. A report with a proved made-up example can be sent to the codemap
//! repository as a GitHub issue, only after the person agrees; the person's
//! own code never leaves the machine.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use codemap_core::store::Store;
use codemap_core::updates::{REPO, VERSION};
use codemap_core::{Config, ScanOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::{Freshness, Project};

const FILE: &str = "reports.jsonl";
const SETTINGS: &str = "report-send.json";
const MAX_BYTES: usize = 2 * 1024 * 1024;

pub const KINDS: &[&str] = &[
    "said_missing",
    "said_exists",
    "wrong_detail",
    "left_out",
    "false_problem",
    "wrong_command",
    "cannot_answer",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub id: String,
    pub at: String,
    /// `ai` or `auto`.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub tool: String,
    pub args: Value,
    pub kind: String,
    pub item: String,
    /// codemap's own answer, as it was sent.
    pub answer: Value,
    pub right: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
    /// `reproduces`, `does not reproduce`, `blocked` or `invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub example_hits: Vec<String>,
    /// codemap's answer on the made-up example.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example_answer: Option<Value>,
    /// The build the example last reproduced on (checked, not assumed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproduced_on: Option<String>,
    pub codemap: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    #[serde(default)]
    pub git_dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
    #[serde(default)]
    pub stale: bool,
    /// Content hashes of the cited files, to tell when they changed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
    pub times: u32,
    /// `open` or `closed`.
    pub status: String,
    /// The answer reported last (its `call_id`): a resend for the same answer
    /// updates the report instead of counting as a repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_call: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_in: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

fn path(project: &Project) -> PathBuf {
    project.index_path.with_file_name(FILE)
}

pub fn load(project: &Project) -> Vec<Report> {
    std::fs::read_to_string(path(project))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Writes all reports, dropping the oldest closed (then open) ones past 2 MB.
pub fn save_all(project: &Project, reports: &[Report]) -> std::io::Result<()> {
    let mut list: Vec<&Report> = reports.iter().collect();
    let size = |l: &[&Report]| -> usize {
        l.iter()
            .map(|r| serde_json::to_string(r).map_or(0, |s| s.len() + 1))
            .sum()
    };
    while size(&list) > MAX_BYTES && !list.is_empty() {
        let i = list.iter().position(|r| r.status == "closed").unwrap_or(0);
        list.remove(i);
    }
    let text: String = list
        .iter()
        .filter_map(|r| serde_json::to_string(r).ok())
        .map(|l| l + "\n")
        .collect();
    std::fs::write(path(project), text)
}

fn short_hash(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex()[..6].to_owned()
}

/// Same tool, arguments, kind and item: the same report.
pub fn id_for(tool: &str, args: &Value, kind: &str, item: &str) -> String {
    format!("r_{}", short_hash(&format!("{tool}|{args}|{kind}|{item}")))
}

fn file_hashes(root: &Path, evidence: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for e in evidence {
        let file = e.split(':').next().unwrap_or(e).trim();
        if file.is_empty() || file.starts_with(".env") || file.contains("..") {
            continue;
        }
        if let Ok(bytes) = std::fs::read(root.join(file)) {
            out.insert(
                file.to_owned(),
                blake3::hash(&bytes).to_hex()[..12].to_owned(),
            );
        }
    }
    out
}

/// Saves a report, or raises `times` on the one already there.
/// Returns `(report, duplicate)`.
pub fn add(project: &Project, mut r: Report) -> std::io::Result<(Report, bool)> {
    let mut all = load(project);
    r.evidence = r
        .evidence
        .into_iter()
        .take(20)
        .map(|e| e.chars().take(500).collect())
        .collect();
    r.files = file_hashes(&project.root, &r.evidence);
    if let Some(old) = all.iter_mut().find(|o| o.id == r.id) {
        let resend = r.last_call.is_some() && old.last_call == r.last_call;
        if !resend {
            old.times += 1;
        }
        old.at = r.at.clone();
        old.last_call = r.last_call.clone();
        // The newest report of the answer is the one to keep.
        old.right = r.right.clone();
        old.evidence = r.evidence.clone();
        old.files = r.files.clone();
        if old.status == "closed" {
            old.status = "open".into();
            old.closed_in = None;
        }
        // Newer details win (a better example, a check).
        if r.expect.is_some() {
            old.expect = r.expect.take();
        }
        if r.example.is_some() {
            old.example = r.example.take();
            old.example_status = r.example_status.take();
            old.example_hits = std::mem::take(&mut r.example_hits);
            old.example_answer = r.example_answer.take();
            old.reproduced_on = r.reproduced_on.take();
        }
        let out = old.clone();
        save_all(project, &all)?;
        return Ok((out, true));
    }
    r.times = 1;
    r.status = "open".into();
    r.codemap = VERSION.into();
    all.push(r.clone());
    save_all(project, &all)?;
    Ok((r, false))
}

/// The answer of `tool` on these apps (a workspace call).
fn ask(apps: &[(&Store, &Project)], tool: &str, args: Value) -> Option<Value> {
    crate::workspace::call(apps, tool, args, Freshness::default())
        .ok()
        .map(|e| e.data)
}

/// Does the answer now match what is right? `None`: no usable check.
///
/// `{ "verify": { "columns": ["orders.x"] }, "is": "exists" }` or
/// `{ "tool": "get_schema", "args": { … }, "contains": "x" | "lacks": "x" }`.
pub fn check(apps: &[(&Store, &Project)], expect: &Value, app: Option<&str>) -> Option<bool> {
    let with_app = |mut args: Value| {
        if let (Some(a), Some(o)) = (app, args.as_object_mut())
            && !o.contains_key("app")
            && apps.len() > 1
        {
            o.insert("app".into(), json!(a));
        }
        args
    };
    if let Some(claims) = expect.get("verify") {
        let is = expect["is"].as_str().unwrap_or("exists");
        let data = ask(apps, "verify", with_app(json!({ "claims": claims })))?;
        let results = data["results"].as_array()?;
        return Some(!results.is_empty() && results.iter().all(|r| r["status"] == is));
    }
    let tool = expect["tool"].as_str()?;
    if !crate::tools::TOOL_NAMES.contains(&tool) {
        return None;
    }
    let data = ask(
        apps,
        tool,
        with_app(expect.get("args").cloned().unwrap_or(json!({}))),
    )?;
    let text = data.to_string();
    if let Some(c) = expect["contains"].as_str() {
        return Some(text.contains(c));
    }
    if let Some(l) = expect["lacks"].as_str() {
        return Some(!text.contains(l));
    }
    None
}

/// Re-checks open reports: those whose check now passes close, with the
/// version that fixed them. Returns the reports with a `note` per report.
pub fn recheck(
    apps: &[(&Store, &Project)],
    project: &Project,
) -> Vec<(Report, Option<&'static str>)> {
    let mut all = load(project);
    let mut changed = false;
    let mut out = Vec::new();
    for r in &mut all {
        let mut note = None;
        if r.status == "open" {
            match r
                .expect
                .as_ref()
                .and_then(|e| check(apps, e, r.app.as_deref()))
            {
                Some(true) => {
                    r.status = "closed".into();
                    r.closed_in = Some(VERSION.into());
                    changed = true;
                }
                Some(false) => {}
                None => note = Some("no check"),
            }
            if r.status == "open"
                && r.files.iter().any(|(f, h)| {
                    std::fs::read(project.root.join(f))
                        .map(|b| blake3::hash(&b).to_hex()[..12] != **h)
                        .unwrap_or(true)
                })
            {
                note = Some("code changed");
            }
        }
        out.push((r.clone(), note));
    }
    if changed {
        let _ = save_all(project, &all);
    }
    out
}

pub fn close(project: &Project, id: &str) -> std::io::Result<bool> {
    let mut all = load(project);
    let Some(r) = all.iter_mut().find(|r| r.id == id) else {
        return Ok(false);
    };
    r.status = "closed".into();
    r.closed_in = Some(format!("{VERSION} (by hand)"));
    save_all(project, &all)?;
    Ok(true)
}

pub fn clear(project: &Project) -> std::io::Result<()> {
    match std::fs::remove_file(path(project)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub fn open_count(project: &Project) -> usize {
    load(project).iter().filter(|r| r.status == "open").count()
}

/// The line for `project_overview` and `codemap doctor`.
pub fn summary_line(projects: &[&Project]) -> Option<String> {
    let n: usize = projects.iter().map(|p| open_count(p)).sum();
    (n > 0).then(|| format!("{n} open wrong-answer reports: run `codemap reports`"))
}

/// `--export`: one folder per open report with the report, the cited lines
/// of indexed files and a suggested test name.
pub fn export(store: &Store, project: &Project, dir: &Path) -> std::io::Result<usize> {
    let indexed: HashSet<String> = store
        .list_files()
        .map(|f| f.into_iter().map(|f| f.path).collect())
        .unwrap_or_default();
    let mut n = 0;
    for r in load(project).into_iter().filter(|r| r.status == "open") {
        let folder = dir.join(&r.id);
        std::fs::create_dir_all(&folder)?;
        std::fs::write(
            folder.join("report.json"),
            serde_json::to_string_pretty(&r).unwrap_or_default(),
        )?;
        let mut snippets = String::new();
        for e in &r.evidence {
            let mut parts = e.splitn(3, ':');
            let (Some(file), line) = (parts.next(), parts.next()) else {
                continue;
            };
            let file = file.trim();
            // Indexed files only: never .env, vendor or storage.
            if !indexed.contains(file) || file.starts_with(".env") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(project.root.join(file)) else {
                continue;
            };
            let at: usize = line.and_then(|l| l.trim().parse().ok()).unwrap_or(1);
            let from = at.saturating_sub(10).max(1);
            snippets.push_str(&format!("== {file}:{at}\n"));
            for (i, l) in text.lines().enumerate().skip(from - 1).take(20) {
                snippets.push_str(&format!("{:>5}  {l}\n", i + 1));
            }
            snippets.push('\n');
        }
        if !snippets.is_empty() {
            std::fs::write(folder.join("snippets.txt"), snippets)?;
        }
        if let Some(ex) = &r.example {
            std::fs::write(
                folder.join("example.json"),
                serde_json::to_string_pretty(ex).unwrap_or_default(),
            )?;
        }
        let test = format!(
            "{}_{}_{}",
            r.tool,
            r.kind,
            r.item
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                })
                .collect::<String>()
        );
        std::fs::write(folder.join("suggested-test.txt"), format!("{test}\n"))?;
        n += 1;
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// Made-up examples

/// Words a made-up example may use even when the real project has them too.
const NEUTRAL: &[&str] = &[
    "post",
    "posts",
    "title",
    "body",
    "user",
    "users",
    "comment",
    "comments",
    "tag",
    "tags",
    "item",
    "items",
    "name",
    "email",
    "status",
    "slug",
    "content",
    "category",
    "categories",
    "product",
    "products",
    "price",
    "quantity",
    "description",
    "image",
    "author",
    "page",
    "pages",
    "home",
    "about",
    "settings",
    "profile",
    "account",
    "login",
    "logout",
    "register",
    "dashboard",
    "admin",
    "card",
    "list",
    "button",
    "header",
    "footer",
    "layout",
    "default",
    "test",
    "tests",
    "example",
    "demo",
    "order",
    "orders",
    "created_at",
    "updated_at",
    "deleted_at",
    "published_at",
    "user_id",
    "post_id",
    "app",
    "index",
    "show",
    "store",
    "update",
    "destroy",
    "create",
    "edit",
    "api",
    "data",
    "value",
    "label",
    "type",
    "count",
    "total",
    "note",
    "notes",
    "text",
    "date",
    "time",
    "active",
    "enabled",
    "visible",
    "public",
    "private",
    "model",
    "models",
    "controller",
    "request",
    "response",
    "resource",
    "migration",
    "schema",
    "table",
    "column",
    "route",
    "routes",
    "component",
    "components",
    "composable",
    "store",
    "stores",
    "props",
    "emit",
    "emits",
    "ref",
    "computed",
    "watch",
    "mounted",
    "setup",
    "template",
    "script",
    "style",
    "string",
    "number",
    "boolean",
    "array",
    "object",
    "null",
    "true",
    "false",
    "function",
    "return",
    "class",
    "extends",
    "public",
    "protected",
    "private",
    "static",
    "const",
    "export",
    "import",
    "from",
    "default",
    "async",
    "await",
    "laravel",
    "illuminate",
    "nuxt",
    "vue",
    "react",
    "next",
    "vite",
    "blueprint",
    "timestamps",
    "foreignid",
    "constrained",
    "nullable",
    "unique",
    "integer",
    "boolean",
    "decimal",
    "biginteger",
    "unsignedbiginteger",
    "longtext",
    "json",
    "jsonb",
    "uuid",
    "fillable",
    "hidden",
    "casts",
    "appends",
    "hasmany",
    "belongsto",
    "hasone",
    "belongstomany",
    "toarray",
    "jsonresource",
    "whenloaded",
    "definecomponent",
    "defineprops",
    "defineemits",
    "usefetch",
    "useasyncdata",
    "fetch",
    "axios",
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "this",
    "self",
    "parent",
    "void",
    "array",
    "mixed",
    "int",
    "bool",
    "float",
    "auth",
    "sanctum",
    "middleware",
    "prefix",
    "group",
    "apiresource",
    "resourcecollection",
    "migrations",
    "database",
    "factory",
    "factories",
    "seeder",
    "seeders",
    "config",
];

fn tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.len() >= 4 && !w.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_owned)
        .collect()
}

/// Names from the person's project that must not appear in an example.
fn real_names(apps: &[(&Store, &Project)]) -> HashSet<String> {
    let mut out = HashSet::new();
    for (store, _) in apps {
        let conn = store.conn();
        let mut add = |sql: &str| {
            if let Ok(mut stmt) = conn.prepare(sql)
                && let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0))
            {
                for n in rows.flatten() {
                    for t in tokens(&n) {
                        out.insert(t.to_lowercase());
                    }
                }
            }
        };
        add(
            "SELECT name FROM symbols WHERE kind IN ('class', 'interface', 'trait', 'enum', 'function')",
        );
        add("SELECT name FROM db_tables");
        add("SELECT DISTINCT name FROM db_columns");
        add("SELECT name FROM js_modules WHERE name IS NOT NULL");
        add("SELECT uri FROM routes");
        add("SELECT key FROM i18n_keys");
    }
    for n in NEUTRAL {
        out.remove(*n);
    }
    out
}

/// The project's own name (folder, composer.json, package.json, config):
/// a plain word that still identifies it.
fn brand_words(apps: &[(&Store, &Project)]) -> HashSet<String> {
    let mut out = HashSet::new();
    for (_, p) in apps {
        let mut add = |s: &str| {
            for t in tokens(s) {
                let t = t.to_lowercase();
                if !NEUTRAL.contains(&t.as_str()) && !COMMON_WORDS.contains(&t.as_str()) {
                    out.insert(t);
                }
            }
        };
        for dir in [Some(p.root.as_path()), p.root.parent()]
            .into_iter()
            .flatten()
        {
            if let Some(n) = dir.file_name() {
                let n = n.to_string_lossy();
                if n != "workspace" {
                    add(&n);
                }
            }
        }
        if let Some(n) = &p.config.project.name {
            add(n);
        }
        for f in ["composer.json", "package.json"] {
            let v: Value = std::fs::read_to_string(p.root.join(f))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            if let Some(n) = v["name"].as_str() {
                add(n);
            }
        }
    }
    out
}

/// Everyday words a project often also uses as a name; they say nothing
/// about the project on their own.
const COMMON_WORDS: &[&str] = &[
    "save",
    "saved",
    "saving",
    "error",
    "errors",
    "phone",
    "address",
    "message",
    "messages",
    "search",
    "filter",
    "filters",
    "result",
    "results",
    "submit",
    "cancel",
    "close",
    "open",
    "delete",
    "remove",
    "edit",
    "view",
    "views",
    "load",
    "loading",
    "loaded",
    "send",
    "sent",
    "check",
    "valid",
    "invalid",
    "required",
    "number",
    "code",
    "codes",
    "token",
    "session",
    "cache",
    "queue",
    "event",
    "events",
    "notification",
    "notifications",
    "mail",
    "upload",
    "download",
    "file",
    "files",
    "photo",
    "photos",
    "image",
    "images",
    "video",
    "link",
    "links",
    "menu",
    "modal",
    "dialog",
    "form",
    "field",
    "fields",
    "input",
    "option",
    "options",
    "select",
    "toggle",
    "badge",
    "banner",
    "banners",
    "icon",
    "icons",
    "logo",
    "theme",
    "color",
    "size",
    "small",
    "large",
    "first",
    "last",
    "next",
    "previous",
    "back",
    "start",
    "stop",
    "done",
    "pending",
    "paid",
    "failed",
    "success",
    "warning",
    "info",
    "member",
    "members",
    "customer",
    "customers",
    "merchant",
    "merchants",
    "shop",
    "shops",
    "cart",
    "carts",
    "checkout",
    "payment",
    "payments",
    "invoice",
    "voucher",
    "vouchers",
    "coupon",
    "coupons",
    "discount",
    "review",
    "reviews",
    "rating",
    "ratings",
    "booking",
    "bookings",
    "room",
    "rooms",
    "city",
    "country",
    "state",
    "location",
    "locations",
    "date",
    "dates",
    "today",
    "week",
    "month",
    "year",
    "hour",
    "minute",
    "time",
    "times",
    "price",
    "prices",
    "amount",
    "total",
    "subtotal",
    "balance",
    "credit",
    "wallet",
    "report",
    "reports",
    "stock",
    "brand",
    "brands",
    "category",
    "outlet",
    "outlets",
    "mall",
    "malls",
    "listing",
    "listings",
    "offer",
    "offers",
    "deal",
    "deals",
    "favourite",
    "favorite",
    "help",
    "support",
    "ticket",
    "tickets",
    "team",
    "role",
    "roles",
    "permission",
    "permissions",
];

/// A word that identifies the person's project: one of its own compound
/// names (`MerchantBusinessProfile`, `order_status`), or its brand. Plain
/// words (`Save`, `phone`, `error`) pass even when the project uses them.
fn is_project_name(token: &str, names: &HashSet<String>, brand: &HashSet<String>) -> bool {
    let lower = token.to_lowercase();
    if brand.contains(&lower) {
        return true;
    }
    if !names.contains(&lower) || COMMON_WORDS.contains(&lower.as_str()) {
        return false;
    }
    let humps = token
        .chars()
        .skip(1)
        .filter(|c| c.is_ascii_uppercase())
        .count();
    token.contains('_')
        || token.chars().any(|c| c.is_ascii_digit())
        || humps >= 1
        || token.len() >= 13
}

/// Long lines of the person's files (to catch copied code).
fn real_lines(apps: &[(&Store, &Project)]) -> HashSet<String> {
    let mut out = HashSet::new();
    for (store, project) in apps {
        for f in store.list_files().unwrap_or_default() {
            if let Ok(text) = std::fs::read_to_string(project.root.join(&f.path)) {
                for l in text.lines() {
                    let t = l.trim();
                    if t.len() >= 30 {
                        out.insert(t.to_owned());
                    }
                }
            }
        }
    }
    out
}

fn looks_secret(line: &str) -> Option<&'static str> {
    let l = line.to_lowercase();
    if l.contains("-----begin") {
        return Some("a private key");
    }
    for k in [
        "password",
        "secret",
        "token",
        "api_key",
        "apikey",
        "private_key",
        "access_key",
    ] {
        if let Some(i) = l.find(k) {
            let rest = l[i + k.len()..].trim_start_matches(['"', '\'', ' ', '_']);
            if let Some(v) = rest
                .strip_prefix(['=', ':'])
                .map(|v| v.trim().trim_matches(['"', '\'', ',', ';']))
            {
                let placeholder = v.is_empty()
                    || v.contains("env(")
                    || v.starts_with('$')
                    || v.contains("secret")
                    || v.contains("example")
                    || v.contains("xxx")
                    || v.contains("null")
                    || v.len() < 8;
                if !placeholder {
                    return Some("something that looks like a secret");
                }
            }
        }
    }
    for w in l.split(|c: char| c.is_whitespace() || "\"'(),;<>`".contains(c)) {
        if let Some((user, host)) = w.split_once('@')
            && !user.is_empty()
            && host.contains('.')
            && !host.ends_with("example.com")
            && !host.ends_with(".test")
            && !host.ends_with("example.org")
        {
            return Some("an email address");
        }
        let parts: Vec<&str> = w.split('.').collect();
        if parts.len() == 4
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
            && !w.starts_with("127.")
            && w != "0.0.0.0"
        {
            return Some("an IP address");
        }
        if let Some(rest) = w
            .strip_prefix("https://")
            .or_else(|| w.strip_prefix("http://"))
        {
            let host = rest.split(['/', ':', '?']).next().unwrap_or(rest);
            let public = [
                "example.com",
                "example.org",
                "localhost",
                "github.com",
                "laravel.com",
                "vuejs.org",
                "nuxt.com",
                "nextjs.org",
            ];
            if !public
                .iter()
                .any(|p| host == *p || host.ends_with(&format!(".{p}")))
                && !host.ends_with(".test")
            {
                return Some("a link to a host that may be private");
            }
        }
    }
    None
}

/// What an example check found.
#[derive(Debug, Clone, Default)]
pub struct Proof {
    pub status: String,
    pub hits: Vec<String>,
    pub answer: Option<Value>,
}

/// Checks a made-up example: no real names, copied lines or secrets; then
/// indexes it in a temporary folder and runs its call. It "reproduces" when
/// its `expect` does not hold there, i.e. codemap gives the same wrong
/// answer on the made-up code.
pub fn prove(apps: &[(&Store, &Project)], example: &Value) -> Proof {
    let invalid = |why: &str| Proof {
        status: "invalid".into(),
        hits: vec![why.to_owned()],
        answer: None,
    };
    let Some(files) = example["files"].as_object() else {
        return invalid("example.files must be an object of path → content");
    };
    if files.is_empty() || files.len() > 10 {
        return invalid("an example has 1 to 10 files");
    }
    let (Some(tool), Some(expect)) = (example["call"]["tool"].as_str(), example.get("expect"))
    else {
        return invalid("example needs call.tool and an expect check (what the right answer is)");
    };
    if !crate::tools::TOOL_NAMES.contains(&tool) {
        return invalid("example.call.tool must be one of codemap's tools");
    }

    // 1. Nothing from the real project.
    let names = real_names(apps);
    let brand = brand_words(apps);
    let lines = real_lines(apps);
    let mut hits = Vec::new();
    for (path, content) in files {
        let Some(text) = content.as_str() else {
            return invalid("every example file must be text");
        };
        if text.len() > 6000 || path.contains("..") || path.starts_with('/') {
            return invalid("example files must be short and inside the example folder");
        }
        for t in tokens(path).into_iter().chain(tokens(text)) {
            if is_project_name(&t, &names, &brand) {
                hits.push(format!(
                    "`{t}` is a name from this project ({path}); use a neutral one"
                ));
            }
        }
        for (i, l) in text.lines().enumerate() {
            let t = l.trim();
            if t.len() >= 30
                && lines.contains(t)
                && tokens(t).iter().any(|w| is_project_name(w, &names, &brand))
            {
                hits.push(format!("{path}:{} is copied from this project", i + 1));
            }
            if let Some(what) = looks_secret(t) {
                hits.push(format!("{path}:{} has {what}", i + 1));
            }
        }
    }
    hits.sort();
    hits.dedup();
    if !hits.is_empty() {
        return Proof {
            status: "blocked".into(),
            hits,
            answer: None,
        };
    }

    // 2. It runs, and codemap gets it wrong the same way.
    let base = std::env::temp_dir().join(format!(
        "codemap-example-{}-{}",
        std::process::id(),
        short_hash(&example.to_string())
    ));
    let root = base.join("project");
    let result = (|| -> Result<Proof, String> {
        for (path, content) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap_or(&root)).map_err(|e| e.to_string())?;
            std::fs::write(&p, content.as_str().unwrap_or_default()).map_err(|e| e.to_string())?;
        }
        let config = Config::load(&root).map_err(|e| e.to_string())?;
        let project = Project {
            root: root.clone(),
            config: config.clone(),
            id: "example".into(),
            index_path: base.join("index.db"),
            app: None,
        };
        let mut store = Store::open(&project.index_path).map_err(|e| e.to_string())?;
        crate::refresh(&mut store, &root, &config, ScanOptions::default())
            .map_err(|e| e.to_string())?;
        let args = example["call"].get("args").cloned().unwrap_or(json!({}));
        let answer = crate::tools::call(&store, &project, tool, args, Freshness::default())
            .map(|e| e.data)
            .map_err(|e| e.to_string())?;
        let here = [(&store, &project)];
        let status = match check(&here, expect, None) {
            Some(false) => "reproduces",
            Some(true) => "does not reproduce",
            None => "invalid",
        };
        Ok(Proof {
            status: status.into(),
            hits: if status == "invalid" {
                vec!["the example's expect could not be checked".into()]
            } else {
                Vec::new()
            },
            answer: Some(answer),
        })
    })();
    let _ = std::fs::remove_dir_all(&base);
    result.unwrap_or_else(|e| invalid(&format!("the example could not be indexed: {e}")))
}

// ---------------------------------------------------------------------------
// Sending to the codemap repository

/// Settings that stop sending: `[report] send = "never"`,
/// `CODEMAP_NO_REPORT_SEND`, or "don't ask again" for this project.
pub fn sending_off(project: &Project) -> Option<&'static str> {
    if std::env::var("CODEMAP_NO_REPORT_SEND")
        .is_ok_and(|v| !matches!(v.trim(), "" | "0" | "false"))
    {
        return Some("CODEMAP_NO_REPORT_SEND is set");
    }
    if project.config.report.send.as_deref() == Some("never") {
        return Some("[report] send = \"never\" in .codemap.toml");
    }
    if std::fs::read_to_string(project.index_path.with_file_name(SETTINGS))
        .is_ok_and(|t| t.contains("\"never\":true"))
    {
        return Some("the person chose not to be asked again for this project");
    }
    None
}

pub fn never_ask_again(project: &Project) {
    let _ = std::fs::write(
        project.index_path.with_file_name(SETTINGS),
        "{\"never\":true}",
    );
}

/// The pattern's fingerprint, to find repeats among issues.
pub fn fingerprint(r: &Report) -> String {
    let ex = r.example.as_ref();
    let mut files: Vec<&String> = ex
        .and_then(|e| e["files"].as_object())
        .map(|o| o.keys().collect())
        .unwrap_or_default();
    files.sort();
    format!(
        "{}/{}/{}",
        r.tool,
        r.kind,
        short_hash(&format!(
            "{}|{:?}",
            ex.map(|e| e["call"].to_string()).unwrap_or_default(),
            files
        ))[..4]
            .to_owned()
    )
}

fn project_type(project: &Project) -> String {
    let read = |f: &str| std::fs::read_to_string(project.root.join(f)).unwrap_or_default();
    let composer = read("composer.json");
    let package = read("package.json");
    let mut parts = Vec::new();
    if composer.contains("laravel/framework") {
        parts.push("Laravel");
    }
    for (k, n) in [
        ("\"nuxt\"", "Nuxt"),
        ("\"next\"", "Next.js"),
        ("\"vue\"", "Vue"),
        ("\"react\"", "React"),
    ] {
        if package.contains(k) {
            parts.push(n);
            break;
        }
    }
    if parts.is_empty() {
        "other".into()
    } else {
        parts.join(" + ")
    }
}

/// `(title, body)` of the issue: only the made-up example and its answer.
pub fn issue(project: &Project, r: &Report) -> Option<(String, String)> {
    let ex = r.example.as_ref()?;
    // Only an example checked on a build: that build is the one named.
    if r.example_status.as_deref() != Some("reproduces") {
        return None;
    }
    let checked_on = r.reproduced_on.as_deref()?;
    let wrong = ex["wrong"].as_str().unwrap_or("codemap's answer was wrong");
    let right = ex["right"].as_str().unwrap_or("");
    let mut body = format!(
        "**codemap {checked_on}** · {} {} · {}\n**Tool:** {} · **Kind:** {} · first seen on {}\n**Wrong:** {wrong}\n**Right:** {right}\n\n### Example (made up, reproduces on {checked_on})\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        project_type(project),
        r.tool,
        r.kind,
        r.codemap,
    );
    for (path, content) in ex["files"].as_object()? {
        let lang = path.rsplit('.').next().unwrap_or("");
        let lang = if path.ends_with(".blade.php") {
            "blade"
        } else {
            lang
        };
        body.push_str(&format!(
            "`{path}`\n```{lang}\n{}\n```\n",
            content.as_str().unwrap_or_default().trim_end()
        ));
    }
    let answer = r
        .example_answer
        .as_ref()
        .map(|a| {
            let s = a.to_string();
            if s.len() > 1500 {
                format!("{}…", &s[..s.floor_char_boundary(1500)])
            } else {
                s
            }
        })
        .unwrap_or_default();
    body.push_str(&format!(
        "**Call:** `{} {}`\n**Answer:** `{answer}`\n\n<!-- codemap-report: {} -->\n",
        ex["call"]["tool"].as_str().unwrap_or(&r.tool),
        ex["call"].get("args").cloned().unwrap_or(json!({})),
        fingerprint(r)
    ));
    let title = format!("{}: {} — {}", r.tool, r.kind.replace('_', " "), wrong);
    Some((title.chars().take(120).collect(), body))
}

fn gh(args: &[&str], input: Option<&str>) -> Option<String> {
    let mut cmd = Command::new("gh");
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::null());
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let mut child = cmd.spawn().ok()?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        use std::io::Write as _;
        let _ = stdin.write_all(text.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Files the issue with the person's own `gh` login (a comment on a matching
/// open issue when there is one); without `gh`, a filled-in new-issue link
/// for the person to submit. Call only after the person said yes.
/// Where a report went.
#[derive(Debug, Clone)]
pub struct Sent {
    pub url: String,
    /// Added to an open issue of the same pattern as a comment.
    pub comment: bool,
}

/// The comment for an issue that already shows the same pattern: a short
/// "seen again" with this build's answer, not the whole issue again.
fn sighting(r: &Report) -> String {
    let checked = r.reproduced_on.as_deref().unwrap_or(VERSION);
    let answer = r
        .example_answer
        .as_ref()
        .map(|a| {
            let s = a.to_string();
            if s.len() > 800 {
                format!("{}…", &s[..s.floor_char_boundary(800)])
            } else {
                s
            }
        })
        .unwrap_or_default();
    format!(
        "Seen again on **codemap {checked}** · {} {} (first seen on {}). Same made-up example; it still reproduces.\n\n**Answer:** `{answer}`\n\n<!-- codemap-report: {} -->\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        r.codemap,
        fingerprint(r)
    )
}

pub fn send(project: &Project, r: &Report, edited: Option<&str>) -> Result<Sent, String> {
    let (title, mut body) =
        issue(project, r).ok_or("only a report with a reproducing made-up example can be sent")?;
    if let Some(t) = edited {
        // Keep the fingerprint line, so repeats are still found.
        body = format!("{t}\n\n<!-- codemap-report: {} -->\n", fingerprint(r));
    }
    let fp = fingerprint(r);
    let gh_ok = gh(&["--version"], None).is_some();
    if gh_ok {
        let found = gh(
            &[
                "issue",
                "list",
                "--repo",
                REPO,
                "--state",
                "open",
                "--search",
                &format!("\"codemap-report: {fp}\" in:body"),
                "--json",
                "number,url",
                "--jq",
                ".[0].url",
            ],
            None,
        )
        .filter(|u| u.starts_with("https://"));
        if let Some(url) = found {
            let note = if edited.is_some() {
                body.clone()
            } else {
                sighting(r)
            };
            gh(&["issue", "comment", &url, "--body-file", "-"], Some(&note))
                .ok_or("posting the comment with gh failed")?;
            return Ok(Sent { url, comment: true });
        }
        return gh(
            &[
                "issue",
                "create",
                "--repo",
                REPO,
                "--title",
                &title,
                "--body-file",
                "-",
            ],
            Some(&body),
        )
        .filter(|u| u.starts_with("https://"))
        .map(|url| Sent {
            url,
            comment: false,
        })
        .ok_or_else(|| "creating the issue with gh failed (is `gh auth login` done?)".to_owned());
    }
    let mut link = format!(
        "https://github.com/{REPO}/issues/new?title={}&body={}",
        url_encode(&title),
        url_encode(&body)
    );
    if link.len() > 7500 {
        link = format!(
            "https://github.com/{REPO}/issues/new?title={}",
            url_encode(&title)
        );
    }
    Err(format!(
        "`gh` is not installed; open this link and press Submit: {link}"
    ))
}

/// Gets a report ready to send: never a closed one, and only when its check
/// still fails and its example still reproduces on this build, with this
/// build's fresh answer. Otherwise the report is closed as fixed and the
/// reason is returned.
pub fn prepare_send(
    apps: &[(&Store, &Project)],
    project: &Project,
    id: &str,
) -> Result<Report, String> {
    let mut all = load(project);
    let Some(r) = all.iter_mut().find(|r| r.id == id) else {
        return Err(format!("no report {id}"));
    };
    if r.status == "closed" {
        return Err(format!(
            "{id} is closed (fixed in {}): nothing to send",
            r.closed_in.as_deref().unwrap_or("an earlier build")
        ));
    }
    let close = |r: &mut Report, why: &str| {
        r.status = "closed".into();
        r.closed_in = Some(format!("{VERSION} ({why})"));
    };
    if r.expect
        .as_ref()
        .and_then(|e| check(apps, e, r.app.as_deref()))
        == Some(true)
    {
        close(r, "its check passes now");
        let msg =
            format!("{id} is fixed in {VERSION} (its check passes now): closed, nothing to send");
        let _ = save_all(project, &all);
        return Err(msg);
    }
    let Some(example) = r.example.clone() else {
        return Err("only a report with a made-up example can be sent".into());
    };
    let proof = prove(apps, &example);
    match proof.status.as_str() {
        "reproduces" => {
            r.example_status = Some(proof.status);
            r.example_answer = proof.answer;
            r.reproduced_on = Some(VERSION.into());
            let out = r.clone();
            save_all(project, &all).map_err(|e| e.to_string())?;
            Ok(out)
        }
        "does not reproduce" => {
            // Keep the earlier result as history; the status is today's.
            let was = r.reproduced_on.clone().unwrap_or_else(|| r.codemap.clone());
            r.example_status = Some(format!("does not reproduce (reproduced on {was})"));
            r.example_answer = proof.answer;
            close(r, "the example no longer reproduces");
            let msg = format!(
                "{id}'s example no longer gives the wrong answer on {VERSION}: closed as fixed, nothing to send"
            );
            let _ = save_all(project, &all);
            Err(msg)
        }
        other => Err(format!(
            "the example cannot be sent ({other}): {}",
            proof.hits.join("; ")
        )),
    }
}

pub fn set_issue(project: &Project, id: &str, url: &str) {
    let mut all = load(project);
    if let Some(r) = all.iter_mut().find(|r| r.id == id) {
        r.issue = Some(url.to_owned());
        let _ = save_all(project, &all);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_private_hosts() {
        assert!(looks_secret("DB_PASSWORD=hunter2hunter2").is_some());
        assert!(looks_secret("'password' => env('DB_PASSWORD'),").is_none());
        assert!(looks_secret("contact ali@company.my").is_some());
        assert!(looks_secret("mail me@example.com").is_none());
        assert!(looks_secret("server 10.2.3.4").is_some());
        assert!(looks_secret("http://localhost:8000/api").is_none());
        assert!(looks_secret("fetch('https://api.internal.corp/x')").is_some());
    }

    #[test]
    fn only_reports_that_still_reproduce_are_sent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app/Note.php"), "<?php class Note {}").unwrap();
        let project = Project {
            root: root.clone(),
            config: Config::default(),
            id: "t".into(),
            index_path: dir.path().join("idx/index.db"),
            app: None,
        };
        let mut store = Store::open(&project.index_path).unwrap();
        crate::refresh(&mut store, &root, &project.config, ScanOptions::default()).unwrap();
        let apps = [(&store, &project)];
        let example = |expect_text: &str| {
            json!({
                "files": { "app/Widget.php": "<?php class Widget { public function spin() {} }" },
                "call": { "tool": "search_symbol", "args": { "query": "Widget" } },
                "expect": { "tool": "search_symbol", "args": { "query": "Widget" }, "contains": expect_text },
                "wrong": "w", "right": "r"
            })
        };
        let report = |id: &str, ex: Value| Report {
            id: id.into(),
            tool: "search_symbol".into(),
            kind: "wrong_detail".into(),
            item: "x".into(),
            codemap: "0.0.9".into(),
            example: Some(ex),
            example_status: Some("reproduces".into()),
            reproduced_on: Some("0.0.9".into()),
            ..Default::default()
        };
        // Still wrong on this build: sent with this build and its answer.
        add(&project, report("r_still", example("NotThere"))).unwrap();
        let r = prepare_send(&apps, &project, "r_still").expect("still reproduces");
        assert_eq!(r.reproduced_on.as_deref(), Some(VERSION));
        let (_, body) = issue(&project, &r).unwrap();
        assert!(
            body.contains(&format!("reproduces on {VERSION}")) && body.contains("first seen on"),
            "{body}"
        );
        // Fixed on this build: closed, not sent; and a closed report never is.
        add(&project, report("r_fixed", example("Widget"))).unwrap();
        let err = prepare_send(&apps, &project, "r_fixed").unwrap_err();
        assert!(err.contains("no longer"), "{err}");
        let fixed = load(&project)
            .into_iter()
            .find(|r| r.id == "r_fixed")
            .unwrap();
        assert_eq!(
            fixed.example_status.as_deref(),
            Some("does not reproduce (reproduced on 0.0.9)")
        );
        let err = prepare_send(&apps, &project, "r_fixed").unwrap_err();
        assert!(err.contains("is closed"), "{err}");
    }

    #[test]
    fn same_report_same_id() {
        let a = id_for(
            "get_schema",
            &json!({"table": "orders"}),
            "said_missing",
            "orders.x",
        );
        let b = id_for(
            "get_schema",
            &json!({"table": "orders"}),
            "said_missing",
            "orders.x",
        );
        let c = id_for(
            "get_schema",
            &json!({"table": "orders"}),
            "said_missing",
            "orders.y",
        );
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
