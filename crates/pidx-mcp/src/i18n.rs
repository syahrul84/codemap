//! Translation health: keys missing per locale, keys used in code but not
//! defined, and keys never used. Works the same for the frontend (vue-i18n /
//! nuxt-i18n) and Laravel (`lang/`, `__()`).

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Value, json};

use crate::tools::ToolError;

type R<T> = Result<T, ToolError>;

pub struct Catalog {
    pub source: String,
    pub locales: BTreeMap<String, HashSet<String>>,
    pub base: String,
    /// A namespace all messages live under (`message.` in `{ en: { message: en } }`).
    pub wrapper: Option<String>,
    /// `(key, file, line, dynamic)`.
    pub uses: Vec<(String, String, u32, bool)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Health {
    pub source: String,
    pub base: String,
    pub locales: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<String>,
    /// Base-locale keys missing in each other locale.
    pub missing: BTreeMap<String, usize>,
    pub missing_samples: BTreeMap<String, Vec<String>>,
    /// Keys used in code but defined in no locale (`file:line key`).
    pub undefined: Vec<String>,
    pub undefined_total: usize,
    /// Plain-text keys (`__('Save changes')`) that fall back to themselves.
    pub literal_text_uses: usize,
    pub possibly_unused: usize,
}

impl Catalog {
    pub fn load(conn: &Connection, source: &str) -> R<Option<Self>> {
        let mut locales: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        {
            let mut stmt = conn.prepare("SELECT locale, key FROM i18n_keys WHERE source = ?1")?;
            let rows = stmt.query_map([source], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (l, k) = row?;
                locales.entry(l).or_default().insert(k);
            }
        }
        let uses: Vec<(String, String, u32, bool)> = {
            let mut stmt = conn.prepare(
                "SELECT u.key, f.path, u.line, u.dynamic FROM i18n_uses u JOIN files f ON f.id = u.file_id
                 WHERE u.source = ?1 ORDER BY f.path, u.line",
            )?;
            let rows = stmt.query_map([source], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if locales.is_empty() && uses.is_empty() {
            return Ok(None);
        }
        let base = if locales.contains_key("en") {
            "en".to_owned()
        } else {
            locales
                .iter()
                .max_by_key(|(_, k)| k.len())
                .map(|(l, _)| l.clone())
                .unwrap_or_else(|| "en".into())
        };
        let mut c = Self {
            source: source.to_owned(),
            locales,
            base,
            wrapper: None,
            uses,
        };
        c.wrapper = c.infer_wrapper();
        Ok(Some(c))
    }

    fn base_keys(&self) -> Option<&HashSet<String>> {
        self.locales.get(&self.base)
    }

    /// The first segment that, stripped from used keys, makes the most of them defined.
    fn infer_wrapper(&self) -> Option<String> {
        let base = self.base_keys()?;
        let direct = self.uses.iter().filter(|u| base.contains(&u.0)).count();
        let mut votes: HashMap<&str, usize> = HashMap::new();
        for (k, _, _, _) in &self.uses {
            if let Some((first, rest)) = k.split_once('.')
                && !base.contains(k)
                && base.contains(rest)
            {
                *votes.entry(first).or_insert(0) += 1;
            }
        }
        let (w, n) = votes.into_iter().max_by_key(|(_, n)| *n)?;
        (n > direct / 4 && n >= 3).then(|| w.to_owned())
    }

    /// The key as stored in locale files.
    pub fn normalize<'k>(&self, key: &'k str) -> &'k str {
        if let Some(w) = &self.wrapper
            && let Some(rest) = key
                .strip_prefix(w.as_str())
                .and_then(|r| r.strip_prefix('.'))
        {
            return rest;
        }
        key
    }

    /// `__('Save changes')`, `__('Unauthorized.')`: text that is its own key.
    fn is_literal_text(key: &str) -> bool {
        key.contains(' ')
            || key.split('.').any(str::is_empty)
            || !key.contains('.') && key.chars().next().is_some_and(|c| c.is_uppercase())
    }

