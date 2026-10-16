//! Who may call a route or open an admin page: access middleware, the
//! checks inside the controller action (`$this->authorize`, `Gate::…`,
//! `->can(…)`), the Form Request's `authorize()`, and the policy method the
//! ability lands on. Read statically; anything computed is shown as written.

use std::path::Path;

use codemap_core::symbols::{self, SymbolRecord};
use rusqlite::Connection;
use serde_json::{Value, json};

use crate::routes::RouteDef;

/// Middleware that may decide access: everything except the usual plumbing
/// (groups, throttling, bindings, sessions, CORS, …).
pub fn access_middleware(mw: &[String]) -> Vec<String> {
    const PLUMBING: &[&str] = &[
        "api",
        "web",
        "bindings",
        "SubstituteBindings",
        "cache.headers",
        "EncryptCookies",
        "AddQueuedCookiesToResponse",
        "StartSession",
        "ShareErrorsFromSession",
        "VerifyCsrfToken",
        "ValidateCsrfToken",
        "HandleCors",
        "TrimStrings",
        "ConvertEmptyStringsToNull",
        "precognitive",
        "HandlePrecognitiveRequests",
        "SetLocale",
        "Localize",
        "TrustProxies",
    ];
    mw.iter()
        .filter(|m| {
            let short = m.rsplit('\\').next().unwrap_or(m);
            !m.starts_with("throttle")
                && !PLUMBING.iter().any(|p| short == *p || m.as_str() == *p)
                && !short.contains("Locale")
                && !short.contains("Throttle")
        })
        .cloned()
        .collect()
}

fn method_symbol(conn: &Connection, class: &str, method: &str) -> Option<SymbolRecord> {
    symbols::by_fqn(conn, &format!("{class}::{method}"))
        .ok()?
        .into_iter()
        .next()
}

/// The source lines of a symbol.
fn body(root: &Path, s: &SymbolRecord) -> String {
    let Ok(src) = std::fs::read_to_string(root.join(&s.file)) else {
        return String::new();
    };
    src.lines()
        .skip(s.line.saturating_sub(1) as usize)
        .take((s.end_line.saturating_sub(s.line) + 1) as usize)
        .collect::<Vec<_>>()
        .join("\n")
}

/// First quoted string after `at` in `text`.
fn quoted_after(text: &str, at: usize) -> Option<String> {
    let rest = &text[at..];
    let close = rest.find(')').unwrap_or(rest.len());
    let rest = &rest[..close];
    let q = rest.find(['\'', '"'])?;
    let quote = rest.as_bytes()[q] as char;
    rest[q + 1..].split(quote).next().map(str::to_owned)
}

/// `(ability, how, line)` for each check in a method body.
fn checks(text: &str, first_line: u32) -> Vec<(String, String, u32)> {
    const CALLS: &[(&str, &str)] = &[
        ("$this->authorize(", "authorize"),
        ("Gate::authorize(", "Gate::authorize"),
        ("Gate::allows(", "Gate::allows"),
        ("Gate::denies(", "Gate::denies"),
        ("Gate::check(", "Gate::check"),
        ("->can(", "can"),
        ("->cannot(", "cannot"),
        ("->hasRole(", "hasRole"),
        ("->hasPermissionTo(", "hasPermissionTo"),
        ("->hasAnyRole(", "hasAnyRole"),
    ];
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        for (needle, how) in CALLS {
            let mut from = 0;
            while let Some(p) = line[from..].find(needle) {
                let at = from + p + needle.len();
                let ability = quoted_after(line, at).unwrap_or_else(|| "?".into());
                out.push((ability, (*how).to_owned(), first_line + i as u32));
                from = at;
            }
        }
        if line.contains("abort(403")
            || line.contains("abort_unless(")
            || line.contains("abort_if(")
        {
            out.push(("(inline)".into(), "abort".into(), first_line + i as u32));
        }
    }
    out
}

/// What `authorize()` of a Form Request returns, compacted.
fn form_request_rule(conn: &Connection, root: &Path, class: &str) -> Option<String> {
    let m = method_symbol(conn, class, "authorize")?;
    let text = body(root, &m);
    let ret = text
        .split("return")
        .nth(1)?
        .split(';')
        .next()?
        .trim()
        .to_owned();
    let ret: String = ret.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(match ret.as_str() {
        "true" => "always allowed".to_owned(),
        "false" => "always denied".to_owned(),
        _ => codemap_php::ast::truncate(&ret, 100),
    })
}

/// `App\Models\Order` → `App\Policies\OrderPolicy::{ability}` when it exists.
fn policy_method(conn: &Connection, model: &str, ability: &str) -> Option<String> {
    let short = model.rsplit('\\').next()?;
    for ns in ["App\\Policies\\", "App\\Models\\Policies\\"] {
        let policy = format!("{ns}{short}Policy");
        if let Some(m) = method_symbol(conn, &policy, ability) {
            return Some(format!("{policy}::{ability} {}:{}", m.file, m.line));
        }
    }
    None
}

