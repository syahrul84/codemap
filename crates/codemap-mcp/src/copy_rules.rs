//! House copy rules: banned words or phrases per locale (`[copy] banned`),
//! found in translation files of that locale and in admin (Filament) labels.

use codemap_core::Store;

use crate::tools::{Project, ToolError};

type R<T> = Result<T, ToolError>;

/// The locale a translation file belongs to (`lang/ms/orders.php`,
/// `lang/ms.json`, `i18n/locales/ms.ts`, `src/lang/ms/index.js`).
fn locale_of(path: &str) -> Option<String> {
    codemap_laravel::lang::lang_file(path)
        .map(|(l, _)| l)
        .or_else(|| codemap_vue::i18n::locale_of(path))
}

/// Whole-word, case-insensitive occurrences of `phrase` in `line`.
fn contains_word(line: &str, phrase: &str) -> bool {
    let hay = line.to_lowercase();
    let needle = phrase.to_lowercase();
    if needle.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(at) = hay[start..].find(&needle) {
        let i = start + at;
        let before = hay[..i].chars().next_back();
        let after = hay[i + needle.len()..].chars().next();
        let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
        if edge(before) && edge(after) {
            return true;
        }
        start = i + needle.len();
    }
    false
}

/// `None` when no rules are configured; otherwise each hit as
/// `file:line "word" (locale): text`.
pub fn check(store: &Store, project: &Project) -> R<Option<Vec<String>>> {
    let rules = &project.config.copy.banned;
    if rules.is_empty() {
        return Ok(None);
    }
    let all: Vec<&String> = rules.get("all").into_iter().flatten().collect();
    let base = project
        .config
        .i18n
        .locales
        .first()
        .cloned()
        .unwrap_or_else(|| "en".into());
    let mut out = Vec::new();
    for f in store.list_files()?.into_iter().map(|f| f.path) {
        let words: Vec<&String> = if let Some(l) = locale_of(&f) {
            rules
                .get(&l)
                .into_iter()
                .flatten()
                .chain(all.iter().copied())
                .collect()
        } else if f.starts_with("app/Filament/") && f.ends_with(".php") {
            rules
                .get(&base)
                .into_iter()
                .flatten()
                .chain(all.iter().copied())
                .collect()
        } else {
            continue;
        };
        if words.is_empty() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(project.root.join(&f)) else {
            continue;
        };
        let filament = f.starts_with("app/Filament/");
        for (i, line) in text.lines().enumerate() {
            // Admin code: only quoted text shown to people (labels, titles, hints).
            if filament
                && ![
                    "label(",
                    "title",
                    "heading",
                    "helperText(",
                    "placeholder(",
                    "description(",
                ]
                .iter()
                .any(|m| line.contains(m))
            {
                continue;
            }
            for w in &words {
                if contains_word(line, w) {
                    let shown: String = line.trim().chars().take(100).collect();
                    out.push(format!("{f}:{} \"{w}\": {shown}", i + 1));
                    break;
                }
            }
            if out.len() >= 200 {
                return Ok(Some(out));
            }
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_words_only() {
        assert!(contains_word("'title' => 'Your Bag is ready'", "bag"));
        assert!(contains_word("Save food today", "save food"));
        assert!(!contains_word("'baggage' => 'x'", "bag"));
        assert!(!contains_word("'handbags'", "bag"));
        assert!(contains_word("'bags.'", "bags"));
    }
}
