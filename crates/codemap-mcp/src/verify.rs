//! `verify`: checks that every class, method, function, route, table, column
//! and relation a change relies on exists. Each item is `exists`, `not_found`
//! (with suggestions), `external` (vendor / PHP built-in) or `unresolved`.

use std::collections::BTreeSet;
use std::path::Path;

use codemap_core::Store;
use codemap_core::envelope::Detail;
use codemap_core::symbols::{self, MemberLookup, SearchInput, SymbolKind, SymbolRecord};
use codemap_laravel::{models, routes, schema};
use codemap_php::Scope;
use codemap_php::ast::{apply_use_declaration, named_children, text};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tree_sitter::Node;

use crate::tools::{DetailParam, Project, ToolError};

/// Things a change relies on.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Claims {
    /// FQNs or short names, e.g. `App\Models\Order`.
    #[serde(default)]
    pub classes: Vec<String>,
    /// `Class::method`.
    #[serde(default)]
    pub methods: Vec<String>,
    /// Global or namespaced functions, e.g. `money`.
    #[serde(default)]
    pub functions: Vec<String>,
    /// Route names (`orders.store`) or `METHOD uri` (`POST api/orders`).
    #[serde(default)]
    pub routes: Vec<String>,
    /// Table names.
    #[serde(default)]
    pub tables: Vec<String>,
    /// `table.column`.
    #[serde(default)]
    pub columns: Vec<String>,
    /// `Model::relation`.
    #[serde(default)]
    pub relations: Vec<String>,
    /// Vue components by name or tag: `OrderCard`, `order-card`, `UButton`.
    #[serde(default)]
    pub components: Vec<String>,
    /// Backend endpoints a frontend call relies on: `GET /orders/{id}` (the API
    /// base is inferred) or `POST api/v1/carts`. Checked against linked backends.
    #[serde(default)]
    pub endpoints: Vec<String>,
    /// Translation keys: `orders.title` (vue-i18n or Laravel). A trailing `.`
    /// or `*` checks a group. Reports locales the key is missing in.
    #[serde(default)]
    pub i18n_keys: Vec<String>,
    /// From code: columns used on a model's query (model FQN or short name, column).
    #[serde(skip)]
    #[schemars(skip)]
    pub model_columns: Vec<(String, String)>,
    /// From code: `table.column` found in raw SQL / qualified names; checked
    /// only when `table` is a real table (it may be an alias).
    #[serde(skip)]
    #[schemars(skip)]
    pub qualified_columns: Vec<String>,
}

