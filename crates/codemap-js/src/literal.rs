//! Reads literal values (objects, arrays, strings, numbers, booleans) from
//! JS/TS code as JSON, e.g. `export default defineNuxtConfig({...})`.
//! Anything that is not a literal becomes `{"$expr": "<source>"}`, so callers
//! can tell "unknown" from "absent".

use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_parser::Parser;
use oxc_span::GetSpan;
use serde_json::{Map, Value, json};

use crate::script::source_type_for;

/// The value of the module's default export (unwrapping one call such as
/// `defineNuxtConfig({...})` / `defineConfig(...)`), or of `module.exports = …`.
pub fn default_export(src: &str, lang: &str) -> Option<Value> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, src, source_type_for(lang)).parse();
    if ret.fatal_error {
        return None;
    }
    let program = &ret.program;
    let consts = top_consts(program);
    for stmt in &program.body {
        let expr = match stmt {
            Statement::ExportDefaultDeclaration(d) => d.declaration.as_expression(),
            Statement::ExpressionStatement(x) => match &x.expression {
                Expression::AssignmentExpression(a)
                    if src[a.left.span().start as usize..a.left.span().end as usize]
                        == *"module.exports" =>
                {
                    Some(&a.right)
                }
                _ => None,
            },
            _ => None,
        };
        let Some(mut expr) = expr else {
            continue;
        };
        if let Expression::CallExpression(c) = expr.without_parentheses()
            && let Some(arg) = c.arguments.first().and_then(|a| a.as_expression())
        {
            expr = arg;
        }
        if let Expression::Identifier(id) = expr.without_parentheses()
            && let Some(init) = consts.iter().find(|(n, _)| *n == id.name.as_str())
        {
            expr = init.1;
        }
        return Some(eval(expr, src, &consts, 0));
    }
    None
}

/// Every top-level `export const name = <literal>` (and `export default`) as JSON.
pub fn exports(src: &str, lang: &str) -> Map<String, Value> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, src, source_type_for(lang)).parse();
    let mut out = Map::new();
    if ret.fatal_error {
        return out;
    }
    let consts = top_consts(&ret.program);
    for stmt in &ret.program.body {
        if let Statement::ExportDeclaration(d) = stmt
            && let Declaration::VariableDeclaration(v) = &d.declaration
        {
            for d in &v.declarations {
                if let (BindingPattern::BindingIdentifier(id), Some(init)) = (&d.id, &d.init) {
                    out.insert(id.name.to_string(), eval(init, src, &consts, 0));
                }
            }
        }
    }
    if let Some(v) = default_export(src, lang) {
        out.insert("default".into(), v);
    }
    out
}

fn top_consts<'b, 'a>(program: &'b Program<'a>) -> Vec<(&'b str, &'b Expression<'a>)> {
    let mut out = Vec::new();
    for stmt in &program.body {
        let decl = match stmt {
            Statement::VariableDeclaration(v) => Some(&**v),
            Statement::ExportDeclaration(d) => match &d.declaration {
                Declaration::VariableDeclaration(v) => Some(&**v),
                _ => None,
            },
            _ => None,
        };
        for d in decl.into_iter().flat_map(|v| v.declarations.iter()) {
            if let (BindingPattern::BindingIdentifier(id), Some(init)) = (&d.id, &d.init) {
                out.push((id.name.as_str(), init));
            }
        }
    }
    out
}

fn unknown(e: &Expression<'_>, src: &str) -> Value {
    let span = e.span();
    let text: String = src[span.start as usize..span.end as usize]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    json!({ "$expr": text.chars().take(120).collect::<String>() })
}

pub fn eval(e: &Expression<'_>, src: &str, consts: &[(&str, &Expression<'_>)], depth: u8) -> Value {
    if depth > 12 {
        return unknown(e, src);
    }
    match e.without_parentheses() {
        Expression::StringLiteral(s) => json!(s.value.as_str()),
        Expression::NumericLiteral(n) => json!(n.value),
        Expression::BooleanLiteral(b) => json!(b.value),
        Expression::NullLiteral(_) => Value::Null,
        Expression::TemplateLiteral(t) if t.expressions.is_empty() => json!(
            t.quasis
                .first()
                .map(|q| q.value.cooked.unwrap_or(q.value.raw).to_string())
                .unwrap_or_default()
        ),
        Expression::TSAsExpression(t) => eval(&t.expression, src, consts, depth + 1),
        Expression::TSSatisfiesExpression(t) => eval(&t.expression, src, consts, depth + 1),
        Expression::Identifier(id) => match consts.iter().find(|(n, _)| *n == id.name.as_str()) {
            Some((_, init)) => eval(init, src, consts, depth + 1),
            None => unknown(e, src),
        },
        Expression::ArrayExpression(a) => Value::Array(
            a.elements
                .iter()
                .map(|el| match el.as_expression() {
                    Some(x) => eval(x, src, consts, depth + 1),
                    None => Value::Null,
                })
                .collect(),
        ),
        Expression::ObjectExpression(o) => {
            let mut map = Map::new();
            for p in &o.properties {
                match p {
                    ObjectPropertyKind::ObjectProperty(p) => {
                        let Some(key) = p.key.static_name() else {
                            continue;
                        };
                        map.insert(key.to_string(), eval(&p.value, src, consts, depth + 1));
                    }
                    ObjectPropertyKind::SpreadProperty(s) => {
                        match eval(&s.argument, src, consts, depth + 1) {
                            // An unknown spread (`...common`): its keys are not known.
                            v if is_unknown(&v) => {
                                map.insert("$partial".into(), Value::Bool(true));
                            }
                            Value::Object(inner) => map.extend(inner),
                            _ => {}
                        }
                    }
                }
            }
            Value::Object(map)
        }
        _ => unknown(e, src),
    }
}

/// `true` when `v` is an unknown expression placeholder.
pub fn is_unknown(v: &Value) -> bool {
    v.get("$expr").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nuxt_config() {
        let src = r#"
import { resolve } from 'node:path'
const prefix = 'gb'
export default defineNuxtConfig({
  srcDir: '.',
  components: [{ path: '~/components', pathPrefix: false }, '~/other'],
  runtimeConfig: { public: { apiBase: process.env.API || 'http://x/api' } },
  i18n: { locales: [{ code: 'en', file: 'en.json' }], defaultLocale: 'en', lazy: true },
  icon: { prefix },
  ...({ ssr: false }),
})
"#;
        let v = default_export(src, "ts").unwrap();
        assert_eq!(v["srcDir"], ".");
        assert_eq!(v["components"][0]["pathPrefix"], false);
        assert_eq!(v["components"][1], "~/other");
        assert!(is_unknown(&v["runtimeConfig"]["public"]["apiBase"]));
        assert_eq!(v["i18n"]["locales"][0]["code"], "en");
        assert_eq!(v["icon"]["prefix"], "gb");
        assert_eq!(v["ssr"], false);
    }

    #[test]
    fn module_exports_and_named_exports() {
        let v = default_export(
            "module.exports = { lintOnSave: false, publicPath: '/app/' }",
            "js",
        )
        .unwrap();
        assert_eq!(v["publicPath"], "/app/");
        let m = exports(
            "export const en = { hello: 'Hello', nested: { a: 'A' } }\nexport default { x: 1 }",
            "ts",
        );
        assert_eq!(m["en"]["nested"]["a"], "A");
        assert_eq!(m["default"]["x"], 1.0);
    }
}
