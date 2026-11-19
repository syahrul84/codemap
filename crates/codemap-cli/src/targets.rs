//! What a search names: file patterns and paths. A search that only looks at
//! files codemap does not read as code (JSON, env, YAML, config, Markdown,
//! another language) or at folders the index holds nothing of is one codemap
//! cannot answer, so the search hook lets it through.

use std::path::{Path, PathBuf};

/// Whether the index holds this file, or any file under this folder; `None`
/// when that cannot be told (no index, a path outside the project).
pub type Indexed<'a> = &'a dyn Fn(&Path) -> Option<bool>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Code,
    NotCode,
    Unknown,
}

/// One token of a shell command: a word (quotes taken off) or an operator
/// that ends a command (`;`, `&&`, `||`, a new line) or a pipeline stage (`|`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(String),
    /// `|`.
    Pipe,
    /// `;`, `&&`, `||`, a new line.
    End,
}

/// Splits a shell command into words and operators; an operator inside
/// quotes is part of its word (`grep "a|b" file`).
pub fn tokens(cmd: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut has_word = false;
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars().peekable();
    let flush = |word: &mut String, has: &mut bool, out: &mut Vec<Token>| {
        if *has {
            out.push(Token::Word(std::mem::take(word)));
            *has = false;
        }
    };
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        word.push(n);
                    }
                } else {
                    word.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has_word = true;
                }
                '\\' => {
                    if let Some(n) = chars.next() {
                        word.push(n);
                        has_word = true;
                    }
                }
                ';' | '\n' => {
                    flush(&mut word, &mut has_word, &mut out);
                    out.push(Token::End);
                }
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    flush(&mut word, &mut has_word, &mut out);
                    out.push(Token::End);
                }
                '|' => {
                    flush(&mut word, &mut has_word, &mut out);
                    if chars.peek() == Some(&'|') {
                        chars.next();
                        out.push(Token::End);
                    } else {
                        out.push(Token::Pipe);
                    }
                }
                c if c.is_whitespace() => flush(&mut word, &mut has_word, &mut out),
                c => {
                    word.push(c);
                    has_word = true;
                }
            },
        }
    }
    flush(&mut word, &mut has_word, &mut out);
    out
}

/// File names without an extension that are configuration, not code: these
/// also as the start of a pattern (`docker-compose*`).
const CONFIG_PREFIXES: &[&str] = &["dockerfile", "docker-compose", "makefile"];
/// And these only as the whole name.
const CONFIG_NAMES: &[&str] = &[
    "procfile",
    "caddyfile",
    "jenkinsfile",
    "vagrantfile",
    "license",
    "readme",
    "changelog",
];

/// What a file name or pattern (`package.json`, `*.yml`, `src/**/*.vue`,
/// `.env*`) says about the files it names.
fn name_kind(word: &str) -> Kind {
    let name = word
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(word);
    if name.is_empty() || name == "." || name == ".." {
        return Kind::Unknown;
    }
    // `*.{json,yml}`: every alternative.
    if let (Some(a), Some(b)) = (name.find('{'), name.rfind('}'))
        && a < b
    {
        let kinds: Vec<Kind> = name[a + 1..b]
            .split(',')
            .map(|alt| name_kind(&format!("{}{alt}{}", &name[..a], &name[b + 1..])))
            .collect();
        return if kinds.contains(&Kind::Code) {
            Kind::Code
        } else if kinds.iter().all(|k| *k == Kind::NotCode) {
            Kind::NotCode
        } else {
            Kind::Unknown
        };
    }
    let lower = name.to_ascii_lowercase();
    // Dotfiles are settings (`.env`, `.gitignore`, `.eslintrc.js`), and the
    // scan does not read hidden files.
    if let Some(rest) = lower.strip_prefix('.')
        && rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        return Kind::NotCode;
    }
    let glob = |s: &str| s.contains(['*', '?', '[']);
    match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && !glob(ext) => {
            // The whole name when it is one file (`types.d.ts` is not read).
            let code = if glob(name) {
                codemap_core::scan::is_code_file(&format!("x.{ext}"))
            } else {
                codemap_core::scan::is_code_file(name)
            };
            if code { Kind::Code } else { Kind::NotCode }
        }
        _ if CONFIG_PREFIXES.iter().any(|c| lower.starts_with(c))
            || (!glob(name) && CONFIG_NAMES.contains(&lower.as_str())) =>
        {
            Kind::NotCode
        }
        _ => Kind::Unknown,
    }
}

