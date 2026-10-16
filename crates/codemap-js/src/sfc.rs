//! Splits a Vue single-file component into its top-level blocks
//! (`<template>`, `<script>`, `<script setup>`, `<style>`, `<i18n>`, …).
//! Only top-level tags count; a `<template>` block may nest other `<template>`s.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Block<'a> {
    /// Lower-case tag name: `template`, `script`, `style`, `i18n`, ….
    pub tag: String,
    pub attrs: Vec<(String, Option<String>)>,
    pub content: &'a str,
    /// Byte offset of `content` in the file.
    pub offset: usize,
    /// 1-based line of the first byte of `content`.
    pub line: u32,
}

impl Block<'_> {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_deref().unwrap_or(""))
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attr(name).is_some()
    }

    /// `lang` attribute, defaulting to `js` for scripts and `html` for templates.
    pub fn lang(&self) -> &str {
        match self.attr("lang") {
            Some(l) if !l.is_empty() => l,
            _ if self.tag == "template" => "html",
            _ => "js",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Sfc<'a> {
    pub blocks: Vec<Block<'a>>,
}

impl<'a> Sfc<'a> {
    pub fn template(&self) -> Option<&Block<'a>> {
        self.blocks.iter().find(|b| b.tag == "template")
    }

    /// The classic `<script>` (not `setup`).
    pub fn script(&self) -> Option<&Block<'a>> {
        self.blocks
            .iter()
            .find(|b| b.tag == "script" && !b.has_attr("setup"))
    }

    pub fn script_setup(&self) -> Option<&Block<'a>> {
        self.blocks
            .iter()
            .find(|b| b.tag == "script" && b.has_attr("setup"))
    }

    pub fn custom(&self, tag: &str) -> impl Iterator<Item = &Block<'a>> {
        self.blocks.iter().filter(move |b| b.tag == tag)
    }
}

pub fn split(src: &str) -> Sfc<'_> {
    let bytes = src.as_bytes();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Only `<` (ASCII, so always a char boundary) can start anything we read.
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if src[i..].starts_with("<!--") {
            i = src[i..].find("-->").map_or(bytes.len(), |e| i + e + 3);
            continue;
        }
        if bytes[i] != b'<' || !bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
            i += 1;
            continue;
        }
        let Some(open) = crate::template::parse_open_tag(src, i) else {
            i += 1;
            continue;
        };
        let tag = open.name.to_ascii_lowercase();
        if open.self_closing {
            i = open.end;
            continue;
        }
        let content_start = open.end;
        let close = if tag == "template" {
            matching_template_close(src, content_start)
        } else {
            find_ci(src, content_start, &format!("</{tag}"))
        };
        let Some(close) = close else {
            // Unterminated block: take the rest of the file.
            blocks.push(Block {
                line: line_at(src, content_start),
                tag,
                attrs: open.attrs.into_iter().map(|a| (a.name, a.value)).collect(),
                content: &src[content_start..],
                offset: content_start,
            });
            break;
        };
        blocks.push(Block {
            line: line_at(src, content_start),
            tag: tag.clone(),
            attrs: open.attrs.into_iter().map(|a| (a.name, a.value)).collect(),
            content: &src[content_start..close],
            offset: content_start,
        });
        i = src[close..]
            .find('>')
            .map_or(bytes.len(), |e| close + e + 1);
    }
    Sfc { blocks }
}

/// 1-based line number of byte `offset`.
pub fn line_at(src: &str, offset: usize) -> u32 {
    1 + src.as_bytes()[..offset.min(src.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u32
}

fn find_ci(src: &str, from: usize, needle: &str) -> Option<usize> {
    let hay = src.as_bytes();
    let n = needle.as_bytes();
    (from..hay.len().saturating_sub(n.len() - 1))
        .find(|&i| hay[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// The `</template>` closing the block that starts at `from`, skipping nested
/// `<template>` elements and comments.
fn matching_template_close(src: &str, from: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &src[i..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(bytes.len(), |e| i + e + 3);
            continue;
        }
        if rest.len() >= 11 && rest.as_bytes()[..11].eq_ignore_ascii_case(b"</template>")
            || rest.len() >= 10
                && rest.as_bytes()[..10].eq_ignore_ascii_case(b"</template")
                && rest
                    .as_bytes()
                    .get(10)
                    .is_some_and(|c| c.is_ascii_whitespace())
        {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
            i += 10;
            continue;
        }
        if rest.len() >= 9
            && rest.as_bytes()[..9].eq_ignore_ascii_case(b"<template")
            && rest
                .as_bytes()
                .get(9)
                .is_some_and(|c| c.is_ascii_whitespace() || *c == b'>' || *c == b'/')
        {
            match crate::template::parse_open_tag(src, i) {
                Some(open) => {
                    if !open.self_closing {
                        depth += 1;
                    }
                    i = open.end;
                }
                None => i += 1,
            }
            continue;
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_blocks_with_nested_templates() {
        let src = "<!-- note <script> -->\n<template>\n  <div>\n    <template v-if=\"a\"><span/></template>\n  </div>\n</template>\n\n<script setup lang=\"ts\">\nconst a = 1\n</script>\n<script>\nexport default { name: 'X' }\n</script>\n<style scoped>\n.a { color: red }\n</style>\n<i18n locale=\"en\">{\"hi\": \"Hi\"}</i18n>\n";
        let sfc = split(src);
        let tags: Vec<&str> = sfc.blocks.iter().map(|b| b.tag.as_str()).collect();
        assert_eq!(tags, ["template", "script", "script", "style", "i18n"]);
        let t = sfc.template().unwrap();
        assert!(
            t.content
                .contains("<template v-if=\"a\"><span/></template>")
        );
        assert!(t.content.trim_end().ends_with("</div>"));
        assert_eq!(t.line, 2);
        let setup = sfc.script_setup().unwrap();
        assert_eq!(setup.lang(), "ts");
        assert_eq!(setup.content.trim(), "const a = 1");
        assert_eq!(setup.line, 8);
        assert_eq!(&src[setup.offset..setup.offset + 4], "\ncon");
        assert_eq!(sfc.script().unwrap().lang(), "js");
        assert_eq!(
            sfc.custom("i18n").next().unwrap().attr("locale"),
            Some("en")
        );
    }

    #[test]
    fn multibyte_text_is_safe() {
        let src = "<template>\n  <p>Price — RM 5 × 2 ≤ ’10’ {{ a }}</p>\n  <template v-if=\"x\">é</template>\n</template>\n<script>\n// café — ok\n</script>\n";
        let sfc = split(src);
        assert_eq!(sfc.blocks.len(), 2);
        let t = crate::template::scan(sfc.template().unwrap().content, 1);
        assert_eq!(t.interpolations[0].expr, "a");
    }

    #[test]
    fn script_containing_template_strings() {
        let src = "<script>\nconst t = '<template>'\n</script>\n<template><div/></template>";
        let sfc = split(src);
        assert_eq!(sfc.script().unwrap().content, "\nconst t = '<template>'\n");
        assert_eq!(sfc.template().unwrap().content, "<div/>");
    }
}
