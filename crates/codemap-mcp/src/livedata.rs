//! Tests that depend on live data: they check content they never create, so
//! they pass only while the database (or the admin-built content in it)
//! happens to hold the right records, and break when someone edits it.
//!
//! Browser tests (Playwright, Cypress) that assert on page text or counts
//! without seeding, API setup or mocked responses; PHP tests that call the app
//! or query models without factories, seeders or inserts.

use std::path::Path;

use crate::browser::is_browser_test;
use crate::usages::is_test_path;

const BROWSER_SETUP: &[&str] = &[
    // Reading the data first (and checking against it) is not depending on it.
    "request.get(",
    "request.fetch(",
    "request.post(",
    "request.put(",
    "request.patch(",
    "request.delete(",
    ".route(",
    "routeFromHAR",
    "cy.intercept(",
    "cy.request(",
    "cy.task(",
    "cy.fixture(",
    "fixture",
    "seed",
    "factory",
    "Factory",
    "mock",
    "Mock",
    "msw",
    "apiContext",
    "newContext(",
];

const BROWSER_CHECKS: &[&str] = &[
    "toHaveText(",
    "toContainText(",
    "toHaveCount(",
    "getByText(",
    "cy.contains(",
    "have.length",
    "contain.text",
    "have.text",
];

const PHP_SETUP: &[&str] = &[
    "::factory(",
    "->create(",
    "::create(",
    "->createMany(",
    "->createQuietly(",
    "->make(",
    "$this->seed(",
    "->insert(",
    "::insert(",
    "->save(",
    "Seeder",
    "Event::fake",
];

const PHP_READS: &[&str] = &[
    "->get(",
    "->getJson(",
    "->post(",
    "->postJson(",
    "->put(",
    "->putJson(",
    "->patch(",
    "->patchJson(",
    "->delete(",
    "->deleteJson(",
    "::where(",
    "::first(",
    "::all(",
    "::find(",
    "DB::",
];

const PHP_CHECKS: &[&str] = &[
    "assertSee(",
    "assertSeeText(",
    "assertJsonCount(",
    "assertJsonFragment(",
    "assertDatabaseHas(",
    "assertDatabaseCount(",
    "assertCount(",
];

/// Occurrences, ignoring line breaks and spaces (`await request\n  .get(`).
fn count(text: &str, needles: &[&str]) -> usize {
    let compact: String = text.split_whitespace().collect();
    needles.iter().map(|n| compact.matches(n).count()).sum()
}

/// Seeding for every test: a global setup, or a base test case that seeds.
fn seeds_everything(root: &Path) -> (bool, bool) {
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap_or_default();
    let browser = [
        "playwright.config.ts",
        "playwright.config.js",
        "playwright.config.mjs",
        "cypress.config.ts",
        "cypress.config.js",
    ]
    .iter()
    .any(|f| {
        let t = read(f);
        t.contains("globalSetup") || t.contains("setupNodeEvents") && t.contains("seed")
    });
    let base = read("tests/TestCase.php");
    let php =
        base.contains("$seed = true") || base.contains("$seeder") || base.contains("$this->seed(");
    (browser, php)
}

/// The app's own text (source files and translations, also the backend's
/// `lang/` in a workspace): a message from it is app text, not stored data.
struct AppText {
    root: std::path::PathBuf,
    text: Option<String>,
}

impl AppText {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            text: None,
        }
    }

    fn has(&mut self, needle: &str) -> bool {
        let root = self.root.clone();
        self.text
            .get_or_insert_with(|| load_app_text(&root))
            .contains(needle)
    }
}

