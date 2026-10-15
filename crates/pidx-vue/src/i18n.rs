//! Frontend translations: locale files (JSON / JS / TS, single or split per
//! locale) and keys used in templates (`$t('a.b')`, `v-t`, `<i18n-t keypath>`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocaleFacts {
    pub locale: String,
    /// Flattened dotted keys.
    pub keys: Vec<String>,
    /// Some values could not be read (spreads, imports, functions).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

const LOCALE_DIRS: &[&str] = &[
    "lang",
    "langs",
    "locale",
    "locales",
    "i18n",
    "translations",
    "messages",
];

/// `en`, `ms`, `zh-Hant`, `pt_BR`, `en-US`.
pub fn is_locale_code(s: &str) -> bool {
    let (base, region) = match s.split_once(['-', '_']) {
        Some((b, r)) => (b, Some(r)),
        None => (s, None),
    };
    (2..=3).contains(&base.len())
        && base.chars().all(|c| c.is_ascii_lowercase())
        && region.is_none_or(|r| {
            (2..=4).contains(&r.len()) && r.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

/// The locale a frontend file holds messages for, when it is a locale file:
/// `…/locales/en.json`, `…/lang/en/index.js`, `…/i18n/locales/en/orders.js`.
pub fn locale_of(path: &str) -> Option<String> {
    if !(path.ends_with(".json")
        || path.ends_with(".js")
        || path.ends_with(".ts")
        || path.ends_with(".mjs"))
    {
        return None;
    }
    let segs: Vec<&str> = path.split('/').collect();
    let dir = segs.iter().rposition(|s| LOCALE_DIRS.contains(s))?;
    let rest = &segs[dir + 1..];
    match rest {
        [file] => {
            let stem = file.split('.').next()?;
            is_locale_code(stem).then(|| stem.to_owned())
        }
        [loc, ..] if is_locale_code(loc) => Some((*loc).to_owned()),
        _ => None,
    }
}

fn flatten(v: &Value, prefix: &str, out: &mut Vec<String>, partial: &mut bool) {
    match v {
        Value::Object(o) if o.contains_key("$expr") => {
            *partial = true;
            if !prefix.is_empty() {
                out.push(prefix.to_owned());
            }
        }
        Value::Object(o) => {
            for (k, child) in o {
                if k == "$partial" {
                    *partial = true;
                    continue;
                }
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(child, &key, out, partial);
            }
        }
        _ => {
            if !prefix.is_empty() {
                out.push(prefix.to_owned());
            }
        }
    }
}

pub fn locale_facts(path: &str, src: &str) -> Option<LocaleFacts> {
    let locale = locale_of(path)?;
    let value = if path.ends_with(".json") {
        serde_json::from_str::<Value>(src).ok()?
    } else {
        let ext = path.rsplit('.').next().unwrap_or("js");
        pidx_js::literal::default_export(src, ext).or_else(|| {
            // `export const en = {...}` / `export const messages = {...}`: one object.
            let all = pidx_js::literal::exports(src, ext);
            let objects: Vec<&Value> = all.values().filter(|v| v.is_object()).collect();
            (objects.len() == 1).then(|| objects[0].clone())
        })?
    };
    if !value.is_object() {
        return None;
    }
    let mut keys = Vec::new();
    let mut partial = false;
    flatten(&value, "", &mut keys, &mut partial);
    Some(LocaleFacts {
        locale,
        keys,
        partial,
    })
}

/// Keys used in a template expression: `$t('a.b')`, `t("a")`, `$tc('x', n)`,
/// `` $t(`status.${s}`) `` (dynamic prefix).
pub fn template_uses(expr: &str) -> Vec<(String, bool)> {
    let b = expr.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !matches!(b[i], b'$' | b't') {
            i += 1;
            continue;
        }
        let rest = &expr[i..];
        let name_len = ["$tc(", "$te(", "$tm(", "$t(", "tc(", "t("]
            .iter()
            .find(|n| rest.starts_with(**n))
            .map(|n| n.len());
        let boundary = i == 0
            || !(b[i - 1].is_ascii_alphanumeric()
                || b[i - 1] == b'_'
                || b[i - 1] == b'.'
                || b[i - 1] == b'$');
        if let (Some(n), true) = (name_len, boundary) {
            let arg = expr[i + n..].trim_start();
            if let Some(q) = arg.chars().next().filter(|c| matches!(c, '\'' | '"' | '`'))
                && let Some(end) = arg[1..].find(q)
            {
                let lit = &arg[1..1 + end];
                if q == '`' && lit.contains("${") {
                    let prefix = lit.split("${").next().unwrap_or("");
                    if !prefix.is_empty() {
                        out.push((prefix.to_owned(), true));
                    }
                } else if !lit.is_empty() {
                    out.push((lit.to_owned(), false));
                }
            }
            i += n;
            continue;
        }
        i += 1;
    }
    out
}

/// `v-t="'a.b'"` / `v-t="{ path: 'a.b' }"`.
pub fn v_t_key(value: &str) -> Option<String> {
    let v = value.trim();
    let v = match v.find("path") {
        Some(at) if v.starts_with('{') => &v[at + 4..],
        _ => v,
    };
    let v = v.trim_start_matches([':', ' ']);
    let q = v.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let end = v[1..].find(q)?;
    Some(v[1..1 + end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_paths() {
        assert_eq!(locale_of("i18n/locales/en.json").as_deref(), Some("en"));
        assert_eq!(locale_of("src/lang/zh/index.js").as_deref(), Some("zh"));
        assert_eq!(locale_of("i18n/locales/en/anp.js").as_deref(), Some("en"));
        assert_eq!(
            locale_of("app/i18n/locales/zh-Hant.json").as_deref(),
            Some("zh-Hant")
        );
        assert_eq!(locale_of("src/i18n/messages.ts"), None);
        assert_eq!(locale_of("src/lang/index.js"), None);
        assert_eq!(locale_of("src/components/en.vue"), None);
        assert_eq!(
            locale_of("src/i18n/locales/missing-translations.json"),
            None
        );
    }

    #[test]
    fn locale_file_keys() {
        let f = locale_facts("src/lang/en/index.js", "export default { menu: { title: 'Menu', items: { a: 'A' } }, 'save.now': 'Save', list: ['a'] }").unwrap();
        assert_eq!(f.keys, ["menu.title", "menu.items.a", "save.now", "list"]);
        let f = locale_facts("i18n/locales/ms.json", r#"{"cart":{"empty":"Kosong"}}"#).unwrap();
        assert_eq!(
            (f.locale.as_str(), f.keys.as_slice()),
            ("ms", ["cart.empty".to_owned()].as_slice())
        );
        let f = locale_facts(
            "src/i18n/locales/en.ts",
            "import common from './common'\nexport default { ...common, x: 'X' }",
        )
        .unwrap();
        assert_eq!(f.keys, ["x"]);
    }

    #[test]
    fn template_keys() {
        assert_eq!(
            template_uses("$t('a.b') + t(\"c\") + item.t('x') + $tc('n', 2) + $t(`status.${s}`)"),
            [
                ("a.b".to_owned(), false),
                ("c".to_owned(), false),
                ("n".to_owned(), false),
                ("status.".to_owned(), true)
            ]
        );
        assert_eq!(template_uses("—é $t('a')"), [("a".to_owned(), false)]);
        assert_eq!(v_t_key("'menu.title'").as_deref(), Some("menu.title"));
        assert_eq!(
            v_t_key("{ path: 'menu.title', args: {} }").as_deref(),
            Some("menu.title")
        );
    }
}
