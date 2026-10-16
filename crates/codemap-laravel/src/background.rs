//! Background work: what runs on a schedule, queued jobs and who dispatches
//! them, events with their listeners and who fires them, notifications and
//! mails and who sends them. Answers "who sends this email / when does this
//! run" without reading the providers, console routes and jobs by hand.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use codemap_core::envelope::Detail;
use codemap_core::symbols::{self, SymbolRecord};
use codemap_core::{Result, Store};
use serde_json::{Value, json};

fn is_test(f: &str) -> bool {
    f.starts_with("tests/") || f.contains("/tests/") || f.ends_with("Test.php")
}

/// One scheduled entry: what runs, how often, where it is declared.
struct Scheduled {
    what: String,
    when: String,
    at: String,
}

/// `Schedule::command('orders:expire')->everyMinute()…;` and
/// `$schedule->job(new X)->daily();` in routes/console.php, the console
/// Kernel and bootstrap/app.php (`withSchedule`).
fn schedule(root: &Path) -> Vec<Scheduled> {
    let mut out = Vec::new();
    for file in [
        "routes/console.php",
        "app/Console/Kernel.php",
        "bootstrap/app.php",
    ] {
        let Ok(src) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        // Statements end at `;`; keep the line each one starts on.
        let mut start_line = 1;
        let mut line = 1;
        let mut stmt = String::new();
        for c in src.chars() {
            if stmt.trim().is_empty() {
                start_line = line;
            }
            if c == '\n' {
                line += 1;
            }
            if c == ';' {
                if let Some(s) = parse_schedule(&stmt) {
                    // The line of the call itself, not of comments before it.
                    let at = stmt
                        .find("Schedule")
                        .or_else(|| stmt.find("$schedule"))
                        .map_or(0, |i| stmt[..i].matches('\n').count());
                    out.push(Scheduled {
                        what: s.0,
                        when: s.1,
                        at: format!("{file}:{}", start_line + at),
                    });
                }
                stmt.clear();
            } else {
                stmt.push(c);
            }
        }
    }
    out
}

/// `(what, when)` of one schedule statement.
fn parse_schedule(stmt: &str) -> Option<(String, String)> {
    let s: String = stmt
        .lines()
        .map(|l| l.split("//").next().unwrap_or(l).trim())
        .collect::<Vec<_>>()
        .join("");
    let starts = [
        "Schedule::command(",
        "Schedule::job(",
        "Schedule::call(",
        "Schedule::exec(",
        "->command(",
        "->job(",
        "->call(",
        "->exec(",
    ];
    let (pos, kw) = starts
        .iter()
        .filter_map(|k| s.find(k).map(|p| (p, *k)))
        .min_by_key(|(p, _)| *p)?;
    if !kw.starts_with("Schedule")
        && !s[..pos].contains("Schedule")
        && !s[..pos].contains("$schedule")
    {
        return None;
    }
    let args_start = pos + kw.len();
    // The matching `)` of the first call.
    let mut depth = 1;
    let mut end = args_start;
    for (i, c) in s[args_start..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = args_start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let args = &s[args_start..end];
    let what = if kw.contains("command") {
        let a = args.trim().trim_matches(['\'', '"']);
        let name = a.split(['\'', '"']).next().unwrap_or(a);
        format!("command {}", name.split_whitespace().next().unwrap_or(name))
    } else if kw.contains("job") {
        let a = args.trim().trim_start_matches("new ");
        let class = a.split(['(', ':', ',']).next().unwrap_or(a).trim();
        format!("job {class}")
    } else if kw.contains("exec") {
        format!("exec {}", args.trim())
    } else {
        "closure".to_owned()
    };
    let when = s[end + 1..].trim_start_matches("->").replace("->", " → ");
    Some((
        what,
        if when.is_empty() {
            "(no frequency)".into()
        } else {
            when
        },
    ))
}

/// `$signature = 'orders:expire {--dry}'` → `orders:expire`.
fn signature(cls: &SymbolRecord, root: &Path) -> Option<String> {
    let src = std::fs::read_to_string(root.join(&cls.file)).ok()?;
    let at = src.find("$signature")?;
    let rest = &src[at..];
    let q = rest.find(['\'', '"'])?;
    let quote = rest.as_bytes()[q] as char;
    let body = &rest[q + 1..];
    let sig = body.split(quote).next()?;
    sig.split_whitespace().next().map(str::to_owned)
}

/// Where a class is used (new X, X::dispatch(), X::class) outside its own
/// file and tests: `file:line`.
fn triggered_at(conn: &rusqlite::Connection, fqn: &str, own_file: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.path, r.line FROM refs r JOIN files f ON f.id = r.file_id
         WHERE ((r.kind = 'class' AND r.target = ?1 COLLATE NOCASE)
             OR (r.kind = 'call' AND r.target LIKE ?2))
           AND f.path != ?3
         ORDER BY f.path, r.line",
    )?;
    let rows = stmt.query_map(rusqlite::params![fqn, format!("{fqn}::%"), own_file], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (f, l) = row?;
        if !is_test(&f) && !f.starts_with("app/Providers/") {
            out.push(format!("{f}:{l}"));
        }
    }
    Ok(out)
}

fn short(fqn: &str) -> &str {
    fqn.rsplit('\\').next().unwrap_or(fqn)
}