/// Permissions of one route, as JSON.
pub fn for_route(conn: &Connection, root: &Path, r: &RouteDef) -> Value {
    let middleware = access_middleware(&r.middleware);
    let mut v = json!({ "middleware": middleware });
    let (Some(class), Some(method)) = (&r.controller_fqn, &r.controller_method) else {
        v["public"] = json!(middleware.is_empty());
        return v;
    };
    let Some(action) = method_symbol(conn, class, method) else {
        return v;
    };
    // Models the action receives (route model binding), for policy lookup.
    let models: Vec<String> = action
        .meta
        .params
        .iter()
        .flat_map(|p| p.type_fqns.iter().cloned())
        .filter(|t| t.contains("\\Models\\"))
        .collect();
    let text = body(root, &action);
    let found = checks(&text, action.line);
    // `can:update,order` → the policy method for the action's `$order` model.
    let mut via_middleware: Vec<String> = Vec::new();
    for m in &middleware {
        let Some(rest) = m.strip_prefix("can:") else {
            continue;
        };
        let mut parts = rest.split(',');
        let ability = parts.next().unwrap_or_default();
        let param = parts.next().unwrap_or_default().trim();
        let model = action
            .meta
            .params
            .iter()
            .find(|p| p.name.trim_start_matches('$') == param)
            .and_then(|p| p.type_fqns.first().cloned())
            .or_else(|| param.contains('\\').then(|| param.to_owned()));
        if let Some(model) = model
            && let Some(p) = policy_method(conn, &model, ability)
        {
            via_middleware.push(format!("{m} → {p}"));
        }
    }
    if !via_middleware.is_empty() {
        v["policies"] = json!(via_middleware);
    }
    let checks_v: Vec<String> = found
        .iter()
        .map(|(ability, how, line)| {
            let policy = models
                .iter()
                .find_map(|m| policy_method(conn, m, ability))
                .map(|p| format!(" → {p}"))
                .unwrap_or_default();
            format!("{how} '{ability}' {}:{line}{policy}", action.file)
        })
        .collect();
    if !checks_v.is_empty() {
        v["checks"] = json!(checks_v);
    }
    // Form Requests type-hinted on the action.
    for p in &action.meta.params {
        for t in &p.type_fqns {
            let is_request = symbols::by_fqn(conn, t)
                .ok()
                .and_then(|s| s.into_iter().next())
                .is_some_and(|s| s.meta.extends.iter().any(|e| e.ends_with("FormRequest")));
            if is_request && let Some(rule) = form_request_rule(conn, root, t) {
                v["form_request"] = json!(format!(
                    "{} authorize(): {rule}",
                    t.rsplit('\\').next().unwrap_or(t)
                ));
            }
        }
    }
    v["public"] =
        json!(middleware.is_empty() && found.is_empty() && v.get("form_request").is_none());
    v
}

/// One line for brief listings: `auth:sanctum, can:update,order · authorize 'update'`.
pub fn summary(v: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = v["middleware"].as_array().filter(|m| !m.is_empty()) {
        parts.push(
            m.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    for c in v["checks"].as_array().into_iter().flatten().take(3) {
        if let Some(c) = c.as_str() {
            parts.push(c.split(" app/").next().unwrap_or(c).to_owned());
        }
    }
    if let Some(f) = v["form_request"].as_str() {
        parts.push(f.to_owned());
    }
    if v["public"] == true {
        parts.push("public: no access check found".into());
    }
    parts.join(" · ")
}

/// Who may open a Filament page or resource: the model's policy methods,
/// and `canAccess()` / `canViewAny()` overrides on the page class.
pub fn for_admin_page(conn: &Connection, root: &Path, class: &str, model: Option<&str>) -> Value {
    let mut v = json!({});
    let mut found = Vec::new();
    for m in [
        "canAccess",
        "canViewAny",
        "canCreate",
        "canEdit",
        "canDelete",
        "shouldRegisterNavigation",
    ] {
        if let Some(s) = method_symbol(conn, class, m) {
            let text = body(root, &s);
            let ret = text
                .split("return")
                .nth(1)
                .and_then(|r| r.split(';').next())
                .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            found.push(format!(
                "{m}(): {} {}:{}",
                codemap_php::ast::truncate(&ret, 80),
                s.file,
                s.line
            ));
        }
    }
    if !found.is_empty() {
        v["overrides"] = json!(found);
    }
    if let Some(model) = model {
        let abilities: Vec<String> = ["viewAny", "view", "create", "update", "delete"]
            .iter()
            .filter_map(|a| policy_method(conn, model, a))
            .collect();
        v["policy"] = if abilities.is_empty() {
            json!(format!(
                "no {}Policy found: every user who can open the panel may use it",
                model.rsplit('\\').next().unwrap_or(model)
            ))
        } else {
            json!(abilities)
        };
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_middleware_skips_plumbing() {
        let mw: Vec<String> = [
            "api",
            "auth:sanctum",
            "throttle:merchant",
            "App\\Http\\Middleware\\EnsureMerchant",
            "can:access,outlet",
            "Illuminate\\Routing\\Middleware\\SubstituteBindings",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(
            access_middleware(&mw),
            [
                "auth:sanctum",
                "App\\Http\\Middleware\\EnsureMerchant",
                "can:access,outlet"
            ]
        );
    }

    #[test]
    fn checks_in_an_action() {
        let body = "public function update(Order $order) {\n    $this->authorize('update', $order);\n    if (! $request->user()->can('refund', $order)) { abort(403); }\n}";
        let found = checks(body, 10);
        assert_eq!(
            found,
            [
                ("update".to_owned(), "authorize".to_owned(), 11),
                ("refund".to_owned(), "can".to_owned(), 12),
                ("(inline)".to_owned(), "abort".to_owned(), 12),
            ]
        );
    }
}