impl Claims {
    pub fn is_empty(&self) -> bool {
        [
            &self.classes,
            &self.methods,
            &self.functions,
            &self.routes,
            &self.tables,
            &self.columns,
            &self.relations,
            &self.components,
            &self.endpoints,
            &self.i18n_keys,
        ]
        .iter()
        .all(|l| l.is_empty())
            && self.model_columns.is_empty()
            && self.qualified_columns.is_empty()
    }
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct VerifyParams {
    /// Workspace app to look at (`backend`, `consumer`, …). Omit to search every app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Explicit claims to check.
    #[serde(default)]
    pub claims: Option<Claims>,
    /// PHP code to scan for class, method, route and table references.
    #[serde(default)]
    pub php_snippet: Option<String>,
    /// Pre-release check instead of claims: only problems (missing endpoints,
    /// fields not returned, translation gaps, unused code added on the branch,
    /// migrations not run, config / env keys, settings without defaults, copy rules).
    #[serde(default)]
    pub release_check: bool,
    /// With `release_check`: the branch to compare with (default: origin/HEAD,
    /// main, master or develop).
    #[serde(default)]
    pub base: Option<String>,
    /// The output of the project's type check (vue-tsc, nuxi typecheck, tsc,
    /// or PHPStan with --error-format=raw): only errors not in the saved
    /// starting point are reported. The first output given becomes the
    /// starting point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_errors: Option<String>,
    /// With `type_errors`: save this output as the new starting point.
    #[serde(default)]
    pub type_errors_save: bool,
    #[serde(default)]
    pub detail: DetailParam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Exists,
    NotFound,
    External,
    Unresolved,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub kind: &'static str,
    pub item: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

impl Check {
    fn new(kind: &'static str, item: &str, status: Status) -> Self {
        Self {
            kind,
            item: item.to_owned(),
            status,
            file: None,
            detail: None,
            suggestions: Vec::new(),
        }
    }
    fn file(mut self, f: Option<String>) -> Self {
        self.file = f;
        self
    }
    fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }
    fn suggest(mut self, s: Vec<String>) -> Self {
        self.suggestions = s;
        self
    }
}

const PHP_BUILTIN_CLASSES: &[&str] = &[
    "Exception",
    "ErrorException",
    "Error",
    "TypeError",
    "ValueError",
    "ArgumentCountError",
    "ArithmeticError",
    "DivisionByZeroError",
    "Throwable",
    "RuntimeException",
    "LogicException",
    "InvalidArgumentException",
    "DomainException",
    "LengthException",
    "OutOfRangeException",
    "OutOfBoundsException",
    "RangeException",
    "OverflowException",
    "UnderflowException",
    "UnexpectedValueException",
    "BadFunctionCallException",
    "BadMethodCallException",
    "JsonException",
    "Closure",
    "Generator",
    "stdClass",
    "DateTime",
    "DateTimeImmutable",
    "DateTimeInterface",
    "DateTimeZone",
    "DateInterval",
    "DatePeriod",
    "ArrayAccess",
    "ArrayIterator",
    "ArrayObject",
    "Countable",
    "Iterator",
    "IteratorAggregate",
    "Traversable",
    "JsonSerializable",
    "Stringable",
    "Serializable",
    "SplObjectStorage",
    "SplQueue",
    "SplStack",
    "WeakMap",
    "BackedEnum",
    "UnitEnum",
    "ReflectionClass",
    "ReflectionMethod",
    "ReflectionProperty",
    "ReflectionFunction",
    "ReflectionNamedType",
    "PDO",
    "PDOException",
    "PDOStatement",
    "SimpleXMLElement",
    "DOMDocument",
    "Attribute",
    "ReturnTypeWillChange",
    "SensitiveParameter",
    "Override",
    "AllowDynamicProperties",
];

/// Namespaces of packages every Laravel app has (used when `vendor/` is absent).
const FRAMEWORK_NAMESPACES: &[&str] = &[
    "Illuminate\\",
    "Laravel\\",
    "Symfony\\",
    "Carbon\\",
    "Psr\\",
    "GuzzleHttp\\",
    "Monolog\\",
    "League\\",
    "Doctrine\\",
    "Faker\\",
    "Termwind\\",
    "Nette\\",
    "Ramsey\\",
    "Brick\\",
];

/// Laravel global helpers (vendor functions, not indexed).
const LARAVEL_HELPERS: &[&str] = &[
    "abort",
    "abort_if",
    "abort_unless",
    "action",
    "app",
    "app_path",
    "asset",
    "auth",
    "back",
    "base_path",
    "bcrypt",
    "blank",
    "broadcast",
    "cache",
    "class_basename",
    "class_uses_recursive",
    "collect",
    "config",
    "config_path",
    "cookie",
    "csrf_field",
    "csrf_token",
    "data_fill",
    "data_get",
    "data_set",
    "database_path",
    "dd",
    "decrypt",
    "dispatch",
    "dispatch_sync",
    "dump",
    "e",
    "encrypt",
    "env",
    "event",
    "fake",
    "filled",
    "head",
    "info",
    "lang_path",
    "last",
    "logger",
    "logs",
    "method_field",
    "mix",
    "now",
    "object_get",
    "old",
    "once",
    "optional",
    "policy",
    "precognitive",
    "public_path",
    "redirect",
    "report",
    "report_if",
    "report_unless",
    "request",
    "rescue",
    "resolve",
    "resource_path",
    "response",
    "retry",
    "route",
    "secure_asset",
    "secure_url",
    "session",
    "storage_path",
    "str",
    "tap",
    "throw_if",
    "throw_unless",
    "to_route",
    "today",
    "trait_uses_recursive",
    "trans",
    "trans_choice",
    "__",
    "transform",
    "url",
    "validator",
    "value",
    "view",
    "when",
    "with",
];

/// Common PHP built-in function names and prefixes (not reported as unknown in snippets).
const PHP_FUNCTION_PREFIXES: &[&str] = &[
    "array_",
    "str_",
    "str",
    "mb_",
    "preg_",
    "json_",
    "is_",
    "ctype_",
    "date",
    "time",
    "intval",
    "floatval",
    "boolval",
    "settype",
    "gettype",
    "get_",
    "func_",
    "call_user_func",
    "in_array",
    "count",
    "implode",
    "explode",
    "sprintf",
    "printf",
    "vsprintf",
    "number_format",
    "round",
    "floor",
    "ceil",
    "abs",
    "max",
    "min",
    "sum",
    "range",
    "compact",
    "extract",
    "isset",
    "unset",
    "empty",
    "trim",
    "ltrim",
    "rtrim",
    "ucfirst",
    "lcfirst",
    "ucwords",
    "nl2br",
    "htmlspecialchars",
    "strip_tags",
    "addslashes",
    "md5",
    "sha1",
    "hash",
    "base64_",
    "urlencode",
    "urldecode",
    "rawurl",
    "http_build_query",
    "parse_",
    "file_",
    "fopen",
    "fclose",
    "fwrite",
    "fread",
    "unlink",
    "mkdir",
    "rmdir",
    "dirname",
    "basename",
    "pathinfo",
    "realpath",
    "sleep",
    "usleep",
    "uniqid",
    "random_",
    "rand",
    "mt_rand",
    "sort",
    "rsort",
    "usort",
    "uasort",
    "uksort",
    "ksort",
    "krsort",
    "asort",
    "arsort",
    "shuffle",
    "serialize",
    "unserialize",
    "var_dump",
    "var_export",
    "print_r",
    "error_log",
    "trigger_error",
    "spl_",
    "iterator_",
    "method_exists",
    "property_exists",
    "class_exists",
    "function_exists",
    "interface_exists",
    "enum_exists",
    "define",
    "defined",
    "constant",
    "ob_",
    "header",
    "setcookie",
    "filter_var",
    "checkdate",
    "mktime",
    "strtotime",
    "microtime",
    "hrtime",
    "array",
    "list",
    "current",
    "key",
    "next",
    "reset",
    "end",
    "each",
    "iterator_to_array",
    "sizeof",
    "pow",
    "sqrt",
    "intdiv",
    "fmod",
    "bin2hex",
    "hex2bin",
    "dechex",
    "hexdec",
    "chr",
    "ord",
    "nl_",
    "wordwrap",
    "similar_text",
    "levenshtein",
    "soundex",
    "metaphone",
    "ini_",
    "set_",
    "restore_",
    "version_compare",
    "phpversion",
    "php_",
    "memory_",
    "gc_",
    "debug_",
    "assert",
    "die",
    "exit",
    "echo",
    "print",
    "unset",
    "fn",
];

pub fn verify(store: &Store, project: &Project, p: VerifyParams) -> Result<Value, ToolError> {
    if p.release_check {
        return crate::release::release_check(store, project, p.base.as_deref());
    }
    let types = p
        .type_errors
        .as_deref()
        .map(|out| crate::typecheck::compare(project, out, p.type_errors_save));
    if p.claims.is_none() && p.php_snippet.is_none() {
        if let Some(t) = types {
            return Ok(json!({ "type_errors": t }));
        }
        return Err(ToolError::InvalidParams(
            "pass `claims`, `php_snippet`, `type_errors`, `release_check: true`, or a mix".into(),
        ));
    }
    let detail: Detail = p.detail.into();
    let mut claims = p.claims.unwrap_or_default();
    let mut notes = Vec::new();
    if let Some(snippet) = &p.php_snippet {
        let found = snippet_claims(snippet);
        if found.is_empty() {
            notes.push(
                "no class, method, route or table references found in php_snippet".to_owned(),
            );
        }
        merge(&mut claims, found);
    }

    let v = Verifier {
        store,
        root: &project.root,
    };
    let mut results = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push = |c: Check| {
        if seen.insert((c.kind, c.item.clone())) {
            results.push(c);
        }
    };
    for c in &claims.classes {
        push(v.class(c)?);
    }
    for m in &claims.methods {
        push(v.method(m)?);
    }
    for f in &claims.functions {
        push(v.function(f)?);
    }
    for r in &claims.routes {
        push(v.route(r)?);
    }
    for t in &claims.tables {
        push(v.table(t)?);
    }
    for c in &claims.columns {
        push(v.column(c)?);
    }
    for r in &claims.relations {
        push(v.relation(r)?);
    }
    for c in &claims.components {
        push(v.component(c)?);
    }
    for k in &claims.i18n_keys {
        // `*`: every problem text, one row each (missing in a shipped
        // language, or used but defined nowhere).
        if k == "*" || k == "all" {
            let cats = crate::i18n::catalogs(store.conn(), &project.config.i18n.locales)?;
            let mut any = false;
            for cat in &cats {
                let h = cat.health(200);
                for (locale, keys) in &h.missing_samples {
                    for key in keys {
                        any = true;
                        push(
                            Check::new("i18n_key", key, Status::NotFound)
                                .detail(format!("{}: missing in {locale}", h.source)),
                        );
                    }
                }
                for u in &h.used_missing {
                    let (at, rest) = u.split_once(' ').unwrap_or(("", u.as_str()));
                    let (key, why) = rest.split_once(' ').unwrap_or((rest, ""));
                    any = true;
                    push(
                        Check::new("i18n_key", key, Status::NotFound)
                            .file(Some(at.to_owned()))
                            .detail(format!("{}: {}", h.source, why.trim_matches(['(', ')']))),
                    );
                }
                for u in &h.undefined {
                    let (at, key) = u.split_once(' ').unwrap_or(("", u.as_str()));
                    any = true;
                    push(
                        Check::new("i18n_key", key, Status::NotFound)
                            .file(Some(at.to_owned()))
                            .detail(format!("{}: used but defined in no locale", h.source)),
                    );
                }
            }
            // Validation messages that show a raw field key in a shipped language.
            if let Some((broken, _)) =
                crate::release::field_names_missing(store, &project.config.i18n.locales)?
            {
                for p in broken {
                    let field = p
                        .split('`')
                        .nth(1)
                        .map(|f| format!("validation.attributes.{f}"))
                        .unwrap_or_else(|| "validation.attributes".into());
                    any = true;
                    push(Check::new("i18n_key", &field, Status::NotFound).detail(p));
                }
            }
            if !any {
                push(
                    Check::new("i18n_key", "*", Status::Exists)
                        .detail("no translation problems in the shipped languages".to_owned()),
                );
            }
            continue;
        }
        let (status, detail, suggestions) =
            crate::i18n::verify_key(store.conn(), &project.config.i18n.locales, k)?;
        let st = match status.as_str() {
            "exists" => Status::Exists,
            "unresolved" => Status::Unresolved,
            _ => Status::NotFound,
        };
        let mut c = Check::new("i18n_key", k, st).suggest(suggestions);
        if let Some(d) = detail {
            c = c.detail(d);
        }
        push(c);
    }
    for e in &claims.endpoints {
        let (status, detail, suggestions) = crate::api::verify_endpoint(store, project, e)?;
        let st = match status.as_str() {
            "exists" => Status::Exists,
            "external" => Status::External,
            "unresolved" => Status::Unresolved,
            _ => Status::NotFound,
        };
        let mut c = Check::new("endpoint", e, st).suggest(suggestions);
        if st == Status::Exists {
            c = c.file(detail);
        } else if let Some(d) = detail {
            c = c.detail(d);
        }
        push(c);
    }
    for (model, column) in &claims.model_columns {
        if let Some(check) = v.model_column(model, column)? {
            push(check);
        }
    }
    let known_tables: BTreeSet<String> = v.tables()?.into_iter().collect();
    for qc in &claims.qualified_columns {
        let table = qc.split('.').next().unwrap_or_default();
        if known_tables.contains(table) {
            push(v.column(qc)?);
        }
    }

    let count = |s: Status| results.iter().filter(|r| r.status == s).count();
    let (ok, missing, external, unresolved) = (
        count(Status::Exists),
        count(Status::NotFound),
        count(Status::External),
        count(Status::Unresolved),
    );
    let mut summary = format!("{ok} ok, {missing} not_found");
    if external > 0 {
        summary.push_str(&format!(", {external} external"));
    }
    if unresolved > 0 {
        summary.push_str(&format!(", {unresolved} unresolved"));
    }
    let shown: Vec<Value> = results
        .iter()
        .map(|r| {
            if detail == Detail::Brief && r.status == Status::Exists {
                json!({ "kind": r.kind, "item": r.item, "status": r.status })
            } else {
                json!(r)
            }
        })
        .collect();
    let mut out = json!({
        "summary": summary,
        "all_exist": missing == 0 && unresolved == 0,
        "results": shown,
    });
    if !notes.is_empty() {
        out["notes"] = json!(notes);
    }
    if let Some(t) = types {
        out["type_errors"] = t;
    }
    Ok(out)
}

fn merge(into: &mut Claims, from: Claims) {
    into.classes.extend(from.classes);
    into.methods.extend(from.methods);
    into.functions.extend(from.functions);
    into.routes.extend(from.routes);
    into.tables.extend(from.tables);
    into.columns.extend(from.columns);
    into.relations.extend(from.relations);
    into.components.extend(from.components);
    into.endpoints.extend(from.endpoints);
    into.i18n_keys.extend(from.i18n_keys);
    into.model_columns.extend(from.model_columns);
    into.qualified_columns.extend(from.qualified_columns);
}

struct Verifier<'a> {
    store: &'a Store,
    root: &'a Path,
}

