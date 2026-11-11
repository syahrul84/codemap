//! The check that runs after an AI edits a file: names the edit brought in
//! that do not exist in the project.
//!
//! `verify` answers when it is asked. This asks for the assistant, right
//! after each edit, and only about what the edit added: the references of
//! the file as it is now, minus those it already had. What was wrong before
//! the edit is not the edit's news.

use std::collections::BTreeSet;

use codemap_core::store::Store;
use serde_json::{Value, json};

use crate::tools::Project;
use crate::verify::{Claims, VerifyParams};

/// What the edit added, by list.
fn added(new: Vec<String>, old: &[String]) -> Vec<String> {
    new.into_iter().filter(|x| !old.contains(x)).collect()
}

/// `kind `item` does not exist (closest: …)` for each not-found result.
fn not_found(v: &Value, out: &mut Vec<String>) {
    for r in v["results"].as_array().into_iter().flatten() {
        if r["status"] != "not_found" {
            continue;
        }
        let mut line = format!(
            "{} `{}` does not exist",
            r["kind"].as_str().unwrap_or("name"),
            r["item"].as_str().unwrap_or_default()
        );
        // The closest name of the same kind: for a class, not a method
        // (`Order::total`) or another language's path (`crate::orders`).
        let class = r["kind"] == "class";
        if let Some(s) = r["suggestions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|s| !class || !s.contains("::"))
        {
            line.push_str(&format!(" (closest: `{s}`)"));
        }
        if !out.contains(&line) {
            out.push(line);
        }
    }
}

fn verify(store: &Store, project: &Project, claims: Claims, out: &mut Vec<String>) {
    if claims.is_empty() {
        return;
    }
    let params = VerifyParams {
        claims: Some(claims),
        ..VerifyParams::default()
    };
    if let Ok(v) = crate::verify::verify(store, project, params) {
        not_found(&v, out);
    }
}

fn php(store: &Store, project: &Project, file: &str, old: &str, new: &str, out: &mut Vec<String>) {
    let (was, now) = (
        crate::verify::snippet_claims(old),
        crate::verify::snippet_claims(new),
    );
    // Calls a class makes on itself (`$this->total()`, `self::make()`): the
    // method must be one it has.
    let parsed = codemap_php::extract(new.as_bytes());
    let own: Vec<&str> = parsed
        .symbols
        .iter()
        // A trait or an abstract class calls methods its users provide.
        .filter(|s| {
            s.kind.is_class_like()
                && s.kind != codemap_core::symbols::SymbolKind::Trait
                && !s.is_abstract
        })
        .map(|s| s.fqn.as_str())
        .collect();
    let calls = |refs: &[codemap_php::refs::Ref], src: &str| -> BTreeSet<String> {
        let rebound = rebound_lines(src);
        refs.iter()
            .filter(|r| r.kind == "call")
            .filter(|r| {
                r.target.split_once("::").is_some_and(|(class, method)| {
                    own.contains(&class)
                        // `$this->$handler()` and the like name no method.
                        && !method.is_empty()
                        && method.chars().all(|c| c.is_alphanumeric() || c == '_')
                })
            })
            // In an anonymous class, or a closure that may be bound to
            // another object, `$this` is not this class.
            .filter(|r| !rebound.iter().any(|(a, b)| *a <= r.line && r.line <= *b))
            .map(|r| r.target.clone())
            .collect()
    };
    let before = calls(&codemap_php::extract(old.as_bytes()).refs, old);
    let mut methods = added(now.methods, &was.methods);
    for call in calls(&parsed.refs, new).difference(&before) {
        if !methods.contains(call) {
            methods.push(call.clone());
        }
    }
    // Calls on a value whose class the code declares: a typed parameter the
    // function never assigns, a typed property reached through `$this`.
    let typed_before = crate::verify::typed_receiver_calls(old);
    for call in crate::verify::typed_receiver_calls(new).difference(&typed_before) {
        // `>Class::method>called`: a value a method returned, whose class
        // is what the index says that method returns.
        let call = match call.strip_prefix('>').and_then(|c| c.split_once('>')) {
            Some((from, called)) => match from
                .split_once("::")
                .and_then(|(class, method)| returned_class(store, class, method))
            {
                Some(class) => format!("{class}::{called}"),
                None => continue,
            },
            None if call.starts_with('>') => continue,
            None => call.clone(),
        };
        if !methods.contains(&call) {
            methods.push(call);
        }
    }
    let new_tables = added(now.tables, &was.tables);
    // Functions nothing defines (the project, a package, PHP).
    let new_functions = added(now.functions, &was.functions);
    let claims = Claims {
        classes: added(now.classes, &was.classes),
        methods,
        routes: added(now.routes, &was.routes),
        tables: new_tables.clone(),
        // `DB::table('orders')->where('totl', …)`
        columns: added(now.columns, &was.columns),
        model_columns: now
            .model_columns
            .into_iter()
            .filter(|x| !was.model_columns.contains(x))
            .collect(),
        qualified_columns: added(now.qualified_columns, &was.qualified_columns),
        ..Claims::default()
    };
    let mut found = Vec::new();
    verify(store, project, claims, &mut found);
    // A column the query names itself (`count(*) as total`) is not a table's
    // column, and after a join an unqualified column may be the other table's.
    let lower = new.to_lowercase();
    let migrations = std::cell::OnceCell::new();
    // A table the code asks about first (`Schema::hasTable('legacy')`: it is
    // ready for it to be missing) or makes itself (a test's stand-in
    // table), by the schema builder or in SQL of its own or of a migration
    // (`CREATE TABLE log_default PARTITION OF log`).
    let made = |t: &str| -> bool {
        !t.is_empty()
            && (["hastable", "dropifexists", "schema::create"]
                .iter()
                .any(|call| {
                    lower.contains(&format!("{call}('{t}'"))
                        || lower.contains(&format!("{call}(\"{t}\""))
                })
                || sql_makes(&lower, t)
                || sql_makes(migrations.get_or_init(|| migration_text(project)), t))
    };
    found.retain(|f| {
        if let Some(table) = f.strip_prefix("table `").and_then(|r| r.split('`').next()) {
            return !made(&table.to_lowercase());
        }
        let Some(item) = f.strip_prefix("column `").and_then(|r| r.split('`').next()) else {
            return true;
        };
        let (owner, column) = item.rsplit_once('.').unwrap_or(("", item));
        let col = column.to_lowercase();
        if made(&owner.to_lowercase()) {
            return false;
        }
        // A name the query gives itself: `count(*) as total`, `count(*) n`
        // (in SQL, not `use X as Total;`).
        let named_here = [format!(" as {col}"), format!(") {col}")]
            .iter()
            .any(|alias| {
                lower.match_indices(alias.as_str()).any(|(i, _)| {
                    let line_start = lower[..i].rfind('\n').map_or(0, |p| p + 1);
                    !lower[line_start..].trim_start().starts_with("use ")
                        && !lower[i + alias.len()..]
                            .starts_with(|c: char| c.is_alphanumeric() || c == '_')
                })
            });
        // `->addSelect(['is_read' => …])`, `->selectSub($q, 'is_read')`.
        let selected_as = (lower.contains("addselect(") || lower.contains("selectsub("))
            && (lower.contains(&format!("'{col}'")) || lower.contains(&format!("\"{col}\"")));
        // After a join an unqualified column may be the other table's: it is
        // a finding only when none of the joined tables has it either.
        let elsewhere = joined_has(store, &lower, &col) != Some(false);
        // The file adds the column itself (a test that rebuilds an old
        // schema): `$table->unsignedBigInteger('category_id')`.
        let defined_here = lower.contains("schema::")
            && [format!("('{col}'"), format!("(\"{col}\"")]
                .iter()
                .any(|call| {
                    lower.match_indices(call.as_str()).any(|(i, _)| {
                        let method: String = lower[..i]
                            .chars()
                            .rev()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        lower[..i - method.len()].ends_with("->")
                            && !method.is_empty()
                            && !method.contains("where")
                            && !QUERY_METHODS.contains(&method.as_str())
                    })
                });
        // A name `withCount('users')` and its kin give (`users_count`,
        // `orders_sum_total`), and a column the code asks about first
        // (`Schema::hasColumn('subscriptions', 'plan')`).
        let counted = ["count", "sum", "avg", "min", "max", "exists"]
            .iter()
            .any(|f| {
                lower.contains(&format!("with{f}("))
                    && (col.ends_with(&format!("_{f}")) || col.contains(&format!("_{f}_")))
            });
        let asked = lower.match_indices("hascolumn(").any(|(i, _)| {
            lower[i..].split(')').next().is_some_and(|call| {
                call.contains(&format!("'{col}'")) || call.contains(&format!("\"{col}\""))
            })
        });
        if counted || asked {
            return false;
        }
        // The model maps the name itself (`'isdeposit' => 'arrec_isdeposit'`).
        let mapped =
            owner.starts_with(char::is_uppercase) && model_maps(store, project, owner, &col);
        !(named_here || selected_as || elsewhere || defined_here || mapped)
    });
    drop_magic_methods(store, project, &mut found);
    unknown_functions(store, project, &lower, &new_functions, &mut found);
    drop_unsure_routes(store, &lower, &mut found);
    drop_history(project, file, &lower, &mut found);
    misspelt_tables(store, project, file, &lower, &new_tables, &mut found);
    out.extend(found);
}

/// The one class a project method says it returns (`: Gateway`, `: ?Gateway`,
/// `: static`); nothing for a union, a built-in type or no declaration.
fn returned_class(store: &Store, class: &str, method: &str) -> Option<String> {
    use codemap_core::symbols::{self, MemberLookup};
    let MemberLookup::Found(m) = symbols::find_member(store.conn(), class, method).ok()? else {
        return None;
    };
    let declared = m.meta.return_type.as_deref()?.trim_start_matches('?');
    if declared.contains(['|', '&']) {
        return None;
    }
    match declared.to_lowercase().as_str() {
        "static" | "$this" => Some(class.to_owned()),
        "self" => m.parent_fqn.clone().or_else(|| Some(class.to_owned())),
        _ => (m.meta.return_fqns.len() == 1).then(|| m.meta.return_fqns[0].clone()),
    }
}

/// The other migrations of the folder `file` is in, in lower case.
fn sibling_migrations(project: &Project, file: &str) -> String {
    let path = project.root.join(file);
    let mut others = String::new();
    let Some(dir) = path.parent() else {
        return others;
    };
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p != path
            && p.extension().is_some_and(|e| e == "php")
            && let Ok(text) = std::fs::read_to_string(&p)
        {
            others.push_str(&text.to_lowercase());
            others.push('\n');
        }
    }
    others
}

