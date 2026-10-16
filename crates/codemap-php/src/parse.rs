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
    PARSER.with(|p| p.borrow_mut().parse(src, None))
}
