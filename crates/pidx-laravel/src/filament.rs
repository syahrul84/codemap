//! Filament admin panels, read statically: each admin URL → the page class,
//! its resource, model, form fields and table columns. Filament registers
//! these routes at runtime, so they are not in `routes/`.

use std::path::Path;

use pidx_core::symbols::{self, ClassRef};
use pidx_core::{Result, ScanReport, Store};
use pidx_php::Scope;
use pidx_php::ast::{find_class, named_children, text};
use rusqlite::params;
use tree_sitter::Node;

use crate::inflector;
use crate::literal::{self, Lit};

pub const RESOURCE: &str = "Filament\\Resources\\Resource";
pub const PAGE: &str = "Filament\\Pages\\Page";
pub const PANEL_PROVIDER: &str = "Filament\\PanelProvider";
const SOURCE: &str = "filament";
const STATE_KEY: &str = "filament_state";
/// Bump when the admin map output changes.
pub const FILAMENT_VERSION: i64 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminPage {
    pub url: String,
    pub name: String,
    pub file: String,
    pub defined_in: String,
    pub meta: String,
}

struct Parsed {
    src: Vec<u8>,
    tree: tree_sitter::Tree,
}

fn parse(root: &Path, file: &str) -> Option<Parsed> {
    let src = std::fs::read(root.join(file)).ok()?;
    let tree = pidx_php::parse::parse(&src)?;
    Some(Parsed { src, tree })
}

fn method<'t>(class: Node<'t>, src: &[u8], name: &str) -> Option<Node<'t>> {
    class
        .child_by_field_name("body")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .find(|m| {
            m.kind() == "method_declaration"
                && m.child_by_field_name("name").map(|n| text(n, src)) == Some(name)
        })
}

/// `protected static ?string $slug = 'x';` and friends.
fn static_prop(class: Node<'_>, src: &[u8], scope: &Scope, name: &str) -> Option<Lit> {
    for decl in class
        .child_by_field_name("body")
        .map(named_children)
        .unwrap_or_default()
    {
        if decl.kind() != "property_declaration" {
            continue;
        }
        for el in named_children(decl)
            .into_iter()
            .filter(|e| e.kind() == "property_element")
        {
            let n = named_children(el);
            if n.first().map(|v| text(*v, src).trim_start_matches('$')) == Some(name) {
                return n.get(1).map(|v| literal::eval(*v, src, scope));
            }
        }
    }
    None
}

fn walk<'t>(n: Node<'t>, out: &mut Vec<Node<'t>>, kind: &str) {
    for c in named_children(n) {
        if c.kind() == kind {
            out.push(c);
        }
        walk(c, out, kind);
    }
}

/// Filament's default resource slug (v4 / v5; v3 matches the second rule).
pub fn resource_slug(fqn: &str) -> String {
    let basename = fqn.rsplit('\\').next().unwrap_or(fqn);
    let model = basename.strip_suffix("Resource").unwrap_or(basename);
    let plural = inflector::plural(model);
    let ns_last = fqn.rsplit('\\').nth(1).unwrap_or("");
    let after = fqn.rsplit_once("\\Resources\\").map(|(_, a)| a);
    let kebab = |s: &str| pidx_js_free_kebab(s);
    if plural == ns_last {
        let path = after
            .and_then(|a| a.rsplit_once('\\').map(|(p, _)| p))
            .unwrap_or(ns_last);
        return path.split('\\').map(kebab).collect::<Vec<_>>().join("/");
    }
    let rel = after.unwrap_or(basename);
    let rel = rel.strip_suffix("Resource").unwrap_or(rel);
    let mut parts: Vec<String> = rel.split('\\').map(str::to_owned).collect();
    if let Some(last) = parts.last_mut() {
        *last = inflector::plural(last);
    }
    parts.iter().map(|p| kebab(p)).collect::<Vec<_>>().join("/")
}

