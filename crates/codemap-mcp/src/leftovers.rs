//! Mentions left behind after a deletion: classes, components, functions and
//! methods deleted (or renamed) on the branch that are still named in code,
//! tests, comments or docs.

use std::collections::BTreeSet;
use std::path::Path;

use codemap_core::git::Change;

/// Names too common to search for: they would match unrelated text.
const COMMON: &[&str] = &[
    "index", "default", "update", "create", "delete", "store", "show", "edit", "handle", "render",
    "setup", "string", "number", "boolean", "object", "value", "items", "data",
];

/// A name specific enough to search for as a whole word.
fn searchable(name: &str) -> bool {
    name.len() >= 5
        && !COMMON.contains(&name.to_lowercase().as_str())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && (name.chars().skip(1).any(|c| c.is_ascii_uppercase()) || name.contains('_'))
}

/// `class Foo`, `function bar`, `export const baz`, … in a file's text.
fn declared(path: &str, text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let php = path.ends_with(".php");
    for line in text.lines() {
        let l = line.trim_start();
        let words: Vec<&str> = l
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|w| !w.is_empty())
            .collect();
        for w in words.windows(2) {
            let decl = if php {
                matches!(w[0], "class" | "interface" | "trait" | "enum" | "function")
            } else {
                matches!(w[0], "function" | "class" | "interface" | "type" | "enum")
                    || (matches!(w[0], "const" | "let") && l.starts_with("export"))
            };
            if decl {
                out.insert(w[1].to_owned());
            }
        }
    }
    out
}

/// Every name a file defines, local variables included (`const x =`,
/// `let x`, destructured `{ a, b }`): a gone name defined anywhere still
/// exists in another form.
fn defined(path: &str, text: &str) -> BTreeSet<String> {
    let mut out = declared(path, text);
    for line in text.lines() {
        let l = line.trim_start();
        for kw in ["const ", "let ", "var ", "$"] {
            let Some(rest) = l.strip_prefix(kw) else {
                continue;
            };
            let head = rest.split(['=', ':']).next().unwrap_or(rest);
            for w in head.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                if !w.is_empty() {
                    out.insert(w.to_owned());
                }
            }
        }
    }
    out
}

/// The name a file goes by: a component or class file's stem.
fn file_name(path: &str) -> Option<String> {
    let file = path.rsplit('/').next()?;
    let stem = file.split('.').next()?;
    let source = [".php", ".vue", ".tsx", ".jsx", ".ts", ".js"]
        .iter()
        .any(|e| file.ends_with(e));
    (source && !file.ends_with(".blade.php")).then(|| stem.to_owned())
}

/// Names gone from the branch: deleted or renamed files and what they
/// declared, and declarations removed from modified files.
pub fn gone(root: &Path, from: &str, changes: &[Change]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |name: String, why: String| {
        if searchable(&name) && !out.iter().any(|(n, _)| *n == name) {
            out.push((name, why));
        }
    };
    for c in changes {
        let old_path = match c.status {
            'D' => c.path.as_str(),
            'R' => c.from.as_deref().unwrap_or(&c.path),
            'M' => c.path.as_str(),
            _ => continue,
        };
        if crate::usages::is_test_path(old_path) || old_path.ends_with(".md") {
            continue;
        }
        let Some(before) = codemap_core::git::show(root, from, old_path) else {
            continue;
        };
        let before_names = declared(old_path, &before);
        let now_names = if c.status == 'D' {
            BTreeSet::new()
        } else {
            declared(
                &c.path,
                &std::fs::read_to_string(root.join(&c.path)).unwrap_or_default(),
            )
        };
        let how = match c.status {
            'D' => format!("{old_path} was deleted"),
            'R' => format!("{old_path} was renamed to {}", c.path),
            _ => format!("removed from {}", c.path),
        };
        if c.status != 'M'
            && let Some(stem) = file_name(old_path)
            && Some(&stem) != file_name(&c.path).as_ref().filter(|_| c.status == 'R')
        {
            add(stem, how.clone());
        }
        for n in before_names.difference(&now_names) {
            add(n.clone(), how.clone());
        }
    }
    out
}

const TEXT: &[&str] = &[
    "php", "js", "ts", "tsx", "jsx", "vue", "mjs", "cjs", "md", "mdx", "txt", "yml", "yaml",
    "json", "neon", "xml", "html",
];

/// Where a line is: a test, a doc, a comment, or code.
fn place(path: &str, line: &str) -> &'static str {
    let t = line.trim_start();
    if path.ends_with(".md")
        || path.ends_with(".mdx")
        || path.ends_with(".txt")
        || path.starts_with("docs/")
    {
        "doc"
    } else if crate::usages::is_test_path(path) {
        "test"
    } else if t.starts_with("//")
        || t.starts_with('#')
        || t.starts_with('*')
        || t.starts_with("/*")
        || t.starts_with("<!--")
        || t.starts_with("{{--")
    {
        "comment"
    } else {
        "code"
    }
}

