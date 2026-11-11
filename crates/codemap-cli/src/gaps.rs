//! What codemap did not cover: the code searches an AI ran anyway.
//!
//! The search hook lets a code search through once a codemap tool has run.
//! Each of those searches says "codemap did not answer this": the hook notes
//! where it looked (folder and file type) and what kind of question it was.
//! `codemap gaps` adds them up, so the owner can see which areas are searched
//! the most, decide which are worth covering, and set the rest aside.
//!
//! Everything stays in the data folder on this machine.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const LOG: &str = "gaps.jsonl";
const IGNORED: &str = "gaps-ignored.json";
const SENT: &str = "gaps-sent.json";
/// The log starts again past this size.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// One search, as the hook saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    /// Folders, files and globs searched, as written.
    pub targets: Vec<String>,
    pub pattern: String,
    /// `definition`, `name`, `text` or `files`.
    pub shape: &'static str,
}

// ---------------------------------------------------------------- reading a search

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    /// `|`, `||`, `&&`, `;` or a new line.
    Op,
}

/// The words of a shell command, quotes removed; operators outside quotes
/// split it into commands.
fn tokens(cmd: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut has_word = false;
    let mut chars = cmd.chars().peekable();
    fn flush(word: &mut String, has: &mut bool, out: &mut Vec<Token>) {
        if *has {
            out.push(Token::Word(std::mem::take(word)));
            *has = false;
        }
    }
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has_word = true;
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    word.push(d);
                }
            }
            '"' => {
                has_word = true;
                while let Some(d) = chars.next() {
                    match d {
                        '"' => break,
                        '\\' => {
                            if let Some(e) = chars.next() {
                                if !matches!(e, '"' | '\\' | '$' | '`') {
                                    word.push('\\');
                                }
                                word.push(e);
                            }
                        }
                        _ => word.push(d),
                    }
                }
            }
            '\\' => {
                if let Some(e) = chars.next() {
                    has_word = true;
                    word.push(e);
                }
            }
            '|' | ';' | '\n' => {
                flush(&mut word, &mut has_word, &mut out);
                if chars.peek() == Some(&'|') {
                    chars.next();
                }
                out.push(Token::Op);
            }
            '&' if chars.peek() == Some(&'&') => {
                chars.next();
                flush(&mut word, &mut has_word, &mut out);
                out.push(Token::Op);
            }
            c if c.is_whitespace() => flush(&mut word, &mut has_word, &mut out),
            _ => {
                has_word = true;
                word.push(c);
            }
        }
    }
    flush(&mut word, &mut has_word, &mut out);
    out
}

/// `definition` (`function total`, `class Order`), `name` (one identifier),
/// or `text`.
fn shape_of(pattern: &str) -> &'static str {
    let p = pattern.trim();
    const DEFINES: &[&str] = &[
        "function ",
        "class ",
        "interface ",
        "trait ",
        "enum ",
        "struct ",
        "def ",
        "fn ",
        "func ",
        "const ",
        "export ",
    ];
    if DEFINES.iter().any(|d| p.contains(d)) {
        "definition"
    } else if !p.is_empty()
        && p.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '\\' | ':' | '$' | '.' | '-' | '>'))
    {
        "name"
    } else {
        "text"
    }
}

/// Flags of grep-like tools that take the next word as their value.
const TAKES_VALUE: &[&str] = &[
    "-A",
    "-B",
    "-C",
    "-m",
    "-f",
    "-d",
    "-j",
    "--exclude",
    "--exclude-dir",
    "--max-count",
    "--context",
    "--after-context",
    "--before-context",
    "--max-depth",
    "-T",
    "--type-not",
];

/// `2>/dev/null`, `&>log`, `1>&2`: where output goes, not a place searched.
/// Not `<x-alert` or `<?php`: those are what was searched for.
fn is_redirect(a: &str) -> bool {
    let rest = a.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
    rest.starts_with('>') || a == "<"
}

/// The file extension a ripgrep type name stands for (`rust` → `rs`); the
/// name itself when it is already one (`php`, `vue`, `ts`).
fn type_extension(name: &str) -> &str {
    match name {
        "rust" => "rs",
        "python" => "py",
        "kotlin" => "kt",
        "ruby" => "rb",
        "markdown" => "md",
        "yaml" => "yml",
        "javascript" => "js",
        "typescript" => "ts",
        "csharp" => "cs",
        "shell" => "sh",
        "cpp" => "cpp",
        other => other,
    }
}