enum ClassHit {
    Found(Box<SymbolRecord>),
    Ambiguous(Vec<String>),
    Missing,
}

impl Verifier<'_> {
    fn find_class(&self, name: &str) -> Result<ClassHit, ToolError> {
        let name = name.trim().trim_start_matches('\\');
        let conn = self.store.conn();
        let exact: Vec<SymbolRecord> = symbols::by_fqn(conn, name)?
            .into_iter()
            .filter(|s| s.kind.is_class_like())
            .collect();
        if let Some(s) = exact.into_iter().next() {
            return Ok(ClassHit::Found(Box::new(s)));
        }
        if name.contains('\\') {
            return Ok(ClassHit::Missing);
        }
        let short: Vec<SymbolRecord> = symbols::all_named(conn, name)?
            .into_iter()
            .filter(|s| s.kind.is_class_like())
            .collect();
        Ok(match short.len() {
            0 => ClassHit::Missing,
            1 => ClassHit::Found(Box::new(short.into_iter().next().expect("one"))),
            _ => ClassHit::Ambiguous(short.into_iter().map(|s| s.fqn).collect()),
        })
    }

    fn suggestions(&self, query: &str, kind: Option<SymbolKind>) -> Result<Vec<String>, ToolError> {
        let r = symbols::search(
            self.store.conn(),
            &SearchInput {
                query: query.to_owned(),
                kind,
                limit: Some(5),
                ..SearchInput::default()
            },
        )?;
        let list = if r.data["found"] == true {
            &r.data["matches"]
        } else {
            &r.data["suggestions"]
        };
        Ok(list
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["fqn"].as_str().map(str::to_owned))
            .take(5)
            .collect())
    }

    /// Is `fqn` a vendor class or a PHP built-in? `Some(reason)` when external.
    fn external_class(&self, fqn: &str) -> Option<String> {
        let fqn = fqn.trim_start_matches('\\');
        if PHP_BUILTIN_CLASSES
            .iter()
            .any(|b| b.eq_ignore_ascii_case(fqn))
        {
            return Some("PHP built-in class".into());
        }
        let composer = self.root.join("vendor/composer");
        if composer.is_dir() {
            if let Some(path) = psr4_path(&composer.join("autoload_psr4.php"), self.root, fqn) {
                return Some(format!("vendor class ({path})"));
            }
            let classmap =
                std::fs::read_to_string(composer.join("autoload_classmap.php")).unwrap_or_default();
            let needle = format!("'{}' =>", fqn.replace('\\', "\\\\"));
            if classmap.contains(&needle) {
                return Some("vendor class (composer classmap)".into());
            }
            return None;
        }
        FRAMEWORK_NAMESPACES
            .iter()
            .any(|ns| fqn.starts_with(ns))
            .then(|| {
                "framework namespace (vendor/ is not installed, so not checked on disk)".into()
            })
    }

    fn class(&self, name: &str) -> Result<Check, ToolError> {
        Ok(match self.find_class(name)? {
            ClassHit::Found(s) => {
                let c = Check::new("class", name, Status::Exists).file(Some(format!("{}:{}", s.file, s.line)));
                if s.fqn.eq_ignore_ascii_case(name.trim_start_matches('\\')) { c } else { c.detail(format!("resolved to {}", s.fqn)) }
            }
            ClassHit::Ambiguous(list) => Check::new("class", name, Status::Unresolved)
                .detail("several classes have this short name; use the FQN")
                .suggest(list),
            ClassHit::Missing => match self.external_class(name) {
                Some(why) => Check::new("class", name, Status::External).detail(why),
                None if self.root.join("vendor/composer").is_dir() || name.contains('\\') => {
                    Check::new("class", name, Status::NotFound)
                        .suggest(self.suggestions(short(name), None)?)
                }
                None => Check::new("class", name, Status::NotFound)
                    .detail("not in project code (vendor/ is not installed, so packages were not checked)")
                    .suggest(self.suggestions(short(name), None)?),
            },
        })
    }

    fn method(&self, item: &str) -> Result<Check, ToolError> {
        let Some((class, method)) = item.split_once("::") else {
            return Ok(
                Check::new("method", item, Status::Unresolved).detail("expected Class::method")
            );
        };
        let method = method.trim_end_matches("()");
        let fqn = match self.find_class(class)? {
            ClassHit::Found(s) => s.fqn,
            ClassHit::Ambiguous(list) => {
                return Ok(Check::new("method", item, Status::Unresolved)
                    .detail("class short name is ambiguous; use the FQN")
                    .suggest(list));
            }
            ClassHit::Missing => {
                return Ok(match self.external_class(class) {
                    Some(why) => Check::new("method", item, Status::External)
                        .detail(format!("class is external: {why}")),
                    None => Check::new("method", item, Status::NotFound)
                        .detail(format!("class {class} does not exist"))
                        .suggest(self.suggestions(short(class), None)?),
                });
            }
        };
        let conn = self.store.conn();
        Ok(match symbols::find_member(conn, &fqn, method)? {
            MemberLookup::Found(m) => Check::new("method", item, Status::Exists)
                .file(Some(format!("{}:{}", m.file, m.line))),
            other => {
                // Eloquent: `Order::paid()` calls `scopePaid`; scopes are methods too.
                let model = models::load(conn)?
                    .into_iter()
                    .find(|m| m.fqn.eq_ignore_ascii_case(&fqn));
                if let Some(scope) = model
                    .as_ref()
                    .and_then(|m| m.scopes.iter().find(|s| s.name == method))
                {
                    return Ok(Check::new("method", item, Status::Exists)
                        .file(Some(format!(
                            "{}:{}",
                            model.as_ref().map_or("", |m| m.file.as_str()),
                            scope.line
                        )))
                        .detail(format!("Eloquent scope {}()", scope.method)));
                }
                match other {
                    MemberLookup::Unverifiable(outside) => {
                        match outside.iter().find(|a| self.external_class(a).is_some()) {
                            Some(vendor) => Check::new("method", item, Status::External).detail(
                                format!("not in project code; may be inherited from {vendor}"),
                            ),
                            None => Check::new("method", item, Status::Unresolved).detail(format!(
                                "not in project code; ancestors {} are not indexed",
                                outside.join(", ")
                            )),
                        }
                    }
                    _ => {
                        let members: Vec<String> = symbols::members(conn, &fqn)?
                            .into_iter()
                            .filter(|m| m.kind == SymbolKind::Method)
                            .map(|m| m.name)
                            .collect();
                        Check::new("method", item, Status::NotFound)
                            .detail(format!("{fqn} has no method {method}()"))
                            .suggest(
                                closest(method, &members)
                                    .into_iter()
                                    .map(|m| format!("{fqn}::{m}"))
                                    .collect(),
                            )
                    }
                }
            }
        })
    }

    fn function(&self, name: &str) -> Result<Check, ToolError> {
        let name = name.trim().trim_start_matches('\\').trim_end_matches("()");
        let conn = self.store.conn();
        let hit = symbols::by_fqn(conn, name)?
            .into_iter()
            .chain(symbols::all_named(conn, name)?)
            .find(|s| s.kind == SymbolKind::Function);
        if let Some(f) = hit {
            return Ok(Check::new("function", name, Status::Exists)
                .file(Some(format!("{}:{}", f.file, f.line))));
        }
        if LARAVEL_HELPERS.contains(&name) {
            return Ok(Check::new("function", name, Status::External).detail("Laravel helper"));
        }
        if is_php_function(name) {
            return Ok(
                Check::new("function", name, Status::External).detail("looks like a PHP built-in")
            );
        }
        Ok(Check::new("function", name, Status::NotFound)
            .detail("not a project function (and not a known Laravel helper)")
            .suggest(self.suggestions(name, Some(SymbolKind::Function))?))
    }

    fn route(&self, item: &str) -> Result<Check, ToolError> {
        let all = routes::all(self.store.conn())?;
        let item = item.trim();
        // `POST api/orders/{order}` form.
        if let Some((method, uri)) = item.split_once(' ')
            && method.chars().all(|c| c.is_ascii_alphabetic())
            && method.len() <= 7
        {
            let uri = uri.trim().trim_matches('/');
            let uri = if uri.is_empty() { "/" } else { uri };
            let hit = all.iter().find(|r| {
                r.methods.iter().any(|m| m.eq_ignore_ascii_case(method)) && uri_matches(&r.uri, uri)
            });
            return Ok(match hit {
                Some(r) => Check::new("route", item, Status::Exists).file(r.location()),
                None => Check::new("route", item, Status::NotFound).suggest(closest(
                    uri,
                    &all.iter()
                        .map(|r| format!("{} {}", r.methods_str(), r.uri))
                        .collect::<Vec<_>>(),
                )),
            });
        }
        let hit = all.iter().find(|r| r.name.as_deref() == Some(item));
        Ok(match hit {
            Some(r) => Check::new("route", item, Status::Exists)
                .file(r.location())
                .detail(format!("{} {}", r.methods_str(), r.uri)),
            None if all.is_empty() => {
                Check::new("route", item, Status::Unresolved).detail("no routes are indexed")
            }
            None => Check::new("route", item, Status::NotFound).suggest(closest(
                item,
                &all.iter()
                    .filter_map(|r| r.name.clone())
                    .collect::<Vec<_>>(),
            )),
        })
    }

    fn tables(&self) -> Result<Vec<String>, ToolError> {
        let v = schema::describe(self.store.conn(), None, Detail::Brief)?;
        Ok(v["tables"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["table"].as_str().map(str::to_owned))
            .collect())
    }

    fn component(&self, name: &str) -> Result<Check, ToolError> {
        use codemap_vue::component::ComponentCheck;
        let name = name.trim();
        Ok(
            match codemap_vue::component::check(self.store, self.root, name)? {
                ComponentCheck::Exists(path) => {
                    Check::new("component", name, Status::Exists).file(Some(path))
                }
                ComponentCheck::External(pkg) => {
                    Check::new("component", name, Status::External).detail(format!("from {pkg}"))
                }
                ComponentCheck::NotFound(suggestions) => Check::new(
                    "component",
                    name,
                    Status::NotFound,
                )
                .detail(
                    "no project component, registration or known library component has this name",
                )
                .suggest(suggestions),
                ComponentCheck::NoFrontend => Check::new("component", name, Status::Unresolved)
                    .detail("no Vue / Nuxt app detected in this project"),
            },
        )
    }

    fn table(&self, name: &str) -> Result<Check, ToolError> {
        let name = name.trim();
        let tables = self.tables()?;
        if tables.is_empty() {
            return Ok(Check::new("table", name, Status::Unresolved)
                .detail("no migrations or schema dump indexed"));
        }
        let v = schema::describe(self.store.conn(), Some(name), Detail::Full)?;
        Ok(if v["found"] == true {
            Check::new("table", name, Status::Exists)
                .file(v["created_in"].as_str().map(str::to_owned))
        } else {
            Check::new("table", name, Status::NotFound)
                .detail(v["hint"].as_str().unwrap_or_default())
                .suggest(closest(name, &tables))
        })
    }

    fn column(&self, item: &str) -> Result<Check, ToolError> {
        let Some((table, column)) = item.trim().split_once('.') else {
            return Ok(
                Check::new("column", item, Status::Unresolved).detail("expected table.column")
            );
        };
        let t = self.table(table)?;
        if t.status != Status::Exists {
            return Ok(Check {
                kind: "column",
                item: item.to_owned(),
                detail: Some(format!(
                    "table {table}: {}",
                    t.detail.unwrap_or_else(|| "not found".into())
                )),
                ..t
            });
        }
        let cols = schema::load_columns(self.store.conn(), table)?;
        Ok(match cols.iter().find(|c| c.name == column) {
            Some(c) => Check::new("column", item, Status::Exists)
                .file(c.created_in.clone())
                .detail(format!(
                    "{}{}",
                    c.r#type,
                    if c.nullable { ", nullable" } else { "" }
                )),
            None => Check::new("column", item, Status::NotFound).suggest(
                closest(
                    column,
                    &cols.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                )
                .into_iter()
                .map(|c| format!("{table}.{c}"))
                .collect(),
            ),
        })
    }

    /// A column used on a model's query, e.g. `Merchant::whereNotNull('popular_until')`.
    fn model_column(&self, model: &str, column: &str) -> Result<Option<Check>, ToolError> {
        let ClassHit::Found(class) = self.find_class(model)? else {
            return Ok(None); // the class itself is reported separately
        };
        let Some(m) = models::load(self.store.conn())?
            .into_iter()
            .find(|m| m.fqn.eq_ignore_ascii_case(&class.fqn))
        else {
            return Ok(None); // not an Eloquent model
        };
        let mut check = self.column(&format!("{}.{column}", m.table))?;
        check.item = format!("{}.{column}", short(&m.fqn));
        let via = format!("column of table {} (model {})", m.table, m.fqn);
        check.detail = Some(match check.detail {
            Some(d) => format!("{via}: {d}"),
            None => via,
        });
        Ok(Some(check))
    }

    fn relation(&self, item: &str) -> Result<Check, ToolError> {
        let Some((class, rel)) = item.split_once("::") else {
            return Ok(
                Check::new("relation", item, Status::Unresolved).detail("expected Model::relation")
            );
        };
        let fqn = match self.find_class(class)? {
            ClassHit::Found(s) => s.fqn,
            ClassHit::Ambiguous(list) => {
                return Ok(Check::new("relation", item, Status::Unresolved).suggest(list));
            }
            ClassHit::Missing => {
                return Ok(Check::new("relation", item, Status::NotFound)
                    .detail(format!("model {class} does not exist"))
                    .suggest(self.suggestions(short(class), Some(SymbolKind::Class))?));
            }
        };
        let Some(model) = models::load(self.store.conn())?
            .into_iter()
            .find(|m| m.fqn.eq_ignore_ascii_case(&fqn))
        else {
            return Ok(Check::new("relation", item, Status::NotFound)
                .detail(format!("{fqn} is not an Eloquent model")));
        };
        Ok(match model.relations.iter().find(|r| r.method == rel) {
            Some(r) => Check::new("relation", item, Status::Exists)
                .file(Some(format!("{}:{}", model.file, r.line)))
                .detail(format!(
                    "{} {}",
                    r.r#type,
                    r.related.as_deref().unwrap_or("?")
                )),
            None => match symbols::find_member(self.store.conn(), &fqn, rel)? {
                MemberLookup::Found(m) => Check::new("relation", item, Status::Unresolved)
                    .file(Some(format!("{}:{}", m.file, m.line)))
                    .detail("method exists but is not a recognisable relation"),
                _ => Check::new("relation", item, Status::NotFound).suggest(
                    closest(
                        rel,
                        &model
                            .relations
                            .iter()
                            .map(|r| r.method.clone())
                            .collect::<Vec<_>>(),
                    )
                    .into_iter()
                    .map(|r| format!("{fqn}::{r}"))
                    .collect(),
                ),
            },
        })
    }
}