    /// Defined in `locale` (a key, a group of keys, or matching a dynamic prefix).
    pub fn defined_in(&self, locale: &str, key: &str, dynamic: bool) -> bool {
        let Some(keys) = self.locales.get(locale) else {
            return false;
        };
        let k = self.normalize(key);
        if keys.contains(k) {
            return true;
        }
        let group = format!("{}.", k.trim_end_matches('.'));
        (dynamic || !k.is_empty())
            && keys
                .iter()
                .any(|x| x.starts_with(&group) || (dynamic && x.starts_with(k)))
    }

    pub fn defined_anywhere(&self, key: &str, dynamic: bool) -> bool {
        self.locales
            .keys()
            .any(|l| self.defined_in(l, key, dynamic))
    }

    pub fn health(&self, samples: usize) -> Health {
        let empty = HashSet::new();
        let base = self.base_keys().unwrap_or(&empty);
        let mut missing = BTreeMap::new();
        let mut missing_samples = BTreeMap::new();
        for (l, keys) in &self.locales {
            if *l == self.base {
                continue;
            }
            let mut m: Vec<&String> = base.iter().filter(|k| !keys.contains(*k)).collect();
            m.sort();
            missing.insert(l.clone(), m.len());
            if !m.is_empty() {
                missing_samples.insert(l.clone(), m.into_iter().take(samples).cloned().collect());
            }
        }
        let mut undefined = Vec::new();
        let mut undefined_total = 0;
        let mut literal = 0;
        for (k, file, line, dynamic) in &self.uses {
            if self.defined_anywhere(k, *dynamic) {
                continue;
            }
            if !dynamic && Self::is_literal_text(k) {
                literal += 1;
                continue;
            }
            undefined_total += 1;
            if undefined.len() < samples * 3 {
                undefined.push(format!(
                    "{file}:{line} {k}{}",
                    if *dynamic { "…" } else { "" }
                ));
            }
        }
        let used: Vec<(&str, bool)> = self
            .uses
            .iter()
            .map(|u| (self.normalize(&u.0), u.3))
            .collect();
        let used_exact: HashSet<&str> = used.iter().filter(|u| !u.1).map(|u| u.0).collect();
        let prefixes: Vec<&str> = used.iter().filter(|u| u.1).map(|u| u.0).collect();
        let possibly_unused = base
            .iter()
            .filter(|k| {
                !used_exact.contains(k.as_str())
                    && !prefixes.iter().any(|p| k.starts_with(p))
                    && !used_exact.iter().any(|u| k.starts_with(&format!("{u}.")))
            })
            .count();
        Health {
            source: self.source.clone(),
            base: self.base.clone(),
            locales: self
                .locales
                .iter()
                .map(|(l, k)| (l.clone(), k.len()))
                .collect(),
            wrapper: self.wrapper.clone(),
            missing,
            missing_samples,
            undefined,
            undefined_total,
            literal_text_uses: literal,
            possibly_unused,
        }
    }

