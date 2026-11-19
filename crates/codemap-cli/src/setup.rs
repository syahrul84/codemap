//! `codemap setup`: makes codemap the AI app's first stop for questions about
//! code. For Claude Code it registers the MCP server, writes a rule block into
//! `CLAUDE.md` and installs a hook pair (`codemap hook check` / `used`) that
//! refuses grep-style code searches until a codemap tool has run in the
//! session. `--check` reports what is installed; `--remove` undoes it.
//!
//! The hooks fail open: any error, bad input or missing state lets the
//! search through.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use serde_json::{Value, json};

const BEGIN: &str = "<!-- codemap:begin -->";
const END: &str = "<!-- codemap:end -->";

/// The rule block written into the instruction file.
const RULES: &str = r#"# codemap first
The `codemap` MCP server indexes this code. It is the first stop for every question about the code, before grep, rg, find, sed -n, cat or reading files.
- Load the tools at the start of a coding task: they may be deferred (in Claude Code, one ToolSearch `select:` call for the `mcp__codemap*__` tools you need).
- `project_overview` once per task (with `packages: "all"` or `package: "name"`: what is installed and what uses it, before adding or removing a dependency). `search_symbol`, `get_symbol` and `get_component` instead of grepping. `get_schema` before any query, migration or database script. `api_usage` for frontend calls. `find_usages` before renaming or removing, and with `git_changes: true` after a change for what it affects and the tests to run. `verify` everything a change relies on before finishing.
- grep and file reads come after codemap: to read a whole file, when codemap answers `found: false` or `stale: true`, or for things it does not index (logs, server output, remote config, build output).
- If the server is not connected, use the CLI: `codemap query --project <path> <tool> '<json>'`.
- A wrong answer: `report_wrong_answer` once, then carry on.
- In the final reply, say what codemap answered and where you fell back to grep."#;

/// What the refused search is told.
const REFUSAL: &str = "codemap first: use the codemap MCP tools before searching the code \
(search_symbol, get_symbol, get_component, get_schema, find_usages, api_usage, verify); \
load them first if they are deferred (ToolSearch select:mcp__codemap...). Run this search \
again after a codemap call when it answers found:false or stale:true, or for things codemap \
does not index (logs, server output, build output). A search that only names files that are not \
code (json, env, yaml, config, markdown) or a folder outside the index is never refused. \
CLI: codemap query --project <path> <tool> '<json>'.";

/// Searches that are never code searches: other machines, containers, logs,
/// git history, dependencies, build output, temp folders.
const SKIP: &[&str] = &[
    "ssh ",
    "docker ",
    "kubectl ",
    "journalctl",
    "git log",
    "git show",
    "git diff",
    "/var/log",
    ".log",
    "logs/",
    "node_modules",
    "vendor/",
    "target/",
    "dist/",
    "build/",
    ".build/",
    ".git/",
    "/tmp",
    "/private/tmp",
    "TMPDIR",
    "scratchpad",
    ".claude/",
    "--help",
    "--version",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

struct Paths {
    rules: PathBuf,
    settings: PathBuf,
}

fn paths(scope: Scope) -> anyhow::Result<Paths> {
    Ok(match scope {
        Scope::User => {
            // Claude Code's own setting for its folder, else `~/.claude`.
            let dir = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
                Some(d) => PathBuf::from(d),
                None => dirs::home_dir().context("no home folder")?.join(".claude"),
            };
            Paths {
                rules: dir.join("CLAUDE.md"),
                settings: dir.join("settings.json"),
            }
        }
        Scope::Project => Paths {
            rules: PathBuf::from("CLAUDE.md"),
            settings: PathBuf::from(".claude").join("settings.json"),
        },
    })
}

fn exe() -> anyhow::Result<String> {
    let p = std::env::current_exe()?.canonicalize()?;
    let s = p.display().to_string();
    Ok(s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s))
}

