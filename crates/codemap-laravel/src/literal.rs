//! Literal PHP values in models, migrations and Form Requests. Anything that
//! is not a literal is kept as its (compacted) source text, never evaluated.

use codemap_php::Scope;
use codemap_php::ast::{compact, named_children, text};
use tree_sitter::Node;

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Str(String),
    Int(i64),
    Float(String),
    Bool(bool),
    Null,
    /// `X::class`, resolved.
    Class(String),
    Arr(Vec<(Option<String>, Lit)>),
    /// Not a literal: the source text, e.g. `Rule::in(['a'])` or `now()`.
    Raw(String),
}

impl Lit {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Lit::Str(s) | Lit::Class(s) => Some(s),
            _ => None,
        }
    }

    /// A string, or a list of strings.
    pub fn strings(&self) -> Option<Vec<String>> {
        match self {
            Lit::Str(s) => Some(vec![s.clone()]),
            Lit::Arr(items) => items
                .iter()
                .map(|(_, v)| v.as_str().map(str::to_owned))
                .collect(),
            _ => None,
        }
    }

    /// PHP-like source form, for defaults and rule text.
    pub fn display(&self) -> String {
        match self {
            Lit::Str(s) => format!("'{s}'"),
            Lit::Int(i) => i.to_string(),
            Lit::Float(f) => f.clone(),
            Lit::Bool(b) => b.to_string(),
            Lit::Null => "null".into(),
            Lit::Class(c) => format!("{c}::class"),
            Lit::Arr(items) => {
                let parts: Vec<String> = items
                    .iter()
                    .map(|(k, v)| match k {
                        Some(k) => format!("'{k}' => {}", v.display()),
                        None => v.display(),
                    })
                    .collect();
                format!("[{}]", parts.join(", "))
            }
            Lit::Raw(r) => r.clone(),
        }
    }

    /// Plain value text for JSON output: strings unquoted, the rest as PHP.
    pub fn plain(&self) -> String {
        match self {
            Lit::Str(s) => s.clone(),
            other => other.display(),
        }
    }
}

pub fn eval(node: Node<'_>, src: &[u8], scope: &Scope) -> Lit {
    let raw = text(node, src);
    match node.kind() {
        "string" => Lit::Str(unquote_single(raw)),
        "encapsed_string" => {
            let interpolated = named_children(node)
                .iter()
                .any(|c| !matches!(c.kind(), "string_content" | "escape_sequence"));
            if interpolated {
                Lit::Raw(compact(raw))
            } else {
                Lit::Str(unquote_double(raw))
            }
        }
        "integer" => raw
            .replace('_', "")
            .parse()
            .map_or_else(|_| Lit::Raw(raw.into()), Lit::Int),
        // <<<'SQL' ... SQL (no interpolation) and <<<SQL ... SQL without variables.
        "nowdoc" | "heredoc" => match heredoc_body(raw) {
            Some(body) if node.kind() == "nowdoc" || !body.contains('$') => Lit::Str(body),
            _ => Lit::Raw(compact(raw)),
        },
        "float" => Lit::Float(raw.to_owned()),
        "boolean" => Lit::Bool(raw.eq_ignore_ascii_case("true")),
        "null" => Lit::Null,
        "unary_op_expression" => match named_children(node).first().map(|c| eval(*c, src, scope)) {
            Some(Lit::Int(i)) if raw.trim_start().starts_with('-') => Lit::Int(-i),
            _ => Lit::Raw(compact(raw)),
        },
        "parenthesized_expression" => named_children(node)
            .first()
            .map_or(Lit::Null, |c| eval(*c, src, scope)),
        "class_constant_access_expression" => {
            let parts = named_children(node);
            match (parts.first(), parts.last()) {
                (Some(cls), Some(name)) if text(*name, src) == "class" => scope
                    .resolve_class(text(*cls, src))
                    .map_or_else(|| Lit::Raw(compact(raw)), Lit::Class),
                _ => Lit::Raw(compact(raw)),
            }
        }
        "array_creation_expression" => {
            let mut items = Vec::new();
            for el in named_children(node) {
                if el.kind() != "array_element_initializer" {
                    continue;
                }
                match named_children(el).as_slice() {
                    [value] if value.kind() != "variadic_unpacking" => {
                        items.push((None, eval(*value, src, scope)));
                    }
                    [key, value] => {
                        let key = match eval(*key, src, scope) {
                            Lit::Str(k) | Lit::Class(k) => k,
                            Lit::Int(i) => i.to_string(),
                            other => other.display(),
                        };
                        items.push((Some(key), eval(*value, src, scope)));
                    }
                    _ => return Lit::Raw(compact(raw)),
                }
            }
            Lit::Arr(items)
        }
        _ => Lit::Raw(compact(raw)),
    }
}

/// The text between the `<<<ID` line and the closing `ID` line.
fn heredoc_body(raw: &str) -> Option<String> {
    let (_, rest) = raw.split_once('\n')?;
    let (body, _) = rest.rsplit_once('\n')?;
    Some(body.to_owned())
}

/// PHP truthiness of a known value; `None` when the value is not known statically.
pub fn truthy(v: &Lit) -> Option<bool> {
    match v {
        Lit::Bool(b) => Some(*b),
        Lit::Null => Some(false),
        Lit::Int(i) => Some(*i != 0),
        Lit::Str(s) => Some(!s.is_empty() && s != "0"),
        Lit::Arr(items) => Some(!items.is_empty()),
        Lit::Class(_) => Some(true),
        Lit::Float(_) | Lit::Raw(_) => None,
    }
}

pub fn unquote_single(raw: &str) -> String {
    let inner = raw
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(raw);
    inner.replace("\\'", "'").replace("\\\\", "\\")
}

pub fn unquote_double(raw: &str) -> String {
    let inner = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    inner
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\"", "\"")
        .replace("\\$", "$")
        .replace("\\\\", "\\")
}

/// `OrderItem` → `order_item` (Laravel `Str::snake`).
pub fn snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else if c == '-' || c == ' ' {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case() {
        assert_eq!(snake("OrderItem"), "order_item");
        assert_eq!(snake("TotalFormatted"), "total_formatted");
        assert_eq!(snake("user"), "user");
    }
}