/// `Schema::table('ordrs', …)` in a migration. The schema keeps a table no
/// migration created as one made outside the migrations, so by itself it
/// "exists". It is a slip when its name is a letter or two from a table the
/// project does have and no other migration names it.
fn misspelt_tables(
    store: &Store,
    project: &Project,
    file: &str,
    lower: &str,
    tables: &[String],
    found: &mut Vec<String>,
) {
    if tables.is_empty() || !lower.contains("extends migration") {
        return;
    }
    let conn = store.conn();
    let unknown = |t: &str| -> bool {
        conn.query_row(
            "SELECT confidence = 'low' AND created_in IS NULL FROM db_tables WHERE name = ?1",
            [t],
            |r| r.get(0),
        )
        .unwrap_or(false)
    };
    let slips: Vec<&String> = tables.iter().filter(|t| unknown(t)).collect();
    if slips.is_empty() {
        return;
    }
    let known: Vec<String> = conn
        .prepare("SELECT name FROM db_tables WHERE confidence != 'low' AND dropped = 0")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default();
    let others = sibling_migrations(project, file);
    // Tables the project has that no migration creates, besides these.
    let outside: bool = conn
        .prepare("SELECT name FROM db_tables WHERE confidence = 'low' AND dropped = 0")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().any(|n| !slips.contains(&&n)))
        })
        .unwrap_or(true);
    for t in slips {
        let name = t.to_lowercase();
        if others.contains(&format!("'{name}'")) || others.contains(&format!("\"{name}\"")) {
            continue;
        }
        let near = known
            .iter()
            .map(|k| (strsim::levenshtein(&k.to_lowercase(), &name), k))
            .filter(|(d, _)| *d > 0 && *d <= if name.len() > 5 { 2 } else { 1 })
            .min_by_key(|(d, _)| *d);
        let line = match near {
            Some((_, k)) => format!("table `{t}` does not exist (closest: `{k}`)"),
            // Every other table of the project is made by a migration: one
            // that is not, is not there. (With tables made outside the
            // migrations, this may be another of those.)
            None if !outside && !known.is_empty() => format!("table `{t}` does not exist"),
            None => continue,
        };
        if !found.contains(&line) {
            found.push(line);
        }
    }
}

