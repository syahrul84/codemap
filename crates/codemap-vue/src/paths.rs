//! Resolves import specifiers to project files the way bundlers do:
//! relative paths, aliases (`@/`, `~/`, webpack/vite/tsconfig aliases),
//! missing extensions and `index` files. Bare specifiers are packages.

use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A project file (relative to the root).
    File(String),
    /// An npm package (`vue`, `@nuxt/ui`).
    Package(String),
    /// Looks like a project path but no file matched.
    Missing(String),
}

const EXTENSIONS: &[&str] = &[".vue", ".ts", ".js", ".tsx", ".jsx", ".mjs", ".mts"];

pub struct Resolver<'a> {
    files: &'a HashSet<String>,
    aliases: &'a [(String, String)],
}

impl<'a> Resolver<'a> {
    pub fn new(files: &'a HashSet<String>, aliases: &'a [(String, String)]) -> Self {
        Self { files, aliases }
    }

    pub fn resolve(&self, from: &str, spec: &str) -> Resolved {
        let spec = spec.split(['?', '#']).next().unwrap_or(spec);
        let base =
            if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
                let dir = from.rsplit_once('/').map_or("", |(d, _)| d);
                Some(join(dir, spec))
            } else {
                self.aliases.iter().find_map(|(alias, dir)| {
                    let rest = if spec == alias {
                        Some("")
                    } else {
                        spec.strip_prefix(alias.as_str())
                            .and_then(|r| r.strip_prefix('/'))
                    }?;
                    Some(join(dir, rest))
                })
            };
        let Some(base) = base else {
            return if spec.starts_with('/') {
                Resolved::Missing(spec.trim_start_matches('/').to_owned())
            } else {
                Resolved::Package(package_name(spec))
            };
        };
        match self.find(&base) {
            Some(f) => Resolved::File(f),
            None => Resolved::Missing(base),
        }
    }

    fn find(&self, base: &str) -> Option<String> {
        if self.files.contains(base) {
            return Some(base.to_owned());
        }
        for ext in EXTENSIONS {
            let c = format!("{base}{ext}");
            if self.files.contains(&c) {
                return Some(c);
            }
        }
        for ext in EXTENSIONS {
            let c = if base.is_empty() {
                format!("index{ext}")
            } else {
                format!("{base}/index{ext}")
            };
            if self.files.contains(&c) {
                return Some(c);
            }
        }
        None
    }
}

/// `lodash/debounce` → `lodash`; `@vue/test-utils/x` → `@vue/test-utils`.
pub fn package_name(spec: &str) -> String {
    let mut segs = spec.split('/');
    let first = segs.next().unwrap_or("");
    if first.starts_with('@') {
        format!("{first}/{}", segs.next().unwrap_or(""))
    } else {
        first.to_owned()
    }
}

/// Joins and normalises `dir` + `rel` (handles `.` and `..`).
pub fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
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

    #[test]
    fn resolves_like_a_bundler() {
        let files: HashSet<String> = [
            "src/components/AppCard/AppCard.vue",
            "src/components/Button.vue",
            "src/api/index.js",
            "src/utils/format.ts",
            "src/views/orders/List.vue",
        ]
        .map(String::from)
        .into();
        let aliases = vec![
            ("Components".to_owned(), "src/components".to_owned()),
            ("@".to_owned(), "src".to_owned()),
        ];
        let r = Resolver::new(&files, &aliases);
        let from = "src/views/orders/List.vue";
        assert_eq!(
            r.resolve(from, "Components/AppCard/AppCard"),
            Resolved::File("src/components/AppCard/AppCard.vue".into())
        );
        assert_eq!(
            r.resolve(from, "@/components/Button.vue"),
            Resolved::File("src/components/Button.vue".into())
        );
        assert_eq!(
            r.resolve(from, "../../api"),
            Resolved::File("src/api/index.js".into())
        );
        assert_eq!(
            r.resolve(from, "@/utils/format?raw"),
            Resolved::File("src/utils/format.ts".into())
        );
        assert_eq!(
            r.resolve(from, "./Missing.vue"),
            Resolved::Missing("src/views/orders/Missing.vue".into())
        );
        assert_eq!(
            r.resolve(from, "@vue/composition-api"),
            Resolved::Package("@vue/composition-api".into())
        );
        assert_eq!(
            r.resolve(from, "lodash/debounce"),
            Resolved::Package("lodash".into())
        );
    }
}