fn short(name: &str) -> &str {
    name.trim_start_matches('\\')
        .rsplit('\\')
        .next()
        .unwrap_or(name)
}

fn is_php_function(name: &str) -> bool {
    !name.contains('\\')
        && PHP_FUNCTION_PREFIXES
            .iter()
            .any(|p| name == *p || (p.ends_with('_') && name.starts_with(p)))
}

/// Best matches by Jaro-Winkler (≥ 0.75), at most 5.
fn closest(needle: &str, haystack: &[String]) -> Vec<String> {
    let n = needle.to_lowercase();
    let mut scored: Vec<(f64, &String)> = haystack
        .iter()
        .map(|h| {
            let l = h.to_lowercase();
            // `order_status` for `status`, `user_id` for `user`: one name
            // with a word added or dropped.
            let part = n.ends_with(&format!("_{l}"))
                || n.starts_with(&format!("{l}_"))
                || l.ends_with(&format!("_{n}"))
                || l.starts_with(&format!("{n}_"));
            let score = strsim::jaro_winkler(&n, &l);
            (if part { score.max(0.85) } else { score }, h)
        })
        .filter(|(s, _)| *s >= 0.75)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut out: Vec<String> = Vec::new();
    for (_, h) in scored {
        if !out.contains(h) {
            out.push(h.clone());
        }
        if out.len() == 5 {
            break;
        }
    }
    out
}

