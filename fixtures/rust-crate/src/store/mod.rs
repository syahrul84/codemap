use crate::{Item, Load, MAX_ITEMS};

/// Keeps items in memory.
#[derive(Default)]
pub struct Store {
    items: Vec<Item>,
}

impl Store {
    pub fn open() -> Self { Self::default() }
    pub fn add(&mut self, item: Item) -> bool {
        if self.items.len() >= MAX_ITEMS { return false; }
        self.items.push(item);
        true
    }
}

impl Load for Store {
    fn load(&self) -> Vec<Item> { self.items.clone() }
}
