//! English singular/plural, ported from the common rules of doctrine/inflector
//! (what Laravel's `Str::singular` / `Str::plural` use). Results derived from
//! these rules are guesses and must be marked `inferred` by callers.

/// Words with the same singular and plural form.
const UNCOUNTABLE: &[&str] = &[
    "audio",
    "bison",
    "cattle",
    "chassis",
    "compensation",
    "coreopsis",
    "data",
    "deer",
    "education",
    "emoji",
    "equipment",
    "evidence",
    "feedback",
    "firmware",
    "fish",
    "furniture",
    "gold",
    "hardware",
    "information",
    "jedi",
    "kin",
    "knowledge",
    "love",
    "metadata",
    "money",
    "moose",
    "news",
    "nutrition",
    "offspring",
    "plankton",
    "pokemon",
    "police",
    "rain",
    "recommended",
    "related",
    "rice",
    "series",
    "sheep",
    "software",
    "species",
    "swine",
    "traffic",
    "wheat",
];

/// (singular, plural)
const IRREGULAR: &[(&str, &str)] = &[
    ("person", "people"),
    ("man", "men"),
    ("woman", "women"),
    ("child", "children"),
    ("tooth", "teeth"),
    ("foot", "feet"),
    ("goose", "geese"),
    ("mouse", "mice"),
    ("ox", "oxen"),
    ("leaf", "leaves"),
    ("life", "lives"),
    ("knife", "knives"),
    ("wife", "wives"),
    ("half", "halves"),
    ("criterion", "criteria"),
    ("cactus", "cacti"),
    ("analysis", "analyses"),
    ("axis", "axes"),
    ("crisis", "crises"),
    ("move", "moves"),
    ("cookie", "cookies"),
    ("movie", "movies"),
    ("zombie", "zombies"),
    ("genus", "genera"),
    ("shoe", "shoes"),
    ("valve", "valves"),
    ("curve", "curves"),
    ("medium", "media"),
];

/// The trailing word of `s` (after the last `_`, `-` or space), and its start index.
fn last_word(s: &str) -> (usize, &str) {
    let start = s.rfind(['_', '-', ' ']).map_or(0, |i| i + 1);
    (start, &s[start..])
}

fn with_case(original: &str, replacement: &str) -> String {
    if original.chars().next().is_some_and(char::is_uppercase) {
        let mut chars = replacement.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
            .unwrap_or_default()
    } else {
        replacement.to_owned()
    }
}

/// Applies `f` to the trailing word only (`order_items` → `order_item`).
fn on_last_word(s: &str, f: impl Fn(&str) -> String) -> String {
    let (start, word) = last_word(s);
    format!("{}{}", &s[..start], f(word))
}

pub fn plural(s: &str) -> String {
    on_last_word(s, plural_word)
}

pub fn singular(s: &str) -> String {
    let (start, _) = last_word(s);
    on_last_word(s, |w| singular_word(w, start == 0))
}