/// `orders/{order}` matches `orders/5` and `orders/{id}`.
fn uri_matches(pattern: &str, uri: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let u: Vec<&str> = uri.split('/').collect();
    p.len() == u.len()
        && p.iter()
            .zip(&u)
            .all(|(a, b)| a == b || (a.starts_with('{') && a.ends_with('}')))
}

/// Resolves `vendor/composer/autoload_psr4.php` prefixes to a file path.
fn psr4_path(autoload: &Path, root: &Path, fqn: &str) -> Option<String> {
    let text = std::fs::read_to_string(autoload).ok()?;
    let mut best: Option<String> = None;
    for line in text.lines() {
        // 'Illuminate\\' => array($vendorDir . '/laravel/framework/src/Illuminate'),
        let line = line.trim();
        let Some(rest) = line.strip_prefix('\'') else {
            continue;
        };
        let Some((prefix, dirs)) = rest.split_once("' =>") else {
            continue;
        };
        let prefix = prefix.replace("\\\\", "\\");
        if !fqn.starts_with(&prefix) {
            continue;
        }
        let relative = fqn[prefix.len()..].replace('\\', "/") + ".php";
        for dir in dirs.split(',') {
            let Some(start) = dir.find('\'') else {
                continue;
            };
            let Some(end) = dir[start + 1..].find('\'') else {
                continue;
            };
            let sub = &dir[start + 1..start + 1 + end];
            let base = if dir.contains("$vendorDir") {
                "vendor"
            } else if dir.contains("$baseDir") {
                ""
            } else {
                continue;
            };
            let path = format!("{base}{sub}/{relative}");
            let path = path.trim_start_matches('/').to_owned();
            if root.join(&path).is_file() && best.as_ref().is_none_or(|b| b.len() < path.len()) {
                best = Some(path);
            }
        }
    }
    best
}

