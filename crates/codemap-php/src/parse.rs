//! A tree-sitter parser per thread, so files can be parsed in parallel.

use std::cell::RefCell;

use tree_sitter::{Parser, Tree};

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .expect("tree-sitter-php grammar matches the tree-sitter version");
        parser
    });
}

/// Parses PHP source (with or without leading HTML). `None` only if parsing was cancelled.
pub fn parse(src: &[u8]) -> Option<Tree> {
    let src = blank_casts_before_match(src);
    PARSER.with(|p| p.borrow_mut().parse(&src, None))
}

const CASTS: &[&[u8]] = &[
    b"int", b"integer", b"bool", b"boolean", b"float", b"double", b"real", b"string", b"array",
    b"object", b"unset", b"binary",
];

/// `(int) match ($x) { … }` is valid PHP 8 the grammar cannot read (it takes
/// `match` for a function name). The cast is blanked out (same length, so
/// lines and columns stay right); it changes nothing codemap reports.
fn blank_casts_before_match(src: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let space = |c: u8| c == b' ' || c == b'\t' || c == b'\n' || c == b'\r';
    let mut i = 0;
    while let Some(at) = src[i..].iter().position(|&c| c == b'(') {
        let start = i + at;
        i = start + 1;
        let mut j = start + 1;
        while j < src.len() && space(src[j]) {
            j += 1;
        }
        let Some(cast) = CASTS
            .iter()
            .find(|c| src.len() >= j + c.len() && src[j..j + c.len()].eq_ignore_ascii_case(c))
        else {
            continue;
        };
        j += cast.len();
        while j < src.len() && space(src[j]) {
            j += 1;
        }
        if src.get(j) != Some(&b')') {
            continue;
        }
        let close = j;
        j += 1;
        while j < src.len() && space(src[j]) {
            j += 1;
        }
        let is_match = src.len() >= j + 5
            && src[j..j + 5].eq_ignore_ascii_case(b"match")
            && src[j + 5..]
                .iter()
                .find(|c| !space(**c))
                .is_some_and(|c| *c == b'(');
        if is_match {
            out.get_or_insert_with(|| src.to_vec())[start..=close].fill(b' ');
            i = close + 1;
        }
    }
    match out {
        Some(v) => std::borrow::Cow::Owned(v),
        None => std::borrow::Cow::Borrowed(src),
    }
}

#[cfg(test)]
mod tests {
    /// #30: a cast in front of `match` no longer stops parsing.
    #[test]
    fn cast_before_match() {
        let src = "<?php\nclass Counter\n{\n    private static function count(Thing $t): int\n    {\n        return (int) match ($t->kind) {\n            'a' => $t->a_count,\n            default => 0,\n        };\n    }\n}\n";
        let t = super::parse(src.as_bytes()).unwrap();
        assert!(!t.root_node().has_error(), "{}", t.root_node().to_sexp());
        // Other casts are left alone.
        assert_eq!(
            &*super::blank_casts_before_match(b"(int) $x; (string) f()"),
            b"(int) $x; (string) f()"
        );
        assert_eq!(
            &*super::blank_casts_before_match(b"( bool )match(1)"),
            b"        match(1)"
        );
    }
}