fn plural_word(word: &str) -> String {
    let lower = word.to_lowercase();
    if lower.is_empty() || UNCOUNTABLE.contains(&lower.as_str()) {
        return word.to_owned();
    }
    if let Some((_, p)) = IRREGULAR.iter().find(|(sg, _)| *sg == lower) {
        return with_case(word, p);
    }
    if IRREGULAR.iter().any(|(_, p)| *p == lower) {
        return word.to_owned();
    }
    let ends = |suffixes: &[&str]| suffixes.iter().any(|e| lower.ends_with(e));
    let stem = |n: usize| &word[..word.len() - n];
    if lower.ends_with("quiz") {
        format!("{word}zes")
    } else if ends(&["matrix", "vertex", "index"]) {
        format!("{}ices", stem(2))
    } else if ends(&[
        "status", "alias", "virus", "campus", "bus", "x", "ch", "ss", "sh", "zz",
    ]) || ends(&["buffalo", "tomato", "potato", "echo", "hero", "veto"])
    {
        format!("{word}es")
    } else if lower.ends_with('y') && !ends_with_vowel_y(&lower) {
        format!("{}ies", stem(1))
    } else if lower.ends_with("fe") && !lower.ends_with("ffe") && !lower.ends_with("hive") {
        format!("{}ves", stem(2))
    } else if (lower.ends_with("lf") || lower.ends_with("rf")) && lower.len() > 2 {
        format!("{}ves", stem(1))
    } else if lower.ends_with("sis") {
        format!("{}ses", stem(3))
    } else if ends(&["tum", "ium"]) {
        format!("{}a", stem(2))
    } else if lower.ends_with('s') {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

fn ends_with_vowel_y(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    bytes.len() >= 2 && matches!(bytes[bytes.len() - 2], b'a' | b'e' | b'i' | b'o' | b'u')
        || lower.ends_with("quy")
}

/// `whole`: `word` is the entire input, not the tail of `knowledge_bases`.
fn singular_word(word: &str, whole: bool) -> String {
    let lower = word.to_lowercase();
    if lower.is_empty() || UNCOUNTABLE.contains(&lower.as_str()) {
        return word.to_owned();
    }
    if let Some((sg, _)) = IRREGULAR.iter().find(|(_, p)| *p == lower) {
        return with_case(word, sg);
    }
    if IRREGULAR.iter().any(|(sg, _)| *sg == lower) {
        return word.to_owned();
    }
    let ends = |suffixes: &[&str]| suffixes.iter().any(|e| lower.ends_with(e));
    let stem = |n: usize| &word[..word.len() - n];
    if lower.ends_with("quizzes") {
        stem(3).to_owned()
    } else if lower.ends_with("matrices") {
        format!("{}ix", stem(4))
    } else if ends(&["vertices", "indices"]) {
        format!("{}ex", stem(4))
    } else if ends(&["statuses", "aliases", "viruses", "campuses", "buses"])
        || ends(&["xes", "ches", "sses", "shes", "zzes"])
        || ends(&[
            "buffaloes",
            "tomatoes",
            "potatoes",
            "echoes",
            "heroes",
            "vetoes",
        ])
    {
        stem(2).to_owned()
    } else if lower.ends_with("ies") && lower.len() > 4 {
        format!("{}y", stem(3))
    } else if ends(&["hives", "tives", "drives", "dives", "olives", "oves"])
        || (lower.ends_with('s') && !ends(&["ss", "us", "is", "ves", "ses", "ia", "ta"]))
    {
        stem(1).to_owned()
    } else if ends(&["lves", "rves"]) {
        format!("{}f", stem(3))
    } else if lower.ends_with("ves") {
        format!("{}fe", stem(3))
    } else if ends(&["analyses", "theses", "diagnoses", "crises"]) || (whole && lower == "bases") {
        // doctrine anchors this one (`^ba`), so `databases` → `database`.
        format!("{}is", stem(2))
    } else if lower.ends_with("ses") {
        stem(1).to_owned()
    } else if ends(&["ia", "ta"]) {
        format!("{}um", stem(1))
    } else {
        word.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values are Laravel `Str::plural` / `Str::singular` outputs.
    const PAIRS: &[(&str, &str)] = &[
        ("order", "orders"),
        ("order_item", "order_items"),
        ("category", "categories"),
        ("day", "days"),
        ("box", "boxes"),
        ("match", "matches"),
        ("address", "addresses"),
        ("status", "statuses"),
        ("alias", "aliases"),
        ("person", "people"),
        ("sales_person", "sales_people"),
        ("child", "children"),
        ("equipment", "equipment"),
        ("news", "news"),
        ("index", "indices"),
        ("knife", "knives"),
        ("leaf", "leaves"),
        ("hero", "heroes"),
        ("analysis", "analyses"),
        ("medium", "media"),
        ("quiz", "quizzes"),
        ("bus", "buses"),
        ("movie", "movies"),
        ("user", "users"),
        ("tag", "tags"),
        ("invoice", "invoices"),
        ("photo", "photos"),
        ("comment", "comments"),
        ("customer", "customers"),
        ("Order", "Orders"),
        ("database", "databases"),
        ("knowledge_base", "knowledge_bases"),
        ("basis", "bases"),
        ("hypothesis", "hypotheses"),
    ];

    #[test]
    fn plural_and_singular_roundtrip() {
        for (sg, pl) in PAIRS {
            assert_eq!(plural(sg), *pl, "plural({sg})");
            assert_eq!(singular(pl), *sg, "singular({pl})");
        }
    }

    #[test]
    fn kebab_segments_keep_their_prefix() {
        assert_eq!(singular("order-items"), "order-item");
        assert_eq!(singular("blog-posts"), "blog-post");
        assert_eq!(singular("knowledge-bases"), "knowledge-base");
        assert_eq!(singular("Databases"), "Database");
    }

    /// doctrine's `ves` rules, including the odd results Laravel really gives.
    #[test]
    fn singular_ves_matches_laravel() {
        assert_eq!(singular("reserves"), "reserf");
        assert_eq!(singular("nerves"), "nerf");
        assert_eq!(singular("drives"), "drive");
        assert_eq!(singular("dives"), "dive");
        assert_eq!(singular("olives"), "olive");
        assert_eq!(singular("gloves"), "glove");
        assert_eq!(singular("archives"), "archive");
        assert_eq!(singular("wolves"), "wolf");
        assert_eq!(singular("knives"), "knife");
    }
}