// ---------------------------------------------------------------- snippet mode

/// References found in PHP code: classes (`use`, `new X`, `X::`, type hints,
/// `instanceof`, `catch`, `extends`, `implements`), static calls,
/// `route('name')`, and tables in `DB::table('t')` / `Schema::table('t')`.
pub fn snippet_claims(code: &str) -> Claims {
    let src = if code.trim_start().starts_with("<?php") {
        code.to_owned()
    } else {
        format!("<?php\n{code}")
    };
    let src = src.as_bytes();
    let mut claims = Claims::default();
    let Some(tree) = codemap_php::parse::parse(src) else {
        return claims;
    };
    let mut scope = Scope::default();
    // Without a `namespace`, unqualified names stay short and are resolved by name.
    let has_namespace = src.windows(10).any(|w| w == b"namespace ");
    collect(
        tree.root_node(),
        src,
        &mut scope,
        has_namespace,
        &mut claims,
    );
    for list in [
        &mut claims.classes,
        &mut claims.methods,
        &mut claims.functions,
        &mut claims.routes,
        &mut claims.tables,
    ] {
        let mut seen = BTreeSet::new();
        list.retain(|x| seen.insert(x.clone()));
    }
    claims
}

fn class_name(raw: &str, scope: &Scope, has_namespace: bool) -> Option<String> {
    let raw = raw.trim();
    let lower = raw.to_ascii_lowercase();
    if matches!(lower.as_str(), "self" | "static" | "parent")
        || codemap_php::names::is_builtin_type(&lower)
    {
        return None;
    }
    if !has_namespace && !raw.contains('\\') && scope.resolve_class(raw).as_deref() == Some(raw) {
        // Unqualified and not imported: keep the short name.
        return Some(raw.to_owned());
    }
    scope.resolve_class(raw)
}

fn first_string_arg(call: Node<'_>, src: &[u8]) -> Option<String> {
    let args = call.child_by_field_name("arguments")?;
    let arg = named_children(args)
        .into_iter()
        .find(|a| a.kind() == "argument")?;
    let value = named_children(arg).into_iter().last()?;
    match value.kind() {
        "string" | "encapsed_string"
            if named_children(value)
                .iter()
                .all(|c| matches!(c.kind(), "string_content" | "escape_sequence")) =>
        {
            let t = text(value, src);
            Some(t[1..t.len() - 1].to_owned())
        }
        _ => None,
    }
}

fn collect(node: Node<'_>, src: &[u8], scope: &mut Scope, ns: bool, claims: &mut Claims) {
    match node.kind() {
        "namespace_definition" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text(n, src))
                .unwrap_or("");
            scope.set_namespace(name);
        }
        "namespace_use_declaration" => {
            apply_use_declaration(node, src, scope);
            // Imported classes are claims too (functions/consts are skipped).
            if node.child_by_field_name("type").is_none() {
                let mut stack = vec![node];
                while let Some(n) = stack.pop() {
                    if n.kind() == "namespace_use_clause" {
                        let alias = n.child_by_field_name("alias").map(|a| a.id());
                        if let Some(name) = named_children(n).into_iter().find(|c| {
                            matches!(c.kind(), "name" | "qualified_name") && Some(c.id()) != alias
                        }) {
                            let prefix = named_children(node)
                                .into_iter()
                                .find(|c| c.kind() == "namespace_name")
                                .map(|p| format!("{}\\", text(p, src).trim_start_matches('\\')))
                                .unwrap_or_default();
                            claims.classes.push(format!(
                                "{prefix}{}",
                                text(name, src).trim_start_matches('\\')
                            ));
                        }
                    } else {
                        stack.extend(named_children(n));
                    }
                }
            }
            return;
        }
        "object_creation_expression" => {
            if let Some(c) = named_children(node)
                .into_iter()
                .find(|c| matches!(c.kind(), "name" | "qualified_name"))
                && let Some(fqn) = class_name(text(c, src), scope, ns)
            {
                claims.classes.push(fqn);
            }
        }
        "named_type" | "base_clause" | "class_interface_clause" | "catch_clause" => {
            for c in named_children(node) {
                if matches!(c.kind(), "name" | "qualified_name")
                    && let Some(fqn) = class_name(text(c, src), scope, ns)
                {
                    claims.classes.push(fqn);
                }
            }
        }
        "binary_expression"
            if node.child_by_field_name("operator").map(|o| text(o, src)) == Some("instanceof") =>
        {
            if let Some(r) = node.child_by_field_name("right")
                && matches!(r.kind(), "name" | "qualified_name")
                && let Some(fqn) = class_name(text(r, src), scope, ns)
            {
                claims.classes.push(fqn);
            }
        }
        "class_constant_access_expression" => {
            if let Some(c) = named_children(node).into_iter().next()
                && matches!(c.kind(), "name" | "qualified_name")
                && let Some(fqn) = class_name(text(c, src), scope, ns)
            {
                claims.classes.push(fqn);
            }
        }
        "scoped_call_expression" => {
            let scope_node = node.child_by_field_name("scope");
            let method = node.child_by_field_name("name").map(|n| text(n, src));
            if let (Some(sn), Some(method)) = (scope_node, method)
                && matches!(sn.kind(), "name" | "qualified_name")
                && let Some(fqn) = class_name(text(sn, src), scope, ns)
            {
                let short_name = fqn.rsplit('\\').next().unwrap_or(&fqn);
                let is_db = short_name == "DB" || fqn == "Illuminate\\Support\\Facades\\DB";
                let is_schema =
                    short_name == "Schema" || fqn == "Illuminate\\Support\\Facades\\Schema";
                let names_table = is_db && method == "table"
                    || is_schema
                        && matches!(method, "table" | "drop" | "dropIfExists" | "hasTable");
                if names_table && let Some(t) = first_string_arg(node, src) {
                    claims.tables.push(t);
                }
                if !is_db && !is_schema {
                    claims.classes.push(fqn.clone());
                    claims.methods.push(format!("{fqn}::{method}"));
                }
            }
        }
        "function_call_expression" => {
            if let Some(f) = node.child_by_field_name("function")
                && matches!(f.kind(), "name" | "qualified_name")
            {
                let name = text(f, src).trim_start_matches('\\');
                if matches!(name, "route" | "to_route")
                    && let Some(r) = first_string_arg(node, src)
                {
                    claims.routes.push(r);
                }
                if !is_php_function(name) && !LARAVEL_HELPERS.contains(&name) {
                    claims.functions.push(
                        scope
                            .resolve_function(name)
                            .trim_start_matches(&format!("{}\\", scope.namespace()))
                            .to_owned(),
                    );
                }
            }
        }
        _ => {}
    }
    if is_chain_top(node) {
        analyze_chain(node, src, scope, ns, claims);
    }
    // redirect()->route('name') / URL::route('name')
    if node.kind() == "member_call_expression"
        && node.child_by_field_name("name").map(|n| text(n, src)) == Some("route")
        && let Some(r) = first_string_arg(node, src)
    {
        claims.routes.push(r);
    }
    for c in named_children(node) {
        collect(c, src, scope, ns, claims);
    }
}

