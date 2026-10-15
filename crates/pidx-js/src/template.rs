//! A small, forgiving tokenizer for Vue templates: elements with their
//! attributes (directives included) and `{{ }}` interpolations, with lines.
//! It never fails; malformed markup is skipped.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attr {
    /// As written: `class`, `:foo`, `@click.prevent`, `v-model:title`, `#item`.
    pub name: String,
    pub value: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Element {
    /// As written: `div`, `BaseButton`, `base-button`, `router-link`.
    pub tag: String,
    pub attrs: Vec<Attr>,
    pub line: u32,
    /// Index of the enclosing element in [`Template::elements`].
    pub parent: Option<usize>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Interpolation {
    pub expr: String,
    pub line: u32,
    /// Innermost enclosing element.
    pub parent: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Template {
    pub elements: Vec<Element>,
    pub interpolations: Vec<Interpolation>,
}

/// An opening tag found at some offset.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenTag {
    pub name: String,
    pub attrs: Vec<Attr>,
    pub self_closing: bool,
    /// Offset just past `>`.
    pub end: usize,
}

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// Raw-text elements whose content is not markup.
const RAW: &[&str] = &["script", "style", "textarea", "pre"];

/// Scans template markup. `first_line` is the line of `src`'s first byte.
pub fn scan(src: &str, first_line: u32) -> Template {
    let bytes = src.as_bytes();
    let mut out = Template::default();
    let mut stack: Vec<(String, usize)> = Vec::new();
    let mut line = first_line;
    let mut counted = 0usize;
    let mut line_of = |offset: usize| -> u32 {
        line += bytes[counted..offset]
            .iter()
            .filter(|&&b| b == b'\n')
            .count() as u32;
        counted = offset;
        line
    };
    let mut i = 0;
    while i < bytes.len() {
        // `<` and `{` are ASCII, so slicing there is always on a char boundary.
        if bytes[i] != b'<' && bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let rest = &src[i..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(bytes.len(), |e| i + e + 3);
            continue;
        }
        if let Some(inner) = rest.strip_prefix("{{") {
            let Some(end) = inner.find("}}") else {
                break;
            };
            out.interpolations.push(Interpolation {
                expr: inner[..end].trim().to_owned(),
                line: line_of(i),
                parent: stack.last().map(|(_, idx)| *idx),
            });
            i += end + 4;
            continue;
        }
        if let Some(after) = rest.strip_prefix("</") {
            let name_end = after
                .find(|c: char| c == '>' || c.is_ascii_whitespace())
                .unwrap_or(after.len());
            let name = &after[..name_end];
            if let Some(pos) = stack
                .iter()
                .rposition(|(t, _)| t.eq_ignore_ascii_case(name))
            {
                stack.truncate(pos);
            }
            i = rest.find('>').map_or(bytes.len(), |e| i + e + 1);
            continue;
        }
        if bytes[i] == b'<' && bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
            let tag_line = line_of(i);
            let Some(open) = parse_open_tag(src, i) else {
                i += 1;
                continue;
            };
            // Attribute lines: count newlines inside the tag up to each attribute.
            let attrs = open
                .attrs
                .into_iter()
                .map(|mut a| {
                    a.line += tag_line;
                    a
                })
                .collect();
            let idx = out.elements.len();
            out.elements.push(Element {
                tag: open.name.clone(),
                attrs,
                line: tag_line,
                parent: stack.last().map(|(_, idx)| *idx),
            });
            let lower = open.name.to_ascii_lowercase();
            i = open.end;
            if open.self_closing || VOID.contains(&lower.as_str()) {
                continue;
            }
            if RAW.contains(&lower.as_str()) {
                i = find_ci(src, i, &format!("</{lower}")).unwrap_or(bytes.len());
                continue;
            }
            stack.push((open.name, idx));
            continue;
        }
        i += 1;
    }
    out
}

/// Parses `<name attr=… …>` at `start` (which must be `<`). Attribute `line`s
/// are relative (0 = the line of `<`).
pub fn parse_open_tag(src: &str, start: usize) -> Option<OpenTag> {
    let bytes = src.as_bytes();
    let mut i = start + 1;
    let name_start = i;
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'-' | b'_' | b'.' | b':'))
    {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = src[name_start..i].to_owned();
    let mut attrs = Vec::new();
    let mut rel_line = 0u32;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            if bytes[i] == b'\n' {
                rel_line += 1;
            }
            i += 1;
        }
        match bytes.get(i)? {
            b'>' => {
                return Some(OpenTag {
                    name,
                    attrs,
                    self_closing: false,
                    end: i + 1,
                });
            }
            b'/' if bytes.get(i + 1) == Some(&b'>') => {
                return Some(OpenTag {
                    name,
                    attrs,
                    self_closing: true,
                    end: i + 2,
                });
            }
            b'/' => {
                i += 1;
                continue;
            }
            b'<' => return None,
            _ => {}
        }
        // Attribute name. `[` … `]` (dynamic arguments) may hold any character.
        let attr_start = i;
        let mut brackets = 0i32;
        while i < bytes.len() {
            let c = bytes[i];
            match c {
                b'[' => brackets += 1,
                b']' => brackets -= 1,
                _ if brackets > 0 => {}
                b'=' | b'>' => break,
                b'/' if bytes.get(i + 1) == Some(&b'>') => break,
                _ if c.is_ascii_whitespace() => break,
                _ => {}
            }
            i += 1;
        }
        let attr_name = src[attr_start..i].to_owned();
        let attr_line = rel_line;
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let value = if bytes.get(j) == Some(&b'=') {
            j += 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            match bytes.get(j)? {
                q @ (b'"' | b'\'') => {
                    let end = src[j + 1..].find(*q as char).map(|e| j + 1 + e)?;
                    let v = &src[j + 1..end];
                    rel_line += v.matches('\n').count() as u32;
                    i = end + 1;
                    Some(v.to_owned())
                }
                _ => {
                    let s = j;
                    while j < bytes.len() && !bytes[j].is_ascii_whitespace() && bytes[j] != b'>' {
                        j += 1;
                    }
                    i = j;
                    Some(src[s..j].to_owned())
                }
            }
        } else {
            None
        };
        if attr_name.is_empty() {
            i += 1;
            continue;
        }
        attrs.push(Attr {
            name: attr_name,
            value,
            line: attr_line,
        });
    }
}

