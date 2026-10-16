//! What a release of the branch needs on the server, from what changed:
//! installs, migrations, caches to rebuild, workers to restart, apps to
//! rebuild, environment variables to set. And the project's own rules tied to
//! files (`[[rules]]` in `.codemap.toml`).

use std::collections::BTreeSet;
use std::path::Path;

use codemap_core::config::RuleConfig;
use codemap_core::git::Change;
use codemap_core::store::Store;
use serde_json::{Value, json};

use crate::tools::Project;

fn read(root: &Path, file: &str) -> String {
    std::fs::read_to_string(root.join(file)).unwrap_or_default()
}

/// `a, b, c and 4 more`.
fn some(files: &[&str]) -> String {
    let shown: Vec<&str> = files.iter().take(3).copied().collect();
    let more = files.len().saturating_sub(3);
    if more > 0 {
        format!("{} and {more} more", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

fn step(do_: &str, why: String) -> Value {
    json!({ "step": do_, "why": why })
}

/// `KEY=` lines of an env example file.
fn env_keys(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') {
                return None;
            }
            let (k, _) = l.split_once('=')?;
            let k = k.trim().trim_start_matches("export ").trim();
            (!k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                .then(|| k.to_owned())
        })
        .collect()
}

/// The install command of the app's package manager (lockfile in the app
/// or its parent folder).
fn js_install(root: &Path) -> (&'static str, &'static str) {
    for dir in [Some(root), root.parent()].into_iter().flatten() {
        if dir.join("pnpm-lock.yaml").exists() {
            return ("pnpm install --frozen-lockfile", "pnpm run build");
        }
        if dir.join("yarn.lock").exists() {
            return ("yarn install --frozen-lockfile", "yarn build");
        }
        if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
            return ("bun install --frozen-lockfile", "bun run build");
        }
        if dir.join("package-lock.json").exists() {
            return ("npm ci", "npm run build");
        }
    }
    ("npm install", "npm run build")
}

fn is_doc_or_test(path: &str) -> bool {
    crate::usages::is_test_path(path)
        || path.ends_with(".md")
        || path.starts_with("docs/")
        || path.starts_with(".github/")
        || path.starts_with(".claude/")
}