fn hook_command(exe: &str, action: &str) -> String {
    format!("\"{exe}\" hook {action}")
}

fn is_ours(entry: &Value) -> bool {
    entry["hooks"].as_array().into_iter().flatten().any(|h| {
        h["command"].as_str().is_some_and(|c| {
            c.contains(" hook check") || c.contains(" hook used") || c.contains(" hook edited")
        }) && h["command"].as_str().is_some_and(|c| c.contains("codemap"))
    })
}

/// The instruction file with the block put in (or replaced), or taken out.
fn with_block(text: &str, block: Option<&str>) -> String {
    let start = text.find(BEGIN);
    let end = text.find(END).map(|e| e + END.len());
    let (before, after) = match (start, end) {
        (Some(s), Some(e)) if e > s => (&text[..s], &text[e..]),
        _ => (text, ""),
    };
    let before = before.trim_end();
    let after = after.trim_start_matches(['\r', '\n']);
    let mut out = String::new();
    out.push_str(before);
    if let Some(b) = block {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("{BEGIN}\n{b}\n{END}\n"));
    } else if !out.is_empty() {
        out.push('\n');
    }
    if !after.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(after);
    }
    out
}

/// The settings with our hook pair put in (or replaced), or taken out.
fn with_hooks(mut settings: Value, exe: Option<&str>) -> Value {
    if !settings.is_object() {
        settings = json!({});
    }
    for (event, ours) in [
        ("PreToolUse", &[("Grep|Glob|Bash", "check")][..]),
        (
            "PostToolUse",
            &[
                ("mcp__codemap.*", "used"),
                // After an edit: names it uses that do not exist.
                ("Edit|Write|MultiEdit", "edited"),
            ][..],
        ),
    ] {
        let list = settings["hooks"][event]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut list: Vec<Value> = list.into_iter().filter(|e| !is_ours(e)).collect();
        if let Some(exe) = exe {
            for (matcher, action) in ours {
                list.push(json!({
                    "matcher": matcher,
                    "hooks": [{ "type": "command", "command": hook_command(exe, action) }],
                }));
            }
        }
        if list.is_empty() {
            if let Some(h) = settings["hooks"].as_object_mut() {
                h.remove(event);
            }
        } else {
            settings["hooks"][event] = json!(list);
        }
    }
    if settings["hooks"].as_object().is_some_and(|h| h.is_empty())
        && let Some(o) = settings.as_object_mut()
    {
        o.remove("hooks");
    }
    settings
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => Ok(json!({})),
        Ok(t) => serde_json::from_str(&t)
            .with_context(|| format!("{} is not valid JSON; nothing was changed", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e.into()),
    }
}

fn write(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
}

/// Puts `text` in `path` the careful way: nothing when it is already there;
/// with `dry_run` only says what would change; otherwise keeps the old file
/// as `<name>.codemap-backup` before writing.
fn apply(path: &Path, text: &str, dry_run: bool, what: &str) -> anyhow::Result<()> {
    let old = std::fs::read_to_string(path).ok();
    if old.as_deref() == Some(text) {
        println!("{what}: already in {}", path.display());
        return Ok(());
    }
    if dry_run {
        let verb = if old.is_some() { "update" } else { "create" };
        println!("{what}: would {verb} {}", path.display());
        return Ok(());
    }
    if old.is_some() {
        let backup = backup_path(path);
        std::fs::copy(path, &backup)
            .with_context(|| format!("cannot back up {}; nothing was changed", path.display()))?;
        println!("{what}: the old file is kept as {}", backup.display());
    }
    write(path, text)
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".codemap-backup");
    path.with_file_name(name)
}

fn claude(args: &[&str]) -> Option<std::process::Output> {
    Command::new("claude").args(args).output().ok()
}

fn registered(scope: Scope) -> Option<bool> {
    let out = claude(&["mcp", "get", "codemap"])?;
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    Some(
        out.status.success()
            && (scope == Scope::User || text.contains("project") || text.contains("local")),
    )
}