/// Query methods that take a column name; any other `->method('name')` in a
/// file that changes the schema is taken as a column being added.
const QUERY_METHODS: &[&str] = &[
    "update",
    "pluck",
    "value",
    "select",
    "addselect",
    "orderby",
    "orderbydesc",
    "groupby",
    "increment",
    "decrement",
    "sum",
    "avg",
    "min",
    "max",
    "latest",
    "oldest",
    "get",
    "insert",
    "insertgetid",
    "table",
    "having",
    "count",
    "exists",
    "with",
    "first",
    "find",
];

/// Whether the model's own file has `name` as a key (`'isdeposit' =>
/// 'arrec_isdeposit'`): a map of short names to columns, a cast.
fn model_maps(store: &Store, project: &Project, model: &str, name: &str) -> bool {
    codemap_core::symbols::all_named(store.conn(), model)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.kind.is_class_like())
        .filter_map(|s| std::fs::read_to_string(project.root.join(&s.file)).ok())
        .any(|text| {
            let lower = text.to_lowercase();
            [format!("'{name}'"), format!("\"{name}\"")]
                .iter()
                .any(|key| {
                    lower
                        .match_indices(key.as_str())
                        .any(|(i, _)| lower[i + key.len()..].trim_start().starts_with("=>"))
                })
        })
}