    /// Closest defined keys (same first segment first).
    pub fn suggest(&self, key: &str) -> Vec<String> {
        let Some(base) = self.base_keys() else {
            return Vec::new();
        };
        let k = self.normalize(key);
        let first = k.split('.').next().unwrap_or(k);
        let mut scored: Vec<(f64, &String)> = base
            .iter()
            .map(|x| {
                let same = x.split('.').next() == Some(first);
                (
                    strsim::jaro_winkler(k, x) + if same { 0.05 } else { 0.0 },
                    x,
                )
            })
            .filter(|(s, _)| *s > 0.85)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        scored
            .into_iter()
            .take(3)
            .map(|(_, x)| match &self.wrapper {
                Some(w) => format!("{w}.{x}"),
                None => x.clone(),
            })
            .collect()
    }
}

pub fn catalogs(conn: &Connection) -> R<Vec<Catalog>> {
    let mut out = Vec::new();
    for s in ["vue", "laravel"] {
        if let Some(c) = Catalog::load(conn, s)? {
            out.push(c);
        }
    }
    Ok(out)
}

/// For `project_overview`.
pub fn overview(conn: &Connection) -> R<Value> {
    let cats = catalogs(conn)?;
    if cats.is_empty() {
        return Ok(Value::Null);
    }
    let list: Vec<Value> = cats
        .iter()
        .map(|c| {
            let h = c.health(0);
            json!({
                "source": h.source,
                "base_locale": h.base,
                "locales": h.locales,
                "missing_per_locale": h.missing,
                "used_but_undefined": h.undefined_total,
            })
        })
        .collect();
    Ok(json!(list))
}

/// `verify` claims `orders.title` / `message.save`.
pub fn verify_key(conn: &Connection, key: &str) -> R<(String, Option<String>, Vec<String>)> {
    let cats = catalogs(conn)?;
    if cats.iter().all(|c| c.locales.is_empty()) {
        return Ok((
            "unresolved".into(),
            Some("no locale files indexed".into()),
            Vec::new(),
        ));
    }
    let dynamic = key.ends_with('.') || key.ends_with('*');
    let k = key.trim_end_matches('*');
    for c in &cats {
        if c.defined_anywhere(k, dynamic) {
            let missing: Vec<&str> = c
                .locales
                .keys()
                .filter(|l| !c.defined_in(l, k, dynamic))
                .map(String::as_str)
                .collect();
            let detail = if missing.is_empty() {
                format!("{} ({} locales)", c.source, c.locales.len())
            } else {
                format!("{}: missing in {}", c.source, missing.join(", "))
            };
            return Ok(("exists".into(), Some(detail), Vec::new()));
        }
    }
    let suggestions = cats.iter().flat_map(|c| c.suggest(k)).take(5).collect();
    Ok(("not_found".into(), None, suggestions))
}

/// Keys a file uses that are not defined.
pub fn file_keys(conn: &Connection, file: &str) -> R<Value> {
    let cats = catalogs(conn)?;
    let mut used = Vec::new();
    let mut undefined = Vec::new();
    for c in &cats {
        for (k, f, line, dynamic) in c.uses.iter().filter(|u| u.1 == file) {
            let label = if *dynamic {
                format!("{k}…")
            } else {
                k.clone()
            };
            if !used.contains(&label) {
                used.push(label.clone());
            }
            if !c.defined_anywhere(k, *dynamic) && (*dynamic || !Catalog::is_literal_text(k)) {
                undefined.push(format!("{label} :{line}"));
            }
            let _ = f;
        }
    }
    if used.is_empty() {
        return Ok(Value::Null);
    }
    let mut v = json!({ "keys": used });
    if !undefined.is_empty() {
        v["undefined"] = json!(undefined);
    }
    Ok(v)
}

pub fn doctor_text(conn: &Connection) -> R<String> {
    let mut s = String::new();
    for c in catalogs(conn)? {
        let h = c.health(8);
        s.push_str(&format!(
            "\ntranslations ({}): base locale {}, locales {}{}\n",
            h.source,
            h.base,
            h.locales
                .iter()
                .map(|(l, n)| format!("{l} {n}"))
                .collect::<Vec<_>>()
                .join(", "),
            h.wrapper
                .as_ref()
                .map(|w| format!(", keys used under `{w}.`"))
                .unwrap_or_default()
        ));
        for (l, n) in &h.missing {
            if *n > 0 {
                s.push_str(&format!(
                    "  {l}: {n} keys missing, e.g. {}\n",
                    h.missing_samples
                        .get(l)
                        .map(|v| v.join(", "))
                        .unwrap_or_default()
                ));
            }
        }
        if h.undefined_total > 0 {
            s.push_str(&format!(
                "  used in code but defined in no locale ({}):\n",
                h.undefined_total
            ));
            for u in &h.undefined {
                s.push_str(&format!("    {u}\n"));
            }
        }
        if h.literal_text_uses > 0 {
            s.push_str(&format!(
                "  {} plain-text keys fall back to their own text\n",
                h.literal_text_uses
            ));
        }
        s.push_str(&format!("  {} base keys look unused\n", h.possibly_unused));
    }
    Ok(s)
}

pub fn health_json(conn: &Connection) -> R<Value> {
    Ok(json!(
        catalogs(conn)?
            .iter()
            .map(|c| c.health(20))
            .collect::<Vec<_>>()
    ))
}