/// What `codemap setup` was asked to do.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Only report what is installed.
    pub check: bool,
    /// Undo it.
    pub remove: bool,
    /// Say what would change, change nothing.
    pub dry_run: bool,
    /// Leave the search hook out.
    pub no_hook: bool,
}

pub fn run(scope: Scope, opts: Options) -> anyhow::Result<()> {
    let Options {
        check,
        remove,
        dry_run,
        no_hook,
    } = opts;
    let p = paths(scope)?;
    let scope_name = if scope == Scope::User {
        "user"
    } else {
        "project"
    };
    if check {
        let rules = std::fs::read_to_string(&p.rules).unwrap_or_default();
        let settings = read_json(&p.settings).unwrap_or_else(|_| json!({}));
        let hooks: Vec<String> = ["PreToolUse", "PostToolUse"]
            .iter()
            .flat_map(|ev| {
                settings["hooks"][*ev]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .filter(is_ours)
            .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_owned))
            .collect();
        let program_ok = hooks.iter().all(|c| {
            c.split('"')
                .nth(1)
                .is_none_or(|exe| Path::new(exe).is_file())
        });
        let mark = |ok: bool| if ok { "yes" } else { "no " };
        println!("codemap setup ({scope_name} scope)");
        match registered(scope) {
            Some(ok) => println!(
                "  {} MCP server registered (`claude mcp get codemap`)",
                mark(ok)
            ),
            None => println!("  ?   MCP server: the `claude` command was not found"),
        }
        println!(
            "  {} rule block in {}",
            mark(rules.contains(BEGIN)),
            p.rules.display()
        );
        let has = |action: &str| {
            hooks
                .iter()
                .any(|c| c.ends_with(&format!(" hook {action}")))
        };
        println!(
            "  {} search hook in {}{}",
            mark(has("check") && has("used")),
            p.settings.display(),
            if hooks.is_empty() || program_ok {
                String::new()
            } else {
                " (its program path no longer exists: run `codemap setup` again)".into()
            }
        );
        println!(
            "  {} check after each edit{}",
            mark(has("edited")),
            if has("edited") || hooks.is_empty() {
                ""
            } else {
                " (run `codemap setup` again to add it)"
            }
        );
        return Ok(());
    }
    if remove {
        if p.rules.is_file() {
            let text = std::fs::read_to_string(&p.rules)?;
            if text.contains(BEGIN) {
                apply(
                    &p.rules,
                    &with_block(&text, None),
                    dry_run,
                    "rule block removed",
                )?;
            }
        }
        if p.settings.is_file() {
            let settings = read_json(&p.settings)?;
            apply(
                &p.settings,
                &(serde_json::to_string_pretty(&with_hooks(settings, None))? + "\n"),
                dry_run,
                "search hook removed",
            )?;
        }
        if dry_run {
            println!(
                "MCP server: would run `claude mcp remove codemap --scope {scope_name}`\nNothing was changed (--dry-run)."
            );
            return Ok(());
        }
        match claude(&["mcp", "remove", "codemap", "--scope", scope_name]) {
            Some(o) if o.status.success() => {
                println!("removed the codemap MCP server ({scope_name} scope)")
            }
            _ => println!(
                "the MCP server was not removed; run `claude mcp remove codemap --scope {scope_name}`"
            ),
        }
        return Ok(());
    }

    let exe = exe()?;
    // 1. The MCP server.
    match registered(scope) {
        Some(true) => println!("MCP server: already registered"),
        Some(false) if dry_run => println!(
            "MCP server: would run `claude mcp add --scope {scope_name} codemap -- \"{exe}\" serve --project .`"
        ),
        Some(false) => {
            let out = claude(&[
                "mcp",
                "add",
                "--scope",
                scope_name,
                "codemap",
                "--",
                &exe,
                "serve",
                "--project",
                ".",
            ]);
            match out {
                Some(o) if o.status.success() => {
                    println!("MCP server: registered ({scope_name} scope)")
                }
                _ => println!(
                    "MCP server: could not register it; run `claude mcp add --scope {scope_name} codemap -- \"{exe}\" serve --project .`"
                ),
            }
        }
        None => println!(
            "MCP server: the `claude` command was not found; run `claude mcp add --scope {scope_name} codemap -- \"{exe}\" serve --project .` once it is installed"
        ),
    }
    // 2. The rule block (the program the hook runs exists already: it is this one).
    let text = std::fs::read_to_string(&p.rules).unwrap_or_default();
    apply(
        &p.rules,
        &with_block(&text, Some(RULES)),
        dry_run,
        "rule block",
    )?;
    // 3. The hooks, last, so they never point at something missing. Without
    // the hook, one installed earlier is taken out.
    let settings = read_json(&p.settings)?;
    let hook_exe = (!no_hook).then_some(exe.as_str());
    let had = p.settings.is_file();
    let updated = with_hooks(settings.clone(), hook_exe);
    if had || hook_exe.is_some() {
        apply(
            &p.settings,
            &(serde_json::to_string_pretty(&updated)? + "\n"),
            dry_run,
            if no_hook {
                "search hook (left out)"
            } else {
                "search hook"
            },
        )?;
    }
    if dry_run {
        println!("Nothing was changed (--dry-run). Run it again without --dry-run to apply.");
        return Ok(());
    }
    println!(
        "Done. Start a new Claude Code session; `codemap setup --check` shows what is installed, `codemap setup --remove` undoes it."
    );
    Ok(())
}