/// Query-builder methods whose arguments name columns.
const COLUMN_METHODS: &[&str] = &[
    "where",
    "orWhere",
    "whereNot",
    "orWhereNot",
    "whereIn",
    "whereNotIn",
    "orWhereIn",
    "orWhereNotIn",
    "whereNull",
    "whereNotNull",
    "orWhereNull",
    "orWhereNotNull",
    "whereBetween",
    "whereNotBetween",
    "orWhereBetween",
    "orWhereNotBetween",
    "whereDate",
    "whereMonth",
    "whereDay",
    "whereYear",
    "whereTime",
    "whereLike",
    "whereNotLike",
    "orWhereLike",
    "whereJsonContains",
    "whereJsonLength",
    "orderBy",
    "orderByDesc",
    "latest",
    "oldest",
    "groupBy",
    "pluck",
    "value",
    "select",
    "addSelect",
    "increment",
    "decrement",
    "firstWhere",
    "sum",
    "avg",
    "min",
    "max",
    "update",
    "whereColumn",
    "orWhereColumn",
];

/// Methods taking raw SQL as the first argument.
const RAW_METHODS: &[&str] = &[
    "orderByRaw",
    "whereRaw",
    "orWhereRaw",
    "selectRaw",
    "havingRaw",
    "orHavingRaw",
    "groupByRaw",
];

/// A call chain is analysed once, at its outermost call.
fn is_chain_top(node: Node<'_>) -> bool {
    if !matches!(
        node.kind(),
        "member_call_expression" | "scoped_call_expression" | "nullsafe_member_call_expression"
    ) {
        return false;
    }
    match node.parent() {
        Some(p)
            if matches!(
                p.kind(),
                "member_call_expression" | "nullsafe_member_call_expression"
            ) =>
        {
            p.child_by_field_name("object").map(|o| o.id()) != Some(node.id())
        }
        _ => true,
    }
}

enum ChainRoot {
    Model(String),
    Table(String),
    Unknown,
}

fn string_value(n: Node<'_>, src: &[u8]) -> Option<String> {
    match n.kind() {
        "string" | "encapsed_string"
            if named_children(n)
                .iter()
                .all(|c| matches!(c.kind(), "string_content" | "escape_sequence")) =>
        {
            let t = text(n, src);
            Some(t[1..t.len() - 1].to_owned())
        }
        "nowdoc" | "heredoc" => {
            let t = text(n, src);
            let (_, rest) = t.split_once('\n')?;
            let (body, _) = rest.rsplit_once('\n')?;
            (n.kind() == "nowdoc" || !body.contains('$')).then(|| body.to_owned())
        }
        _ => None,
    }
}