fn load_app_text(root: &Path) -> String {
    let mut text = String::new();
    let mut read_tree = |dir: &Path, exts: &[&str]| {
        let walker = ignore::WalkBuilder::new(dir)
            .require_git(false)
            .filter_entry(|e| {
                let n = e.file_name().to_string_lossy();
                !matches!(
                    n.as_ref(),
                    "node_modules"
                        | "vendor"
                        | ".nuxt"
                        | ".output"
                        | ".next"
                        | "dist"
                        | "storage"
                        | ".git"
                )
            })
            .build();
        for e in walker.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(dir)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let ok = p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| exts.contains(&x))
                && !is_test_path(&rel)
                && e.metadata().is_ok_and(|m| m.is_file() && m.len() < 300_000);
            if ok {
                text.push_str(&std::fs::read_to_string(p).unwrap_or_default());
                text.push('\n');
            }
        }
    };
    read_tree(
        root,
        &[
            "vue", "ts", "js", "tsx", "jsx", "json", "php", "yml", "yaml",
        ],
    );
    // In a workspace, the backend's translations (validation messages).
    if let Some(parent) = root.parent()
        && let Ok(rd) = std::fs::read_dir(parent)
    {
        for e in rd.flatten() {
            let d = e.path();
            if d == root || !d.is_dir() {
                continue;
            }
            for lang in [d.join("lang"), d.join("resources/lang")] {
                if lang.is_dir() {
                    read_tree(&lang, &["php", "json"]);
                }
            }
        }
    }
    text
}

/// The first quoted text after `at` on a line.
fn literal_after(line: &str, at: usize) -> Option<&str> {
    let rest = &line[at..];
    let q = rest.find(['\'', '"', '`'])?;
    let close_paren = rest.find(')').unwrap_or(rest.len());
    if q > close_paren {
        return None;
    }
    let quote = rest.as_bytes()[q] as char;
    let body = &rest[q + 1..];
    body.find(quote).map(|e| &body[..e])
}

/// Checks on page content that only stored data can satisfy. Left out:
/// negated checks and `toHaveCount(0)`; text the test itself typed or sent;
/// sentences and text found in the app's own code or translations.
fn data_checks(src: &str, app_text: &mut AppText) -> usize {
    let mut typed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for line in src.lines() {
        let entering = [
            "fill(",
            "type(",
            "selectOption(",
            "pressSequentially(",
            "data:",
            "body:",
            "form:",
            "post(",
            "put(",
            "patch(",
        ]
        .iter()
        .any(|k| line.contains(k));
        if entering {
            let mut rest = line;
            while let Some(q) = rest.find(['\'', '"', '`']) {
                let quote = rest.as_bytes()[q] as char;
                let body = &rest[q + 1..];
                let Some(e) = body.find(quote) else { break };
                typed.insert(&body[..e]);
                rest = &body[e + 1..];
            }
        }
    }
    let mut n = 0;
    for line in src.lines() {
        if line.contains(".not.") || line.contains("not.to") {
            continue;
        }
        for check in BROWSER_CHECKS {
            for (at, _) in line.match_indices(check) {
                let after = at + check.len();
                if *check == "toHaveCount(" && line[after..].trim_start().starts_with('0') {
                    continue;
                }
                // `getByText('x')` inside a line: its own literal; otherwise
                // the literal the assertion compares with.
                match literal_after(line, after) {
                    Some(lit) => {
                        let sentence = lit.split_whitespace().count() >= 4;
                        if typed.contains(lit)
                            || sentence
                            || lit.trim().is_empty()
                            || app_text.has(lit)
                        {
                            continue;
                        }
                        n += 1;
                    }
                    // A count depends on how much is stored; a text pattern
                    // (`/at least 8 characters/`) is the app's own wording.
                    None => {
                        if matches!(*check, "toHaveCount(" | "have.length") {
                            n += 1;
                        }
                    }
                }
            }
        }
    }
    n
}

/// `class XTest extends Base` → `Base`.
fn parent_class(src: &str) -> Option<String> {
    let after = src.split(" extends ").nth(1)?;
    let name: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '\\')
        .collect();
    name.rsplit('\\')
        .next()
        .map(str::to_owned)
        .filter(|n| !n.is_empty())
}

/// Files a spec imports with a relative path (`./x`, `../support/y`).
fn imports(spec: &str, src: &str) -> Vec<String> {
    let dir = spec.rsplit_once('/').map_or("", |(d, _)| d);
    let mut out = Vec::new();
    for line in src.lines().filter(|l| l.trim_start().starts_with("import")) {
        let Some(q) = line.rfind(['\'', '"']) else {
            continue;
        };
        let Some(start) = line[..q].rfind(['\'', '"']).map(|s| s + 1) else {
            continue;
        };
        let target = &line[start..q];
        if !target.starts_with('.') {
            continue;
        }
        let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
        for seg in target.split('/') {
            match seg {
                "." => {}
                ".." => {
                    parts.pop();
                }
                s => parts.push(s),
            }
        }
        let base = parts.join("/");
        for ext in ["", ".ts", ".js", ".mjs", "/index.ts", "/index.js"] {
            out.push(format!("{base}{ext}"));
        }
    }
    out
}