fn resolve(word: &str, base: Option<&Path>) -> Option<PathBuf> {
    let p = Path::new(word);
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    if word.starts_with('~') {
        return None;
    }
    Some(base?.join(p))
}

/// What a path given to a search holds: decided by its name when that says
/// it, else by what is on disk and in the index.
fn target_kind(word: &str, base: Option<&Path>, indexed: Indexed<'_>) -> Kind {
    let by_name = name_kind(word);
    if by_name == Kind::NotCode || word.contains(['*', '?', '[']) {
        return match by_name {
            Kind::Unknown => Kind::Code,
            k => k,
        };
    }
    let Some(path) = resolve(word, base) else {
        return Kind::Code;
    };
    if path.is_file() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // A code file the index does not hold (build output, a copy in a
        // folder outside the scan) is one codemap cannot answer for.
        return if codemap_core::scan::is_code_file(&name) && indexed(&path) != Some(false) {
            Kind::Code
        } else {
            Kind::NotCode
        };
    }
    if path.is_dir() && indexed(&path) == Some(false) {
        return Kind::NotCode;
    }
    Kind::Code
}

/// ripgrep's `-t` names that are not the extension itself.
fn type_pattern(name: &str) -> String {
    let ext = match name {
        "rust" => "rs",
        "kotlin" => "kt",
        "typescript" => "ts",
        "javascript" => "js",
        "python" => "py",
        "ruby" => "rb",
        "markdown" => "md",
        other => other,
    };
    format!("*.{ext}")
}

/// The file patterns and paths a search command names.
#[derive(Debug, Default, PartialEq, Eq)]
struct Named {
    patterns: Vec<String>,
    targets: Vec<String>,
}

fn is_redirect(w: &str) -> bool {
    w.starts_with('>') || w.starts_with('<') || w.starts_with("2>") || w.starts_with("&>")
}

/// `grep`, `rg`, `ag`, `ack`, `git grep`: the words after the program's name.
fn grep_names(args: &[String]) -> Named {
    let mut n = Named::default();
    let mut pattern_given = false;
    let mut only_paths = false;
    let mut i = 0;
    while i < args.len() {
        let w = args[i].as_str();
        i += 1;
        if is_redirect(w) {
            if matches!(w, ">" | ">>" | "<" | "2>" | "&>") {
                i += 1;
            }
            continue;
        }
        if !only_paths && w == "--" {
            only_paths = true;
            continue;
        }
        if !only_paths && w.starts_with('-') && w.len() > 1 {
            if let Some((k, v)) = w.split_once('=') {
                match k {
                    "--include" | "--glob" | "--iglob" => n.patterns.push(v.to_owned()),
                    "--type" => n.patterns.push(type_pattern(v)),
                    "--regexp" | "--file" => pattern_given = true,
                    _ => {}
                }
                continue;
            }
            let mut value = || {
                let v = args.get(i).cloned();
                i += 1;
                v
            };
            match w {
                "--include" | "-g" | "--glob" | "--iglob" => n.patterns.extend(value()),
                "-t" | "--type" => n.patterns.extend(value().map(|v| type_pattern(&v))),
                "-e" | "--regexp" | "-f" | "--file" => {
                    value();
                    pattern_given = true;
                }
                "-A" | "-B" | "-C" | "-m" | "--max-count" | "--context" | "--after-context"
                | "--before-context" | "--exclude" | "--exclude-dir" | "--max-depth" | "-j"
                | "--threads" | "-M" | "--max-columns" | "-T" | "--type-not" | "--sort"
                | "--sortr" | "-G" => {
                    value();
                }
                // `-rnA 3`, `-rne pattern`: the last letter of a group takes
                // the next word (`-A3` carries its own).
                _ if !w.starts_with("--") && !w.contains(|c: char| c.is_ascii_digit()) => {
                    match w.chars().last() {
                        Some('A' | 'B' | 'C' | 'm') => {
                            value();
                        }
                        Some('e' | 'f') => {
                            value();
                            pattern_given = true;
                        }
                        Some('g') => n.patterns.extend(value()),
                        Some('t') => n.patterns.extend(value().map(|v| type_pattern(&v))),
                        _ => {}
                    }
                }
                _ => {}
            }
            continue;
        }
        if !pattern_given {
            pattern_given = true;
            continue;
        }
        n.targets.push(w.to_owned());
    }
    n
}