fn grep_like(args: &[String]) -> Search {
    let (mut pattern, mut targets, mut globs) = (None::<String>, Vec::new(), Vec::new());
    let mut i = 0;
    let mut flags_done = false;
    while i < args.len() {
        let a = args[i].as_str();
        let next = args.get(i + 1).cloned();
        if !flags_done && a == "--" {
            flags_done = true;
        } else if !flags_done && matches!(a, "-e" | "--regexp") {
            pattern = pattern.or(next);
            i += 1;
        } else if !flags_done && matches!(a, "--include" | "-g" | "--glob") {
            // `-g '!*.min.js'` leaves a type out: not where the search looked.
            globs.extend(next.filter(|g| !g.starts_with('!')));
            i += 1;
        } else if !flags_done && matches!(a, "-t" | "--type") {
            globs.extend(next.map(|t| format!("*.{}", type_extension(&t))));
            i += 1;
        } else if let Some(g) = a
            .strip_prefix("--include=")
            .or_else(|| a.strip_prefix("--glob="))
            .filter(|_| !flags_done)
        {
            if !g.starts_with('!') {
                globs.push(g.to_owned());
            }
        } else if !flags_done && TAKES_VALUE.contains(&a) {
            i += 1;
        } else if !flags_done && a.starts_with('-') && a.len() > 1 {
            // Any other flag.
        } else if pattern.is_some() && is_redirect(a) {
            // A redirection (after the pattern: `'>'` can be one), and its
            // file when written apart.
            if matches!(a, ">" | ">>" | "2>" | "&>" | "<") {
                i += 1;
            }
        } else if pattern.is_none() {
            pattern = Some(a.to_owned());
        } else {
            targets.push(a.to_owned());
        }
        i += 1;
    }
    let pattern = pattern.unwrap_or_default();
    with_globs(targets, globs, shape_of(&pattern), pattern)
}

fn find_like(tool: &str, args: &[String]) -> Search {
    let (mut targets, mut globs, mut names) = (Vec::new(), Vec::new(), Vec::new());
    let mut i = 0;
    let mut in_paths = tool == "find";
    while i < args.len() {
        let a = args[i].as_str();
        let next = args.get(i + 1).cloned();
        if matches!(
            a,
            "-name" | "-iname" | "-path" | "-ipath" | "-regex" | "-wholename"
        ) {
            if let Some(n) = next {
                names.push(n.clone());
                globs.push(n);
            }
            i += 1;
        } else if tool == "fd" && matches!(a, "-e" | "--extension") {
            globs.extend(next.map(|e| format!("*.{e}")));
            i += 1;
        } else if tool == "fd" && matches!(a, "-t" | "--type" | "-d" | "--max-depth") {
            i += 1;
        } else if a.starts_with('-') || matches!(a, "(" | ")" | "!") {
            in_paths = false;
            if matches!(
                a,
                "-type" | "-maxdepth" | "-mindepth" | "-newer" | "-size" | "-mtime"
            ) {
                i += 1;
            }
        } else if is_redirect(a) {
            if matches!(a, ">" | ">>" | "2>" | "&>" | "<") {
                i += 1;
            }
        } else if in_paths {
            targets.push(a.to_owned());
        } else if tool == "fd" {
            if names.is_empty() {
                names.push(a.to_owned());
            } else {
                targets.push(a.to_owned());
            }
        }
        i += 1;
    }
    with_globs(targets, globs, "files", names.join(" "))
}

