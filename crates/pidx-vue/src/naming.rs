//! Component names: Nuxt's auto-import naming (ported from `nuxt/src/components/scan.ts`)
//! and the generated `.nuxt/components.d.ts`, which is exact when present.

use std::path::Path;

use crate::detect::ComponentDir;

/// scule's `splitByCase`: `base-button` / `BaseButton` / `base/button` → `[base, button]`-ish parts.
pub fn split_by_case(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut buf = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, '-' | '_' | '/' | '.' | ' ' | '\\') {
            if !buf.is_empty() {
                parts.push(std::mem::take(&mut buf));
            }
            continue;
        }
        if !buf.is_empty() {
            let prev = chars[i - 1];
            let upper = c.is_uppercase();
            // fooBar → foo|Bar; HTMLEditor → HTML|Editor
            let boundary = (upper && (prev.is_lowercase() || prev.is_ascii_digit()))
                || (upper
                    && prev.is_uppercase()
                    && chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
            if boundary {
                parts.push(std::mem::take(&mut buf));
            }
        }
        buf.push(c);
    }
    if !buf.is_empty() {
        parts.push(buf);
    }
    parts
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn pascal_parts(parts: &[String]) -> String {
    parts.iter().map(|p| upper_first(p)).collect()
}

/// Nuxt's name for `path` (relative to the project root) inside `dir`, or
/// `None` when the file is not under the dir or not a component file.
pub fn nuxt_component_name(dir: &ComponentDir, path: &str) -> Option<String> {
    let rel = path.strip_prefix(&dir.path)?.strip_prefix('/')?;
    if !rel.ends_with(".vue") && !rel.ends_with(".tsx") && !rel.ends_with(".jsx") {
        return None;
    }
    let (dir_rel, file) = rel.rsplit_once('/').unwrap_or(("", rel));
    let mut stem = file.rsplit_once('.').map_or(file, |(s, _)| s).to_owned();
    for suffix in [".client", ".server", ".global"] {
        if let Some(s) = stem.strip_suffix(suffix) {
            stem = s.to_owned();
        }
    }
    if stem.eq_ignore_ascii_case("index") {
        stem = if dir.path_prefix {
            String::new()
        } else {
            dir_rel.rsplit('/').next().unwrap_or("").to_owned()
        };
    }
    let mut prefix_parts: Vec<String> =
        dir.prefix.as_deref().map(split_by_case).unwrap_or_default();
    if dir.path_prefix {
        prefix_parts.extend(split_by_case(dir_rel));
    }
    let file_parts = split_by_case(&stem);
    let file_content = file_parts.join("/").to_lowercase();
    let mut keep = prefix_parts.len();
    let mut matched: Vec<String> = Vec::new();
    for index in (0..prefix_parts.len()).rev() {
        let mut add: Vec<String> = split_by_case(&prefix_parts[index])
            .iter()
            .map(|p| p.to_lowercase())
            .collect();
        add.extend(matched);
        matched = add;
        let matched_content = matched.join("/");
        let duplicate_dir = prefix_parts[index].to_lowercase() == file_content
            && prefix_parts
                .get(index + 1)
                .is_some_and(|n| *n == prefix_parts[index]);
        if file_content == matched_content
            || file_content.starts_with(&format!("{matched_content}/"))
            || duplicate_dir
        {
            keep = index;
        }
    }
    let name = pascal_parts(&prefix_parts[..keep]) + &pascal_parts(&file_parts);
    (!name.is_empty()).then_some(name)
}

/// An entry of `.nuxt/components.d.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedComponent {
    pub name: String,
    /// Project-relative path for project components.
    pub path: Option<String>,
    /// npm package for library components.
    pub package: Option<String>,
}

/// Reads `.nuxt/components.d.ts` (both the `export const X: typeof import("…")`
/// and the `'X': typeof import("…")` formats). Lazy and hydration variants are skipped.
pub fn read_generated(root: &Path, nuxt_dir: &str) -> Option<Vec<GeneratedComponent>> {
    let text = std::fs::read_to_string(root.join(nuxt_dir).join("components.d.ts")).ok()?;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in text.lines() {
        let Some(at) = line.find("typeof import(") else {
            continue;
        };
        let head = line[..at].trim().trim_end_matches(':').trim();
        let name = head
            .trim_start_matches("export const ")
            .trim_matches(['\'', '"'])
            .trim();
        if name.is_empty() || name.contains(' ') || name.starts_with("Lazy") {
            continue;
        }
        let rest = &line[at + "typeof import(".len()..];
        let Some(q) = rest.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            continue;
        };
        let Some(end) = rest[1..].find(q) else {
            continue;
        };
        let target = &rest[1..1 + end];
        if !seen.insert(name.to_owned()) {
            continue;
        }
        // Bare specifiers are packages (`primevue/card`); `#app/…` is Nuxt itself.
        if target.starts_with('#') {
            out.push(GeneratedComponent {
                name: name.to_owned(),
                path: None,
                package: Some("nuxt".into()),
            });
            continue;
        }
        if !target.starts_with('.') && !target.starts_with('/') {
            out.push(GeneratedComponent {
                name: name.to_owned(),
                path: None,
                package: Some(crate::paths::package_name(target)),
            });
            continue;
        }
        let (path, package) = match target.rfind("node_modules/") {
            Some(i) => {
                let after = &target[i + "node_modules/".len()..];
                let mut segs = after.split('/');
                let first = segs.next().unwrap_or("");
                let pkg = if first.starts_with('@') {
                    format!("{first}/{}", segs.next().unwrap_or(""))
                } else {
                    first.to_owned()
                };
                (None, Some(pkg))
            }
            None => (Some(normalize_relative(nuxt_dir, target)), None),
        };
        out.push(GeneratedComponent {
            name: name.to_owned(),
            path,
            package,
        });
    }
    Some(out)
}