/// `find`: the paths before the first test, and the names it looks for.
fn find_names(args: &[String]) -> Named {
    let mut n = Named::default();
    let mut in_tests = false;
    let mut negated = false;
    let mut i = 0;
    while i < args.len() {
        let w = args[i].as_str();
        i += 1;
        if is_redirect(w) {
            if matches!(w, ">" | ">>" | "<" | "2>" | "&>") {
                i += 1;
            }
            continue;
        }
        if w.starts_with('-') || matches!(w, "(" | ")" | "!") {
            in_tests = true;
            match w {
                "!" | "-not" => {
                    negated = true;
                    continue;
                }
                "-name" | "-iname" => {
                    if let Some(v) = args.get(i)
                        && !negated
                    {
                        n.patterns.push(v.clone());
                    }
                    i += 1;
                }
                _ => {}
            }
            negated = false;
            continue;
        }
        if !in_tests {
            n.targets.push(w.to_owned());
        }
        negated = false;
    }
    n
}

/// `fd [pattern] [path…]`, with `-e ext`.
fn fd_names(args: &[String]) -> Named {
    let mut n = Named::default();
    let mut pattern_given = false;
    let mut glob = false;
    let mut i = 0;
    while i < args.len() {
        let w = args[i].as_str();
        i += 1;
        if is_redirect(w) {
            continue;
        }
        if w.starts_with('-') && w.len() > 1 {
            if let Some(v) = w.strip_prefix("--extension=") {
                n.patterns.push(format!("*.{v}"));
                continue;
            }
            match w {
                "-e" | "--extension" => {
                    n.patterns.extend(
                        args.get(i)
                            .map(|v| format!("*.{}", v.trim_start_matches('.'))),
                    );
                    i += 1;
                }
                "-g" | "--glob" => glob = true,
                "-t" | "--type" | "-d" | "--max-depth" | "-E" | "--exclude" | "-x" | "--exec" => {
                    i += 1
                }
                _ => {}
            }
            continue;
        }
        if !pattern_given {
            pattern_given = true;
            if glob {
                n.patterns.push(w.to_owned());
            }
            continue;
        }
        n.targets.push(w.to_owned());
    }
    n
}

fn no_code(n: &Named, base: Option<&Path>, indexed: Indexed<'_>) -> bool {
    let kinds: Vec<Kind> = n.patterns.iter().map(|p| name_kind(p)).collect();
    if kinds.contains(&Kind::Code) {
        return false;
    }
    if !kinds.is_empty() && kinds.iter().all(|k| *k == Kind::NotCode) {
        return true;
    }
    !n.targets.is_empty()
        && n.targets
            .iter()
            .all(|t| target_kind(t, base, indexed) == Kind::NotCode)
}

/// Whether a shell search (`program` with the words after it) only names
/// files that are not code, or folders the index holds nothing of.
pub fn shell_search_names_no_code(
    program: &str,
    args: &[String],
    base: Option<&Path>,
    indexed: Indexed<'_>,
) -> bool {
    let named = match program {
        "find" => find_names(args),
        "fd" => fd_names(args),
        _ => grep_names(args),
    };
    no_code(&named, base, indexed)
}

/// The same for the AI app's own search tools: file patterns (`glob`, a
/// type, or the Glob tool's pattern) and the folder searched.
pub fn tool_search_names_no_code(
    patterns: &[&str],
    path: Option<&str>,
    base: Option<&Path>,
    indexed: Indexed<'_>,
) -> bool {
    let named = Named {
        patterns: patterns.iter().map(|p| (*p).to_owned()).collect(),
        targets: path.into_iter().map(str::to_owned).collect(),
    };
    no_code(&named, base, indexed)
}