/// `MerchantPerformance` → `merchant-performance` (Laravel's `kebab()` + `slug()`).
fn pidx_js_free_kebab(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if c.is_uppercase()
            && i > 0
            && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit())
        {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

struct Panel {
    id: String,
    path: String,
    note: Option<String>,
}

/// `->id('admin')->path('admin')` in each PanelProvider.
fn panels(conn: &rusqlite::Connection, root: &Path) -> Vec<Panel> {
    let mut out = Vec::new();
    for p in symbols::descendants(conn, &[PANEL_PROVIDER]).unwrap_or_default() {
        let Some(parsed) = parse(root, &p.file) else {
            continue;
        };
        let mut calls = Vec::new();
        walk(
            parsed.tree.root_node(),
            &mut calls,
            "member_call_expression",
        );
        let mut id = None;
        let mut path = None;
        let mut note = None;
        for c in calls {
            let name = c
                .child_by_field_name("name")
                .map(|n| text(n, &parsed.src))
                .unwrap_or("");
            let arg = c
                .child_by_field_name("arguments")
                .map(named_children)
                .unwrap_or_default()
                .into_iter()
                .find(|a| a.kind() == "argument")
                .and_then(|a| named_children(a).into_iter().last());
            let Some(arg) = arg else { continue };
            let value = literal::eval(arg, &parsed.src, &Scope::default());
            let string = value.as_str().map(str::to_owned).or_else(|| {
                // `filled($domain) ? '' : 'admin'`: take the last literal and say so.
                let mut strings = Vec::new();
                walk(arg, &mut strings, "string");
                strings
                    .last()
                    .map(|s| text(*s, &parsed.src).trim_matches(['\'', '"']).to_owned())
            });
            match name {
                "id" if id.is_none() => id = string,
                "path" if path.is_none() => {
                    if value.as_str().is_none() && string.is_some() {
                        note = Some(format!(
                            "panel path is conditional: {}",
                            pidx_php::ast::compact(text(arg, &parsed.src))
                        ));
                    }
                    path = string;
                }
                _ => {}
            }
        }
        out.push(Panel {
            id: id.unwrap_or_else(|| "admin".into()),
            path: path.unwrap_or_else(|| "admin".into()),
            note,
        });
    }
    if out.is_empty() {
        out.push(Panel {
            id: "admin".into(),
            path: "admin".into(),
            note: Some("no PanelProvider found; assuming /admin".into()),
        });
    }
    out
}

/// `::make('name')` field / column names in a class (layout components skipped).
fn made_names(root: &Path, file: &str, method_name: Option<&str>, fqn: &str) -> Vec<String> {
    const LAYOUT: &[&str] = &[
        "Section",
        "Grid",
        "Tabs",
        "Tab",
        "Fieldset",
        "Group",
        "Wizard",
        "Step",
        "Actions",
        "Action",
        "Split",
        "Card",
        "Flex",
        "Stack",
        "Panel",
        "Layout",
        "View",
        "Placeholder",
        "Callout",
        "Text",
        "Notification",
        "BulkAction",
        "BulkActionGroup",
        "ActionGroup",
        "EditAction",
        "ViewAction",
        "DeleteAction",
        "Filter",
        "SelectFilter",
        "TernaryFilter",
        "Column",
        "ColumnGroup",
        "Heading",
    ];
    let Some(p) = parse(root, file) else {
        return Vec::new();
    };
    let Some((class, _)) = find_class(p.tree.root_node(), &p.src, fqn) else {
        return Vec::new();
    };
    let scope_node = match method_name {
        Some(m) => match method(class, &p.src, m) {
            Some(n) => n,
            None => return Vec::new(),
        },
        None => class,
    };
    let mut calls = Vec::new();
    walk(scope_node, &mut calls, "scoped_call_expression");
    let mut out = Vec::new();
    for c in calls {
        let cls = c
            .child_by_field_name("scope")
            .map(|n| text(n, &p.src))
            .unwrap_or("");
        let short = cls.rsplit('\\').next().unwrap_or(cls);
        if c.child_by_field_name("name").map(|n| text(n, &p.src)) != Some("make")
            || LAYOUT.contains(&short)
            || short.ends_with("Action")
        {
            continue;
        }
        let arg = c
            .child_by_field_name("arguments")
            .map(named_children)
            .unwrap_or_default()
            .into_iter()
            .find(|a| a.kind() == "argument")
            .and_then(|a| named_children(a).into_iter().last());
        if let Some(Lit::Str(name)) = arg.map(|a| literal::eval(a, &p.src, &Scope::default()))
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    out
}

/// `form()` / `table()`: fields in the method itself, or in the class it delegates to
/// (`return MerchantForm::configure($schema);`).
fn schema_names(
    conn: &rusqlite::Connection,
    root: &Path,
    res: &ClassRef,
    scope: &Scope,
    class: Node<'_>,
    src: &[u8],
    m: &str,
) -> (Option<String>, Vec<String>) {
    let Some(body) = method(class, src, m) else {
        return (None, Vec::new());
    };
    let mut calls = Vec::new();
    walk(body, &mut calls, "scoped_call_expression");
    for c in &calls {
        let name = c
            .child_by_field_name("name")
            .map(|n| text(n, src))
            .unwrap_or("");
        if name == "configure"
            && let Some(cls) = c
                .child_by_field_name("scope")
                .and_then(|s| scope.resolve_class(text(s, src)))
            && let Some(sym) = symbols::by_fqn(conn, &cls)
                .ok()
                .and_then(|v| v.into_iter().next())
        {
            let short = cls.rsplit('\\').next().unwrap_or(&cls).to_owned();
            return (Some(short), made_names(root, &sym.file, None, &cls));
        }
    }
    (None, made_names(root, &res.file, Some(m), &res.fqn))
}

pub fn extract(conn: &rusqlite::Connection, root: &Path) -> Vec<AdminPage> {
    let resources = symbols::descendants(conn, &[RESOURCE]).unwrap_or_default();
    let custom = symbols::descendants(conn, &[PAGE]).unwrap_or_default();
    if resources.is_empty() && custom.is_empty() {
        return Vec::new();
    }
    let panels = panels(conn, root);
    let mut out = Vec::new();
    for panel in &panels {
        let base = format!("/{}", panel.path.trim_matches('/'));
        let base = if base == "/" { String::new() } else { base };
        for res in &resources {
            let Some(p) = parse(root, &res.file) else {
                continue;
            };
            let Some((class, scope)) = find_class(p.tree.root_node(), &p.src, &res.fqn) else {
                continue;
            };
            if class
                .child_by_field_name("modifiers")
                .is_some_and(|m| text(m, &p.src).contains("abstract"))
                || text(class, &p.src).starts_with("abstract")
            {
                continue;
            }
            let slug = match static_prop(class, &p.src, &scope, "slug") {
                Some(Lit::Str(s)) => s,
                _ => resource_slug(&res.fqn),
            };
            let model = match static_prop(class, &p.src, &scope, "model") {
                Some(v) => v.display().trim_end_matches("::class").to_owned(),
                None => String::new(),
            };
            let model_short = model.rsplit('\\').next().unwrap_or(&model).to_owned();
            let (form_class, fields) = schema_names(conn, root, res, &scope, class, &p.src, "form");
            let (table_class, columns) =
                schema_names(conn, root, res, &scope, class, &p.src, "table");
            let mut meta = format!(
                "resource {}",
                res.fqn.rsplit('\\').next().unwrap_or(&res.fqn)
            );
            if !model_short.is_empty() {
                meta.push_str(&format!(" · model {model_short}"));
            }
            if !fields.is_empty() {
                meta.push_str(&format!(
                    " · form {}: {}",
                    form_class.as_deref().unwrap_or("form()"),
                    fields.join(", ")
                ));
            }
            if !columns.is_empty() {
                meta.push_str(&format!(
                    " · table {}: {}",
                    table_class.as_deref().unwrap_or("table()"),
                    columns.join(", ")
                ));
            }
            if let Some(n) = &panel.note {
                meta.push_str(&format!(" · {n}"));
            }
            // getPages(): 'edit' => EditMerchant::route('/{record}/edit')
            let Some(pages) = method(class, &p.src, "getPages") else {
                continue;
            };
            let mut arrays = Vec::new();
            walk(pages, &mut arrays, "array_element_initializer");
            for el in arrays {
                let parts = named_children(el);
                let [key, value] = parts.as_slice() else {
                    continue;
                };
                let Some(key) = literal::eval(*key, &p.src, &scope)
                    .as_str()
                    .map(str::to_owned)
                else {
                    continue;
                };
                if value.kind() != "scoped_call_expression" {
                    continue;
                }
                let page_cls = value
                    .child_by_field_name("scope")
                    .and_then(|s| scope.resolve_class(text(s, &p.src)));
                let path = value
                    .child_by_field_name("arguments")
                    .map(named_children)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|a| a.kind() == "argument")
                    .and_then(|a| named_children(a).into_iter().last())
                    .and_then(|a| literal::eval(a, &p.src, &scope).as_str().map(str::to_owned))
                    .unwrap_or_default();
                let file = page_cls
                    .as_deref()
                    .and_then(|c| {
                        symbols::by_fqn(conn, c)
                            .ok()
                            .and_then(|v| v.into_iter().next())
                    })
                    .map(|s| s.file)
                    .unwrap_or_else(|| res.file.clone());
                let url = format!(
                    "{base}/{slug}{}",
                    if path == "/" { String::new() } else { path }
                );
                out.push(AdminPage {
                    url,
                    name: format!(
                        "filament.{}.resources.{}.{key}",
                        panel.id,
                        slug.replace('/', ".")
                    ),
                    file,
                    defined_in: format!("{}:{}", res.file, value.start_position().row + 1),
                    // Resource details once, on its index page.
                    meta: if key == "index" {
                        meta.clone()
                    } else {
                        format!(
                            "resource {}",
                            res.fqn.rsplit('\\').next().unwrap_or(&res.fqn)
                        )
                    },
                });
            }
        }
        for pg in &custom {
            // Resource pages extend Filament\Pages\Page too; they are listed above.
            if pg.fqn.contains("\\Resources\\") {
                continue;
            }
            let Some(p) = parse(root, &pg.file) else {
                continue;
            };
            let Some((class, scope)) = find_class(p.tree.root_node(), &p.src, &pg.fqn) else {
                continue;
            };
            if text(class, &p.src).trim_start().starts_with("abstract") {
                continue;
            }
            let slug = match static_prop(class, &p.src, &scope, "slug") {
                Some(Lit::Str(s)) => s,
                _ => pidx_js_free_kebab(pg.fqn.rsplit('\\').next().unwrap_or(&pg.fqn)),
            };
            let label = match static_prop(class, &p.src, &scope, "navigationLabel") {
                Some(Lit::Str(s)) => format!("page \"{s}\""),
                _ => "page".to_owned(),
            };
            let fields = made_names(root, &pg.file, None, &pg.fqn);
            let mut meta = label;
            if !fields.is_empty() {
                meta.push_str(&format!(" · fields: {}", fields.join(", ")));
            }
            out.push(AdminPage {
                url: format!("{base}/{slug}"),
                name: format!("filament.{}.pages.{}", panel.id, slug.replace('/', ".")),
                file: pg.file.clone(),
                defined_in: format!("{}:{}", pg.file, pg.line),
                meta,
            });
        }
    }
    out.sort_by(|a, b| a.url.cmp(&b.url));
    out
}

/// Rebuilds the Filament rows of `pages` when admin code may have changed.
pub fn refresh(store: &mut Store, root: &Path, scan: &ScanReport, full: bool) -> Result<usize> {
    let relevant = |p: &String| {
        p.starts_with("app/Filament/") || p.starts_with("app/Providers/") || p == "composer.json"
    };
    let state = pidx_core::store::get_meta(store.conn(), STATE_KEY)?;
    let current = state.as_deref() == Some(FILAMENT_VERSION.to_string().as_str());
    let any = scan
        .changed_paths
        .iter()
        .chain(&scan.removed_paths)
        .any(relevant);
    if !full && !any && current {
        return Ok(0);
    }
    let pages = extract(store.conn(), root);
    let tx = store.conn_mut().transaction()?;
    tx.execute("DELETE FROM pages WHERE source = ?1", [SOURCE])?;
    for p in &pages {
        tx.execute(
            "INSERT INTO pages (route_path, name, file, source, defined_in, meta) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![p.url, p.name, p.file, SOURCE, p.defined_in, p.meta],
        )?;
    }
    pidx_core::store::set_meta(&tx, STATE_KEY, &FILAMENT_VERSION.to_string())?;
    tx.commit()?;
    Ok(pages.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(
            resource_slug("App\\Filament\\Resources\\Merchants\\MerchantResource"),
            "merchants"
        );
        assert_eq!(
            resource_slug("App\\Filament\\Resources\\OrderResource"),
            "orders"
        );
        assert_eq!(
            resource_slug("App\\Filament\\Resources\\ActivityLogs\\ActivityLogResource"),
            "activity-logs"
        );
        assert_eq!(
            resource_slug("App\\Filament\\Resources\\Shop\\ProductResource"),
            "shop/products"
        );
        assert_eq!(
            pidx_js_free_kebab("MerchantPerformance"),
            "merchant-performance"
        );
    }
}
