//! A small library.
pub mod store;

pub use store::Store;

/// Items have names.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: u32,
    pub name: String,
}

pub trait Load {
    fn load(&self) -> Vec<Item>;
}

pub const MAX_ITEMS: usize = 100;

pub fn label(item: &Item) -> String {
    format!("{}: {}", item.id, item.name)
}

pub fn unused_helper() -> u32 { 1 }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn labels() { assert_eq!(label(&Item { id: 1, name: "a".into() }), "1: a"); }
}