// ---------------------------------------------------------------- hooks

pub(crate) fn state_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("hooks")
}

pub(crate) fn safe(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(80)
        .collect()
}

/// One line per hook decision, for `codemap usage`.
pub(crate) fn log(data_dir: &Path, event: &str, session: &str) {
    let path = data_dir.join("hooks.jsonl");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    let line = json!({ "at": codemap_core::time::now_rfc3339(), "event": event, "session": safe(session) });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{line}");
    }
}

/// Counts of the hook's decisions since `since` (RFC 3339).
pub fn counts(data_dir: &Path, since: &str) -> Value {
    let text = std::fs::read_to_string(data_dir.join("hooks.jsonl")).unwrap_or_default();
    let mut c = std::collections::BTreeMap::<String, u64>::new();
    for l in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        if v["at"].as_str().is_some_and(|a| a >= since)
            && let Some(e) = v["event"].as_str()
        {
            *c.entry(e.to_owned()).or_default() += 1;
        }
    }
    json!(c)
}

/// What a tool call is to the search hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Search {
    /// Not a search of the project's files (or one of logs, history, other
    /// machines, an allowed path).
    No,
    /// A search that may look at the project's code.
    Code,
    /// A search that only names files codemap does not read as code (JSON,
    /// env, YAML, config, Markdown) or folders the index holds nothing of:
    /// codemap cannot answer it.
    NotCode,
}

/// Is this a search of the project's files (not of logs, history, other machines)?
pub fn is_code_search(tool: &str, input: &Value, allow: &[String]) -> bool {
    search_kind(tool, input, allow, None, &|_| None) != Search::No
}