fn call_args(n: Node<'_>) -> Vec<Node<'_>> {
    n.child_by_field_name("arguments")
        .map(|a| {
            named_children(a)
                .into_iter()
                .filter(|x| x.kind() == "argument")
                .filter_map(|x| named_children(x).into_iter().last())
                .collect()
        })
        .unwrap_or_default()
}

/// `table.column` pairs in raw SQL (quotes removed). Unqualified names are
/// not reported: without a parser they cannot be told from aliases and functions.
fn qualified_identifiers(sql: &str) -> Vec<String> {
    let clean: String = sql.chars().filter(|c| *c != '"' && *c != '`').collect();
    let bytes = clean.as_bytes();
    let is_start = |c: u8| c.is_ascii_alphabetic() || c == b'_';
    let is_part = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if is_start(bytes[i])
            && (i == 0 || !is_part(bytes[i - 1]) && bytes[i - 1] != b'.' && bytes[i - 1] != b':')
        {
            let start = i;
            while i < bytes.len() && is_part(bytes[i]) {
                i += 1;
            }
            if i + 1 < bytes.len() && bytes[i] == b'.' && is_start(bytes[i + 1]) {
                let dot = i;
                i += 1;
                while i < bytes.len() && is_part(bytes[i]) {
                    i += 1;
                }
                // Skip function calls like schema.fn( and json paths.
                if i < bytes.len() && bytes[i] == b'(' {
                    continue;
                }
                out.push(format!("{}.{}", &clean[start..dot], &clean[dot + 1..i]));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// A column name as written in a query argument, or None when it is an expression.
fn column_name(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let base = match lower.find(" as ") {
        Some(i) => &raw[..i],
        None => raw,
    };
    let base = base.split("->").next().unwrap_or(base).trim();
    (!base.is_empty()
        && base != "*"
        && !base.ends_with(".*")
        && base
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.'))
    .then(|| base.to_owned())
}

fn analyze_chain(top: Node<'_>, src: &[u8], scope: &Scope, ns: bool, claims: &mut Claims) {
    // Collect calls from the outermost inwards, then find the root.
    let mut calls = Vec::new();
    let mut node = top;
    let root = loop {
        match node.kind() {
            "member_call_expression" | "nullsafe_member_call_expression" => {
                calls.push(node);
                match node.child_by_field_name("object") {
                    Some(o) => node = o,
                    None => break ChainRoot::Unknown,
                }
            }
            "scoped_call_expression" => {
                calls.push(node);
                let Some(class) = node
                    .child_by_field_name("scope")
                    .filter(|s| matches!(s.kind(), "name" | "qualified_name"))
                    .and_then(|s| class_name(text(s, src), scope, ns))
                else {
                    break ChainRoot::Unknown;
                };
                let short_name = class.rsplit('\\').next().unwrap_or(&class).to_owned();
                let method = node
                    .child_by_field_name("name")
                    .map(|n| text(n, src))
                    .unwrap_or("");
                if short_name == "DB" {
                    match method {
                        "table" => {
                            break call_args(node)
                                .first()
                                .and_then(|a| string_value(*a, src))
                                .map_or(ChainRoot::Unknown, ChainRoot::Table);
                        }
                        "raw" | "select" | "selectOne" | "statement" | "unprepared" | "update"
                        | "delete" | "insert" => {
                            if let Some(sql) =
                                call_args(node).first().and_then(|a| string_value(*a, src))
                            {
                                claims.qualified_columns.extend(qualified_identifiers(&sql));
                            }
                            break ChainRoot::Unknown;
                        }
                        _ => break ChainRoot::Unknown,
                    }
                }
                break if short_name == "Schema" {
                    ChainRoot::Unknown
                } else {
                    ChainRoot::Model(class)
                };
            }
            _ => break ChainRoot::Unknown,
        }
    };
    calls.reverse();
    let add = |raw: &str, claims: &mut Claims| {
        let Some(col) = column_name(raw) else { return };
        if col.contains('.') {
            claims.qualified_columns.push(col);
            return;
        }
        match &root {
            ChainRoot::Model(m) => claims.model_columns.push((m.clone(), col)),
            ChainRoot::Table(t) => claims.columns.push(format!("{t}.{col}")),
            ChainRoot::Unknown => {}
        }
    };
    for call in calls {
        let name = call
            .child_by_field_name("name")
            .map(|n| text(n, src))
            .unwrap_or("");
        let args = call_args(call);
        if RAW_METHODS.contains(&name) {
            if let Some(sql) = args.first().and_then(|a| string_value(*a, src)) {
                claims.qualified_columns.extend(qualified_identifiers(&sql));
            }
            continue;
        }
        if !COLUMN_METHODS.contains(&name) {
            continue;
        }
        let all_args = matches!(name, "select" | "addSelect" | "groupBy" | "pluck");
        for (i, arg) in args.iter().enumerate() {
            if i > 0 && !all_args && !(name.ends_with("Column") && i == args.len() - 1) {
                break;
            }
            match arg.kind() {
                "array_creation_expression" => {
                    for el in named_children(*arg)
                        .into_iter()
                        .filter(|e| e.kind() == "array_element_initializer")
                    {
                        let parts = named_children(el);
                        // ['col' => value] uses the key; ['a', 'b'] the values.
                        let target = if parts.len() == 2 {
                            parts.first()
                        } else {
                            parts.last()
                        };
                        if let Some(v) = target.and_then(|t| string_value(*t, src)) {
                            add(&v, claims);
                        }
                    }
                }
                _ => {
                    if let Some(v) = string_value(*arg, src) {
                        add(&v, claims);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_columns_and_raw_sql() {
        let c = snippet_claims(
            r#"<?php
use App\Models\Merchant;
use Illuminate\Support\Facades\DB;
Merchant::query()
    ->whereNotNull('popular_untill')
    ->where(['status' => 'active'])
    ->orderByRaw('merchants.popular_start desc, outlets.name')
    ->select(['id', 'name as label', 'meta->colour', DB::raw('count(*) as n')])
    ->pluck('name');
DB::table('orders')->where('totl', '>', 5)->orderBy('orders.created_at');
$query->where('anything', 1);
"#,
        );
        let mc: Vec<String> = c
            .model_columns
            .iter()
            .map(|(m, col)| format!("{m}.{col}"))
            .collect();
        assert_eq!(
            mc,
            [
                "App\\Models\\Merchant.popular_untill",
                "App\\Models\\Merchant.status",
                "App\\Models\\Merchant.id",
                "App\\Models\\Merchant.name",
                "App\\Models\\Merchant.meta",
                "App\\Models\\Merchant.name"
            ]
        );
        assert_eq!(c.columns, ["orders.totl"]);
        assert!(
            c.qualified_columns
                .contains(&"merchants.popular_start".to_owned())
        );
        assert!(c.qualified_columns.contains(&"outlets.name".to_owned()));
        assert!(
            c.qualified_columns
                .contains(&"orders.created_at".to_owned())
        );
        assert!(!c.model_columns.iter().any(|(_, col)| col == "anything"));
    }

    #[test]
    fn raw_identifier_scanner() {
        assert_eq!(
            qualified_identifiers(
                "select m.id, \"merchants\".\"name\", pg_catalog.now(), 1.5 from merchants m"
            ),
            ["m.id", "merchants.name"]
        );
    }

    #[test]
    fn snippet_references() {
        let c = snippet_claims(
            r#"
namespace App\Http\Controllers;

use App\Models\Order;
use App\Services\{OrderService, Billing as Bill};
use Illuminate\Support\Facades\DB;

class PayController extends Controller implements \JsonSerializable
{
    public function __invoke(StorePayRequest $request, OrderService $orders): RedirectResponse
    {
        $order = Order::findOrFail($request->id);
        $total = DB::table('order_items')->sum('price');
        Order::paid()->get();
        new Bill();
        if ($order instanceof Payable) {}
        try { money(5); } catch (PaymentFailed $e) {}
        return to_route('orders.show', $order);
    }
}
"#,
        );
        for class in [
            "App\\Models\\Order",
            "App\\Services\\OrderService",
            "App\\Services\\Billing",
            "App\\Http\\Controllers\\Controller",
            "JsonSerializable",
            "App\\Http\\Controllers\\StorePayRequest",
            "App\\Http\\Controllers\\RedirectResponse",
            "App\\Http\\Controllers\\Payable",
            "App\\Http\\Controllers\\PaymentFailed",
        ] {
            assert!(
                c.classes.contains(&class.to_owned()),
                "{class} missing: {:?}",
                c.classes
            );
        }
        assert!(
            c.methods
                .contains(&"App\\Models\\Order::findOrFail".to_owned())
        );
        assert!(c.methods.contains(&"App\\Models\\Order::paid".to_owned()));
        assert_eq!(c.tables, ["order_items"]);
        assert_eq!(c.routes, ["orders.show"]);
        assert!(c.functions.contains(&"money".to_owned()));
        assert!(!c.functions.iter().any(|f| f == "to_route"));
    }

    #[test]
    fn snippet_without_namespace_keeps_short_names() {
        let c = snippet_claims("$o = Order::create(['x' => 1]); echo route('home');");
        assert_eq!(c.classes, ["Order"]);
        assert_eq!(c.methods, ["Order::create"]);
        assert_eq!(c.routes, ["home"]);
    }

    #[test]
    fn helpers() {
        assert!(uri_matches("orders/{order}", "orders/5"));
        assert!(!uri_matches("orders/{order}", "orders"));
        assert!(
            is_php_function("array_map") && is_php_function("count") && !is_php_function("money")
        );
        assert_eq!(
            closest("custmer_id", &["customer_id".into(), "status".into()]),
            ["customer_id"]
        );
    }
}

#[cfg(test)]
mod closest_tests {
    #[test]
    fn a_word_added_or_dropped_is_close() {
        let cols: Vec<String> = ["id", "status", "user_id", "total"]
            .map(String::from)
            .to_vec();
        assert_eq!(super::closest("order_status", &cols)[0], "status");
        assert_eq!(super::closest("user", &cols)[0], "user_id");
    }
}