/// `../app/components/X.vue` relative to `.nuxt` → `app/components/X.vue`.
fn normalize_relative(base: &str, target: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(path: &str, path_prefix: bool, prefix: Option<&str>) -> ComponentDir {
        ComponentDir {
            path: path.into(),
            path_prefix,
            prefix: prefix.map(str::to_owned),
        }
    }

    #[test]
    fn nuxt_names() {
        let d = dir("components", true, None);
        let n = |p: &str| nuxt_component_name(&d, p);
        assert_eq!(n("components/AppHeader.vue").as_deref(), Some("AppHeader"));
        assert_eq!(
            n("components/base/Button.vue").as_deref(),
            Some("BaseButton")
        );
        assert_eq!(
            n("components/base/BaseButton.vue").as_deref(),
            Some("BaseButton")
        );
        assert_eq!(
            n("components/base/foo/Button.vue").as_deref(),
            Some("BaseFooButton")
        );
        assert_eq!(
            n("components/form/input/index.vue").as_deref(),
            Some("FormInput")
        );
        assert_eq!(
            n("components/order-card/OrderCardItem.vue").as_deref(),
            Some("OrderCardItem")
        );
        assert_eq!(n("components/Map.client.vue").as_deref(), Some("Map"));
        assert_eq!(
            n("components/user/user-avatar.vue").as_deref(),
            Some("UserAvatar")
        );
        assert_eq!(n("components/x.ts"), None);
        assert_eq!(n("pages/x.vue"), None);

        let flat = dir("app/components", false, None);
        assert_eq!(
            nuxt_component_name(&flat, "app/components/base/Button.vue").as_deref(),
            Some("Button")
        );
        assert_eq!(
            nuxt_component_name(&flat, "app/components/modal/index.vue").as_deref(),
            Some("Modal")
        );
        let prefixed = dir("components/ui", true, Some("Ui"));
        assert_eq!(
            nuxt_component_name(&prefixed, "components/ui/Button.vue").as_deref(),
            Some("UiButton")
        );
    }

    #[test]
    fn split() {
        assert_eq!(split_by_case("HTMLEditor"), ["HTML", "Editor"]);
        assert_eq!(split_by_case("base/foo-bar"), ["base", "foo", "bar"]);
        assert_eq!(split_by_case("step2Form"), ["step2", "Form"]);
    }

    #[test]
    fn generated_types() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".nuxt")).unwrap();
        std::fs::write(
            dir.path().join(".nuxt/components.d.ts"),
            "import type { DefineComponent } from 'vue'\n\
             export const AppFooter: typeof import(\"../app/components/AppFooter.vue\")['default']\n\
             export const UButton: typeof import(\"../node_modules/@nuxt/ui/dist/runtime/components/Button.vue\")['default']\n\
             export const NuxtLink: typeof import(\"../node_modules/.pnpm/nuxt@4.4.5/node_modules/nuxt/dist/app/components/nuxt-link\")['default']\n\
             export const Card: typeof import(\"primevue/card\")['default']\n\
             export const LazyAppFooter: LazyComponent<typeof import(\"../app/components/AppFooter.vue\")['default']>\n\
             declare module 'vue' { export interface GlobalComponents {\n  'OldStyle': typeof import(\"../components/OldStyle.vue\")['default']\n} }\n",
        )
        .unwrap();
        let g = read_generated(dir.path(), ".nuxt").unwrap();
        let got: Vec<(String, Option<String>, Option<String>)> =
            g.into_iter().map(|c| (c.name, c.path, c.package)).collect();
        assert_eq!(
            got,
            [
                (
                    "AppFooter".into(),
                    Some("app/components/AppFooter.vue".into()),
                    None
                ),
                ("UButton".into(), None, Some("@nuxt/ui".into())),
                ("NuxtLink".into(), None, Some("nuxt".into())),
                ("Card".into(), None, Some("primevue".into())),
                (
                    "OldStyle".into(),
                    Some("components/OldStyle.vue".into()),
                    None
                ),
            ]
        );
    }
}
