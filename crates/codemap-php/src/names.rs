//! PHP name resolution: namespace + `use` imports → fully-qualified names
//! (no leading `\`).

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseKind {
    Class,
    Function,
    Const,
}

/// Names visible at a point in a file.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    namespace: String,
    /// Lowercased alias → FQN (class and function names are case-insensitive).
    classes: HashMap<String, String>,
    functions: HashMap<String, String>,
    /// Constants are case-sensitive.
    consts: HashMap<String, String>,
    /// FQN of the class-like being extracted, for `self` / `static`.
    pub class: Option<String>,
    /// FQN of its parent class, for `parent`.
    pub parent: Option<String>,
}

impl Scope {
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Enters a namespace. Imports do not carry over between namespaces.
    pub fn set_namespace(&mut self, namespace: &str) {
        self.namespace = namespace.trim_matches('\\').to_owned();
        self.classes.clear();
        self.functions.clear();
        self.consts.clear();
    }

    pub fn add_use(&mut self, kind: UseKind, name: &str, alias: Option<&str>) {
        let fqn = name.trim_start_matches('\\').to_owned();
        let alias = alias
            .map(str::to_owned)
            .unwrap_or_else(|| last_segment(&fqn).to_owned());
        match kind {
            UseKind::Class => self.classes.insert(alias.to_lowercase(), fqn),
            UseKind::Function => self.functions.insert(alias.to_lowercase(), fqn),
            UseKind::Const => self.consts.insert(alias, fqn),
        };
    }

    /// `name` declared in the current namespace.
    pub fn qualify(&self, name: &str) -> String {
        if self.namespace.is_empty() {
            name.to_owned()
        } else {
            format!("{}\\{name}", self.namespace)
        }
    }

    /// Resolves a class name as written in code. `None` for built-in types
    /// (`int`, `array`, ...) and for `self`/`static`/`parent` outside a class.
    pub fn resolve_class(&self, raw: &str) -> Option<String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some(abs) = raw.strip_prefix('\\') {
            return Some(abs.to_owned());
        }
        let lower = raw.to_ascii_lowercase();
        match lower.as_str() {
            "self" | "static" => return self.class.clone(),
            "parent" => return self.parent.clone(),
            _ if is_builtin_type(&lower) => return None,
            _ => {}
        }
        if lower.starts_with("namespace\\") {
            return Some(self.qualify(&raw["namespace\\".len()..]));
        }
        let (first, rest) = match raw.split_once('\\') {
            Some((first, rest)) => (first, Some(rest)),
            None => (raw, None),
        };
        if let Some(target) = self.classes.get(&first.to_ascii_lowercase()) {
            return Some(match rest {
                Some(rest) => format!("{target}\\{rest}"),
                None => target.clone(),
            });
        }
        Some(self.qualify(raw))
    }

    /// Resolves a function call name. Unqualified names that are not imported
    /// return the namespaced candidate; PHP falls back to the global function
    /// at runtime, so callers should also try the bare name.
    pub fn resolve_function(&self, raw: &str) -> String {
        let raw = raw.trim();
        if let Some(abs) = raw.strip_prefix('\\') {
            return abs.to_owned();
        }
        if !raw.contains('\\')
            && let Some(target) = self.functions.get(&raw.to_ascii_lowercase())
        {
            return target.clone();
        }
        self.resolve_class(raw).unwrap_or_else(|| raw.to_owned())
    }

    pub fn resolve_const(&self, raw: &str) -> String {
        let raw = raw.trim();
        if let Some(abs) = raw.strip_prefix('\\') {
            return abs.to_owned();
        }
        if let Some(target) = self.consts.get(raw) {
            return target.clone();
        }
        self.qualify(raw)
    }
}

/// Type keywords that are never class names.
pub fn is_builtin_type(lower: &str) -> bool {
    matches!(
        lower,
        "int"
            | "float"
            | "string"
            | "bool"
            | "array"
            | "callable"
            | "iterable"
            | "object"
            | "mixed"
            | "void"
            | "null"
            | "never"
            | "false"
            | "true"
    )
}

pub fn last_segment(fqn: &str) -> &str {
    fqn.rsplit('\\').next().unwrap_or(fqn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        let mut s = Scope::default();
        s.set_namespace("App\\Http\\Controllers");
        s.add_use(UseKind::Class, "App\\Models\\Order", None);
        s.add_use(
            UseKind::Class,
            "\\Illuminate\\Support\\Facades\\DB",
            Some("Database"),
        );
        s.add_use(UseKind::Class, "App\\Models", None);
        s.add_use(UseKind::Function, "App\\Support\\money", None);
        s.add_use(UseKind::Const, "App\\Support\\MAX_ITEMS", None);
        s
    }

    #[test]
    fn resolves_class_names() {
        let s = scope();
        assert_eq!(s.resolve_class("Order").unwrap(), "App\\Models\\Order");
        assert_eq!(s.resolve_class("order").unwrap(), "App\\Models\\Order");
        assert_eq!(
            s.resolve_class("Database").unwrap(),
            "Illuminate\\Support\\Facades\\DB"
        );
        assert_eq!(
            s.resolve_class("Models\\Customer").unwrap(),
            "App\\Models\\Customer"
        );
        assert_eq!(s.resolve_class("\\Exception").unwrap(), "Exception");
        assert_eq!(
            s.resolve_class("Controller").unwrap(),
            "App\\Http\\Controllers\\Controller"
        );
        assert_eq!(
            s.resolve_class("namespace\\Admin\\X").unwrap(),
            "App\\Http\\Controllers\\Admin\\X"
        );
        assert_eq!(
            s.resolve_class("Sub\\Thing").unwrap(),
            "App\\Http\\Controllers\\Sub\\Thing"
        );
    }

    #[test]
    fn builtins_and_relative_keywords() {
        let mut s = scope();
        for t in ["int", "ARRAY", "mixed", "void", "null", "never", "false"] {
            assert_eq!(s.resolve_class(t), None, "{t}");
        }
        assert_eq!(s.resolve_class("self"), None);
        s.class = Some("App\\Models\\Order".into());
        s.parent = Some("Illuminate\\Database\\Eloquent\\Model".into());
        assert_eq!(s.resolve_class("static").unwrap(), "App\\Models\\Order");
        assert_eq!(
            s.resolve_class("parent").unwrap(),
            "Illuminate\\Database\\Eloquent\\Model"
        );
    }

    #[test]
    fn functions_and_consts() {
        let s = scope();
        assert_eq!(s.resolve_function("money"), "App\\Support\\money");
        assert_eq!(s.resolve_function("\\strlen"), "strlen");
        assert_eq!(s.resolve_function("route"), "App\\Http\\Controllers\\route");
        assert_eq!(s.resolve_const("MAX_ITEMS"), "App\\Support\\MAX_ITEMS");
        assert_eq!(
            s.resolve_const("max_items"),
            "App\\Http\\Controllers\\max_items"
        );
    }

    #[test]
    fn namespace_change_drops_imports() {
        let mut s = scope();
        s.set_namespace("\\Other\\");
        assert_eq!(s.namespace(), "Other");
        assert_eq!(s.resolve_class("Order").unwrap(), "Other\\Order");
        s.set_namespace("");
        assert_eq!(s.resolve_class("Order").unwrap(), "Order");
    }
}