/// A folder searched for one kind of file (`grep --include='*.blade.php' -r
/// resources`) is `resources/*.blade.php`.
fn with_globs(
    targets: Vec<String>,
    globs: Vec<String>,
    shape: &'static str,
    pattern: String,
) -> Search {
    let targets = if globs.is_empty() {
        targets
    } else if targets.is_empty() {
        globs
    } else {
        targets
            .iter()
            .flat_map(|t| {
                let dir = t.trim_end_matches('/');
                let file_like = Path::new(dir)
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().contains('.'))
                    && !t.ends_with('/');
                globs
                    .iter()
                    .map(move |g| {
                        if file_like {
                            t.clone()
                        } else {
                            format!("{dir}/{}", g.trim_start_matches('/'))
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    Search {
        targets,
        pattern,
        shape,
    }
}

/// What a tool call searched; `None` when it is not a search we can read.
pub fn search_of(tool: &str, input: &Value) -> Option<Search> {
    let s = |k: &str| input[k].as_str().unwrap_or_default().to_owned();
    match tool {
        "Grep" => {
            let mut globs = Vec::new();
            if !s("glob").is_empty() {
                globs.push(s("glob"));
            }
            if !s("type").is_empty() {
                globs.push(format!("*.{}", type_extension(&s("type"))));
            }
            let targets = Some(s("path"))
                .filter(|p| !p.is_empty())
                .into_iter()
                .collect();
            let pattern = s("pattern");
            Some(with_globs(targets, globs, shape_of(&pattern), pattern))
        }
        "Glob" => {
            let targets = Some(s("path"))
                .filter(|p| !p.is_empty())
                .into_iter()
                .collect();
            Some(with_globs(
                targets,
                vec![s("pattern")],
                "files",
                s("pattern"),
            ))
        }
        "Bash" => {
            let all = tokens(&s("command"));
            let mut found: Option<Search> = None;
            // `cd api && grep -rn x app` searched `api/app`.
            let mut base = String::new();
            for segment in all.split(|t| *t == Token::Op) {
                let words: Vec<String> = segment
                    .iter()
                    .filter_map(|t| match t {
                        Token::Word(w) => Some(w.clone()),
                        Token::Op => None,
                    })
                    .skip_while(|w| {
                        (w.contains('=') && !w.starts_with('-'))
                            || matches!(w.as_str(), "sudo" | "command" | "xargs")
                    })
                    .collect();
                let Some(first) = words.first() else { continue };
                if first == "cd" {
                    if let Some(dir) = words.get(1).filter(|d| !d.starts_with('-')) {
                        base = if Path::new(dir).is_absolute() || base.is_empty() {
                            dir.clone()
                        } else {
                            format!("{}/{dir}", base.trim_end_matches('/'))
                        };
                    }
                    continue;
                }
                let first = first.rsplit(['/', '\\']).next().unwrap_or(first);
                let mut one = match first {
                    "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" => grep_like(&words[1..]),
                    "git" if words.get(1).map(String::as_str) == Some("grep") => {
                        grep_like(&words[2..])
                    }
                    "find" | "fd" => find_like(first, &words[1..]),
                    _ => continue,
                };
                if !base.is_empty() {
                    if one.targets.is_empty() {
                        one.targets.push(base.clone());
                    } else {
                        for t in &mut one.targets {
                            if !Path::new(t.as_str()).is_absolute() && !t.starts_with('$') {
                                *t = format!("{}/{t}", base.trim_end_matches('/'));
                            }
                        }
                    }
                }
                // Several searches in one command: their places together.
                found = Some(match found {
                    None => one,
                    Some(mut f) => {
                        f.targets.extend(one.targets);
                        f
                    }
                });
            }
            found
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- the log

fn enabled() -> bool {
    !std::env::var("CODEMAP_NO_USAGE_LOG").is_ok_and(|v| v == "1")
}

/// Notes one search that went ahead without codemap answering it.
pub fn record(data_dir: &Path, session: &str, cwd: Option<&Path>, tool: &str, input: &Value) {
    if !enabled() {
        return;
    }
    let Some(search) = search_of(tool, input) else {
        return;
    };
    let path = data_dir.join(LOG);
    // Too large: the older half goes, the recent searches stay.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES)
        && let Ok(text) = std::fs::read_to_string(&path)
    {
        let lines: Vec<&str> = text.lines().collect();
        let kept = lines[lines.len() / 2..].join("\n") + "\n";
        let _ = std::fs::write(&path, kept);
    }
    let pattern: String = search.pattern.chars().take(120).collect();
    let line = json!({
        "at": codemap_core::time::now_rfc3339(),
        "session": session,
        "cwd": cwd.map(|c| c.display().to_string()),
        "tool": tool,
        "targets": search.targets,
        "pattern": pattern,
        "shape": search.shape,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{line}");
    }
}

/// Fills the log from past Claude Code sessions (`~/.claude/projects`), for
/// the time before the hook noted searches itself: every code search of a
/// session that also used codemap, since `since`. Running it again replaces
/// what the last run added. Returns `(sessions, searches)`.
pub fn backfill(data_dir: &Path, history: &Path, since: &str) -> std::io::Result<(usize, usize)> {
    use std::io::{BufRead as _, Write as _};
    let log = data_dir.join(LOG);
    // Keep what the hook noted; searches from then on are already counted.
    let kept: Vec<String> = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.contains("\"src\":\"history\""))
        .map(str::to_owned)
        .collect();
    let live_from = kept
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["at"].as_str().map(str::to_owned))
        .min();

    let mut files = Vec::new();
    let mut stack = vec![history.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                files.push(p);
            }
        }
    }
    // To the second: the hook writes whole seconds, transcripts milliseconds.
    let second = |t: &str| t.get(..19).unwrap_or(t).to_owned();
    let live_from = live_from.map(|t| second(&t));
    // A resumed or forked session repeats its earlier tool calls.
    let mut seen_calls: BTreeSet<String> = BTreeSet::new();
    let mut used_codemap: BTreeSet<String> = BTreeSet::new();
    let mut found: Vec<(String, Value)> = Vec::new();
    for file in files {
        let Ok(f) = std::fs::File::open(&file) else {
            continue;
        };
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            if !line.contains("\"tool_use\"") {
                continue;
            }
            let codemap = line.contains("mcp__codemap");
            if !codemap
                && !["\"Bash\"", "\"Grep\"", "\"Glob\""]
                    .iter()
                    .any(|t| line.contains(t))
            {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let session = v["sessionId"].as_str().unwrap_or_default().to_owned();
            let at = v["timestamp"].as_str().unwrap_or_default();
            if session.is_empty() || at < since {
                continue;
            }
            for part in v["message"]["content"].as_array().into_iter().flatten() {
                if part["type"] != "tool_use" {
                    continue;
                }
                let tool = part["name"].as_str().unwrap_or_default();
                if tool.starts_with("mcp__codemap") {
                    used_codemap.insert(session.clone());
                    continue;
                }
                if live_from
                    .as_deref()
                    .is_some_and(|from| second(at).as_str() >= from)
                    || !crate::setup::is_code_search(tool, &part["input"], &[])
                    || part["id"]
                        .as_str()
                        .is_some_and(|id| !seen_calls.insert(format!("{session}:{id}")))
                {
                    continue;
                }
                let Some(search) = search_of(tool, &part["input"]) else {
                    continue;
                };
                let pattern: String = search.pattern.chars().take(120).collect();
                found.push((
                    session.clone(),
                    json!({
                        "at": at,
                        "session": session,
                        "cwd": v["cwd"],
                        "tool": tool,
                        "targets": search.targets,
                        "pattern": pattern,
                        "shape": search.shape,
                        "src": "history",
                    }),
                ));
            }
        }
    }
    found.retain(|(s, _)| used_codemap.contains(s));
    let sessions: BTreeSet<&String> = found.iter().map(|(s, _)| s).collect();
    let counts = (sessions.len(), found.len());
    std::fs::create_dir_all(data_dir)?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(&log)?);
    for (_, v) in &found {
        writeln!(out, "{v}")?;
    }
    for l in &kept {
        writeln!(out, "{l}")?;
    }
    out.flush()?;
    Ok(counts)
}

pub fn clear(data_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(data_dir.join(LOG)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------- areas

/// Words that come before an extension and are part of the kind of file
/// (`index.blade.php`, `cart.test.ts`). Any other word there is a name the
/// project chose (`deploy.acme.sh`), and stays out of the kind.
const COMPOUND: &[&str] = &[
    "blade", "test", "spec", "d", "min", "config", "stories", "module", "lock", "map",
];

/// `index.blade.php` → `blade.php`, `cart.test.ts` → `test.ts`,
/// `Order.php` → `php`, `.env.staging` → `env`; nothing for a name without
/// an extension.
fn kind_of(name: &str) -> Option<String> {
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric());
    // A dotfile's kind is its first word: what follows is the project's.
    if let Some(rest) = name.strip_prefix('.') {
        return rest.split('.').next().filter(|p| ok(p)).map(str::to_owned);
    }
    if !name.contains('.') {
        return None;
    }
    let parts: Vec<&str> = name.split('.').collect();
    let last = parts.last().copied().filter(|p| ok(p))?;
    let before = parts
        .len()
        .checked_sub(2)
        .map(|i| parts[i])
        .filter(|p| parts.len() >= 3 && COMPOUND.contains(p));
    Some(match before {
        Some(b) => format!("{b}.{last}"),
        None => last.to_owned(),
    })
}

/// An area as it is compared: `resources/css *.css`, however many spaces
/// were typed between the folder and the type.
fn norm(area: &str) -> String {
    area.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The area a target belongs to: its first two folders and its file type.
/// `resources/views/orders/index.blade.php` → `resources/views  *.blade.php`.
fn area_of(target: &str, cwd: &Path, root: &Path) -> Option<String> {
    let t = target.trim();
    // A shell variable (`$f`) says nothing about where the search looked.
    // Nor does the home folder: it is not this project's.
    if t.is_empty() || t.contains('$') || t.starts_with('~') {
        return None;
    }
    // Where it is, from the project root.
    let joined = if Path::new(t).is_absolute() {
        PathBuf::from(t)
    } else {
        cwd.join(t.trim_start_matches("./"))
    };
    // `..` is followed, so `../other-repo/src` is seen to leave the project.
    let mut abs = PathBuf::new();
    for c in joined.components() {
        match c {
            std::path::Component::ParentDir => {
                abs.pop();
            }
            std::path::Component::CurDir => {}
            other => abs.push(other),
        }
    }
    let rel = abs.strip_prefix(root).ok()?;
    let mut parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    // A git worktree is a copy of the project: its folders are the project's.
    if parts.len() > 2 && matches!(parts[0].as_str(), ".worktrees" | "worktrees") {
        parts.drain(..2);
    }
    let last = parts.last().cloned().unwrap_or_default();
    let is_glob = last.contains('*');
    let file_like = is_glob || (last.contains('.') && !t.ends_with('/') && !abs.is_dir());
    let kind = if file_like {
        parts.pop();
        if is_glob && !last.trim_start_matches('*').contains('.') && last != "*.*" {
            // `*Controller*`: a name, no type.
            if last.trim_matches('*').is_empty() {
                None
            } else {
                Some("(by name)".to_owned())
            }
        } else {
            kind_of(&last)
        }
    } else {
        None
    };
    // Folders up to the first wildcard, two deep.
    let dirs: Vec<String> = parts
        .into_iter()
        .take_while(|p| !p.contains('*'))
        .take(2)
        .collect();
    let dir = if dirs.is_empty() {
        "(whole project)".to_owned()
    } else {
        dirs.join("/")
    };
    Some(match kind {
        Some(k) if k.starts_with('(') => format!("{dir}  {k}"),
        Some(k) => format!("{dir}  *.{k}"),
        None => dir,
    })
}

/// The issues sent for this project's gaps: file type → issue URL.
pub fn sent(data_dir: &Path, root: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(data_dir.join(SENT))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v[root.display().to_string()].as_object().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| Some((k, v.as_str()?.to_owned())))
        .collect()
}

pub fn set_sent(data_dir: &Path, root: &Path, kind: &str, url: &str) -> std::io::Result<()> {
    let file = data_dir.join(SENT);
    let mut all: Value = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    all[root.display().to_string()][kind] = json!(url);
    std::fs::write(
        file,
        serde_json::to_string_pretty(&all).unwrap_or_default() + "\n",
    )
}

/// A gap as an issue for the codemap repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// `*.blade.php`
    pub kind: String,
    pub title: String,
    pub body: String,
    /// The line that tells the same gap from another project.
    pub marker: String,
    /// The comment when that gap already has an open issue.
    pub again: String,
}

/// The issue for one file type of a report: how often it was searched and
/// what kind of question was asked, and the owner's own `note`. Nothing of
/// the project goes in: no folder, no file name, no search pattern.
/// `what` is a file type (`*.md`) or an area (`docs  *.md`).
pub fn issue(report: &Value, what: &str, days: i64, note: Option<&str>) -> Result<Issue, String> {
    let kind = what
        .split_whitespace()
        .rev()
        .find(|w| w.starts_with("*."))
        .ok_or(
            "pass a file type (`*.md`) or an area with one (`docs *.md`): a folder alone is the project's own and is never sent",
        )?
        .to_owned();
    let t = &report["file_types"][&kind];
    let searches = t["searches"].as_u64().unwrap_or(0);
    if searches == 0 {
        return Err(format!(
            "no searches in `{kind}` files are logged for this project in the last {days} days"
        ));
    }
    let asked: Vec<String> = t["kinds_of_question"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, n)| {
            let what = match k.as_str() {
                "definition" => "for a definition",
                "name" => "for a name",
                "files" => "for files by name",
                _ => "for other text",
            };
            format!("{n} {what}")
        })
        .collect();
    let (state, title) = match t["codemap"].as_str().unwrap_or_default() {
        "read" => (
            "codemap reads this file type; the assistant searched it anyway.",
            format!("Searched anyway: `{kind}` files"),
        ),
        _ => (
            "codemap does not read this file type.",
            format!("Not covered: `{kind}` files"),
        ),
    };
    let seen = format!(
        "{searches} searches in {} session(s) over {days} days in one project ({})",
        t["sessions"],
        asked.join(", ")
    );
    let version = env!("CARGO_PKG_VERSION");
    let platform = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    // One word no other issue holds, so the search for it finds this file
    // type's issue and no other (`*.php` must not find `*.blade.php`).
    let id = kind.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let marker = format!("codemap-gap-{id:016x}");
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    let mut body = format!(
        "After a codemap tool had run, an AI assistant still searched `{kind}` files with grep / find.\n\n- **File type:** `{kind}`. {state}\n- **Seen:** {seen}\n- **codemap:** {version} · {platform}\n"
    );
    if let Some(n) = note {
        body.push_str(&format!("\n**What was needed (the owner's words):** {n}\n"));
    }
    body.push_str(&format!(
        "\nSent with `codemap gaps --send`. It holds counts only: no folder, file name or search text of the project.\n\n<!-- codemap-gap: {kind} {marker} -->\n"
    ));
    let mut again = format!("Seen in another project: {seen} · codemap {version} · {platform}.\n");
    if let Some(n) = note {
        again.push_str(&format!("\n**What was needed (the owner's words):** {n}\n"));
    }
    Ok(Issue {
        kind,
        title,
        body,
        marker,
        again,
    })
}

fn ignored_file(data_dir: &Path) -> PathBuf {
    data_dir.join(IGNORED)
}

fn ignored(data_dir: &Path, root: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(ignored_file(data_dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v[root.display().to_string()].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect()
}

/// Sets an area aside (`set` true) or brings it back.
pub fn set_ignored(data_dir: &Path, root: &Path, area: &str, set: bool) -> std::io::Result<()> {
    let mut all: Value = std::fs::read_to_string(ignored_file(data_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let mut list = ignored(data_dir, root);
    let area = norm(area);
    if set {
        list.insert(area);
    } else {
        list.remove(&area);
    }
    all[root.display().to_string()] = json!(list);
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(
        ignored_file(data_dir),
        serde_json::to_string_pretty(&all).unwrap_or_default() + "\n",
    )
}

/// The areas searched without codemap, most searched first. `reads` says
/// whether codemap reads that kind of file at all (`null`: a folder of
/// mixed files); `not_scanned` that the folder is outside `[scan] include`.
pub fn report(data_dir: &Path, root: &Path, since: &str, unscanned: &[String]) -> Value {
    #[derive(Default)]
    struct Area {
        searches: u64,
        sessions: BTreeSet<String>,
        shapes: BTreeMap<String, u64>,
        patterns: BTreeMap<String, u64>,
        last: String,
    }
    let text = std::fs::read_to_string(data_dir.join(LOG)).unwrap_or_default();
    let set_aside = ignored(data_dir, root);
    let mut areas: BTreeMap<String, Area> = BTreeMap::new();
    let (mut total, mut sessions) = (0u64, BTreeSet::new());
    // `not_read` (a file type codemap does not read), `not_scanned` (a folder
    // outside the scan), `read`, or `mixed` (a folder, any file in it).
    let status_of = |key: &str| -> &'static str {
        let (dir, kind) = match key.split_once("  ") {
            Some((d, k)) => (d, Some(k)),
            None => (key, None),
        };
        let first = dir.split('/').next().unwrap_or_default();
        if unscanned.iter().any(|u| u.trim_end_matches('/') == first) {
            return "not_scanned";
        }
        match kind.and_then(|k| k.strip_prefix("*.")) {
            Some(k) if codemap_core::scan::reads_file(&format!("x.{k}")) => "read",
            Some(_) => "not_read",
            None => "mixed",
        }
    };
    // Each search once: outside what codemap reads, or by what it asked.
    let mut totals: BTreeMap<&'static str, u64> = BTreeMap::new();
    // Per file type, whatever the folder: the part of a gap that says
    // nothing about the project.
    let mut types: BTreeMap<String, Area> = BTreeMap::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = v["at"].as_str().unwrap_or_default();
        let Some(cwd) = v["cwd"].as_str().map(PathBuf::from) else {
            continue;
        };
        if at < since || !cwd.starts_with(root) {
            continue;
        }
        let mut targets: Vec<&str> = v["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if targets.is_empty() {
            targets.push(".");
        }
        // One search counts once per area it looked in.
        let keys: BTreeSet<String> = targets
            .iter()
            .filter_map(|t| area_of(t, &cwd, root))
            .collect();
        if keys.is_empty() {
            continue;
        }
        total += 1;
        let outside = keys
            .iter()
            .any(|k| matches!(status_of(k), "not_read" | "not_scanned"));
        let asked = match v["shape"].as_str().unwrap_or("text") {
            _ if outside => "not_covered",
            "definition" | "name" => "definition_or_name",
            "files" => "files",
            _ => "text",
        };
        *totals.entry(asked).or_default() += 1;
        let session = v["session"].as_str().unwrap_or_default().to_owned();
        sessions.insert(session.clone());
        let kinds: BTreeSet<&str> = keys
            .iter()
            .filter_map(|k| k.split_once("  ").map(|(_, kind)| kind))
            .filter(|k| k.starts_with("*."))
            .collect();
        for kind in kinds {
            let t = types.entry(kind.to_owned()).or_default();
            t.searches += 1;
            t.sessions.insert(session.clone());
            *t.shapes
                .entry(v["shape"].as_str().unwrap_or("text").to_owned())
                .or_default() += 1;
        }
        for key in keys {
            let a = areas.entry(key).or_default();
            a.searches += 1;
            a.sessions.insert(session.clone());
            *a.shapes
                .entry(v["shape"].as_str().unwrap_or("text").to_owned())
                .or_default() += 1;
            if let Some(p) = v["pattern"].as_str().filter(|p| !p.is_empty()) {
                *a.patterns.entry(p.to_owned()).or_default() += 1;
            }
            if at > a.last.as_str() {
                a.last = at.to_owned();
            }
        }
    }
    let mut rows: Vec<(String, Area)> = areas.into_iter().collect();
    rows.sort_by(|a, b| b.1.searches.cmp(&a.1.searches).then_with(|| a.0.cmp(&b.0)));
    let (mut shown, mut aside) = (Vec::new(), Vec::new());
    for (key, a) in rows {
        let status = status_of(&key);
        let mut examples: Vec<(&String, &u64)> = a.patterns.iter().collect();
        examples.sort_by(|x, y| y.1.cmp(x.1).then_with(|| x.0.cmp(y.0)));
        let row = json!({
            "area": key,
            "searches": a.searches,
            "sessions": a.sessions.len(),
            "kinds_of_question": a.shapes,
            "examples": examples.iter().take(3).map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
            "last": a.last,
            "codemap": status,
        });
        if set_aside.contains(&norm(&key)) {
            aside.push(row);
        } else {
            shown.push(row);
        }
    }
    let sent = sent(data_dir, root);
    let types: BTreeMap<String, Value> = types
        .into_iter()
        .map(|(kind, t)| {
            let row = json!({
                "searches": t.searches,
                "sessions": t.sessions.len(),
                "kinds_of_question": t.shapes,
                "codemap": status_of(&format!("x  {kind}")),
                "issue": sent.get(&kind),
            });
            (kind, row)
        })
        .collect();
    json!({
        "project": root.display().to_string(),
        "since": since,
        "file_types": types,
        "searches": total,
        "sessions": sessions.len(),
        "by_question": totals,
        "areas": shown,
        "set_aside": aside,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(cmd: &str) -> Search {
        search_of("Bash", &json!({ "command": cmd })).unwrap()
    }

    #[test]
    fn searches_are_read() {
        let s = bash("grep -rn \"function total\" -A15 app/Models/Order.php");
        assert_eq!(s.targets, ["app/Models/Order.php"]);
        assert_eq!(
            (s.pattern.as_str(), s.shape),
            ("function total", "definition")
        );

        let s = bash(
            "cd api && grep -rnE 'isProduction\\(\\)|isLocal' app config --include='*.php' | head -5",
        );
        assert_eq!(s.targets, ["api/app/*.php", "api/config/*.php"]);
        assert_eq!(s.shape, "text");

        let s = bash("rg -n OrderCard -g '*.vue' src");
        assert_eq!(s.targets, ["src/*.vue"]);
        assert_eq!(s.shape, "name");

        let s = bash("find resources/views -name '*.blade.php' -type f 2>/dev/null");
        assert_eq!(s.targets, ["resources/views/*.blade.php"]);
        assert_eq!(s.shape, "files");

        let s = bash("git grep -n 'x-alert' -- resources/views");
        assert_eq!(s.targets, ["resources/views"]);

        // A type left out, a type by name, a redirection, a folder entered first.
        assert_eq!(bash("rg Order app -g '!*.blade.php'").targets, ["app"]);
        assert_eq!(bash("rg -t rust Store crates").targets, ["crates/*.rs"]);
        assert_eq!(
            bash("grep -rn --color foo app &>/dev/null").targets,
            ["app"]
        );
        assert_eq!(bash("rg -T js foo src 1>&2").targets, ["src"]);
        assert_eq!(bash("fd -t f order src").targets, ["src"]);
        assert_eq!(bash("cd api && grep -rn x app").targets, ["api/app"]);
        assert_eq!(bash("cd api && rg x").targets, ["api"]);
        // What was searched for may start with `<`.
        let s = bash("grep -rn '<x-alert' resources/views 2>/dev/null");
        assert_eq!(
            (s.pattern.as_str(), s.targets),
            ("<x-alert", vec!["resources/views".to_owned()])
        );
        let s = search_of(
            "Grep",
            &json!({ "pattern": "x", "path": "app", "glob": "**/*.php" }),
        )
        .unwrap();
        assert_eq!(s.targets, ["app/**/*.php"]);

        assert!(search_of("Bash", &json!({ "command": "ls -la" })).is_none());
        let s = search_of(
            "Grep",
            &json!({ "pattern": "class Order", "path": "app", "glob": "*.php" }),
        )
        .unwrap();
        assert_eq!(s.targets, ["app/*.php"]);
        let s = search_of(
            "Glob",
            &json!({ "pattern": "resources/views/**/*.blade.php" }),
        )
        .unwrap();
        assert_eq!(s.targets, ["resources/views/**/*.blade.php"]);
    }

    #[test]
    fn areas_are_folder_and_type() {
        let root = Path::new("/p");
        let a = |t: &str| area_of(t, root, root).unwrap();
        assert_eq!(
            a("resources/views/orders/index.blade.php"),
            "resources/views  *.blade.php"
        );
        assert_eq!(
            a("resources/views/**/*.blade.php"),
            "resources/views  *.blade.php"
        );
        assert_eq!(a("app/Models/Order.php"), "app/Models  *.php");
        assert_eq!(a("src/*.vue"), "src  *.vue");
        assert_eq!(a("tests/Feature/cart.test.ts"), "tests/Feature  *.test.ts");
        assert_eq!(a("app"), "app");
        assert_eq!(a("."), "(whole project)");
        assert_eq!(a("*.md"), "(whole project)  *.md");
        assert_eq!(a("app/*Controller*"), "app  (by name)");
        assert_eq!(a("/p/config/app.php"), "config  *.php");
        // Outside the project: not this project's gap.
        assert_eq!(area_of("/etc/hosts", root, root), None);
        assert_eq!(area_of("$f", root, root), None);
        assert_eq!(area_of("../other/src", root, root), None);
        assert_eq!(a("app/../config/app.php"), "config  *.php");
        assert_eq!(a("app/**/*.php"), "app  *.php");
        // Words the project chose stay out of the kind.
        assert_eq!(a(".env.acmecorp"), "(whole project)  *.env");
        assert_eq!(a("deploy.clientx.sh"), "(whole project)  *.sh");
        assert_eq!(
            a(".worktrees/fix-1/app/Models/Order.php"),
            "app/Models  *.php"
        );
        assert_eq!(
            area_of("views/a.blade.php", Path::new("/p/resources"), root).unwrap(),
            "resources/views  *.blade.php"
        );
    }

    #[test]
    fn report_counts_areas_and_sets_some_aside() {
        let data = tempfile::tempdir().unwrap();
        let root = Path::new("/p");
        for (session, cmd) in [
            ("s1", "grep -rn '@include' resources/views"),
            (
                "s1",
                "grep -n 'x-alert' resources/views/orders/index.blade.php",
            ),
            ("s2", "find resources/views -name '*.blade.php'"),
            ("s2", "grep -rn color resources/css/app.css"),
        ] {
            record(
                data.path(),
                session,
                Some(root),
                "Bash",
                &json!({ "command": cmd }),
            );
        }
        // Another project's search is not counted here.
        record(
            data.path(),
            "s3",
            Some(Path::new("/other")),
            "Bash",
            &json!({ "command": "grep -rn x app" }),
        );
        let r = report(data.path(), root, "2000-01-01T00:00:00Z", &[]);
        assert_eq!(r["searches"], 4, "{r}");
        assert_eq!(r["sessions"], 2, "{r}");
        let areas = r["areas"].as_array().unwrap();
        assert_eq!(areas[0]["area"], "resources/views  *.blade.php", "{r}");
        assert_eq!(areas[0]["searches"], 2, "{r}");
        assert_eq!(areas[0]["codemap"], "read", "{r}");
        let css = areas
            .iter()
            .find(|a| a["area"] == "resources/css  *.css")
            .unwrap();
        assert_eq!(css["codemap"], "not_read", "{r}");
        assert_eq!(r["by_question"]["not_covered"], 1, "{r}");

        // The issue for a file type: counts only, nothing of the project.
        let i = issue(
            &r,
            "resources/views  *.blade.php",
            30,
            Some("which controller renders a view"),
        )
        .unwrap();
        assert_eq!(i.title, "Searched anyway: `*.blade.php` files");
        assert!(
            i.marker.starts_with("codemap-gap-") && i.body.contains(&i.marker),
            "{}",
            i.body
        );
        assert_ne!(i.marker, issue(&r, "*.css", 30, None).unwrap().marker);
        assert!(
            i.body.contains("2 searches in 2 session(s) over 30 days"),
            "{}",
            i.body
        );
        assert!(
            i.body.contains("which controller renders a view"),
            "{}",
            i.body
        );
        for private in ["resources", "views", "x-alert", "orders", "/p"] {
            assert!(
                !i.body.contains(private) && !i.again.contains(private),
                "{private}: {}",
                i.body
            );
        }
        assert_eq!(
            issue(&r, "*.css", 30, None).unwrap().title,
            "Not covered: `*.css` files"
        );
        assert!(issue(&r, "resources/views", 30, None).is_err());
        assert!(issue(&r, "*.md", 30, None).is_err());
        set_sent(data.path(), root, "*.css", "https://example.test/1").unwrap();
        let again = report(data.path(), root, "2000-01-01T00:00:00Z", &[]);
        assert_eq!(
            again["file_types"]["*.css"]["issue"], "https://example.test/1",
            "{again}"
        );

        set_ignored(data.path(), root, "resources/views", true).unwrap();
        let folder = report(data.path(), root, "2000-01-01T00:00:00Z", &[]);
        assert!(
            folder["set_aside"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["area"] == "resources/views"),
            "{folder}"
        );
        set_ignored(data.path(), root, "resources/views", false).unwrap();
        set_ignored(data.path(), root, "resources/css *.css", true).unwrap();
        let r = report(data.path(), root, "2000-01-01T00:00:00Z", &[]);
        assert!(
            r["areas"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["area"] != "resources/css  *.css")
        );
        assert_eq!(r["set_aside"][0]["area"], "resources/css  *.css", "{r}");
        set_ignored(data.path(), root, "resources/css  *.css", false).unwrap();
        let r = report(data.path(), root, "2000-01-01T00:00:00Z", &[]);
        assert_eq!(r["set_aside"].as_array().unwrap().len(), 0, "{r}");
    }
}
