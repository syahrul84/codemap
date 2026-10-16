//! New type errors only. Claude runs the project's type check as usual
//! (`vue-tsc --noEmit`, `nuxi typecheck`, `tsc`, PHPStan with
//! `--error-format=raw`) and passes the output to `verify` as `type_errors`.
//! The first time, it becomes the starting point; after that only errors that
//! were not there before are reported. Line numbers are ignored when
//! comparing (they move when code above changes): an error is its file, code
//! and message, counted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::Project;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError {
    pub file: String,
    pub line: u32,
    pub code: String,
    pub message: String,
}

impl TypeError {
    fn key(&self) -> String {
        format!("{}\t{}\t{}", self.file, self.code, self.message)
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC [ … letter
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// A path as the project sees it: relative, `/` separators.
fn relative(root: &Path, file: &str) -> String {
    let f = file.trim().replace('\\', "/");
    let f = f.trim_start_matches("./");
    let r = root.to_string_lossy().replace('\\', "/");
    let f = f
        .strip_prefix(&format!("{}/", r.trim_end_matches('/')))
        .unwrap_or(f);
    // Docker paths such as /var/www/html/app/X.php: keep from a known folder.
    if f.starts_with('/') {
        for top in [
            "/app/",
            "/src/",
            "/components/",
            "/pages/",
            "/resources/",
            "/tests/",
            "/server/",
        ] {
            if let Some(i) = f.find(top) {
                return f[i + 1..].to_owned();
            }
        }
    }
    f.to_owned()
}

/// Errors in type-check output: TypeScript (`file(1,2): error TS123: …` and
/// `file:1:2 - error TS123: …`) and PHPStan raw (`file:12:message`).
pub fn parse(root: &Path, output: &str) -> Vec<TypeError> {
    let text = strip_ansi(output);
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim_end();
        // `src/a.ts(12,5): error TS2322: Type …`
        if let Some((loc, rest)) = l.split_once("): error ")
            && let Some((file, pos)) = loc.rsplit_once('(')
        {
            let (code, msg) = rest.split_once(": ").unwrap_or(("", rest));
            out.push(TypeError {
                file: relative(root, file),
                line: pos
                    .split(',')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0),
                code: code.trim().to_owned(),
                message: msg.trim().to_owned(),
            });
            continue;
        }
        // `src/a.vue:12:5 - error TS2322: Type …`
        if let Some((loc, rest)) = l.split_once(" - error ") {
            let mut parts = loc.rsplitn(3, ':');
            let (_col, ln, file) = (parts.next(), parts.next(), parts.next());
            if let (Some(file), Some(ln)) = (file, ln) {
                let (code, msg) = rest.split_once(": ").unwrap_or(("", rest));
                out.push(TypeError {
                    file: relative(root, file),
                    line: ln.trim().parse().unwrap_or(0),
                    code: code.trim().to_owned(),
                    message: msg.trim().to_owned(),
                });
                continue;
            }
        }
        // PHPStan raw: `/path/app/X.php:12:Message`
        if let Some(i) = l.find(".php:") {
            let file = &l[..i + 4];
            let rest = &l[i + 5..];
            if let Some((ln, msg)) = rest.split_once(':')
                && let Ok(n) = ln.trim().parse::<u32>()
            {
                out.push(TypeError {
                    file: relative(root, file),
                    line: n,
                    code: "phpstan".into(),
                    message: msg.trim().to_owned(),
                });
            }
        }
    }
    out
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Baseline {
    saved_at: String,
    total: usize,
    /// key → count
    errors: BTreeMap<String, usize>,
}

fn baseline_path(project: &Project) -> PathBuf {
    project.index_path.with_file_name("type-baseline.json")
}

fn counts(errors: &[TypeError]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for e in errors {
        *m.entry(e.key()).or_insert(0) += 1;
    }
    m
}

fn save(project: &Project, errors: &[TypeError]) {
    let b = Baseline {
        saved_at: jiff::Timestamp::now().to_string(),
        total: errors.len(),
        errors: counts(errors),
    };
    if let Ok(t) = serde_json::to_string(&b) {
        let _ = std::fs::write(baseline_path(project), t);
    }
}

/// The comparison with the starting point, for `verify`'s answer.
pub fn compare(project: &Project, output: &str, save_as_baseline: bool) -> Value {
    let errors = parse(&project.root, output);
    if errors.is_empty() && !output.trim().is_empty() && !output.contains("Found 0 errors") {
        let looks_clean = output.to_lowercase().contains("no errors") || output.trim().len() < 40;
        if !looks_clean {
            return json!({
                "note": "no errors could be read from this output; pass the plain output of vue-tsc / nuxi typecheck / tsc, or PHPStan with --error-format=raw",
            });
        }
    }
    let previous: Option<Baseline> = std::fs::read_to_string(baseline_path(project))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let Some(base) = previous else {
        save(project, &errors);
        return json!({
            "baseline_saved": true,
            "current_total": errors.len(),
            "note": format!("{} errors saved as the starting point; from now on only new ones are reported", errors.len()),
        });
    };
    // Errors beyond what the starting point had, per file + code + message.
    let mut left = base.errors.clone();
    let mut new = Vec::new();
    for e in &errors {
        match left.get_mut(&e.key()) {
            Some(n) if *n > 0 => *n -= 1,
            _ => new.push(e),
        }
    }
    let fixed: usize = left.values().sum();
    let mut v = json!({
        "new_total": new.len(),
        "new": new.iter().take(50).map(|e| format!("{}:{} {} {}", e.file, e.line, e.code, e.message)).collect::<Vec<_>>(),
        "fixed_since_baseline": fixed,
        "current_total": errors.len(),
        "baseline_total": base.total,
        "baseline_saved_at": base.saved_at,
    });
    if new.is_empty() {
        v["note"] = json!("no new type errors");
    }
    if save_as_baseline {
        save(project, &errors);
        v["baseline_saved"] = json!(true);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_typescript_and_phpstan_output() {
        let root = Path::new("/workspace/consumer");
        let out = "\u{1b}[96mapp/pages/cart.vue\u{1b}[0m:12:5 - \u{1b}[91merror\u{1b}[0m TS2322: Type 'string' is not assignable to type 'number'.\n\n12   total.value = '1'\n\
                   src/a.ts(3,1): error TS2304: Cannot find name 'foo'.\n\
                   /var/www/html/app/Models/Order.php:44:Method App\\Models\\Order::x() has no return type.\n\
                   Found 3 errors.";
        let e = parse(root, out);
        assert_eq!(e.len(), 3, "{e:?}");
        assert_eq!(
            (e[0].file.as_str(), e[0].line, e[0].code.as_str()),
            ("app/pages/cart.vue", 12, "TS2322")
        );
        assert_eq!(e[1].message, "Cannot find name 'foo'.");
        assert_eq!(
            (e[2].file.as_str(), e[2].line),
            ("app/Models/Order.php", 44)
        );
    }

    #[test]
    fn only_new_errors_after_the_starting_point() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
            config: Default::default(),
            id: "t".into(),
            index_path: dir.path().join("index.db"),
            app: None,
        };
        let first = "a.ts(1,1): error TS1: old one.\na.ts(9,1): error TS1: old one.\nb.ts(2,1): error TS2: gone later.";
        assert_eq!(compare(&project, first, false)["baseline_saved"], true);
        // Lines moved, one old error fixed, one new.
        let now = "a.ts(5,1): error TS1: old one.\na.ts(20,1): error TS1: old one.\nc.ts(7,3): error TS3: brand new.";
        let v = compare(&project, now, false);
        assert_eq!(v["new_total"], 1);
        assert_eq!(v["new"][0], "c.ts:7 TS3 brand new.");
        assert_eq!(v["fixed_since_baseline"], 1);
    }
}