fn whole_word(line: &str, name: &str) -> bool {
    let b = line.as_bytes();
    line.match_indices(name).any(|(i, _)| {
        let before = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        let j = i + name.len();
        let after = j >= b.len() || !(b[j].is_ascii_alphanumeric() || b[j] == b'_');
        before && after
    })
}

/// A gone name still mentioned: `(name, why, [(place, "file:line")])`.
pub type Mention = (String, String, Vec<(&'static str, String)>);

/// Lines still naming a gone name.
pub fn mentions(root: &Path, gone: &[(String, String)]) -> Vec<Mention> {
    if gone.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<Vec<(&'static str, String)>> = vec![Vec::new(); gone.len()];
    // A name declared somewhere now was moved, or is another class's own
    // method of the same name: not a leftover.
    let mut declared_now = vec![false; gone.len()];
    let walker = ignore::WalkBuilder::new(root)
        .require_git(false)
        .hidden(true)
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            !matches!(
                n.as_ref(),
                "vendor"
                    | "node_modules"
                    | ".git"
                    | "storage"
                    | "dist"
                    | ".nuxt"
                    | ".output"
                    | ".next"
            )
        })
        .build();
    // The files to read, then read and searched on all cores.
    let mut paths: Vec<(std::path::PathBuf, String, bool)> = Vec::new();
    for entry in walker.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let Ok(rel) = p.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_uppercase();
        if !TEXT.contains(&ext)
            || name.starts_with("CHANGELOG")
            || rel.ends_with(".lock")
            || rel.ends_with("lock.json")
            || rel.contains(".min.")
        {
            continue;
        }
        if entry
            .metadata()
            .map(|m| m.len() > 1_000_000)
            .unwrap_or(true)
        {
            continue;
        }
        let code = !matches!(
            ext,
            "md" | "mdx" | "txt" | "json" | "yml" | "yaml" | "neon" | "xml" | "html"
        );
        paths.push((p.to_path_buf(), rel, code));
    }
    paths.sort_by(|a, b| a.1.cmp(&b.1));
    use rayon::prelude::*;
    // Per file: (names declared there, per name its hits).
    type FileHits = (Vec<usize>, Vec<(usize, Vec<(&'static str, String)>)>);
    let per_file: Vec<FileHits> = paths
        .par_iter()
        .map(|(p, rel, code)| {
            let Ok(text) = std::fs::read_to_string(p) else {
                return (Vec::new(), Vec::new());
            };
            if !gone.iter().any(|(n, _)| text.contains(n.as_str())) {
                return (Vec::new(), Vec::new());
            }
            let here = if *code {
                defined(rel, &text)
            } else {
                BTreeSet::new()
            };
            let mut declared = Vec::new();
            let mut hits = Vec::new();
            for (i, (n, _)) in gone.iter().enumerate() {
                if here.contains(n) {
                    declared.push(i);
                }
                if !text.contains(n.as_str()) {
                    continue;
                }
                let mut h = Vec::new();
                for (ln, line) in text.lines().enumerate() {
                    if whole_word(line, n) {
                        h.push((place(rel, line), format!("{rel}:{}", ln + 1)));
                        if h.len() >= 10 {
                            break;
                        }
                    }
                }
                if !h.is_empty() {
                    hits.push((i, h));
                }
            }
            (declared, hits)
        })
        .collect();
    for (declared, hits) in per_file {
        for i in declared {
            declared_now[i] = true;
        }
        for (i, h) in hits {
            for x in h {
                if found[i].len() < 10 {
                    found[i].push(x);
                }
            }
        }
    }
    gone.iter()
        .zip(found)
        .zip(declared_now)
        .filter(|((_, f), now)| !f.is_empty() && !now)
        .map(|(((n, why), f), _)| (n.clone(), why.clone(), f))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_worth_searching() {
        assert!(searchable("UnscheduledItem"));
        assert!(searchable("fetchOrders"));
        assert!(searchable("order_total"));
        assert!(!searchable("refund"));
        assert!(!searchable("Index"));
        assert!(!searchable("item"));
    }

    #[test]
    fn declarations_and_places() {
        let php = declared(
            "app/X.php",
            "class OrderMailer {\n  public function sendReceipt() {}\n}",
        );
        assert!(php.contains("OrderMailer") && php.contains("sendReceipt"));
        let js = declared(
            "a.ts",
            "export const useCartApi = () => {}\nexport function fetchCart() {}\nconst local = 1",
        );
        assert!(js.contains("useCartApi") && js.contains("fetchCart") && !js.contains("local"));
        assert_eq!(place("app/X.php", "  // uses OrderMailer"), "comment");
        assert_eq!(place("tests/Feature/XTest.php", "new OrderMailer"), "test");
        assert_eq!(place("docs/a.md", "OrderMailer"), "doc");
        assert!(whole_word("new OrderMailer();", "OrderMailer"));
        assert!(!whole_word("new OrderMailerV2();", "OrderMailer"));
    }
}