/// Whether SQL in `lower` makes table `t` or names it as one it works on
/// (`create table t`, `if not exists t`, `rename to t`).
fn sql_makes(lower: &str, t: &str) -> bool {
    lower.match_indices(t).any(|(i, _)| {
        let before = lower[..i].trim_end_matches(['`', '"']);
        !lower[i + t.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
            && ["table ", "exists ", " to "]
                .iter()
                .any(|k| before.ends_with(k))
    })
}

/// The migrations of the project, in lower case, for [`sql_makes`].
fn migration_text(project: &Project) -> String {
    let mut out = String::new();
    for entry in std::fs::read_dir(project.root.join("database/migrations"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "php")
            && let Ok(text) = std::fs::read_to_string(&p)
        {
            out.push_str(&text.to_lowercase());
            out.push('\n');
        }
    }
    out
}

/// Route findings that may be wrong are left out: a name the code asks about
/// first (`Route::has('login')`), and a name under a first word no route of
/// the project has (`filament.…`, `telescope`: a package adds those when the
/// app runs, where they cannot be read from the code).
fn drop_unsure_routes(store: &Store, lower: &str, found: &mut Vec<String>) {
    if !found.iter().any(|f| f.starts_with("route `")) {
        return;
    }
    let firsts: BTreeSet<String> = codemap_laravel::routes::all(store.conn())
        .map(|all| {
            all.iter()
                .filter_map(|r| r.name.as_deref())
                .filter_map(|n| n.split('.').next())
                .map(str::to_lowercase)
                .collect()
        })
        .unwrap_or_default();
    found.retain(|f| {
        let Some(name) = f.strip_prefix("route `").and_then(|r| r.split('`').next()) else {
            return true;
        };
        let n = name.to_lowercase();
        if lower.contains(&format!("route::has('{n}'"))
            || lower.contains(&format!("route::has(\"{n}\""))
        {
            return false;
        }
        match n.split_once('.') {
            // A known first word, or one a letter or two away from it
            // (`order.index` beside `orders.index`).
            Some((first, _)) => firsts.iter().any(|k| {
                k == first || strsim::levenshtein(k, first) <= if first.len() > 5 { 2 } else { 1 }
            }),
            None => true,
        }
    });
}

/// A migration works on the database as it was when it was written: a table
/// or column that a later migration renamed or dropped is not a mistake in
/// it. Such a name is one another migration of the same folder also names;
/// a name no other migration has is still reported. The same holds for a
/// test, which may roll a migration back and work on the old columns.
fn drop_history(project: &Project, file: &str, lower: &str, found: &mut Vec<String>) {
    let schema = |f: &str| f.starts_with("table `") || f.starts_with("column `");
    let migration = lower.contains("extends migration");
    let test = codemap_core::scan::is_test_path(file);
    if !(migration || test) || !found.iter().any(|f| schema(f)) {
        return;
    }
    let others = if migration {
        sibling_migrations(project, file)
    } else {
        migration_text(project)
    };
    found.retain(|f| {
        if !schema(f) {
            return true;
        }
        let item = f.split('`').nth(1).unwrap_or_default().to_lowercase();
        let name = item.rsplit('.').next().unwrap_or(&item);
        !(others.contains(&format!("'{name}'")) || others.contains(&format!("\"{name}\"")))
    });
}

/// What binds a closure it is given to another object (`macro`, Pest's
/// `it`, `Artisan::command`), or may (`bind`, `call`).
const REBINDERS: &[&str] = &[
    "macro",
    "mixin",
    "bind",
    "bindto",
    "call",
    "fromcallable",
    "command",
    "extend",
    "test",
    "it",
    "describe",
    "beforeeach",
    "aftereach",
    "beforeall",
    "afterall",
    "dataset",
    "uses",
];

/// The lines where `$this` may not be the class's own object, first and
/// last: an anonymous class, and a closure unless it is handed straight to
/// a call that only runs it (`->each(function () { … })`,
/// `DB::transaction(fn () => …)`). A closure kept in a variable, returned or
/// put in an array may be bound to something else later.
fn rebound_lines(src: &str) -> Vec<(u32, u32)> {
    let Some(tree) = codemap_php::parse::parse(src.as_bytes()) else {
        return Vec::new();
    };
    let text = |n: tree_sitter::Node<'_>| n.utf8_text(src.as_bytes()).unwrap_or_default();
    let only_run = |closure: tree_sitter::Node<'_>| -> bool {
        let call = closure
            .parent()
            .filter(|a| a.kind() == "argument")
            .and_then(|a| a.parent())
            .filter(|a| a.kind() == "arguments")
            .and_then(|a| a.parent());
        let name = call.and_then(|c| match c.kind() {
            "member_call_expression"
            | "nullsafe_member_call_expression"
            | "scoped_call_expression" => c.child_by_field_name("name"),
            "function_call_expression" => c.child_by_field_name("function"),
            _ => None,
        });
        name.is_some_and(|n| {
            let name = text(n)
                .rsplit('\\')
                .next()
                .unwrap_or_default()
                .to_lowercase();
            !name.is_empty() && !name.starts_with('$') && !REBINDERS.contains(&name.as_str())
        })
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        let closure = n.kind().contains("anonymous_function") || n.kind() == "arrow_function";
        if (closure && !only_run(n)) || (!closure && n.kind().contains("anonymous")) {
            out.push((
                n.start_position().row as u32 + 1,
                n.end_position().row as u32 + 1,
            ));
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    out
}

/// After `->join('orders', …)`: whether a joined table has `column`.
/// `None` when what is joined cannot be read (a sub-query, a table the
/// index does not know).
fn joined_has(store: &Store, lower: &str, column: &str) -> Option<bool> {
    let mut any = false;
    for name in [
        "join(",
        "leftjoin(",
        "rightjoin(",
        "crossjoin(",
        "joinsub(",
        "leftjoinsub(",
        "rightjoinsub(",
        "joinlateral(",
        "leftjoinlateral(",
    ] {
        for (i, _) in lower.match_indices(name) {
            // `->join(` on a query, `Order::join(` on a model.
            if !(lower[..i].ends_with("->") || lower[..i].ends_with("::")) {
                continue;
            }
            if name.ends_with("sub(") || name.ends_with("lateral(") {
                return None;
            }
            let rest = &lower[i + name.len()..];
            let Some(quote) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
                // `->leftJoin($table, …)`, `->join(DB::raw(…), …)`: a table
                // that cannot be read. (`$list->join($glue)` lands here too.)
                return None;
            };
            let named = rest[1..].split(quote).next().unwrap_or_default();
            let table = named.split(" as ").next().unwrap_or(named).trim();
            // `collect($x)->join(', ')` is not a table.
            if !table.starts_with(|c: char| c.is_ascii_alphabetic())
                || !table
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            {
                continue;
            }
            let columns =
                codemap_laravel::schema::load_columns(store.conn(), table).unwrap_or_default();
            if columns.is_empty() {
                return None;
            }
            any |= columns.iter().any(|c| c.name.eq_ignore_ascii_case(column));
        }
    }
    Some(any)
}

/// A method a class answers without declaring it: through `__call` /
/// `__callStatic` of its own, or one its docblock promises (`@method`,
/// `@mixin`). Such findings are dropped.
fn drop_magic_methods(store: &Store, project: &Project, found: &mut Vec<String>) {
    use codemap_core::symbols::{self, MemberLookup};
    let conn = store.conn();
    found.retain(|f| {
        let Some((class, method)) = f
            .strip_prefix("method `")
            .and_then(|r| r.split('`').next())
            .and_then(|item| item.split_once("::"))
        else {
            return true;
        };
        if ["__call", "__callStatic"].iter().any(|m| {
            matches!(
                symbols::find_member(conn, class, m),
                Ok(MemberLookup::Found(_))
            )
        }) {
            return false;
        }
        // An interface or an abstract class: the value is always something
        // more, which may well have the method (a repository behind its
        // interface).
        if symbols::by_fqn(conn, class).is_ok_and(|all| {
            all.iter().any(|s| {
                s.kind == symbols::SymbolKind::Interface
                    || (s.kind.is_class_like() && s.is_abstract)
            })
        }) {
            return false;
        }
        let promised = symbols::by_fqn(conn, class)
            .ok()
            .and_then(|s| s.into_iter().find(|s| s.kind.is_class_like()))
            .and_then(|s| std::fs::read_to_string(project.root.join(&s.file)).ok())
            .is_some_and(|text| {
                text.contains("@mixin")
                    || text
                        .lines()
                        .any(|l| l.contains("@method") && l.contains(&format!(" {method}(")))
            });
        !promised
    });
}

/// A function the edit calls that nothing defines: not the project, not a
/// file a package loads at start, not PHP itself (`setting()`, `reponse()`).
/// Needs `vendor/`, where the packages' helpers can be read; without it only
/// PHP's own are known, so nothing is said. In a project with folders the
/// scan leaves out, where a helper may live unseen, only a name a letter or
/// two from a known one is reported.
fn unknown_functions(
    store: &Store,
    project: &Project,
    lower: &str,
    functions: &[String],
    found: &mut Vec<String>,
) {
    let composer = project.root.join("vendor/composer");
    if functions.is_empty() || !composer.is_dir() {
        return;
    }
    let mut known: Vec<String> = store
        .conn()
        .prepare("SELECT DISTINCT name FROM symbols WHERE kind = 'function'")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default();
    known.extend(
        crate::verify::LARAVEL_HELPERS
            .iter()
            .map(|h| (*h).to_owned()),
    );
    let unseen = !codemap_core::scan::unscanned_folders(&project.root, &project.config).is_empty();
    let mut vendor: Option<String> = None;
    for f in functions {
        let name = f.rsplit('\\').next().unwrap_or(f);
        let l = name.to_lowercase();
        // Declared in this very file: a function inside a method, which
        // exists once the method has run.
        if l.is_empty()
            || lower.contains(&format!("function {l}("))
            || known.iter().any(|k| k.eq_ignore_ascii_case(name))
            || lower.contains(&format!("function_exists('{l}'"))
            || lower.contains(&format!("function_exists(\"{l}\""))
        {
            continue;
        }
        // A package's helper of that name: the files composer loads at
        // start, and the files beside them (a polyfill's `bootstrap80.php`).
        let helpers = vendor.get_or_insert_with(|| vendor_functions(project, &composer));
        if helpers.contains(&format!(" {l}(")) {
            continue;
        }
        let allowed = match l.len() {
            0..=4 => 0,
            5..=7 => 1,
            _ => 2,
        };
        let near = known
            .iter()
            .map(|k| (strsim::levenshtein(&k.to_lowercase(), &l), k))
            .filter(|(d, _)| *d > 0 && *d <= allowed)
            .min_by_key(|(d, _)| *d)
            .map(|(_, k)| k);
        let line = match near {
            Some(near) => format!("function `{name}` does not exist (closest: `{near}`)"),
            None if unseen => continue,
            None => format!("function `{name}` does not exist"),
        };
        if !found.contains(&line) {
            found.push(line);
        }
    }
}

/// Every `function name(` in the files composer loads at start and the PHP
/// files beside them, in lower case, as ` name(` one after another.
fn vendor_functions(project: &Project, composer: &std::path::Path) -> String {
    let list = std::fs::read_to_string(composer.join("autoload_files.php")).unwrap_or_default();
    let mut dirs: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    for line in list.lines() {
        let Some((_, path)) = line.trim().trim_end_matches(',').rsplit_once(" . '") else {
            continue;
        };
        let base = if line.contains("$vendorDir") {
            "vendor"
        } else {
            ""
        };
        let file = project
            .root
            .join(format!("{base}{}", path.trim_end_matches('\'')).trim_start_matches('/'));
        if let Some(dir) = file.parent() {
            dirs.insert(dir.to_path_buf());
        }
    }
    let mut out = String::new();
    for dir in dirs {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.extension().is_none_or(|e| e != "php") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            for line in text.lines() {
                let line = line.trim_start();
                if let Some(rest) = line.strip_prefix("function ")
                    && let Some(name) = rest.split('(').next()
                {
                    out.push(' ');
                    out.push_str(&name.trim().trim_start_matches('&').to_lowercase());
                    out.push('(');
                }
            }
        }
    }
    out
}