/// Server steps for one app's changes.
pub fn steps(store: &Store, project: &Project, from: &str, changes: &[Change]) -> Vec<Value> {
    let root = &project.root;
    let mut out = Vec::new();
    let paths = |pred: &dyn Fn(&Change) -> bool| -> Vec<&str> {
        changes
            .iter()
            .filter(|c| pred(c))
            .map(|c| c.path.as_str())
            .collect()
    };
    let composer = read(root, "composer.json");
    let laravel = root.join("artisan").is_file();

    if laravel {
        let deps = paths(&|c| c.path == "composer.json" || c.path == "composer.lock");
        if !deps.is_empty() {
            out.push(step(
                "composer install --no-dev --optimize-autoloader",
                format!("dependencies changed ({})", some(&deps)),
            ));
        } else {
            // Without an install, the class map must learn of added, deleted
            // and renamed classes.
            let moved = paths(&|c| {
                c.path.ends_with(".php")
                    && !is_doc_or_test(&c.path)
                    && !c.path.starts_with("database/migrations/")
                    && !c.path.starts_with("config/")
                    && !c.path.starts_with("routes/")
                    && !c.path.starts_with("lang/")
                    && !c.path.starts_with("resources/")
                    && matches!(c.status, 'A' | 'D' | 'R')
            });
            if !moved.is_empty() {
                out.push(step(
                    "composer dump-autoload --optimize",
                    format!("classes were added, deleted or renamed ({})", some(&moved)),
                ));
            }
        }
        let migrations = paths(&|c| c.path.starts_with("database/migrations/") && c.status == 'A');
        if !migrations.is_empty() {
            out.push(step(
                "php artisan migrate --force",
                format!(
                    "{} new migration(s): {}",
                    migrations.len(),
                    some(&migrations)
                ),
            ));
        }
        let cached = paths(&|c| {
            c.path.starts_with("config/")
                || c.path.starts_with("routes/")
                || c.path == "bootstrap/app.php"
                || c.path.starts_with("app/Providers/")
                || c.path.starts_with("app/Events/")
                || c.path.starts_with("app/Listeners/")
        });
        if !cached.is_empty() {
            out.push(step(
                "php artisan optimize",
                format!(
                    "config, routes or event listeners changed, which are cached ({})",
                    some(&cached)
                ),
            ));
        }
        if composer.contains("\"filament/filament\"") {
            let filament = paths(&|c| {
                c.path.starts_with("app/Filament/") && matches!(c.status, 'A' | 'D' | 'R')
            });
            if !filament.is_empty() {
                out.push(step(
                    "php artisan filament:optimize",
                    format!(
                        "admin pages or resources were added or removed ({})",
                        some(&filament)
                    ),
                ));
            }
        }
        let php = paths(&|c| c.path.ends_with(".php") && !is_doc_or_test(&c.path));
        if !php.is_empty() {
            let queued: i64 = store
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM symbol_refs WHERE relation = 'implements' AND target_fqn LIKE '%ShouldQueue'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if composer.contains("\"laravel/horizon\"") {
                out.push(step(
                    "php artisan horizon:terminate",
                    "PHP code changed; Horizon's workers keep running the old code until restarted"
                        .into(),
                ));
            } else if queued > 0 {
                out.push(step(
                    "php artisan queue:restart",
                    format!("PHP code changed and {queued} classes run on the queue; workers keep the old code until restarted"),
                ));
            }
            if composer.contains("\"laravel/octane\"") {
                out.push(step(
                    "php artisan octane:reload",
                    "PHP code changed; Octane keeps the app in memory".into(),
                ));
            } else {
                out.push(step(
                    "reload PHP-FPM",
                    format!(
                        "PHP code changed ({}); needed when OPcache does not re-check files (opcache.validate_timestamps=0)",
                        some(&php)
                    ),
                ));
            }
        }
    }

    // A JavaScript app (Vue, Nuxt, Next.js, Vite).
    let package = read(root, "package.json");
    let frontend = ["\"vue\"", "\"nuxt\"", "\"next\"", "\"react\"", "\"vite\""]
        .iter()
        .any(|k| package.contains(k));
    if frontend {
        let (install, build) = js_install(root);
        let deps = paths(&|c| {
            c.path == "package.json"
                || c.path.ends_with("lock.yaml")
                || c.path.ends_with("lock.json")
                || c.path == "yarn.lock"
                || c.path.starts_with("bun.lock")
        });
        if !deps.is_empty() {
            out.push(step(
                install,
                format!("dependencies changed ({})", some(&deps)),
            ));
        }
        let config_file = |p: &str| {
            let name = p.rsplit('/').next().unwrap_or(p);
            [
                "vite.config",
                "nuxt.config",
                "next.config",
                "tailwind.config",
                "postcss.config",
                "tsconfig",
                "app.config",
            ]
            .iter()
            .any(|c| name.starts_with(c))
        };
        let source = paths(&|c| {
            let p = c.path.as_str();
            let name = p.rsplit('/').next().unwrap_or(p);
            if is_doc_or_test(p) || name.starts_with('.') {
                return false;
            }
            if laravel {
                // Laravel + Vite: only what the bundler builds.
                (p.starts_with("resources/")
                    && !p.starts_with("resources/views/")
                    && !p.starts_with("resources/lang/"))
                    || config_file(p)
            } else {
                !p.ends_with(".php")
                    && !p.starts_with("database/")
                    && !name.starts_with("playwright.config")
                    && !name.starts_with("vitest.config")
                    && !name.starts_with("cypress.config")
            }
        });
        if !source.is_empty() {
            let server = package.contains("\"nuxt\"") || package.contains("\"next\"");
            out.push(step(
                build,
                format!(
                    "the app changed ({}); its built files must be rebuilt",
                    some(&source)
                ),
            ));
            out.push(step(
                if server {
                    "restart the app's Node server"
                } else {
                    "deploy the new build output"
                },
                if server {
                    "the server keeps serving the old build until restarted".into()
                } else {
                    "browsers load the files from the build folder".into()
                },
            ));
        }
    }

    // Environment variables new in the example file.
    for example in [".env.example", ".env.sample", ".env.dist"] {
        if !changes.iter().any(|c| c.path == example) {
            continue;
        }
        let now = env_keys(&read(root, example));
        let before = codemap_core::git::show(root, from, example)
            .map(|t| env_keys(&t))
            .unwrap_or_default();
        let added: Vec<&String> = now.difference(&before).collect();
        if !added.is_empty() {
            let public = added.iter().any(|k| {
                k.starts_with("NUXT_PUBLIC_")
                    || k.starts_with("NEXT_PUBLIC_")
                    || k.starts_with("VITE_")
            });
            out.push(step(
                &format!(
                    "set on the server: {}",
                    added
                        .iter()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                format!(
                    "new in {example}{}",
                    if public {
                        "; public variables are read at build time, so set them before building"
                    } else if laravel {
                        "; then php artisan optimize again"
                    } else {
                        ""
                    }
                ),
            ));
        }
    }
    out
}

fn status_word(s: char) -> &'static str {
    match s {
        'A' => "added",
        'D' => "deleted",
        'R' => "renamed",
        _ => "modified",
    }
}

