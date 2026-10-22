use demo_tool::{Load, Store, label};

#[derive(clap::Subcommand)]
enum Command {
    /// List the items.
    List,
    /// Add an item.
    Add { name: String },
}

fn main() {
    let store = Store::open();
    for item in store.load() { println!("{}", label(&item)); }
}

#[cfg(test)]
mod tests {
    #[test]
    fn opens() { let _ = demo_tool::Store::open(); }
}

fn shout(item: &demo_tool::Item) -> String {
    // Neither is `demo_tool::label`: another crate's function, and a method.
    format!("{} {}", fancy::label(item), item.label())
}