/// ripgrep's type name (the Grep tool's `type`) as a file pattern.
pub fn type_as_pattern(name: &str) -> String {
    type_pattern(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        tokens(s)
            .into_iter()
            .filter_map(|t| match t {
                Token::Word(w) => Some(w),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn quotes_keep_operators_inside_a_word() {
        assert_eq!(
            tokens(r#"grep -n "a|b; c" 'x y' z && ls | wc"#),
            vec![
                Token::Word("grep".into()),
                Token::Word("-n".into()),
                Token::Word("a|b; c".into()),
                Token::Word("x y".into()),
                Token::Word("z".into()),
                Token::End,
                Token::Word("ls".into()),
                Token::Pipe,
                Token::Word("wc".into()),
            ]
        );
    }

    #[test]
    fn names_say_what_a_file_is() {
        for n in [
            "package.json",
            ".env",
            ".env.example",
            ".env*",
            "*.yml",
            "docker/nginx/default.conf",
            "**/*.md",
            "*.{json,yaml}",
            "Dockerfile",
            "docker-compose.prod.yml",
            ".gitignore",
            "composer.lock",
            "notes.txt",
            "main.py",
            "types.d.ts",
        ] {
            assert_eq!(name_kind(n), Kind::NotCode, "{n}");
        }
        for n in [
            "app/Models/Order.php",
            "*.vue",
            "src/**/*.ts",
            "*.{js,json}",
            "x.blade.php",
            "Main.kt",
            "lib.rs",
        ] {
            assert_eq!(name_kind(n), Kind::Code, "{n}");
        }
        for n in ["app", "src/", ".", "*", "Order*", "foo.*"] {
            assert_eq!(name_kind(n), Kind::Unknown, "{n}");
        }
    }

    #[test]
    fn searches_of_files_that_are_not_code() {
        let none: Indexed<'_> = &|_| None;
        let grep = |c: &str| {
            let w = words(c);
            shell_search_names_no_code(&w[0], &w[1..], None, none)
        };
        assert!(grep("grep -n socket.io package.json composer.json"));
        assert!(grep("grep -rn 'echo' --include=*.json ."));
        assert!(grep("rg -n redis -g '*.yml' -g '*.conf'"));
        assert!(grep("rg -t json laravel-echo"));
        assert!(grep("grep -rnA 3 -e APP_KEY .env .env.example"));
        assert!(grep("grep -c '' README.md 2>/dev/null"));
        assert!(!grep("grep -rn Order app/"));
        assert!(!grep("grep -rn Order"));
        assert!(!grep("rg Order -g '*.php'"));
        assert!(!grep("grep -n x package.json app/Models/Order.php"));
        assert!(!grep("grep -rn x --include=*.json --include=*.php ."));
        assert!(grep("find . -name '*.conf' -not -path '*/node_modules/*'"));
        assert!(grep("find . -iname 'docker-compose*'"));
        assert!(!grep("find . -name '*.php'"));
        assert!(!grep("find app -type f"));
        assert!(!grep("find . -name '*.json' -o -name '*.ts'"));
        assert!(grep("fd -e yml -e yaml"));
        assert!(!grep("fd Order app"));
    }

    #[test]
    fn folders_the_index_holds_nothing_of() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("document/assets")).unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        std::fs::write(dir.path().join("document/assets/app.js"), "x").unwrap();
        std::fs::write(dir.path().join("app/a.js"), "x").unwrap();
        let root = dir.path().to_path_buf();
        let indexed: Indexed<'_> = &|p: &Path| Some(p.starts_with(root.join("app")));
        let grep = |c: &str| {
            let w = words(c);
            shell_search_names_no_code(&w[0], &w[1..], Some(dir.path()), indexed)
        };
        assert!(grep("grep -rn foo document/"));
        assert!(grep("grep -n foo document/assets/app.js"));
        assert!(grep("find document -type f"));
        assert!(!grep("grep -rn foo app"));
        assert!(!grep("grep -n foo app/a.js"));
        assert!(!grep("grep -rn foo document app"));
        // A folder that is not there, or when nothing is known: a code search.
        assert!(!grep("grep -rn foo nowhere/"));
        let unknown: Indexed<'_> = &|_| None;
        let w = words("grep -rn foo document/");
        assert!(!shell_search_names_no_code(
            &w[0],
            &w[1..],
            Some(dir.path()),
            unknown
        ));
    }
}
