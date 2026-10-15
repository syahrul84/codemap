//! Reads `config('file.key')` values from the literal arrays in `config/*.php`
//! (e.g. spatie/laravel-permission's table names). `env('X', 'default')`
//! resolves to its default and is flagged, since the environment may differ.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pidx_php::Scope;
use pidx_php::ast::{named_children, text};
use tree_sitter::Node;

use crate::literal::{self, Lit};

/// A config value and whether an `env()` default was involved.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigValue {
    pub value: Lit,
    pub from_env_default: bool,
}

pub struct ConfigReader {
    root: PathBuf,
    files: RefCell<HashMap<String, Option<ConfigValue>>>,
}

impl ConfigReader {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            files: RefCell::new(HashMap::new()),
        }
    }

    /// `permission.table_names.roles` → `'roles'`. Keys missing from the
    /// project's config fall back to a package's own default config in
    /// `vendor/*/*/config/` (what `mergeConfigFrom` does), flagged as a default.
    pub fn get(&self, key: &str) -> Option<ConfigValue> {
        let file = key.split('.').next()?.to_owned();
        if file.is_empty() || file.contains(['/', '\\']) || file.starts_with('.') {
            return None;
        }
        let rest: Vec<&str> = key.split('.').skip(1).collect();
        if let Some(found) = self
            .cached(&format!("config/{file}.php"))
            .and_then(|v| lookup(v, &rest))
        {
            return Some(found);
        }
        for package_config in self.vendor_configs(&file) {
            if let Some(mut found) = self.cached(&package_config).and_then(|v| lookup(v, &rest)) {
                found.from_env_default = true;
                return Some(found);
            }
        }
        None
    }

    /// Whether `config/<file>.php` (or a package default) exists, so a missing
    /// key means `null` rather than "unknown".
    pub fn has_file(&self, key: &str) -> bool {
        let file = key.split('.').next().unwrap_or_default();
        !file.is_empty()
            && !file.contains(['/', '\\'])
            && (self
                .root
                .join("config")
                .join(format!("{file}.php"))
                .is_file()
                || !self.vendor_configs(file).is_empty())
    }

    fn cached(&self, rel: &str) -> Option<ConfigValue> {
        self.files
            .borrow_mut()
            .entry(rel.to_owned())
            .or_insert_with(|| self.load(rel))
            .clone()
    }

    /// `vendor/<vendor>/<package>/config/<file>.php` candidates.
    fn vendor_configs(&self, file: &str) -> Vec<String> {
        let mut out = Vec::new();
        for vendor in std::fs::read_dir(self.root.join("vendor"))
            .into_iter()
            .flatten()
            .flatten()
        {
            for package in std::fs::read_dir(vendor.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let candidate = package.path().join("config").join(format!("{file}.php"));
                if candidate.is_file()
                    && let Ok(rel) = candidate.strip_prefix(&self.root)
                {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
        out.sort();
        out
    }

    fn load(&self, rel: &str) -> Option<ConfigValue> {
        let src = std::fs::read(self.root.join(rel)).ok()?;
        let tree = pidx_php::parse::parse(&src)?;
        let ret = named_children(tree.root_node())
            .into_iter()
            .find(|n| n.kind() == "return_statement")
            .and_then(|r| named_children(r).into_iter().next())?;
        let mut env = false;
        let value = eval_config(ret, &src, &Scope::default(), &mut env);
        Some(ConfigValue {
            value,
            from_env_default: env,
        })
    }
}

fn lookup(whole: ConfigValue, path: &[&str]) -> Option<ConfigValue> {
    let mut current = whole.value;
    for part in path {
        current = match current {
            Lit::Arr(items) => {
                items
                    .into_iter()
                    .find(|(k, _)| k.as_deref() == Some(part))?
                    .1
            }
            _ => return None,
        };
    }
    Some(ConfigValue {
        value: current,
        from_env_default: whole.from_env_default,
    })
}

/// Literal evaluation where `env('X', default)` becomes `default`.
fn eval_config(node: Node<'_>, src: &[u8], scope: &Scope, env: &mut bool) -> Lit {
    match node.kind() {
        "function_call_expression" => {
            let name = node
                .child_by_field_name("function")
                .map(|f| text(f, src))
                .unwrap_or("");
            if name.trim_start_matches('\\') == "env" {
                *env = true;
                let default = node
                    .child_by_field_name("arguments")
                    .and_then(|a| {
                        named_children(a)
                            .into_iter()
                            .filter(|x| x.kind() == "argument")
                            .nth(1)
                    })
                    .and_then(|a| named_children(a).into_iter().last());
                return default.map_or(Lit::Null, |d| eval_config(d, src, scope, env));
            }
            literal::eval(node, src, scope)
        }
        "array_creation_expression" => {
            let mut items = Vec::new();
            for el in named_children(node) {
                if el.kind() != "array_element_initializer" {
                    continue;
                }
                match named_children(el).as_slice() {
                    [value] => items.push((None, eval_config(*value, src, scope, env))),
                    [key, value] => {
                        let key = match literal::eval(*key, src, scope) {
                            Lit::Str(k) => Some(k),
                            Lit::Int(i) => Some(i.to_string()),
                            other => Some(other.display()),
                        };
                        items.push((key, eval_config(*value, src, scope, env)));
                    }
                    _ => {}
                }
            }
            Lit::Arr(items)
        }
        _ => literal::eval(node, src, scope),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_keys_and_env_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config/permission.php"),
            "<?php\nreturn [\n    'table_names' => ['roles' => 'roles', 'permissions' => 'perms'],\n    'teams' => false,\n];\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("config/activitylog.php"),
            "<?php\nreturn ['table_name' => env('ACTIVITY_LOGGER_TABLE_NAME', 'activity_log')];\n",
        )
        .unwrap();
        let c = ConfigReader::new(dir.path());
        assert_eq!(
            c.get("permission.table_names.permissions").unwrap().value,
            Lit::Str("perms".into())
        );
        assert!(matches!(
            c.get("permission.table_names").unwrap().value,
            Lit::Arr(_)
        ));
        let a = c.get("activitylog.table_name").unwrap();
        assert_eq!(a.value, Lit::Str("activity_log".into()));
        assert!(a.from_env_default);
        assert_eq!(c.get("missing.key"), None);
        assert_eq!(c.get("../etc"), None);

        // Package defaults merged by mergeConfigFrom().
        std::fs::create_dir_all(dir.path().join("vendor/spatie/laravel-tags/config")).unwrap();
        std::fs::write(
            dir.path()
                .join("vendor/spatie/laravel-tags/config/tags.php"),
            "<?php\nreturn ['table' => 'tags', 'pivot' => 'taggables'];\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("config/tags.php"),
            "<?php\nreturn ['table' => 'labels'];\n",
        )
        .unwrap();
        let c = ConfigReader::new(dir.path());
        assert_eq!(
            c.get("tags.table").unwrap().value,
            Lit::Str("labels".into())
        );
        let pivot = c.get("tags.pivot").unwrap();
        assert_eq!(pivot.value, Lit::Str("taggables".into()));
        assert!(pivot.from_env_default);
    }
}
