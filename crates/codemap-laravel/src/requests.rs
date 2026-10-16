//! Form Requests: classes extending `Illuminate\Foundation\Http\FormRequest`,
//! with the field → rules map from a literal `rules()` array.

use std::path::Path;

use codemap_core::Result;
use codemap_core::symbols::{self, SymbolKind};
use codemap_php::ast::{compact, find_class, named_children, text};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::literal::{self, Lit};
use crate::models::return_expression;

pub const FORM_REQUEST: &str = "Illuminate\\Foundation\\Http\\FormRequest";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldRules {
    pub field: String,
    pub rules: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormRequest {
    pub fqn: String,
    pub rules: Vec<FieldRules>,
    pub authorize: Option<String>,
    pub file: String,
    pub line: u32,
    pub unresolved_reason: Option<String>,
}

fn rule_list(v: &Lit) -> Vec<String> {
    match v {
        Lit::Str(s) => s
            .split('|')
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty())
            .collect(),
        Lit::Arr(items) => items
            .iter()
            .flat_map(|(_, r)| match r {
                Lit::Str(s) => s
                    .split('|')
                    .map(|x| x.trim().to_owned())
                    .collect::<Vec<_>>(),
                other => vec![other.display()],
            })
            .collect(),
        other => vec![other.display()],
    }
}

pub fn extract(conn: &Connection, root: &Path) -> Result<Vec<FormRequest>> {
    let mut out = Vec::new();
    for class in symbols::descendants(conn, &[FORM_REQUEST])? {
        let mut req = FormRequest {
            fqn: class.fqn.clone(),
            file: class.file.clone(),
            line: class.line,
            ..FormRequest::default()
        };
        let Ok(src) = std::fs::read(root.join(&class.file)) else {
            continue;
        };
        let Some(tree) = codemap_php::parse::parse(&src) else {
            continue;
        };
        let Some((node, scope)) = find_class(tree.root_node(), &src, &class.fqn) else {
            continue;
        };
        let methods = node
            .child_by_field_name("body")
            .map(named_children)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.kind() == "method_declaration");
        let mut has_rules = false;
        for m in methods {
            let name = m
                .child_by_field_name("name")
                .map(|n| text(n, &src))
                .unwrap_or("");
            let returned = m.child_by_field_name("body").and_then(return_expression);
            match name {
                "rules" => {
                    has_rules = true;
                    match returned.map(|r| literal::eval(r, &src, &scope)) {
                        Some(Lit::Arr(items)) => {
                            for (key, value) in items {
                                match key {
                                    Some(field) => req.rules.push(FieldRules {
                                        field,
                                        rules: rule_list(&value),
                                    }),
                                    None => {
                                        req.unresolved_reason =
                                            Some("rules() has a non-literal key".into());
                                    }
                                }
                            }
                        }
                        Some(other) => {
                            req.unresolved_reason = Some(format!(
                                "rules() does not return an array literal: {}",
                                codemap_php::ast::truncate(&other.display(), 80)
                            ));
                        }
                        None => {
                            req.unresolved_reason = Some("rules() has no top-level return".into())
                        }
                    }
                }
                "authorize" => {
                    req.authorize =
                        returned.map(|r| codemap_php::ast::truncate(&compact(text(r, &src)), 120));
                }
                _ => {}
            }
        }
        if !has_rules {
            // Inherited from a project parent request?
            let inherited = symbols::find_member(conn, &class.fqn, "rules")?;
            req.unresolved_reason = Some(match inherited {
                symbols::MemberLookup::Found(m) => format!(
                    "rules() is inherited from {}",
                    m.parent_fqn.unwrap_or_default()
                ),
                _ => "no rules() method".into(),
            });
        }
        out.push(req);
    }
    Ok(out)
}

pub fn store(conn: &Connection, requests: &[FormRequest]) -> Result<()> {
    conn.execute("DELETE FROM form_requests", [])?;
    for r in requests {
        conn.execute(
            "INSERT INTO form_requests (fqn, rules_json, authorize_raw, file, line, unresolved_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![r.fqn, serde_json::to_string(&r.rules)?, r.authorize, r.file, r.line, r.unresolved_reason],
        )?;
    }
    Ok(())
}

pub fn load(conn: &Connection, fqn: Option<&str>) -> Result<Vec<FormRequest>> {
    let mut stmt = conn.prepare_cached(
        "SELECT fqn, rules_json, authorize_raw, file, line, unresolved_reason FROM form_requests
         WHERE ?1 IS NULL OR fqn = ?1 COLLATE NOCASE ORDER BY fqn",
    )?;
    let rows = stmt.query_map([fqn], |r| {
        Ok(FormRequest {
            fqn: r.get(0)?,
            rules: r
                .get::<_, Option<String>>(1)?
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default(),
            authorize: r.get(2)?,
            file: r.get(3)?,
            line: r.get(4)?,
            unresolved_reason: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Controller methods that receive a Form Request (by parameter type).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestUse {
    pub method: String,
    pub request: String,
    pub param: String,
}

pub fn used_by_controller(conn: &Connection, controller: &str) -> Result<Vec<RequestUse>> {
    let requests: Vec<String> = load(conn, None)?.into_iter().map(|r| r.fqn).collect();
    let mut out = Vec::new();
    for m in symbols::members(conn, controller)? {
        if m.kind != SymbolKind::Method {
            continue;
        }
        for p in &m.meta.params {
            for t in &p.type_fqns {
                if requests.iter().any(|r| r.eq_ignore_ascii_case(t)) {
                    out.push(RequestUse {
                        method: m.name.clone(),
                        request: t.clone(),
                        param: p.name.clone(),
                    });
                }
            }
        }
    }
    Ok(out)
}