fn blade(store: &Store, project: &Project, old: &str, new: &str, out: &mut Vec<String>) {
    use codemap_php::blade::facts;
    let names = |src: &str| -> BTreeSet<String> {
        let f = facts(src);
        f.extends
            .into_iter()
            .chain(f.includes)
            .map(|n| n.name)
            // A package's view, or a name built at run time: not ours to check.
            .filter(|n| !n.contains("::") && !n.ends_with('*'))
            .collect()
    };
    let was = names(old);
    let conn = store.conn();
    for name in names(new).difference(&was) {
        // `@includeIf` and `@includeFirst` are for views that may be missing.
        if !must_exist(new, name) {
            continue;
        }
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM views WHERE framework = 'blade' AND name = ?1 COLLATE NOCASE)",
                [name],
                |r| r.get(0),
            )
            .unwrap_or(true);
        if exists {
            continue;
        }
        let mut line = format!("view `{name}` does not exist");
        if let Some(s) = codemap_laravel::blade::suggestions(conn, name)
            .ok()
            .and_then(|s| s.into_iter().next())
        {
            line.push_str(&format!(" (closest: `{s}`)"));
        }
        out.push(line);
    }
    let routes = |src: &str| -> Vec<String> {
        codemap_php::refs::blade_refs(src)
            .into_iter()
            .filter(|r| r.kind == "route")
            .map(|r| r.target)
            .collect()
    };
    let claims = Claims {
        routes: added(routes(new), &routes(old)),
        ..Claims::default()
    };
    let mut found = Vec::new();
    verify(store, project, claims, &mut found);
    drop_unsure_routes(store, &new.to_lowercase(), &mut found);
    out.extend(found);
}