/// What kind of search a tool call is. `cwd` is the session's folder (paths
/// in the search are relative to it); `indexed` says whether the index holds
/// a file or anything under a folder.
pub fn search_kind(
    tool: &str,
    input: &Value,
    allow: &[String],
    cwd: Option<&Path>,
    indexed: crate::targets::Indexed<'_>,
) -> Search {
    use crate::targets::{self, Token};
    let allowed = |text: &str| {
        SKIP.iter().any(|s| text.contains(s))
            || allow.iter().any(|a| {
                let a = a.trim_matches('*');
                !a.is_empty() && text.contains(a)
            })
    };
    let kind = |no_code: bool| {
        if no_code {
            Search::NotCode
        } else {
            Search::Code
        }
    };
    match tool {
        "Grep" | "Glob" => {
            let path = input["path"].as_str().filter(|p| !p.is_empty());
            let glob = input["glob"].as_str().unwrap_or("");
            let pattern = input["pattern"]
                .as_str()
                .filter(|_| tool == "Glob")
                .unwrap_or("");
            if allowed(&[path.unwrap_or(""), glob, pattern].join(" ")) {
                return Search::No;
            }
            let file_type = input["type"].as_str().map(targets::type_as_pattern);
            let patterns: Vec<&str> = [glob, pattern]
                .into_iter()
                .chain(file_type.as_deref())
                .filter(|p| !p.is_empty())
                .collect();
            kind(targets::tool_search_names_no_code(
                &patterns, path, cwd, indexed,
            ))
        }
        "Bash" => {
            let cmd = input["command"].as_str().unwrap_or("");
            if allowed(cmd) {
                return Search::No;
            }
            // Commands (`;`, `&&`, `||`, a new line), each a pipeline.
            let mut found = Search::No;
            let mut base = cwd.map(Path::to_path_buf);
            let tokens = targets::tokens(cmd);
            for command in tokens.split(|t| *t == Token::End) {
                for (stage, seg) in command.split(|t| *t == Token::Pipe).enumerate() {
                    let mut words = seg
                        .iter()
                        .filter_map(|t| match t {
                            Token::Word(w) => Some(w),
                            _ => None,
                        })
                        .skip_while(|w| {
                            (w.contains('=') && !w.starts_with('-'))
                                || matches!(w.as_str(), "sudo" | "command" | "xargs")
                        });
                    let first = words.next().map(String::as_str).unwrap_or("");
                    let mut args: Vec<String> = words.cloned().collect();
                    // `cd x && grep …`: what follows runs there.
                    if first == "cd" && stage == 0 {
                        base = match (&base, args.first()) {
                            (Some(b), Some(d)) if !d.starts_with('~') => Some(b.join(d)),
                            _ => None,
                        };
                        continue;
                    }
                    let first = first.rsplit(['/', '\\']).next().unwrap_or(first);
                    let mut program = first;
                    let tool = matches!(
                        first,
                        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "find" | "fd"
                    ) || (first == "git"
                        && args.first().map(String::as_str) == Some("grep"));
                    if !tool {
                        continue;
                    }
                    if first == "git" {
                        args.remove(0);
                        program = "grep";
                    }
                    // `cmd | grep x` filters the output of `cmd`: not a
                    // search, unless it is recursive (`| xargs grep -r`).
                    let recursive = args.iter().any(|w| {
                        w == "--recursive"
                            || (w.starts_with('-')
                                && !w.starts_with("--")
                                && (w.contains('r') || w.contains('R')))
                    });
                    if !(stage == 0
                        || recursive
                        || matches!(first, "rg" | "find" | "fd" | "ag" | "ack"))
                    {
                        continue;
                    }
                    if targets::shell_search_names_no_code(program, &args, base.as_deref(), indexed)
                    {
                        if found == Search::No {
                            found = Search::NotCode;
                        }
                    } else {
                        found = Search::Code;
                    }
                }
            }
            found
        }
        _ => Search::No,
    }
}

fn recently_used(data_dir: &Path, session: &str, window: Duration) -> bool {
    let file = state_dir(data_dir).join(format!("used-{}", safe(session)));
    std::fs::metadata(file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age <= window)
}