fn find_ci(src: &str, from: usize, needle: &str) -> Option<usize> {
    let hay = src.as_bytes();
    let n = needle.as_bytes();
    (from..hay.len().saturating_sub(n.len() - 1))
        .find(|&i| hay[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// `base-button` → `BaseButton`; `BaseButton` stays.
pub fn pascal(tag: &str) -> String {
    tag.split(['-', '_', '.', ':'])
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut c = s.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// `BaseButton` → `base-button`.
pub fn kebab(name: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            let prev_lower =
                i > 0 && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit());
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if i > 0 && (prev_lower || (next_lower && chars[i - 1].is_ascii_uppercase())) {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else if *c == '_' {
            out.push('-');
        } else {
            out.push(*c);
        }
    }
    out
}

/// HTML and SVG element names, and Vue's built-ins: never project components.
pub fn is_native_or_builtin(tag: &str) -> bool {
    const BUILTIN: &[&str] = &[
        "template",
        "slot",
        "component",
        "transition",
        "transition-group",
        "keep-alive",
        "teleport",
        "suspense",
        "router-view",
        "router-link",
        "nuxt-link",
        "nuxt",
        "nuxt-child",
        "client-only",
        "no-ssr",
    ];
    if tag.contains('-') || tag.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return BUILTIN.contains(&kebab(tag).as_str());
    }
    // Lower-case, no dash: HTML / SVG / MathML elements (custom elements need a dash).
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_attrs_and_interpolations() {
        let src = "<div class=\"a\">\n  <BaseButton\n    :label=\"t('save')\"\n    @click.prevent=\"save\"\n    v-model:title=\"x\"\n    disabled\n  />\n  <ul><li v-for=\"item in items\" :key=\"item.id\">{{ item.name }}</li></ul>\n  <!-- <Ignored/> -->\n  <base-input v-bind:[key]=\"v\" #default=\"{ row }\">{{ row.a }}</base-input>\n  <img src=x>\n  <pre><Fake></pre>\n</div>";
        let t = scan(src, 10);
        let tags: Vec<(&str, u32, Option<usize>)> = t
            .elements
            .iter()
            .map(|e| (e.tag.as_str(), e.line, e.parent))
            .collect();
        assert_eq!(
            tags,
            [
                ("div", 10, None),
                ("BaseButton", 11, Some(0)),
                ("ul", 17, Some(0)),
                ("li", 17, Some(2)),
                ("base-input", 19, Some(0)),
                ("img", 20, Some(0)),
                ("pre", 21, Some(0)),
            ]
        );
        let b = &t.elements[1];
        let names: Vec<(&str, u32)> = b.attrs.iter().map(|a| (a.name.as_str(), a.line)).collect();
        assert_eq!(
            names,
            [
                (":label", 12),
                ("@click.prevent", 13),
                ("v-model:title", 14),
                ("disabled", 15)
            ]
        );
        assert_eq!(
            b.attr(":label").unwrap().value.as_deref(),
            Some("t('save')")
        );
        assert_eq!(t.elements[4].attrs[0].name, "v-bind:[key]");
        assert_eq!(t.elements[4].attrs[1].name, "#default");
        let interp: Vec<(&str, u32, Option<usize>)> = t
            .interpolations
            .iter()
            .map(|i| (i.expr.as_str(), i.line, i.parent))
            .collect();
        assert_eq!(interp, [("item.name", 17, Some(3)), ("row.a", 19, Some(4))]);
    }

    #[test]
    fn names() {
        assert_eq!(pascal("base-button"), "BaseButton");
        assert_eq!(pascal("BaseButton"), "BaseButton");
        assert_eq!(kebab("BaseButton"), "base-button");
        assert_eq!(kebab("UButton"), "u-button");
        assert_eq!(kebab("HTMLEditor"), "html-editor");
        assert_eq!(kebab("Step2Form"), "step2-form");
        assert!(is_native_or_builtin("div"));
        assert!(is_native_or_builtin("RouterLink"));
        assert!(is_native_or_builtin("nuxt-link"));
        assert!(!is_native_or_builtin("BaseButton"));
        assert!(!is_native_or_builtin("my-thing"));
    }
}
