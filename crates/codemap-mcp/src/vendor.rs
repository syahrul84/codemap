//! Reading installed vendor classes, for questions the project index cannot
//! answer alone: does a vendor parent class have this method?

use std::path::{Path, PathBuf};

use codemap_core::symbols::SymbolKind;

/// The answer for a method looked up in vendor ancestors.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// Declared by this vendor class (or one of its own ancestors).
    Found(String),
    /// Every ancestor was read; none declares it.
    Missing(Vec<String>),
    /// None declares it, but this class has `__call`.
    Magic(String),
    /// An ancestor's file is not installed.
    Unknown,
}

/// `'App\\Models\\Widget' => $baseDir . '/app/Models/Widget.php',` →
/// `app/Models/Widget.php`; `$vendorDir . '/x'` → `vendor/x`.
pub fn classmap_path(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("=>")?;
    let base = if rest.contains("$vendorDir") {
        "vendor"
    } else if rest.contains("$baseDir") {
        ""
    } else {
        return None;
    };
    let start = rest.find('\'')? + 1;
    let end = rest[start..].find('\'')? + start;
    let sub = &rest[start..end];
    Some(format!("{base}{sub}").trim_start_matches('/').to_owned())
}

/// The installed file of a vendor class: Composer's PSR-4 map and classmap
/// when present, else the usual package layouts (`vendor/acme/pkg/src/…`).
pub fn class_file(root: &Path, fqn: &str) -> Option<PathBuf> {
    let fqn = fqn.trim_start_matches('\\');
    let composer = root.join("vendor/composer");
    if let Some(p) = crate::verify::psr4_path(&composer.join("autoload_psr4.php"), root, fqn) {
        return Some(root.join(p));
    }
    if let Ok(map) = std::fs::read_to_string(composer.join("autoload_classmap.php")) {
        let needle = format!("'{}' =>", fqn.replace('\\', "\\\\"));
        if let Some(p) = map
            .lines()
            .find(|l| l.contains(&needle))
            .and_then(classmap_path)
            .map(|p| root.join(p))
            .filter(|p| p.is_file())
        {
            return Some(p);
        }
    }
    // `Illuminate\Database\Eloquent\Model` → `…/src/Illuminate/Database/Eloquent/Model.php`,
    // or with the first segments dropped (`League\Flysystem\X` → `…/src/X.php`).
    let segs: Vec<&str> = fqn.split('\\').collect();
    let tails: Vec<String> = (0..segs.len().saturating_sub(1).min(3))
        .map(|skip| segs[skip..].join("/") + ".php")
        .collect();
    let vendor = root.join("vendor");
    for org in std::fs::read_dir(&vendor).ok()?.flatten() {
        if org.file_name() == "composer" || !org.path().is_dir() {
            continue;
        }
        for pkg in std::fs::read_dir(org.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            for dir in ["src", "lib", ""] {
                let base = pkg.path().join(dir);
                for t in &tails {
                    let p = base.join(t);
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

/// Looks `method` up in `classes` (vendor ancestors) and their own
/// parents and traits, reading their files.
pub fn find_method(root: &Path, classes: &[String], method: &str) -> Lookup {
    let mut queue: Vec<String> = classes.to_vec();
    let mut checked: Vec<String> = Vec::new();
    let mut magic: Option<String> = None;
    let mut unknown = false;
    while let Some(class) = queue.pop() {
        if checked.iter().any(|c| c.eq_ignore_ascii_case(&class)) || checked.len() > 24 {
            continue;
        }
        let Some(file) = class_file(root, &class) else {
            unknown = true;
            checked.push(class);
            continue;
        };
        let Ok(src) = std::fs::read(&file) else {
            unknown = true;
            checked.push(class);
            continue;
        };
        let parsed = codemap_php::extract(&src);
        let Some(decl) = parsed
            .symbols
            .iter()
            .find(|s| s.kind.is_class_like() && s.fqn.eq_ignore_ascii_case(&class))
        else {
            unknown = true;
            checked.push(class);
            continue;
        };
        let member = |name: &str| {
            parsed.symbols.iter().any(|s| {
                s.kind == SymbolKind::Method
                    && s.parent_fqn
                        .as_deref()
                        .is_some_and(|p| p.eq_ignore_ascii_case(&decl.fqn))
                    && s.name.eq_ignore_ascii_case(name)
            })
        };
        if member(method) {
            return Lookup::Found(decl.fqn.clone());
        }
        if magic.is_none() && member("__call") {
            magic = Some(decl.fqn.clone());
        }
        queue.extend(decl.meta.extends.iter().cloned());
        queue.extend(decl.meta.traits.iter().cloned());
        checked.push(decl.fqn.clone());
    }
    match (magic, unknown) {
        (Some(m), _) => Lookup::Magic(m),
        (None, true) => Lookup::Unknown,
        (None, false) => Lookup::Missing(checked),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classmap_paths() {
        assert_eq!(
            classmap_path("    'App\\\\Models\\\\Widget' => $baseDir . '/app/Models/Widget.php',")
                .as_deref(),
            Some("app/Models/Widget.php")
        );
        assert_eq!(
            classmap_path("'A\\\\B' => $vendorDir . '/acme/pkg/src/B.php',").as_deref(),
            Some("vendor/acme/pkg/src/B.php")
        );
    }
}