/// `codemap hook check|used`, called by the AI app with the tool call as JSON
/// on stdin. Never fails: errors let the call through.
pub fn hook(
    action: &str,
    data_dir: Option<PathBuf>,
    indexed: &dyn Fn(&Path, &Path) -> Option<bool>,
) {
    let _ = (|| -> Option<()> {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input).ok()?;
        let v: Value = serde_json::from_str(&input).ok()?;
        let session = v["session_id"].as_str().unwrap_or("default").to_owned();
        let data_dir = data_dir?;
        match action {
            "used" => {
                let dir = state_dir(&data_dir);
                std::fs::create_dir_all(&dir).ok()?;
                std::fs::write(dir.join(format!("used-{}", safe(&session))), b"").ok()?;
                log(&data_dir, "codemap_call", &session);
            }
            "check" => {
                let tool = v["tool_name"].as_str()?;
                let cwd = v["cwd"].as_str().map(PathBuf::from);
                let agent = cwd
                    .as_deref()
                    .and_then(|c| codemap_core::Config::load(c).ok())
                    .map(|c| c.agent)
                    .unwrap_or_default();
                // By what the search names alone first: cheap, and enough
                // for most calls.
                let by_name = search_kind(
                    tool,
                    &v["tool_input"],
                    &agent.allow_search,
                    cwd.as_deref(),
                    &|_| None,
                );
                if by_name == Search::No {
                    return None;
                }
                let window = Duration::from_secs(60 * agent.search_window_minutes.unwrap_or(30));
                // A search codemap cannot answer (files that are not code, a
                // folder outside the index) needs no codemap call first. The
                // index is only asked when the search would be refused.
                let passes = recently_used(&data_dir, &session, window)
                    || by_name == Search::NotCode
                    || cwd.as_deref().is_some_and(|cwd| {
                        search_kind(
                            tool,
                            &v["tool_input"],
                            &agent.allow_search,
                            Some(cwd),
                            &|p| indexed(cwd, p),
                        ) == Search::NotCode
                    });
                if passes {
                    log(&data_dir, "search_allowed", &session);
                    // A search codemap did not answer: where it looked.
                    crate::gaps::record(
                        &data_dir,
                        &safe(&session),
                        cwd.as_deref(),
                        tool,
                        &v["tool_input"],
                    );
                    return None;
                }
                log(&data_dir, "search_refused", &session);
                println!(
                    "{}",
                    json!({
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "deny",
                            "permissionDecisionReason": REFUSAL,
                        }
                    })
                );
            }
            _ => {}
        }
        Some(())
    })();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_searches_are_told_apart() {
        let bash = |c: &str| is_code_search("Bash", &json!({ "command": c }), &[]);
        assert!(bash("grep -rn foo app/"));
        assert!(bash("cd x && rg Order"));
        assert!(bash("git grep -n Order"));
        assert!(bash("FOO=1 find . -name '*.php'"));
        assert!(!bash("ls -la"));
        assert!(!bash("git log --oneline | grep fix"));
        assert!(!bash("docker logs api | grep ERROR"));
        assert!(!bash("grep -r foo node_modules/x"));
        assert!(!bash("grep error storage/logs/laravel.log"));
        assert!(!bash("make check 2>&1 | grep 'test result'"));
        assert!(bash("ls app | xargs grep -rn Order"));
        let grep = |p: &str| {
            is_code_search(
                "Grep",
                &json!({ "pattern": "x", "path": p }),
                &["docs/".into()],
            )
        };
        assert!(grep("app/Models"));
        assert!(!grep("vendor/laravel"));
        assert!(!grep("docs/guide.md"));
        assert!(!is_code_search("Read", &json!({}), &[]));
    }

    #[test]
    fn searches_codemap_cannot_answer_are_told_apart() {
        let kind = |tool: &str, input: Value| search_kind(tool, &input, &[], None, &|_| None);
        let bash = |c: &str| kind("Bash", json!({ "command": c }));
        // Files that are not code.
        assert_eq!(bash("grep -n laravel-echo package.json"), Search::NotCode);
        assert_eq!(
            bash("grep -rn 'pusher|echo' --include=*.json ."),
            Search::NotCode
        );
        assert_eq!(bash("cat composer.json | grep scripts"), Search::No);
        assert_eq!(
            bash("rg -n PUSHER .env.example docker-compose.yml"),
            Search::NotCode
        );
        assert_eq!(bash("find . -name '*.conf'"), Search::NotCode);
        assert_eq!(
            bash("grep -n x .gitignore && grep -rn Order app/"),
            Search::Code
        );
        assert_eq!(
            bash("grep -rn Order app/ ; grep -n x .gitignore"),
            Search::Code
        );
        assert_eq!(bash("grep -rn Order app/"), Search::Code);
        assert_eq!(bash("grep -rn 'a|b' src"), Search::Code);
        assert_eq!(bash("ls"), Search::No);
        assert_eq!(
            kind("Grep", json!({ "pattern": "x", "glob": "*.json" })),
            Search::NotCode
        );
        assert_eq!(
            kind("Grep", json!({ "pattern": "x", "type": "yaml" })),
            Search::NotCode
        );
        assert_eq!(
            kind("Grep", json!({ "pattern": "x", "path": "package.json" })),
            Search::NotCode
        );
        assert_eq!(kind("Grep", json!({ "pattern": "x" })), Search::Code);
        assert_eq!(
            kind("Grep", json!({ "pattern": "x", "glob": "*.vue" })),
            Search::Code
        );
        assert_eq!(
            kind("Glob", json!({ "pattern": "**/*.yml" })),
            Search::NotCode
        );
        assert_eq!(
            kind("Glob", json!({ "pattern": "src/**/*.ts" })),
            Search::Code
        );
        // A folder the index holds nothing of, in the folder a `cd` moved to.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("web/document")).unwrap();
        std::fs::create_dir_all(dir.path().join("web/src")).unwrap();
        let src = dir.path().join("web/src");
        let at = |c: &str| {
            search_kind(
                "Bash",
                &json!({ "command": c }),
                &[],
                Some(dir.path()),
                &|p| Some(p.starts_with(&src)),
            )
        };
        assert_eq!(at("cd web && grep -rn foo document/"), Search::NotCode);
        assert_eq!(at("grep -rn foo web/document"), Search::NotCode);
        assert_eq!(at("cd web && grep -rn foo src"), Search::Code);
        assert_eq!(at("grep -rn foo document/"), Search::Code);
    }

    #[test]
    fn rule_block_is_put_in_and_taken_out() {
        let original = "# My rules\n\n- be kind\n";
        let with = with_block(original, Some("RULES"));
        assert_eq!(
            with,
            "# My rules\n\n- be kind\n\n<!-- codemap:begin -->\nRULES\n<!-- codemap:end -->\n"
        );
        // Running setup again replaces the block, not adds a second one.
        let again = with_block(&with, Some("NEW"));
        assert_eq!(again.matches(BEGIN).count(), 1);
        assert!(again.contains("NEW") && !again.contains("RULES"));
        assert_eq!(with_block(&with, None), original);
        // Text after the block stays.
        let around = format!("a\n{BEGIN}\nx\n{END}\nb\n");
        assert_eq!(with_block(&around, None), "a\n\nb\n");
    }

    #[test]
    fn hooks_are_merged_and_removed() {
        let mine = json!({ "hooks": { "PreToolUse": [{ "matcher": "Write", "hooks": [{ "type": "command", "command": "lint" }] }] }, "model": "x" });
        let with = with_hooks(mine.clone(), Some("/bin/codemap"));
        assert_eq!(with["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(with["hooks"]["PostToolUse"][0]["matcher"], "mcp__codemap.*");
        assert_eq!(
            with["hooks"]["PostToolUse"][1]["matcher"],
            "Edit|Write|MultiEdit"
        );
        assert!(
            with["hooks"]["PostToolUse"][1]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with(" hook edited")
        );
        // Again: replaced, not doubled.
        let twice = with_hooks(with, Some("/bin/codemap"));
        assert_eq!(twice["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(twice["hooks"]["PostToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(with_hooks(twice, None), mine);
    }
}