pub fn describe(store: &Store, root: &Path, prefix: Option<&str>, detail: Detail) -> Result<Value> {
    let conn = store.conn();
    let classes: Vec<SymbolRecord> = symbols::all(conn)?
        .into_iter()
        .filter(|s| s.kind.is_class_like() && s.file.ends_with(".php") && !is_test(&s.file))
        .collect();
    let extends = |s: &SymbolRecord, base: &str| s.meta.extends.iter().any(|e| short(e) == base);
    let implements = |s: &SymbolRecord, i: &str| s.meta.implements.iter().any(|e| short(e) == i);
    let keep = |name: &str| prefix.is_none_or(|p| name.to_lowercase().contains(&p.to_lowercase()));
    let limit = if detail == Detail::Full { 500 } else { 40 };
    let uses = |list: &[String]| -> Value {
        if detail == Detail::Full || list.len() <= 3 {
            json!(list)
        } else {
            json!(format!(
                "{} (+{} more)",
                list[..3].join(", "),
                list.len() - 3
            ))
        }
    };

    // Commands and the schedule.
    let mut commands: HashMap<String, &SymbolRecord> = HashMap::new();
    for c in classes
        .iter()
        .filter(|c| c.file.starts_with("app/Console/") || extends(c, "Command"))
    {
        if let Some(sig) = signature(c, root) {
            commands.insert(sig, c);
        }
    }
    let scheduled: Vec<Value> = schedule(root)
        .into_iter()
        .filter(|s| keep(&s.what))
        .take(limit)
        .map(|s| {
            let class = s
                .what
                .strip_prefix("command ")
                .and_then(|sig| commands.get(sig))
                .map(|c| format!(" ({})", c.file));
            json!(format!(
                "{}{} · {} · {}",
                s.what,
                class.unwrap_or_default(),
                s.when,
                s.at
            ))
        })
        .collect();

    // Queued jobs.
    let mut jobs = Vec::new();
    for j in classes
        .iter()
        .filter(|c| {
            c.file.starts_with("app/Jobs/")
                || implements(c, "ShouldQueue")
                    && !extends(c, "Notification")
                    && !extends(c, "Mailable")
        })
        .filter(|c| keep(&c.fqn))
        .take(limit)
    {
        let at = triggered_at(conn, &j.fqn, &j.file)?;
        jobs.push(json!({
            "job": short(&j.fqn),
            "file": j.file,
            "queued": implements(j, "ShouldQueue"),
            "dispatched_at": uses(&at),
        }));
    }

    // Events → listeners (by the type of `handle`'s first parameter).
    let mut listeners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for l in classes
        .iter()
        .filter(|c| c.file.starts_with("app/Listeners/"))
    {
        if let Some(h) = symbols::by_fqn(conn, &format!("{}::handle", l.fqn))?
            .into_iter()
            .next()
            && let Some(p) = h.meta.params.first()
        {
            for t in &p.type_fqns {
                listeners
                    .entry(t.to_lowercase())
                    .or_default()
                    .push(short(&l.fqn).to_owned());
            }
        }
    }
    let mut events = Vec::new();
    for e in classes
        .iter()
        .filter(|c| c.file.starts_with("app/Events/"))
        .filter(|c| keep(&c.fqn))
        .take(limit)
    {
        let at: Vec<String> = triggered_at(conn, &e.fqn, &e.file)?
            .into_iter()
            .filter(|a| !a.starts_with("app/Listeners/"))
            .collect();
        let ls = listeners
            .get(&e.fqn.to_lowercase())
            .cloned()
            .unwrap_or_default();
        events.push(json!({
            "event": short(&e.fqn),
            "file": e.file,
            "listeners": ls,
            "broadcast": implements(e, "ShouldBroadcast") || implements(e, "ShouldBroadcastNow"),
            "fired_at": uses(&at),
        }));
    }

    // Notifications and mails.
    let mut messages = Vec::new();
    for m in classes
        .iter()
        .filter(|c| {
            extends(c, "Notification")
                || extends(c, "Mailable")
                || (c.file.starts_with("app/Mail/") && !c.file.contains("/Concerns/"))
        })
        .filter(|c| keep(&c.fqn))
        .take(limit)
    {
        let at = triggered_at(conn, &m.fqn, &m.file)?;
        let kind = if extends(m, "Notification") {
            "notification"
        } else {
            "mail"
        };
        messages.push(json!({
            "kind": kind,
            "class": short(&m.fqn),
            "file": m.file,
            "queued": implements(m, "ShouldQueue"),
            "sent_at": if at.is_empty() { json!("nothing sends it") } else { uses(&at) },
        }));
    }

    let found =
        !(scheduled.is_empty() && jobs.is_empty() && events.is_empty() && messages.is_empty());
    Ok(json!({
        "found": found,
        "schedule": scheduled,
        "jobs": jobs,
        "events": events,
        "messages": messages,
        "note": "Read statically: dispatch/send sites are where the class is constructed or dispatched (X::dispatch, new X); listeners by handle()'s parameter type. Pass key_prefix to filter by name.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_statements() {
        assert_eq!(
            parse_schedule(
                "Schedule::command('orders:expire-holds')->everyMinute()->withoutOverlapping(5)"
            ),
            Some((
                "command orders:expire-holds".into(),
                "everyMinute() → withoutOverlapping(5)".into()
            ))
        );
        assert_eq!(
            parse_schedule("\n$schedule->job(new SendDigest(true))\n    ->dailyAt('07:00')"),
            Some(("job SendDigest".into(), "dailyAt('07:00')".into()))
        );
        assert_eq!(parse_schedule("$x->command('nope')"), None);
    }
}