/// `(test file, why)` for tests that read data they never create.
pub fn find(root: &Path, files: &[String]) -> Vec<(String, String)> {
    let (browser_seeded, php_seeded) = seeds_everything(root);
    let mut app_text = AppText::new(root);
    let mut out = Vec::new();
    for f in files.iter().filter(|f| is_test_path(f)) {
        let Ok(src) = std::fs::read_to_string(root.join(f)) else {
            continue;
        };
        if f.ends_with(".php") {
            if php_seeded || !f.contains("Feature") && !f.contains("Browser") {
                continue;
            }
            let checks = count(&src, PHP_CHECKS);
            // Setup in the test's own parent class (`extends IntegrationTestCase`).
            let parent = parent_class(&src)
                .filter(|p| p != "TestCase")
                .and_then(|p| {
                    files
                        .iter()
                        .find(|x| x.ends_with(&format!("/{p}.php")))
                        .and_then(|x| std::fs::read_to_string(root.join(x)).ok())
                })
                .map_or(0, |t| count(&t, PHP_SETUP));
            if count(&src, PHP_SETUP) + parent == 0 && count(&src, PHP_READS) > 0 && checks > 0 {
                out.push((
                    f.clone(),
                    format!("{checks} checks on data the test never creates (no factory, seeder or insert)"),
                ));
            }
        } else if is_browser_test(f, &src) {
            // A page opened from a file or written by the test itself: its
            // content is in the repository, not in a database.
            if browser_seeded || src.contains("file://") || src.contains("setContent(") {
                continue;
            }
            // `toHaveCount(0)` checks that something is absent: no data needed.
            let checks = data_checks(&src, &mut app_text);
            // Setup can live in helpers the spec imports (`../support/cart`).
            let helpers: usize = imports(f, &src)
                .iter()
                .filter_map(|h| std::fs::read_to_string(root.join(h)).ok())
                .map(|t| count(&t, BROWSER_SETUP))
                .sum();
            if count(&src, BROWSER_SETUP) + helpers == 0 && checks > 0 {
                out.push((
                    f.clone(),
                    format!("{checks} checks on page content the test never sets up (no seeding, API setup or mocked responses)"),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_without_their_own_data() {
        let dir = tempfile::tempdir().unwrap();
        let w = |p: &str, t: &str| {
            let path = dir.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, t).unwrap();
        };
        w(
            "tests/e2e/home.spec.ts",
            "import { test, expect } from '@playwright/test'\ntest('home', async ({ page }) => { await page.goto('/'); await expect(page.getByTestId('banner')).toHaveCount(3) })",
        );
        w(
            "tests/e2e/cart.spec.ts",
            "import { test, expect } from '@playwright/test'\ntest('cart', async ({ page }) => { await page.route('**/api/cart', r => r.fulfill({})); await page.goto('/cart'); await expect(page.getByText('Empty')).toBeVisible() })",
        );
        w(
            "tests/Feature/HomeTest.php",
            "<?php class HomeTest extends TestCase { public function test_it() { $this->getJson('/api/home')->assertJsonCount(3, 'data'); } }",
        );
        w(
            "tests/Feature/OrderTest.php",
            "<?php class OrderTest extends TestCase { public function test_it() { $o = Order::factory()->create(); $this->getJson('/api/orders')->assertJsonCount(1, 'data'); } }",
        );
        let files: Vec<String> = [
            "tests/e2e/home.spec.ts",
            "tests/e2e/cart.spec.ts",
            "tests/Feature/HomeTest.php",
            "tests/Feature/OrderTest.php",
        ]
        .map(String::from)
        .to_vec();
        let found: Vec<String> = find(dir.path(), &files)
            .into_iter()
            .map(|(f, _)| f)
            .collect();
        assert_eq!(
            found,
            ["tests/e2e/home.spec.ts", "tests/Feature/HomeTest.php"]
        );
    }
}