fn rule_applies(rule: &RuleConfig, c: &Change) -> bool {
    match rule.when.as_deref().unwrap_or("changed") {
        "added" => c.status == 'A',
        "modified" => c.status == 'M',
        "deleted" => c.status == 'D',
        "renamed" => c.status == 'R',
        _ => true,
    }
}

/// The project's rules hit by these changes: `(rule, files)`.
pub fn rules_hit<'r>(
    root: &Path,
    rules: &'r [RuleConfig],
    changes: &[Change],
) -> Vec<(&'r RuleConfig, Vec<String>)> {
    let mut out = Vec::new();
    for rule in rules {
        if rule.files.is_empty() || rule.note.trim().is_empty() {
            continue;
        }
        let mut b = ignore::gitignore::GitignoreBuilder::new(root);
        for f in &rule.files {
            let _ = b.add_line(None, f);
        }
        let Ok(m) = b.build() else { continue };
        let files: Vec<String> = changes
            .iter()
            .filter(|c| rule_applies(rule, c))
            .filter(|c| {
                let p = c
                    .from
                    .as_deref()
                    .filter(|_| c.status == 'D')
                    .unwrap_or(&c.path);
                m.matched_path_or_any_parents(p, false).is_ignore()
            })
            .map(|c| format!("{} ({})", c.path, status_word(c.status)))
            .collect();
        if !files.is_empty() {
            out.push((rule, files));
        }
    }
    out
}

/// Rules for files changed outside git's view (change impact): the kind is
/// told by whether the file exists now and existed at `HEAD`.
pub fn changes_of(root: &Path, files: &[String]) -> Vec<Change> {
    files
        .iter()
        .map(|f| {
            let now = root.join(f).exists();
            let before = codemap_core::git::show(root, "HEAD", f).is_some();
            Change {
                status: match (before, now) {
                    (false, _) => 'A',
                    (true, false) => 'D',
                    (true, true) => 'M',
                },
                path: f.clone(),
                from: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_keys_of_an_example_file() {
        let k = env_keys("# c\nAPP_NAME=x\nexport MAIL_HOST=\n\nBAD LINE\nNUXT_PUBLIC_API=1");
        assert_eq!(
            k.into_iter().collect::<Vec<_>>(),
            ["APP_NAME", "MAIL_HOST", "NUXT_PUBLIC_API"]
        );
    }

    #[test]
    fn rules_match_patterns_and_kinds() {
        let rules = vec![RuleConfig {
            files: vec!["database/migrations/**".into()],
            note: "Never edit a migration that has run; add a new one.".into(),
            when: Some("modified".into()),
            ..RuleConfig::default()
        }];
        let ch = |s: char, p: &str| Change {
            status: s,
            path: p.into(),
            from: None,
        };
        let hit = rules_hit(
            Path::new("/p"),
            &rules,
            &[
                ch(
                    'M',
                    "database/migrations/2024_01_01_000000_create_orders.php",
                ),
                ch('A', "database/migrations/2026_10_17_000000_add_x.php"),
                ch('M', "app/Models/Order.php"),
            ],
        );
        assert_eq!(hit.len(), 1);
        assert_eq!(
            hit[0].1,
            ["database/migrations/2024_01_01_000000_create_orders.php (modified)"]
        );
    }
}