/// Whether the template names `view` in a directive that fails when the
/// view is missing (`@include`, `@extends`, `@each`, …), not only in
/// `@includeIf` / `@includeFirst`.
fn must_exist(src: &str, view: &str) -> bool {
    let slashed = view.replace('.', "/");
    let mut strict = false;
    for name in [view, slashed.as_str()] {
        for quote in ['\'', '"'] {
            let needle = format!("{quote}{name}{quote}");
            let mut from = 0;
            while let Some(at) = src
                .get(from..)
                .and_then(|s| s.find(&needle))
                .map(|i| i + from)
            {
                from = at + needle.len();
                // The directive this name belongs to: the last `@word(` before it.
                let directive = src[..at]
                    .rfind('@')
                    .map(|i| {
                        src[i + 1..at]
                            .chars()
                            .take_while(|c| c.is_ascii_alphabetic())
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                if !matches!(directive.as_str(), "includeIf" | "includeFirst") {
                    strict = true;
                }
            }
        }
    }
    strict
}

/// Line numbers of `new` whose text `old` did not have.
fn changed_lines(old: &str, new: &str) -> BTreeSet<u32> {
    let had: BTreeSet<&str> = old.lines().map(str::trim).collect();
    new.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !had.contains(l.trim()))
        .map(|(i, _)| i as u32 + 1)
        .collect()
}

fn line_of(at: &str) -> Option<u32> {
    at.rsplit(':').next()?.trim().parse().ok()
}

fn frontend(
    store: &Store,
    project: &Project,
    file: &str,
    old: &str,
    new: &str,
    out: &mut Vec<String>,
) {
    let lines = changed_lines(old, new);
    if lines.is_empty() {
        return;
    }
    // What the file called and translated before the edit, read the same way.
    let was = codemap_vue::extract::extract_file(file, old);
    let old_urls: BTreeSet<&str> = was
        .script
        .api_calls
        .iter()
        .map(|c| c.url.as_str())
        .collect();
    let old_keys: BTreeSet<&str> = was
        .script
        .i18n_uses
        .iter()
        .chain(&was.template_i18n)
        .map(|u| u.key.as_str())
        .collect();
    // Calls to the backend that no route serves.
    let params = serde_json::from_value(json!({ "file": file, "limit": 100 }));
    if let Ok(p) = params
        && let Ok(answer) = crate::api::api_usage(store, project, p)
        && answer.data["backends"]
            .as_array()
            .is_some_and(|b| !b.is_empty())
    {
        for c in answer.data["calls"].as_array().into_iter().flatten() {
            let at = c["at"].as_str().unwrap_or_default();
            // `at` is `file:line`; the file may only be part of another's path.
            if !at.starts_with(&format!("{file}:"))
                || !line_of(at).is_some_and(|l| lines.contains(&l))
            {
                continue;
            }
            let call = c["call"].as_str().unwrap_or_default();
            // A call the file already made (only its options changed on this
            // line) is not the edit's.
            let path = call.split_once(' ').map_or(call, |(_, p)| p);
            if old_urls.contains(path) {
                continue;
            }
            match c["status"].as_str().unwrap_or_default() {
                "missing" => out.push(format!(
                    "endpoint `{call}` (line {}) matches no backend route",
                    line_of(at).unwrap_or(0)
                )),
                "method_mismatch" => {
                    let as_route: Vec<&str> = c["path_exists_as"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .take(2)
                        .collect();
                    out.push(format!(
                        "endpoint `{call}` (line {}): the route exists with another method{}",
                        line_of(at).unwrap_or(0),
                        if as_route.is_empty() {
                            String::new()
                        } else {
                            format!(" (`{}`)", as_route.join("`, `"))
                        }
                    ));
                }
                _ => {}
            }
        }
    }
    // Translation keys no locale defines. Not in a test, which makes up
    // its own keys.
    if !codemap_core::scan::is_test_path(file)
        && let Ok(keys) = crate::i18n::file_keys(store.conn(), &project.config.i18n.locales, file)
    {
        for k in keys["undefined"].as_array().into_iter().flatten() {
            let Some((key, line)) = k.as_str().and_then(|s| s.rsplit_once(" :")) else {
                continue;
            };
            if line.parse().is_ok_and(|l: u32| lines.contains(&l))
                && !key.ends_with('…')
                && !old_keys.contains(key)
            {
                out.push(format!(
                    "translation key `{key}` (line {line}) is not defined in any locale"
                ));
            }
        }
    }
}

/// What an edit of `file` (a path from the project root) brought in that
/// does not exist: classes, methods, functions, routes, tables and columns
/// in PHP; views and route names in Blade; endpoints and translation keys in
/// Vue / JS / TS. `old` is the file before the edit, `new` after; the index
/// must already hold `new`. Never fails: a check that cannot run says nothing.
pub fn check_edit(
    store: &Store,
    project: &Project,
    file: &str,
    old: &str,
    new: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    if old == new {
        return out;
    }
    let lower = file.to_lowercase();
    if lower.ends_with(".blade.php") {
        blade(store, project, old, new, &mut out);
    } else if lower.ends_with(".php") {
        php(store, project, file, old, new, &mut out);
    } else if [
        ".vue", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".mts", ".cjs", ".cts",
    ]
    .iter()
    .any(|e| lower.ends_with(e))
    {
        frontend(store, project, file, old, new, &mut out);
    } else {
        match crate::edits_native::handles(std::path::Path::new(file)) {
            Some("rust") => crate::edits_native::rust(store, project, file, old, new, &mut out),
            Some("kotlin") => crate::edits_native::kotlin(store, project, old, new, &mut out),
            Some("swift") => crate::edits_native::swift(store, project, old, new, &mut out),
            _ => {}
        }
    }
    out.truncate(40);
    out
}

/// The file as it was before an Edit: `new_string` put back to `old_string`
/// (every occurrence when the edit replaced all). `None` when the new text
/// is not in the file, so the old one cannot be known.
pub fn before_edit(now: &str, old_string: &str, new_string: &str, all: bool) -> Option<String> {
    if new_string.is_empty() {
        // A deletion adds nothing.
        return Some(now.to_owned());
    }
    if !now.contains(new_string) {
        return None;
    }
    Some(if all {
        now.replace(new_string, old_string)
    } else {
        now.replacen(new_string, old_string, 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_before_an_edit() {
        assert_eq!(
            before_edit("a\nNEW\nb\nNEW\n", "old", "NEW", false).as_deref(),
            Some("a\nold\nb\nNEW\n")
        );
        assert_eq!(
            before_edit("a\nNEW\nb\nNEW\n", "old", "NEW", true).as_deref(),
            Some("a\nold\nb\nold\n")
        );
        assert_eq!(before_edit("abc", "x", "missing", false), None);
        assert_eq!(
            before_edit("abc", "gone", "", false).as_deref(),
            Some("abc")
        );
        assert_eq!(changed_lines("a\nb\n", "a\nc\n\nb\n"), BTreeSet::from([2]));
        let src = "@includeIf('promo.banner')\n@includeFirst(['a.custom', 'a.default'])\n@include('a.row')\n@include(\"b/row\")";
        assert!(!must_exist(src, "promo.banner"));
        assert!(!must_exist(src, "a.custom") && !must_exist(src, "a.default"));
        assert!(must_exist(src, "a.row") && must_exist(src, "b.row"));
        // SQL of the file's own that makes a table, or renames one to it.
        assert!(sql_makes("alter table log rename to log_old", "log_old"));
        assert!(sql_makes(
            "create table if not exists `log_default` partition of log",
            "log_default"
        ));
        assert!(!sql_makes("select * from orders", "orders"));
        assert!(!sql_makes("create table orders_archive (id int)", "orders"));
    }
}
