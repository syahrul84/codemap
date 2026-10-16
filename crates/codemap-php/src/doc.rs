//! Docblock summaries: the first sentence of a `/** ... */` comment.

use tree_sitter::Node;

use crate::ast::{text, truncate};

pub const MAX_SUMMARY_CHARS: usize = 120;

/// Summary of the docblock directly above `node`, if any.
pub fn doc_before(node: Node<'_>, src: &[u8]) -> Option<String> {
    let prev = node.prev_named_sibling()?;
    if prev.kind() != "comment" || prev.end_position().row + 1 < node.start_position().row {
        return None;
    }
    summary(text(prev, src))
}

/// Text before the first blank line or `@tag`, cut to the first sentence.
pub fn summary(raw: &str) -> Option<String> {
    let body = raw.trim().strip_prefix("/**")?;
    let body = body.strip_suffix("*/").unwrap_or(body);
    let mut parts: Vec<&str> = Vec::new();
    for line in body.lines() {
        let line = line.trim().trim_start_matches('*').trim();
        if line.starts_with('@') {
            break;
        }
        if line.is_empty() {
            if parts.is_empty() {
                continue;
            }
            break;
        }
        parts.push(line);
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join(" ");
    let sentence = match joined.find(". ") {
        Some(i) => &joined[..=i],
        None => joined.as_str(),
    };
    Some(truncate(sentence.trim(), MAX_SUMMARY_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sentence_before_tags() {
        let doc = "/**\n * Creates an order for the customer.\n * Also sends mail.\n *\n * @param int $id\n */";
        assert_eq!(summary(doc).unwrap(), "Creates an order for the customer.");
    }

    #[test]
    fn multi_line_sentence_is_joined() {
        let doc = "/**\n * Recalculates totals\n * across all items\n */";
        assert_eq!(
            summary(doc).unwrap(),
            "Recalculates totals across all items"
        );
    }

    #[test]
    fn single_line_and_tag_only() {
        assert_eq!(summary("/** Marks it paid. */").unwrap(), "Marks it paid.");
        assert_eq!(summary("/** @var array<string> */"), None);
        assert_eq!(summary("// not a docblock"), None);
        assert_eq!(summary("/* plain block */"), None);
    }

    #[test]
    fn long_summaries_are_cut() {
        let doc = format!("/** {} */", "word ".repeat(60));
        let s = summary(&doc).unwrap();
        assert_eq!(s.chars().count(), MAX_SUMMARY_CHARS);
        assert!(s.ends_with('…'));
    }
}
