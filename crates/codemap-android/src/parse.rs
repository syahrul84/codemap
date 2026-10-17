//! tree-sitter parsers per thread, so files can be parsed in parallel.

use std::cell::RefCell;

use tree_sitter::{Parser, Tree};

thread_local! {
    static KOTLIN: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .expect("tree-sitter-kotlin grammar matches the tree-sitter version");
        parser
    });
    static JAVA: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .expect("tree-sitter-java grammar matches the tree-sitter version");
        parser
    });
}

/// Parses Kotlin source. `None` only if parsing was cancelled.
pub fn kotlin(src: &[u8]) -> Option<Tree> {
    let src = blank_inline_setters(src);
    let src = mask_soft_keywords(&src);
    KOTLIN.with(|p| p.borrow_mut().parse(&src, None))
}

/// Modifier words Kotlin also allows as names.
const SOFT_KEYWORDS: &[&[u8]] = &[
    b"open",
    b"data",
    b"value",
    b"inner",
    b"sealed",
    b"abstract",
    b"final",
    b"override",
    b"lateinit",
    b"annotation",
    b"enum",
    b"external",
    b"inline",
    b"infix",
    b"operator",
];

/// Words that may follow a modifier.
const DECLARATION_WORDS: &[&[u8]] = &[
    b"class",
    b"fun",
    b"val",
    b"var",
    b"interface",
    b"object",
    b"constructor",
    b"companion",
    b"private",
    b"public",
    b"protected",
    b"internal",
    b"override",
    b"open",
    b"data",
    b"value",
    b"inner",
    b"sealed",
    b"abstract",
    b"final",
    b"lateinit",
    b"annotation",
    b"enum",
    b"external",
    b"inline",
    b"infix",
    b"operator",
    b"suspend",
    b"tailrec",
    b"const",
    b"crossinline",
    b"noinline",
    b"reified",
    b"vararg",
    b"expect",
    b"actual",
    b"fun",
];

