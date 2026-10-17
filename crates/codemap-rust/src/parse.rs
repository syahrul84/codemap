//! A tree-sitter parser per thread, so files can be parsed in parallel.

use std::cell::RefCell;

use tree_sitter::{Parser, Tree};

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("tree-sitter-rust grammar matches the tree-sitter version");
        parser
    });
}

/// Parses Rust source. `None` only if parsing was cancelled.
pub fn parse(src: &[u8]) -> Option<Tree> {
    PARSER.with(|p| p.borrow_mut().parse(src, None))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn dump() {
        let src = r#"use crate::store::{self, Store};
use std::path::Path;

/// A thing.
#[derive(Debug, Clone)]
pub struct Item<T> { pub name: String, id: T }

pub enum Kind { A, B(u32), C { x: i32 } }

pub trait Load { fn load(&self) -> u32; const N: usize = 1; }

impl<T> Item<T> {
    pub fn new(name: &str) -> Self { Self { name: name.into(), id: todo!() } }
}

impl Load for Item<u8> { fn load(&self) -> u32 { Store::open().conn(); helper(1); 0 } }

pub const MAX: usize = 3;
static NAME: &str = "x";
pub type R<T> = Result<T, Error>;
macro_rules! m { () => {} }
mod inner { pub fn f() {} }

#[derive(Subcommand)]
enum Command { Scan { project: String }, Serve }

fn main() { let k = Kind::A; println!("{}", MAX); x.items.len(); }

#[cfg(test)]
mod tests { #[test] fn t() { super::main(); } }
"#;
        let t = super::parse(src.as_bytes()).unwrap();
        println!("{}", t.root_node().to_sexp());
    }
}