/// `var open by remember { … }` / `{ open = true }`: a modifier word used as
/// a name confuses the grammar. Its first letter becomes `_` (same length;
/// names are read from the original text).
fn mask_soft_keywords(src: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out: Option<Vec<u8>> = None;
    let mut i = 0;
    let mut in_string = false;
    while i < src.len() {
        let c = src[i];
        if c == b'"' && (i == 0 || src[i - 1] != b'\\') {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if in_string
            || !c.is_ascii_lowercase()
            || (i > 0 && (is_word(src[i - 1]) || src[i - 1] == b'@'))
        {
            i += 1;
            continue;
        }
        let Some(kw) = SOFT_KEYWORDS
            .iter()
            .find(|k| src[i..].starts_with(k) && src.get(i + k.len()).is_none_or(|c| !is_word(*c)))
        else {
            i += 1;
            continue;
        };
        let mut j = i + kw.len();
        while j < src.len()
            && (src[j] == b' ' || src[j] == b'\t' || src[j] == b'\n' || src[j] == b'\r')
        {
            j += 1;
        }
        let next_is_declaration = src[j..].starts_with(b"@")
            || DECLARATION_WORDS.iter().any(|w| {
                src[j..].starts_with(w) && src.get(j + w.len()).is_none_or(|c| !is_word(*c))
            });
        if !next_is_declaration {
            out.get_or_insert_with(|| src.to_vec())[i] = b'_';
        }
        i += kw.len();
    }
    match out {
        Some(v) => std::borrow::Cow::Owned(v),
        None => std::borrow::Cow::Borrowed(src),
    }
}

/// `var x by mutableStateOf(0); private set` is valid Kotlin the grammar
/// cannot read: the `; private set` is blanked out (same length, so lines
/// and columns stay right).
fn blank_inline_setters(src: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let mut i = 0;
    while i < src.len() {
        if src[i] == b';' {
            let mut j = i + 1;
            let skip_spaces = |j: &mut usize| {
                while *j < src.len() && (src[*j] == b' ' || src[*j] == b'\t') {
                    *j += 1;
                }
            };
            skip_spaces(&mut j);
            for vis in [&b"private"[..], b"internal", b"protected", b"public"] {
                if src[j..].starts_with(vis) {
                    j += vis.len();
                    skip_spaces(&mut j);
                    break;
                }
            }
            let accessor = src[j..].starts_with(b"set") || src[j..].starts_with(b"get");
            let end = j + 3;
            let word_ends = src
                .get(end)
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_' || *c == b'('));
            if accessor && word_ends {
                let buf = out.get_or_insert_with(|| src.to_vec());
                buf[i..end].fill(b' ');
                i = end;
                continue;
            }
        }
        i += 1;
    }
    match out {
        Some(v) => std::borrow::Cow::Owned(v),
        None => std::borrow::Cow::Borrowed(src),
    }
}

/// Parses Java source. `None` only if parsing was cancelled.
pub fn java(src: &[u8]) -> Option<Tree> {
    JAVA.with(|p| p.borrow_mut().parse(src, None))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn dump() {
        let src = r#"package com.example.app

import androidx.compose.runtime.Composable

/** Screens. */
sealed interface Route {
    data class Detail(val id: String) : Route
    data object Home : Route
}

class MainActivity : ComponentActivity() {
    private val state get() = 1
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent { Root(state) }
    }
}

@Composable
fun Root(state: AppState, onBack: () -> Unit = {}) {
    var tab by rememberSaveable { mutableIntStateOf(0) }
    when (val top = stack.lastOrNull()) {
        is Route.Detail -> DetailScreen(state, top.id)
        Route.Home -> HomeScreen(state)
        else -> when (tab) {
            0 -> TimesScreen(state)
            else -> SettingsScreen(state)
        }
    }
    Text(stringResource(R.string.title))
    NavHost(nav, startDestination = "home") {
        composable("home") { HomeScreen(state) }
        composable<Detail> { DetailScreen(state, it.id) }
    }
    if (show) ModalBottomSheet(onDismissRequest = {}) { AdjustSheet() }
}

object Store { const val KEY = "k"; fun load(): Int = 1 }
enum class Lang(val code: String) { EN("en"), MS("ms") }
typealias Cb = () -> Unit
"#;
        let t = super::kotlin(src.as_bytes()).unwrap();
        println!("{}", t.root_node().to_sexp());
        let j = "package a;\npublic class MainActivity extends AppCompatActivity implements Foo {\n  private int x = 1;\n  @Override protected void onCreate(Bundle b) { super.onCreate(b); setContentView(R.layout.main); getString(R.string.hi); new Helper().go(); }\n}\n";
        println!(
            "{}",
            super::java(j.as_bytes()).unwrap().root_node().to_sexp()
        );
    }
}

#[cfg(test)]
mod setter_tests {
    #[test]
    fn inline_private_setter() {
        let src = "class A {\n    var a by mutableStateOf(1); private set\n    val b = 2\n}\n";
        assert!(
            !super::kotlin(src.as_bytes())
                .unwrap()
                .root_node()
                .has_error()
        );
        assert_eq!(&*super::blank_inline_setters(b"f(); g()"), b"f(); g()");
    }

    #[test]
    fn modifier_words_as_names() {
        let src = "open class A {\n    open fun f() {\n        var open by remember { mutableStateOf(false) }\n        g(onClick = { open = true }, value = open)\n    }\n}\nenum class E { X }\ndata class D(val data: Int)\n";
        assert!(
            !super::kotlin(src.as_bytes())
                .unwrap()
                .root_node()
                .has_error()
        );
        assert_eq!(
            String::from_utf8(super::mask_soft_keywords(src.as_bytes()).into_owned()).unwrap(),
            src.replace("var open", "var _pen")
                .replace("{ open =", "{ _pen =")
                .replace("value = open", "_alue = _pen")
                .replace("val data", "val _ata")
        );
    }
}
