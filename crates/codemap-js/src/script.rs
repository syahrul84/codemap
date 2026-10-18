//! Static analysis of one JS/TS module or Vue `<script>` block with oxc:
//! declarations, imports/exports, Options API objects, `<script setup>`
//! macros, composable calls, component registrations, Pinia and Vuex.
//! Nothing is executed; anything dynamic is skipped rather than guessed.

use std::collections::{HashMap, HashSet};

use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::{GetSpan, SourceType, Span};

use crate::facts::*;

/// What kind of code this is; macros only count in `<script setup>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    /// A `.js` / `.ts` file.
    Module,
    /// A Vue `<script>` block.
    VueScript,
    /// A Vue `<script setup>` block.
    VueSetup,
}

/// Analyzes `src`. `lang` is a file extension or `<script lang>` value;
/// `first_line` is the line of `src`'s first byte.
pub fn analyze(src: &str, lang: &str, first_line: u32, kind: ScriptKind) -> ScriptFacts {
    let allocator = Allocator::default();
    let source_type = source_type_for(lang);
    let ret = Parser::new(&allocator, src, source_type)
        .with_options(ParseOptions {
            preserve_parens: false,
            ..ParseOptions::default()
        })
        .parse();
    let mut a = Analyzer::new(src, first_line, kind);
    for d in ret.diagnostics.iter().take(3) {
        let line = d.labels.first().map_or(first_line, |l| a.line(l.offset()));
        a.facts.errors.push(NamedLine {
            name: d.message.to_string(),
            line,
        });
    }
    if ret.panicked_or_fatal() {
        return a.facts;
    }
    let program: &Program<'_> = allocator.alloc(ret.program);
    a.program(program);
    a.facts
}

pub fn source_type_for(lang: &str) -> SourceType {
    match lang.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "ts" | "mts" | "cts" => SourceType::ts(),
        "tsx" => SourceType::tsx(),
        "jsx" => SourceType::jsx(),
        "cjs" => SourceType::cjs(),
        // Plain JS may still hold JSX in Vue projects; module syntax is the norm.
        _ => SourceType::mjs().with_jsx(true),
    }
}

trait ReturnExt {
    fn panicked_or_fatal(&self) -> bool;
}

impl ReturnExt for oxc_parser::ParserReturn<'_> {
    fn panicked_or_fatal(&self) -> bool {
        self.fatal_error
    }
}

struct Analyzer<'s, 'a> {
    src: &'s str,
    first_line: u32,
    kind: ScriptKind,
    newlines: Vec<u32>,
    facts: ScriptFacts,
    /// Top-level `const x = <expr>` initialisers, to resolve shorthand.
    consts: HashMap<&'a str, &'a Expression<'a>>,
    /// Top-level function declarations (route helpers).
    functions: HashMap<&'a str, &'a Function<'a>>,
    /// Top-level TS interfaces / type aliases (for `defineProps<Props>()`).
    types: HashMap<&'a str, TypeDef<'a>>,
    /// Call span start → binding name (`const useCart = defineStore(…)`).
    call_bindings: HashMap<u32, String>,
    /// Destructured defaults of `const { a = 1 } = defineProps()`.
    prop_defaults: HashMap<u32, Vec<(String, String)>>,
    /// Identifiers bound to `defineEmits()` (usually `emit`).
    emit_fns: HashSet<String>,
    handled: HashSet<u32>,
    comments: Vec<(u32, u32, bool)>,
    saw_component_call: bool,
    /// Enclosing function bodies while visiting: `(start, end)`.
    scopes: Vec<(u32, u32)>,
    /// API calls found, with the call span and the scope they sit in.
    calls: Vec<(ApiCall, u32, u32, (u32, u32))>,
    /// Call span start → names bound to its result, and their scope.
    /// Names bound to a call's result.
    result_bindings: Vec<ResultBinding>,
    /// Function bodies being visited (arrows by their own span), and the
    /// `.then(cb)` callbacks among them: a return inside a callback returns
    /// from the enclosing function's promise chain, not from a new function.
    fn_stack: Vec<(u32, u32)>,
    then_cbs: HashSet<(u32, u32)>,
    /// Small URL builders anywhere in the file
    /// (`const url = (id) => \`/outlets/${id}\``): name → URL parts.
    url_helpers: HashMap<String, UrlHelper>,
    /// Functions that may wrap an HTTP call (object methods such as
    /// `api.get`, and function-valued consts).
    wrappers: Vec<WrapperFn>,
    /// Member chains rooted at an identifier: `(root, path, start)`.
    chains: Vec<(String, Vec<String>, u32)>,
    /// `target = root.path…`: `(target, root, path, start)`.
    assigns: Vec<(String, String, Vec<String>, u32)>,
    /// Calls to plain functions: `(name, start, end, scope)`.
    fn_calls: Vec<(String, u32, u32, (u32, u32))>,
    /// Return values built from a binding: `(returned path, root, path, start)`.
    returns: Vec<(String, String, Vec<String>, u32)>,
    /// Calls (span start, end) whose result is returned as is (`return api.get(…)`).
    direct_returns: HashSet<(u32, u32)>,
    /// Names bound to props: `(binding, prop)`; prop is empty for the whole
    /// props object (`const props = defineProps()`).
    prop_bindings: Vec<(String, String)>,
    /// Request bodies that spoof a method: `const payload = { _method: 'PATCH' }`,
    /// `payload._method = 'PUT'`, `form.append('_method', 'PUT')`.
    /// Keyed by name, each with the function it is declared in; `None` hides
    /// an outer one. A method starting with `?` is set only on some paths.
    spoofs: HashMap<String, Vec<ScopedSpoof>>,
    /// Code that runs only sometimes: `if` branches, `?:` branches, the right
    /// side of `&&` / `||` / `??`.
    cond_spans: Vec<(u32, u32)>,
    /// Parameters typed as a small choice of strings (`kind: 'a' | 'b'`, or
    /// an alias of one): `(name, (start of the parameter, end of the
    /// parameter list), the values)`.
    /// Every parameter is kept (`None`: not such a type), so one hides an
    /// outer one of the same name.
    param_types: Vec<TypedParam>,
    /// End of the parameter list being visited.
    params_end: u32,
    /// Every parameter's position: `(name, index, (start, end of the list))`.
    param_index: Vec<(String, u32, (u32, u32))>,
    /// Named code spans: functions bound to a name, objects bound to a name,
    /// and functions in an object property (`api = { save: (…) => … }`).
    names: Vec<(u32, u32, String)>,
    /// Component computed properties / methods that return a small choice of
    /// strings (`reportSegment() { return v ? 'a' : 'b' }`), for `this.x`.
    member_options: HashMap<String, Vec<String>>,
    /// Callbacks and loop bodies that copy an object into a body built here
    /// (`Object.entries(payload).forEach(…)`): calls inside them hand nothing off.
    copied_spans: Vec<(u32, u32)>,
    /// Request bodies built in a variable: an object literal, `new FormData()`
    /// with `.append('k', …)`, plus `x.k = …` assignments.
    /// Keyed by name, each with the function it is declared in.
    bodies: HashMap<String, Vec<ScopedBody>>,
    /// Constants anywhere (also inside functions) holding a small choice of
    /// strings: `const prefix = kind === 'x' ? '/area/x' : '/area'`.
    local_options: HashMap<String, Vec<String>>,
}

/// A small URL builder: `(id, suffix = '') => \`/outlets/${id}${suffix}\``.
struct UrlHelper {
    params: Vec<String>,
    /// URL parts of each parameter's default value.
    defaults: Vec<Option<Vec<(bool, String)>>>,
    /// URL parts of the body; a non-static part naming a parameter is
    /// replaced by the argument at the call.
    parts: Vec<(bool, String)>,
}

/// `(key, parameter names, parameter defaults as URL parts, function span)`.
type WrapperFn = (
    String,
    Vec<String>,
    Vec<Option<Vec<(bool, String)>>>,
    (u32, u32),
);

/// `(call start, name, scope, path of the result the name holds)`.
type ResultBinding = (u32, String, (u32, u32), Vec<String>);

#[derive(Clone, Copy)]
enum TypeDef<'a> {
    Interface(&'a TSInterfaceDeclaration<'a>),
    Literal(&'a TSTypeLiteral<'a>),
    Other(&'a TSType<'a>),
}

const MAX_SIG: usize = 160;
const MAX_DEFAULT: usize = 60;

impl<'s, 'a> Analyzer<'s, 'a>
where
    's: 'a,
{
    fn new(src: &'s str, first_line: u32, kind: ScriptKind) -> Self {
        let newlines = src
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| i as u32)
            .collect();
        Self {
            src,
            first_line,
            kind,
            newlines,
            facts: ScriptFacts::default(),
            consts: HashMap::new(),
            functions: HashMap::new(),
            types: HashMap::new(),
            call_bindings: HashMap::new(),
            prop_defaults: HashMap::new(),
            emit_fns: HashSet::new(),
            handled: HashSet::new(),
            comments: Vec::new(),
            saw_component_call: false,
            scopes: Vec::new(),
            calls: Vec::new(),
            result_bindings: Vec::new(),
            fn_stack: Vec::new(),
            then_cbs: HashSet::new(),
            url_helpers: HashMap::new(),
            wrappers: Vec::new(),
            chains: Vec::new(),
            assigns: Vec::new(),
            fn_calls: Vec::new(),
            returns: Vec::new(),
            direct_returns: HashSet::new(),
            prop_bindings: Vec::new(),
            spoofs: HashMap::new(),
            cond_spans: Vec::new(),
            param_types: Vec::new(),
            params_end: 0,
            param_index: Vec::new(),
            names: Vec::new(),
            member_options: HashMap::new(),
            copied_spans: Vec::new(),
            bodies: HashMap::new(),
            local_options: HashMap::new(),
        }
    }

    fn line(&self, offset: u32) -> u32 {
        self.first_line + self.newlines.partition_point(|&n| n < offset) as u32
    }

    fn text(&self, span: Span) -> &'s str {
        &self.src[span.start as usize..(span.end as usize).min(self.src.len())]
    }

    fn compact(&self, span: Span, max: usize) -> String {
        let t: String = strip_comments(self.text(span))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if t.chars().count() > max {
            let cut: String = t.chars().take(max - 1).collect();
            format!("{cut}…")
        } else {
            t
        }
    }

    // ------------------------------------------------------------ program

    fn program(&mut self, program: &'a Program<'a>) {
        self.comments = program
            .comments
            .iter()
            .map(|c| {
                (
                    c.span.start,
                    c.span.end,
                    c.is_block() && self.text(c.span).starts_with("/**"),
                )
            })
            .collect();
        // Pass 1: names, so later passes can resolve identifiers.
        for stmt in &program.body {
            self.collect_names(stmt);
        }
        // Pass 2: top-level declarations, imports and exports.
        let mut default_export: Option<&'a ExportDefaultDeclarationKind<'a>> = None;
        let mut named_export_locals: HashSet<String> = HashSet::new();
        for stmt in &program.body {
            match stmt {
                Statement::ImportDeclaration(d) => self.import(d),
                Statement::ExportDeclaration(d) => self.declaration(&d.declaration, true, d.span),
                Statement::ExportDefaultDeclaration(d) => {
                    default_export = Some(&d.declaration);
                    self.default_decl(&d.declaration, d.span);
                }
                Statement::ExportNamedDeclaration(d) => {
                    for s in &d.specifiers {
                        let local = s.local.name().to_string();
                        let exported = s.exported.name().to_string();
                        if local != exported {
                            self.facts.reexports.push(Reexport {
                                exported: exported.clone(),
                                local: local.clone(),
                                from: None,
                                line: self.line(s.span.start),
                            });
                        }
                        named_export_locals.insert(local);
                    }
                }
                Statement::ExportFromDeclaration(d) => {
                    for s in &d.specifiers {
                        self.facts.reexports.push(Reexport {
                            exported: s.exported.name().to_string(),
                            local: s.local.name().to_string(),
                            from: Some(d.source.value.to_string()),
                            line: self.line(s.span.start),
                        });
                    }
                }
                Statement::ExportAllDeclaration(d) => self.facts.reexports.push(Reexport {
                    exported: d
                        .exported
                        .as_ref()
                        .map_or_else(|| "*".to_owned(), |e| e.name().to_string()),
                    local: "*".into(),
                    from: Some(d.source.value.to_string()),
                    line: self.line(d.span.start),
                }),
                Statement::VariableDeclaration(_)
                | Statement::FunctionDeclaration(_)
                | Statement::ClassDeclaration(_)
                | Statement::TSInterfaceDeclaration(_)
                | Statement::TSTypeAliasDeclaration(_)
                | Statement::TSEnumDeclaration(_) => {
                    if let Some(decl) = stmt.as_declaration() {
                        self.declaration(decl, false, decl.span());
                    }
                }
                _ => {}
            }
        }
        for d in &mut self.facts.decls {
            if named_export_locals.contains(&d.name) {
                d.exported = true;
            }
            if d.name.as_str() == "default" {
                d.default_export = true;
            }
        }
        // `export default name` where `name` is a local declaration.
        if let Some(ExportDefaultDeclarationKind::Identifier(id)) = default_export {
            for d in &mut self.facts.decls {
                if d.name == id.name.as_str() {
                    d.default_export = true;
                }
            }
        }

        // Component options / Vuex module from the default export.
        if let Some(kind) = default_export {
            if let Some(expr) = kind.as_expression() {
                self.default_export_expr(expr);
            } else if let ExportDefaultDeclarationKind::ClassDeclaration(c) = kind {
                self.class_component(c);
            }
        }
        if self.facts.vuex_module.is_none() {
            self.nuxt2_vuex_named_exports();
        }

        // vue-router route records in top-level consts and the default export.
        for stmt in &program.body {
            self.route_statement(stmt);
        }

        // React function components and file directives.
        self.facts.directives = program
            .directives
            .iter()
            .map(|d| d.directive.to_string())
            .collect();
        for stmt in &program.body {
            self.react_statement(stmt);
        }

        // Pass 3: calls anywhere.
        self.visit_program(program);
        if !self.saw_component_call {
            self.facts.registration_globs.clear();
        }
        self.finish_api_calls();
        self.finish_fn_spoofs();
        dedupe(&mut self.facts.composables);
        dedupe(&mut self.facts.emitted);
    }

    fn setup_bindings(&mut self, pattern: &BindingPattern<'a>) {
        match pattern {
            BindingPattern::BindingIdentifier(id) => {
                if id
                    .name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
                {
                    self.facts.setup_bindings.push(id.name.to_string());
                }
            }
            BindingPattern::ObjectPattern(o) => {
                for p in &o.properties {
                    self.setup_bindings(&p.value);
                }
                if let Some(rest) = &o.rest {
                    self.setup_bindings(&rest.argument);
                }
            }
            BindingPattern::ArrayPattern(a) => {
                for el in a.elements.iter().flatten() {
                    self.setup_bindings(el);
                }
            }
            BindingPattern::AssignmentPattern(a) => self.setup_bindings(&a.left),
        }
    }

    fn collect_names(&mut self, stmt: &'a Statement<'a>) {
        if self.kind == ScriptKind::VueSetup
            && let Statement::VariableDeclaration(v) = stmt
        {
            for d in &v.declarations {
                self.setup_bindings(&d.id);
            }
        }
        let decl = match stmt {
            Statement::ExportDeclaration(d) => &d.declaration,
            other => match other.as_declaration() {
                Some(d) => d,
                None => return,
            },
        };
        match decl {
            Declaration::VariableDeclaration(v) => {
                for d in &v.declarations {
                    match (&d.id, &d.init) {
                        (BindingPattern::BindingIdentifier(id), Some(init)) => {
                            self.consts.insert(id.name.as_str(), init);
                            if let Expression::CallExpression(c) = init.without_parentheses() {
                                self.call_bindings.insert(c.span.start, id.name.to_string());
                                if callee_name(&c.callee) == Some("defineEmits") {
                                    self.emit_fns.insert(id.name.to_string());
                                }
                            }
                        }
                        (BindingPattern::ObjectPattern(p), Some(init)) => {
                            // `const { a = 1, b } = defineProps<…>()` (Vue 3.5 destructure).
                            if let Some(c) = find_call(init, "defineProps") {
                                let defaults = p
                                    .properties
                                    .iter()
                                    .filter_map(|prop| {
                                        let name = prop.key.static_name()?.to_string();
                                        match &prop.value {
                                            BindingPattern::AssignmentPattern(a) => Some((
                                                name,
                                                self.compact(a.right.span(), MAX_DEFAULT),
                                            )),
                                            _ => None,
                                        }
                                    })
                                    .collect();
                                self.prop_defaults.insert(c.span.start, defaults);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Declaration::FunctionDeclaration(f) => {
                if let Some(id) = &f.id {
                    self.functions.insert(id.name.as_str(), f);
                }
            }
            Declaration::TSInterfaceDeclaration(i) => {
                self.types.insert(i.id.name.as_str(), TypeDef::Interface(i));
                let def = TsTypeDef {
                    name: i.id.name.to_string(),
                    params: i
                        .type_parameters
                        .as_ref()
                        .map(|tp| tp.params.iter().map(|p| p.name.name.to_string()).collect())
                        .unwrap_or_default(),
                    extends: i
                        .extends
                        .iter()
                        .map(|h| self.compact(h.span, 200))
                        .collect(),
                    body: self.type_body(i.body.span),
                    line: self.line(i.span.start),
                };
                self.push_type(def);
            }
            Declaration::TSTypeAliasDeclaration(t) => {
                let def = match &t.type_annotation {
                    TSType::TSTypeLiteral(l) => TypeDef::Literal(l),
                    other => TypeDef::Other(other),
                };
                self.types.insert(t.id.name.as_str(), def);
                let def = TsTypeDef {
                    name: t.id.name.to_string(),
                    params: t
                        .type_parameters
                        .as_ref()
                        .map(|tp| tp.params.iter().map(|p| p.name.name.to_string()).collect())
                        .unwrap_or_default(),
                    extends: Vec::new(),
                    body: self.type_body(t.type_annotation.span()),
                    line: self.line(t.span.start),
                };
                self.push_type(def);
            }
            _ => {}
        }
    }

    fn import(&mut self, d: &ImportDeclaration<'a>) {
        let mut imp = Import {
            source: d.source.value.to_string(),
            type_only: d.import_kind.is_type(),
            line: self.line(d.span.start),
            ..Import::default()
        };
        for s in d.specifiers.iter().flatten() {
            match s {
                ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                    imp.default = Some(s.local.name.to_string());
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                    imp.namespace = Some(s.local.name.to_string());
                }
                ImportDeclarationSpecifier::ImportSpecifier(s) => {
                    imp.named
                        .push((s.imported.name().to_string(), s.local.name.to_string()));
                }
            }
        }
        self.facts.imports.push(imp);
    }

    fn doc_before(&self, start: u32) -> Option<String> {
        let idx = self.comments.partition_point(|c| c.1 <= start);
        let (cs, ce, jsdoc) = *self.comments.get(idx.checked_sub(1)?)?;
        if !jsdoc || !self.src[ce as usize..start as usize].trim().is_empty() {
            return None;
        }
        let body = self.text(Span::new(cs + 3, ce.saturating_sub(2)));
        let summary: Vec<&str> = body
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .take_while(|l| !l.starts_with('@'))
            .filter(|l| !l.is_empty())
            .collect();
        let s = summary.join(" ");
        (!s.is_empty()).then(|| s.chars().take(200).collect())
    }

    fn push_decl(
        &mut self,
        name: &str,
        kind: DeclKind,
        exported: bool,
        signature: String,
        span: Span,
        doc_at: u32,
    ) {
        let doc = self.doc_before(doc_at);
        self.facts.decls.push(Decl {
            name: name.to_owned(),
            kind,
            exported,
            default_export: false,
            signature,
            doc,
            line: self.line(span.start),
            end_line: self.line(span.end),
            members: Vec::new(),
        });
    }

    /// Fields and methods of an interface or object type literal.
    fn members_of(&self, list: &[TSSignature<'a>]) -> Vec<crate::facts::Member> {
        list.iter()
            .filter_map(|m| {
                let (key, span) = match m {
                    TSSignature::TSPropertySignature(p) => (&p.key, p.span),
                    TSSignature::TSMethodSignature(p) => (&p.key, p.span),
                    _ => return None,
                };
                Some(crate::facts::Member {
                    name: key.static_name()?.to_string(),
                    signature: self.compact(span, 120),
                    line: self.line(span.start),
                })
            })
            .collect()
    }

    fn declaration(&mut self, decl: &'a Declaration<'a>, exported: bool, outer: Span) {
        match decl {
            Declaration::FunctionDeclaration(f) => {
                if let Some(id) = &f.id {
                    let sig = format!("{}{}", id.name, self.compact(f.params.span, MAX_SIG));
                    self.push_decl(
                        &id.name,
                        DeclKind::Function,
                        exported,
                        sig,
                        f.span,
                        outer.start,
                    );
                }
            }
            Declaration::ClassDeclaration(c) => {
                if let Some(id) = &c.id {
                    let sig = match &c.heritage {
                        Some(h) => {
                            format!("class {} extends {}", id.name, self.compact(h.span(), 60))
                        }
                        None => format!("class {}", id.name),
                    };
                    self.push_decl(
                        &id.name,
                        DeclKind::Class,
                        exported,
                        sig,
                        c.span,
                        outer.start,
                    );
                }
            }
            Declaration::VariableDeclaration(v) => {
                let kind = if v.kind == VariableDeclarationKind::Const {
                    DeclKind::Const
                } else {
                    DeclKind::Let
                };
                for d in &v.declarations {
                    let BindingPattern::BindingIdentifier(id) = &d.id else {
                        continue;
                    };
                    let func = d.init.as_ref().and_then(|i| match i.without_parentheses() {
                        Expression::ArrowFunctionExpression(f) => Some(f.params.span),
                        Expression::FunctionExpression(f) => Some(f.params.span),
                        _ => None,
                    });
                    let (k, sig) = match func {
                        Some(params) => (
                            DeclKind::Function,
                            format!("{}{}", id.name, self.compact(params, MAX_SIG)),
                        ),
                        None => {
                            let ty = d
                                .type_annotation
                                .as_ref()
                                .map(|t| {
                                    format!(": {}", self.compact(t.type_annotation.span(), 60))
                                })
                                .unwrap_or_default();
                            let kw = if kind == DeclKind::Const {
                                "const"
                            } else {
                                "let"
                            };
                            (kind, format!("{kw} {}{ty}", id.name))
                        }
                    };
                    let doc_at = if v.declarations.len() == 1 {
                        outer.start
                    } else {
                        d.span.start
                    };
                    self.push_decl(&id.name, k, exported, sig, d.span, doc_at);
                }
            }
            Declaration::TSInterfaceDeclaration(i) => {
                let sig = format!("interface {}", i.id.name);
                self.push_decl(
                    &i.id.name,
                    DeclKind::Interface,
                    exported,
                    sig,
                    i.span,
                    outer.start,
                );
                let members = self.members_of(&i.body.body);
                if let Some(d) = self.facts.decls.last_mut() {
                    d.members = members;
                }
            }
            Declaration::TSTypeAliasDeclaration(t) => {
                let sig = format!("type {}", t.id.name);
                self.push_decl(
                    &t.id.name,
                    DeclKind::Type,
                    exported,
                    sig,
                    t.span,
                    outer.start,
                );
                if let TSType::TSTypeLiteral(l) = &t.type_annotation {
                    let members = self.members_of(&l.members);
                    if let Some(d) = self.facts.decls.last_mut() {
                        d.members = members;
                    }
                }
            }
            Declaration::TSEnumDeclaration(e) => {
                let sig = format!("enum {}", e.id.name);
                self.push_decl(
                    &e.id.name,
                    DeclKind::Enum,
                    exported,
                    sig,
                    e.span,
                    outer.start,
                );
            }
            _ => {}
        }
    }

    fn default_decl(&mut self, kind: &'a ExportDefaultDeclarationKind<'a>, outer: Span) {
        match kind {
            ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                let name = f.id.as_ref().map_or("default", |i| i.name.as_str());
                let sig = format!("{name}{}", self.compact(f.params.span, MAX_SIG));
                self.push_decl(name, DeclKind::Function, true, sig, f.span, outer.start);
                if let Some(d) = self.facts.decls.last_mut() {
                    d.default_export = true;
                }
            }
            ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                let name = c.id.as_ref().map_or("default", |i| i.name.as_str());
                self.push_decl(
                    name,
                    DeclKind::Class,
                    true,
                    format!("class {name}"),
                    c.span,
                    outer.start,
                );
                if let Some(d) = self.facts.decls.last_mut() {
                    d.default_export = true;
                }
            }
            _ => {
                if let Some(e) = kind.as_expression() {
                    let (params, span) = match e.without_parentheses() {
                        Expression::ArrowFunctionExpression(f) => (Some(f.params.span), f.span),
                        Expression::FunctionExpression(f) => (Some(f.params.span), f.span),
                        _ => (None, e.span()),
                    };
                    if let Some(params) = params {
                        let sig = format!("default{}", self.compact(params, MAX_SIG));
                        self.push_decl("default", DeclKind::Function, true, sig, span, outer.start);
                        if let Some(d) = self.facts.decls.last_mut() {
                            d.default_export = true;
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ default export

    fn default_export_expr<'b>(&mut self, expr: &'b Expression<'a>) {
        let expr = strip_ts(expr);
        let (obj, style) = match expr {
            Expression::ObjectExpression(o) => (Some(&**o), "object"),
            Expression::CallExpression(c) => {
                let name = callee_name(&c.callee).unwrap_or("");
                let style = match name {
                    "defineComponent" | "defineNuxtComponent" => "defineComponent",
                    "extend" => "Vue.extend",
                    "createStore" | "Store" => "store",
                    _ => "",
                };
                let obj = c
                    .arguments
                    .first()
                    .and_then(|a| a.as_expression())
                    .and_then(|e| match strip_ts(e) {
                        Expression::ObjectExpression(o) => Some(&**o),
                        _ => None,
                    });
                (if style.is_empty() { None } else { obj }, style)
            }
            Expression::NewExpression(n) => {
                // `export default new Vuex.Store({...})`
                let obj = n
                    .arguments
                    .first()
                    .and_then(|a| a.as_expression())
                    .and_then(|e| match strip_ts(e) {
                        Expression::ObjectExpression(o) => Some(&**o),
                        _ => None,
                    });
                (obj, "store")
            }
            Expression::Identifier(id) => {
                if let Some(init) = self.consts.get(id.name.as_str()).copied()
                    && !matches!(strip_ts(init), Expression::Identifier(_))
                {
                    self.default_export_expr(init);
                }
                return;
            }
            _ => (None, ""),
        };
        let Some(obj) = obj else {
            return;
        };
        if style == "store" || is_vuex_shape(obj) {
            let mut m = self.vuex_object(obj);
            m.root = style == "store";
            self.facts.vuex_module = Some(m);
            return;
        }
        if self.kind == ScriptKind::Module && style == "object" && !is_component_shape(obj) {
            return;
        }
        self.facts.options = Some(self.component_options(obj, style));
    }

    fn component_options<'b>(
        &mut self,
        obj: &'b ObjectExpression<'a>,
        style: &str,
    ) -> ComponentOptions {
        let mut o = ComponentOptions {
            style: style.to_owned(),
            line: self.line(obj.span.start),
            ..ComponentOptions::default()
        };
        for prop in &obj.properties {
            let ObjectPropertyKind::ObjectProperty(p) = prop else {
                continue;
            };
            let Some(key) = p.key.static_name() else {
                continue;
            };
            let value = self.resolve(&p.value);
            match key.as_ref() {
                "name" => o.name = string_value(value),
                "props" => o.props = self.runtime_props(value),
                "emits" => o.emits = self.names_with_lines(value),
                "components" => {
                    if let Expression::ObjectExpression(c) = value {
                        for cp in &c.properties {
                            let ObjectPropertyKind::ObjectProperty(cp) = cp else {
                                continue;
                            };
                            let Some(name) = cp.key.static_name() else {
                                continue;
                            };
                            let local = match strip_ts(&cp.value) {
                                Expression::Identifier(id) => id.name.to_string(),
                                // `'unit-lookup': { props: [...] }` defined in place.
                                Expression::ObjectExpression(_) => "inline".to_owned(),
                                other => match lazy_import_source(other) {
                                    Some(src) => format!("import:{src}"),
                                    None => continue,
                                },
                            };
                            o.components.push((name.to_string(), local));
                        }
                    }
                }
                "mixins" => {
                    if let Expression::ArrayExpression(a) = value {
                        o.mixins = a
                            .elements
                            .iter()
                            .filter_map(|e| match e.as_expression().map(strip_ts) {
                                Some(Expression::Identifier(id)) => Some(id.name.to_string()),
                                _ => None,
                            })
                            .collect();
                    }
                }
                "model" => {
                    if let Expression::ObjectExpression(m) = value {
                        let get = |k: &str| {
                            m.properties.iter().find_map(|p| match p {
                                ObjectPropertyKind::ObjectProperty(p)
                                    if p.key.static_name().as_deref() == Some(k) =>
                                {
                                    string_value(&p.value)
                                }
                                _ => None,
                            })
                        };
                        o.model = Some((
                            get("prop").unwrap_or_else(|| "value".into()),
                            get("event").unwrap_or_else(|| "input".into()),
                        ));
                    }
                }
                "data" => o.data = self.keys(value, true),
                "computed" => o.computed = self.keys(value, false),
                "methods" => o.methods = self.keys(value, false),
                "watch" => o.watch = self.keys(value, false),
                "inject" => o.inject = self.names(value),
                "provide" => o.provide = self.keys(value, true),
                "layout" => o.layout = string_value(value),
                "middleware" => o.middleware = self.names(value),
                "setup" => o.has_setup = true,
                "template" | "render" => o.renders = true,
                _ => {}
            }
        }
        o
    }

    fn class_component<'b>(&mut self, c: &'b Class<'a>) {
        if c.decorators.is_empty() {
            return;
        }
        let mut o = ComponentOptions {
            name: c.id.as_ref().map(|i| i.name.to_string()),
            style: "class".into(),
            line: self.line(c.span.start),
            ..ComponentOptions::default()
        };
        for el in &c.body.body {
            match el {
                ClassElement::PropertyDefinition(p) => {
                    let Some(name) = p.key.static_name() else {
                        continue;
                    };
                    let deco = p.decorators.iter().find_map(|d| match &d.expression {
                        Expression::CallExpression(call) => callee_name(&call.callee),
                        Expression::Identifier(i) => Some(i.name.as_str()),
                        _ => None,
                    });
                    match deco {
                        Some("Prop" | "PropSync" | "Model" | "ModelSync") => o.props.push(Prop {
                            name: name.to_string(),
                            r#type: p
                                .type_annotation
                                .as_ref()
                                .map(|t| self.compact(t.type_annotation.span(), 60)),
                            required: !p.optional,
                            default: None,
                            line: self.line(p.span.start),
                        }),
                        _ => o.data.push(name.to_string()),
                    }
                }
                ClassElement::MethodDefinition(m) => {
                    let Some(name) = m.key.static_name() else {
                        continue;
                    };
                    if m.kind == MethodDefinitionKind::Get {
                        o.computed.push(name.to_string());
                    } else if m.kind == MethodDefinitionKind::Method {
                        o.methods.push(name.to_string());
                    }
                }
                _ => {}
            }
        }
        self.facts.options = Some(o);
    }

    // ------------------------------------------------------------ props / emits

    fn runtime_props<'b>(&self, value: &'b Expression<'a>) -> Vec<Prop> {
        match value {
            Expression::ArrayExpression(a) => a
                .elements
                .iter()
                .filter_map(|e| {
                    let e = e.as_expression()?;
                    Some(Prop {
                        name: string_value(e)?,
                        line: self.line(e.span().start),
                        ..Prop::default()
                    })
                })
                .collect(),
            Expression::ObjectExpression(o) => o
                .properties
                .iter()
                .filter_map(|p| {
                    let ObjectPropertyKind::ObjectProperty(p) = p else {
                        return None;
                    };
                    let name = p.key.static_name()?.to_string();
                    let mut prop = Prop {
                        name,
                        line: self.line(p.span.start),
                        ..Prop::default()
                    };
                    match strip_ts(&p.value) {
                        Expression::ObjectExpression(def) if !p.shorthand => {
                            for d in &def.properties {
                                let ObjectPropertyKind::ObjectProperty(d) = d else {
                                    continue;
                                };
                                match d.key.static_name().as_deref() {
                                    Some("type") => prop.r#type = Some(self.compact(d.value.span(), 60)),
                                    Some("required") => {
                                        prop.required = matches!(&d.value, Expression::BooleanLiteral(b) if b.value)
                                    }
                                    Some("default") => {
                                        prop.default = Some(self.compact(d.value.span(), MAX_DEFAULT))
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Expression::NullLiteral(_) => {}
                        other => prop.r#type = Some(self.compact(other.span(), 60)),
                    }
                    Some(prop)
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Props from a TS type argument: a type literal or a local interface/alias.
    fn type_props<'b>(&self, ty: &'b TSType<'a>, unresolved: &mut Option<String>) -> Vec<Prop> {
        let members: Vec<&'b TSSignature<'a>> = match ty {
            TSType::TSTypeLiteral(l) => l.members.iter().collect(),
            TSType::TSTypeReference(r) => {
                let name = match &r.type_name {
                    TSTypeName::IdentifierReference(i) => i.name.as_str(),
                    _ => "",
                };
                match self.types.get(name) {
                    Some(TypeDef::Interface(i)) => {
                        if !i.extends.is_empty() {
                            *unresolved = Some(format!(
                                "props interface `{name}` extends other types; inherited props are not listed"
                            ));
                        }
                        i.body.body.iter().collect()
                    }
                    Some(TypeDef::Literal(l)) => l.members.iter().collect(),
                    _ => {
                        *unresolved =
                            Some(match self.facts.imports.iter().find(|i| i.binds(name)) {
                                Some(i) => {
                                    format!("props type `{name}` is imported from '{}'", i.source)
                                }
                                None => format!("props type `{name}` could not be read"),
                            });
                        Vec::new()
                    }
                }
            }
            TSType::TSIntersectionType(_) | TSType::TSUnionType(_) => {
                *unresolved = Some("props use a union/intersection type; not expanded".into());
                Vec::new()
            }
            _ => Vec::new(),
        };
        members
            .into_iter()
            .filter_map(|m| match m {
                TSSignature::TSPropertySignature(p) => Some(Prop {
                    name: p.key.static_name()?.to_string(),
                    r#type: p
                        .type_annotation
                        .as_ref()
                        .map(|t| self.compact(t.type_annotation.span(), 60)),
                    required: !p.optional,
                    default: None,
                    line: self.line(p.span.start),
                }),
                TSSignature::TSMethodSignature(p) => Some(Prop {
                    name: p.key.static_name()?.to_string(),
                    r#type: Some("Function".into()),
                    required: !p.optional,
                    default: None,
                    line: self.line(p.span.start),
                }),
                _ => None,
            })
            .collect()
    }

    fn type_emits<'b>(&self, ty: &'b TSType<'a>) -> Vec<NamedLine> {
        let members: Vec<&'b TSSignature<'a>> = match ty {
            TSType::TSTypeLiteral(l) => l.members.iter().collect(),
            TSType::TSTypeReference(r) => match &r.type_name {
                TSTypeName::IdentifierReference(i) => match self.types.get(i.name.as_str()) {
                    Some(TypeDef::Interface(i)) => i.body.body.iter().collect(),
                    Some(TypeDef::Literal(l)) => l.members.iter().collect(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        members
            .into_iter()
            .filter_map(|m| match m {
                // `(e: 'change', id: number): void`
                TSSignature::TSCallSignatureDeclaration(c) => {
                    let first = c.params.items.first()?;
                    let ty = &first.type_annotation.as_ref()?.type_annotation;
                    let name = match ty {
                        TSType::TSLiteralType(l) => match &l.literal {
                            TSLiteral::StringLiteral(s) => s.value.to_string(),
                            _ => return None,
                        },
                        _ => return None,
                    };
                    Some(NamedLine {
                        name,
                        line: self.line(c.span.start),
                    })
                }
                // Vue 3.3: `change: [id: number]`
                TSSignature::TSPropertySignature(p) => Some(NamedLine {
                    name: p.key.static_name()?.to_string(),
                    line: self.line(p.span.start),
                }),
                _ => None,
            })
            .collect()
    }

    fn names_with_lines<'b>(&self, value: &'b Expression<'a>) -> Vec<NamedLine> {
        match value {
            Expression::ArrayExpression(a) => a
                .elements
                .iter()
                .filter_map(|e| {
                    let e = e.as_expression()?;
                    Some(NamedLine {
                        name: string_value(e)?,
                        line: self.line(e.span().start),
                    })
                })
                .collect(),
            Expression::ObjectExpression(o) => o
                .properties
                .iter()
                .filter_map(|p| match p {
                    ObjectPropertyKind::ObjectProperty(p) => Some(NamedLine {
                        name: p.key.static_name()?.to_string(),
                        line: self.line(p.span.start),
                    }),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn names<'b>(&self, value: &'b Expression<'a>) -> Vec<String> {
        match value {
            Expression::StringLiteral(s) => vec![s.value.to_string()],
            _ => self
                .names_with_lines(value)
                .into_iter()
                .map(|n| n.name)
                .collect(),
        }
    }

    // ------------------------------------------------------------ object keys

    fn resolve<'b>(&self, e: &'b Expression<'a>) -> &'b Expression<'a> {
        let mut e = strip_ts(e);
        for _ in 0..3 {
            match e {
                Expression::Identifier(id) => match self.consts.get(id.name.as_str()) {
                    Some(init) => e = strip_ts(init),
                    None => break,
                },
                _ => break,
            }
        }
        e
    }

    /// Keys of an object, or of the object a function returns (`fn_ok`).
    /// Spreads of `mapState(…)`-style helpers contribute their names.
    fn keys<'b>(&mut self, value: &'b Expression<'a>, fn_ok: bool) -> Vec<String> {
        let value = self.resolve(value);
        let obj = match value {
            Expression::ObjectExpression(o) => Some(&**o),
            Expression::ArrowFunctionExpression(f) if fn_ok => arrow_returned_object(&f.body),
            Expression::FunctionExpression(f) if fn_ok => {
                f.body.as_deref().and_then(returned_object)
            }
            _ => None,
        };
        let Some(obj) = obj else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for p in &obj.properties {
            match p {
                ObjectPropertyKind::ObjectProperty(p) => {
                    if let Some(k) = p.key.static_name() {
                        out.push(k.to_string());
                    }
                }
                ObjectPropertyKind::SpreadProperty(s) => {
                    if let Expression::CallExpression(c) = strip_ts(&s.argument) {
                        out.extend(self.map_helper_names(c));
                    } else {
                        out.extend(self.keys(&s.argument, false));
                    }
                }
            }
        }
        out
    }

    /// Names a `mapState` / `mapGetters` / `mapActions` / `mapMutations` call
    /// (Vuex or Pinia) adds.
    fn map_helper_names<'b>(&self, c: &'b CallExpression<'a>) -> Vec<String> {
        let Some(name) = callee_name(&c.callee) else {
            return Vec::new();
        };
        if !matches!(
            name,
            "mapState"
                | "mapGetters"
                | "mapActions"
                | "mapMutations"
                | "mapWritableState"
                | "mapStores"
        ) {
            return Vec::new();
        }
        let last = c.arguments.last().and_then(|a| a.as_expression());
        match last.map(strip_ts) {
            Some(Expression::ArrayExpression(a)) => a
                .elements
                .iter()
                .filter_map(|e| e.as_expression().and_then(string_value))
                .collect(),
            Some(Expression::ObjectExpression(o)) => o
                .properties
                .iter()
                .filter_map(|p| match p {
                    ObjectPropertyKind::ObjectProperty(p) => {
                        p.key.static_name().map(|k| k.to_string())
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    // ------------------------------------------------------------ Vuex

    fn vuex_object<'b>(&mut self, obj: &'b ObjectExpression<'a>) -> VuexModule {
        let mut m = VuexModule {
            line: self.line(obj.span.start),
            ..VuexModule::default()
        };
        for p in &obj.properties {
            let ObjectPropertyKind::ObjectProperty(p) = p else {
                continue;
            };
            let Some(key) = p.key.static_name() else {
                continue;
            };
            match key.as_ref() {
                "namespaced" => {
                    m.namespaced =
                        matches!(self.resolve(&p.value), Expression::BooleanLiteral(b) if b.value)
                }
                "state" => m.state = self.keys(&p.value, true),
                "getters" => m.getters = self.keys(&p.value, false),
                "mutations" => m.mutations = self.keys(&p.value, false),
                "actions" => m.actions = self.keys(&p.value, false),
                "modules" => {
                    if let Expression::ObjectExpression(mods) = self.resolve(&p.value) {
                        for mp in &mods.properties {
                            if let ObjectPropertyKind::ObjectProperty(mp) = mp
                                && let Some(name) = mp.key.static_name()
                                && let Expression::Identifier(id) = strip_ts(&mp.value)
                            {
                                m.modules.push((name.to_string(), id.name.to_string()));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        m
    }

    /// Nuxt 2 store files: `export const state = () => ({…})`, `export const mutations = {…}`.
    fn nuxt2_vuex_named_exports(&mut self) {
        let exported: HashSet<String> = self
            .facts
            .decls
            .iter()
            .filter(|d| d.exported)
            .map(|d| d.name.clone())
            .collect();
        let parts = ["state", "getters", "mutations", "actions"];
        if !parts.iter().any(|p| exported.contains(*p))
            || !(exported.contains("mutations") || exported.contains("state"))
        {
            return;
        }
        let mut m = VuexModule {
            namespaced: true,
            line: self.first_line,
            ..VuexModule::default()
        };
        for part in parts {
            if !exported.contains(part) {
                continue;
            }
            let Some(init) = self.consts.get(part).copied() else {
                // `export function state() {}` is rare; skip.
                continue;
            };
            let keys = self.keys(init, true);
            match part {
                "state" => m.state = keys,
                "getters" => m.getters = keys,
                "mutations" => m.mutations = keys,
                _ => m.actions = keys,
            }
        }
        self.facts.vuex_module = Some(m);
    }

    // ------------------------------------------------------------ Pinia

    fn define_store<'b>(&mut self, c: &'b CallExpression<'a>) {
        let args: Vec<&'b Expression<'a>> = c
            .arguments
            .iter()
            .filter_map(|a| a.as_expression())
            .collect();
        let (id, def) = match args.as_slice() {
            [first, rest @ ..] => match string_value(self.resolve(first)) {
                Some(id) => (Some(id), rest.first().copied()),
                // `defineStore({ id: 'cart', … })`
                None => (None, Some(*first)),
            },
            [] => return,
        };
        let mut s = PiniaStore {
            id: id.unwrap_or_default(),
            export: self.call_bindings.get(&c.span.start).cloned(),
            style: "options".into(),
            line: self.line(c.span.start),
            end_line: self.line(c.span.end),
            ..PiniaStore::default()
        };
        match def.map(strip_ts) {
            Some(Expression::ObjectExpression(o)) => {
                for p in &o.properties {
                    let ObjectPropertyKind::ObjectProperty(p) = p else {
                        continue;
                    };
                    match p.key.static_name().as_deref() {
                        Some("id") => {
                            if let Some(v) = string_value(self.resolve(&p.value)) {
                                s.id = v;
                            }
                        }
                        Some("state") => s.state = self.keys(&p.value, true),
                        Some("getters") => s.getters = self.keys(&p.value, false),
                        Some("actions") => s.actions = self.keys(&p.value, false),
                        _ => {}
                    }
                }
            }
            Some(Expression::ArrowFunctionExpression(f)) => {
                s.style = "setup".into();
                if let ArrowFunctionBody::FunctionBody(body) = &f.body {
                    self.setup_store_body(&body.statements, &mut s);
                }
            }
            Some(Expression::FunctionExpression(f)) => {
                s.style = "setup".into();
                if let Some(body) = &f.body {
                    self.setup_store_body(&body.statements, &mut s);
                }
            }
            _ => {}
        }
        if !s.id.is_empty() || s.export.is_some() {
            self.facts.pinia_stores.push(s);
        }
    }

    fn setup_store_body<'b>(&self, stmts: &'b [Statement<'a>], s: &mut PiniaStore) {
        let mut kinds: HashMap<&str, u8> = HashMap::new(); // 0 state, 1 getter, 2 action
        for stmt in stmts {
            match stmt {
                Statement::VariableDeclaration(v) => {
                    for d in &v.declarations {
                        let BindingPattern::BindingIdentifier(id) = &d.id else {
                            continue;
                        };
                        let k = match d.init.as_ref().map(strip_ts) {
                            Some(
                                Expression::ArrowFunctionExpression(_)
                                | Expression::FunctionExpression(_),
                            ) => 2,
                            Some(Expression::CallExpression(c))
                                if callee_name(&c.callee) == Some("computed") =>
                            {
                                1
                            }
                            _ => 0,
                        };
                        kinds.insert(id.name.as_str(), k);
                    }
                }
                Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        kinds.insert(id.name.as_str(), 2);
                    }
                }
                Statement::ReturnStatement(r) => {
                    let Some(Expression::ObjectExpression(o)) = r.argument.as_ref().map(strip_ts)
                    else {
                        continue;
                    };
                    for p in &o.properties {
                        let ObjectPropertyKind::ObjectProperty(p) = p else {
                            continue;
                        };
                        let Some(key) = p.key.static_name() else {
                            continue;
                        };
                        let local = match strip_ts(&p.value) {
                            Expression::Identifier(i) => i.name.as_str(),
                            _ => "",
                        };
                        match kinds.get(local).copied().unwrap_or(0) {
                            1 => s.getters.push(key.to_string()),
                            2 => s.actions.push(key.to_string()),
                            _ => s.state.push(key.to_string()),
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------ vue-router

    fn route_statement(&mut self, stmt: &'a Statement<'a>) {
        let (decl, exported) = match stmt {
            Statement::ExportDeclaration(d) => (&d.declaration, true),
            Statement::ExportDefaultDeclaration(d) => {
                if let Some(e) = d.declaration.as_expression()
                    && let Some(items) = self.route_items_of(e, true)
                {
                    let line = self.line(d.span.start);
                    self.facts.route_groups.push(RouteGroup {
                        binding: "default".into(),
                        exported: true,
                        items,
                        line,
                    });
                }
                return;
            }
            other => match other.as_declaration() {
                Some(d) => (d, false),
                None => return,
            },
        };
        let Declaration::VariableDeclaration(v) = decl else {
            return;
        };
        for d in &v.declarations {
            let (BindingPattern::BindingIdentifier(id), Some(init)) = (&d.id, &d.init) else {
                continue;
            };
            let hint = id.name.to_ascii_lowercase().contains("route");
            if let Some(items) = self.route_items_of(init, hint) {
                let line = self.line(d.span.start);
                self.facts.route_groups.push(RouteGroup {
                    binding: id.name.to_string(),
                    exported,
                    items,
                    line,
                });
            }
        }
    }

    /// `routes` of router options.
    fn router_options<'b>(&mut self, o: &'b ObjectExpression<'a>) {
        let Some(routes) = o.properties.iter().find_map(|p| match p {
            ObjectPropertyKind::ObjectProperty(p)
                if p.key.static_name().as_deref() == Some("routes") =>
            {
                Some(&p.value)
            }
            _ => None,
        }) else {
            return;
        };
        let line = self.line(o.span.start);
        let items = match strip_ts(routes) {
            Expression::Identifier(id) => vec![RouteItem::Ref {
                name: id.name.to_string(),
                line,
            }],
            other => self.route_items_of(other, true).unwrap_or_else(|| {
                vec![RouteItem::Dynamic {
                    text: self.compact(other.span(), 80),
                    line,
                }]
            }),
        };
        self.facts.route_groups.push(RouteGroup {
            binding: "#router".into(),
            exported: false,
            items,
            line,
        });
    }

    /// Route items when `e` looks like route records (an array of them, or one
    /// record). `hint`: the name says "routes", so arrays of references count.
    fn route_items_of<'b>(&self, e: &'b Expression<'a>, hint: bool) -> Option<Vec<RouteItem>> {
        match strip_ts(e) {
            Expression::ArrayExpression(a) => {
                let mut items = Vec::new();
                let mut nodes = 0;
                for el in &a.elements {
                    let item = match el {
                        ArrayExpressionElement::SpreadElement(sp) => {
                            self.route_ref_or_dynamic(&sp.argument)
                        }
                        ArrayExpressionElement::Elision(_) => continue,
                        other => {
                            let x = other.as_expression()?;
                            match strip_ts(x) {
                                Expression::ObjectExpression(o) => {
                                    if !has_key(o, &["path"]) {
                                        return None;
                                    }
                                    nodes += 1;
                                    RouteItem::Node(self.route_node(o))
                                }
                                other => self.route_ref_or_dynamic(other),
                            }
                        }
                    };
                    items.push(item);
                }
                (nodes > 0 || (hint && !items.is_empty())).then_some(items)
            }
            Expression::ObjectExpression(o)
                if has_key(o, &["path"])
                    && has_key(o, &["component", "components", "children", "redirect"]) =>
            {
                Some(vec![RouteItem::Node(self.route_node(o))])
            }
            _ => None,
        }
    }

    fn route_ref_or_dynamic<'b>(&self, e: &'b Expression<'a>) -> RouteItem {
        let line = self.line(e.span().start);
        match strip_ts(e) {
            Expression::Identifier(id) => RouteItem::Ref {
                name: id.name.to_string(),
                line,
            },
            Expression::CallExpression(c) => match self.route_helper_call(c) {
                Some(node) => RouteItem::Node(node),
                None => RouteItem::Dynamic {
                    text: self.compact(c.span, 80),
                    line,
                },
            },
            other => RouteItem::Dynamic {
                text: self.compact(other.span(), 80),
                line,
            },
        }
    }

    /// `list('a/b', 'a.b', 'kind')` where `const list = (path, name, kind) => ({ path, name, … })`:
    /// the record with the call's literal arguments substituted.
    fn route_helper_call<'b>(&self, c: &'b CallExpression<'a>) -> Option<RouteNode> {
        let Expression::Identifier(callee) = c.callee.without_parentheses() else {
            return None;
        };
        let name = callee.name.as_str();
        let (params, obj): (&FormalParameters<'a>, &ObjectExpression<'a>) =
            match self.consts.get(name).map(|e| strip_ts(e)) {
                Some(Expression::ArrowFunctionExpression(f)) => {
                    (&f.params, arrow_returned_object(&f.body)?)
                }
                Some(Expression::FunctionExpression(f)) => {
                    (&f.params, f.body.as_deref().and_then(returned_object)?)
                }
                _ => {
                    let f = self.functions.get(name)?;
                    (&f.params, f.body.as_deref().and_then(returned_object)?)
                }
            };
        if !has_key(obj, &["path"]) {
            return None;
        }
        let mut subst: Vec<(&str, &Expression<'_>)> = Vec::new();
        for (i, p) in params.items.iter().enumerate() {
            let pname = match &p.pattern {
                BindingPattern::BindingIdentifier(id) => id.name.as_str(),
                BindingPattern::AssignmentPattern(a) => match &a.left {
                    BindingPattern::BindingIdentifier(id) => id.name.as_str(),
                    _ => continue,
                },
                _ => continue,
            };
            if let Some(arg) = c.arguments.get(i).and_then(|a| a.as_expression()) {
                subst.push((pname, arg));
            }
        }
        let mut n = self.route_node_with(obj, &subst);
        n.line = self.line(c.span.start);
        Some(n)
    }

    /// A string built from literals, consts, substituted parameters,
    /// template literals and `+`.
    fn eval_str(
        &self,
        e: &Expression<'_>,
        subst: &[(&str, &Expression<'_>)],
        depth: u8,
    ) -> Option<String> {
        if depth > 6 {
            return None;
        }
        match strip_ts(e) {
            Expression::StringLiteral(s) => Some(s.value.to_string()),
            Expression::Identifier(id) => {
                if let Some((_, arg)) = subst.iter().find(|(p, _)| *p == id.name.as_str()) {
                    return self.eval_str(arg, &[], depth + 1);
                }
                self.consts
                    .get(id.name.as_str())
                    .and_then(|c| self.eval_str(c, &[], depth + 1))
            }
            Expression::TemplateLiteral(t) => {
                let mut out = String::new();
                for (i, q) in t.quasis.iter().enumerate() {
                    out.push_str(q.value.cooked.unwrap_or(q.value.raw).as_str());
                    if let Some(x) = t.expressions.get(i) {
                        out.push_str(&self.eval_str(x, subst, depth + 1)?);
                    }
                }
                Some(out)
            }
            Expression::BinaryExpression(b)
                if b.operator == oxc_ast::ast::BinaryOperator::Addition =>
            {
                Some(
                    self.eval_str(&b.left, subst, depth + 1)?
                        + &self.eval_str(&b.right, subst, depth + 1)?,
                )
            }
            _ => None,
        }
    }

    fn route_node<'b>(&self, o: &'b ObjectExpression<'a>) -> RouteNode {
        self.route_node_with(o, &[])
    }

    fn route_node_with<'b>(
        &self,
        o: &'b ObjectExpression<'a>,
        subst: &[(&str, &Expression<'_>)],
    ) -> RouteNode {
        let mut n = RouteNode {
            line: self.line(o.span.start),
            ..RouteNode::default()
        };
        for p in &o.properties {
            let ObjectPropertyKind::ObjectProperty(p) = p else {
                continue;
            };
            let Some(key) = p.key.static_name() else {
                continue;
            };
            match key.as_ref() {
                "path" => n.path = self.eval_str(&p.value, subst, 0),
                "name" => n.name = self.eval_str(&p.value, subst, 0),
                "component" => {
                    let arg = match strip_ts(&p.value) {
                        Expression::Identifier(id) => subst
                            .iter()
                            .find(|(q, _)| *q == id.name.as_str())
                            .map(|(_, a)| *a),
                        _ => None,
                    };
                    n.component = match arg {
                        Some(a) => self.route_component(a),
                        None => self.route_component(&p.value),
                    }
                }
                "components" => {
                    if let Expression::ObjectExpression(c) = strip_ts(&p.value) {
                        n.component = c.properties.iter().find_map(|cp| match cp {
                            ObjectPropertyKind::ObjectProperty(cp)
                                if cp.key.static_name().as_deref() == Some("default") =>
                            {
                                self.route_component(&cp.value)
                            }
                            _ => None,
                        });
                    }
                }
                "redirect" => {
                    n.redirect = Some(
                        string_value(&p.value).unwrap_or_else(|| self.compact(p.value.span(), 80)),
                    )
                }
                "meta" => n.meta = Some(self.compact(p.value.span(), 120)),
                "children" => {
                    n.children = match strip_ts(&p.value) {
                        Expression::Identifier(id) => {
                            vec![RouteItem::Ref {
                                name: id.name.to_string(),
                                line: self.line(p.span.start),
                            }]
                        }
                        other => self.route_items_of(other, true).unwrap_or_default(),
                    }
                }
                _ => {}
            }
        }
        n
    }

    fn route_component<'b>(&self, e: &'b Expression<'a>) -> Option<String> {
        let e = strip_ts(e);
        if let Some(src) = lazy_import_source(e).or_else(|| require_source(e)) {
            return Some(format!("import:{src}"));
        }
        match e {
            Expression::Identifier(id) => {
                // `const Login = () => import('...')` at the top of the file.
                match self
                    .consts
                    .get(id.name.as_str())
                    .and_then(|init| lazy_import_source(init).or_else(|| require_source(init)))
                {
                    Some(src) => Some(format!("import:{src}")),
                    None => Some(format!("local:{}", id.name)),
                }
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------ translations

    fn record_i18n_use<'b>(&mut self, c: &'b CallExpression<'a>) {
        const NAMES: &[&str] = &["t", "$t", "tc", "$tc", "te", "$te", "tm", "$tm", "rt"];
        let Some(name) = callee_name(&c.callee) else {
            return;
        };
        if !NAMES.contains(&name) {
            return;
        }
        // Member calls only on i18n-ish objects: this.$t, i18n.t, i18n.global.t, vm.$t.
        if let Some(m) = c.callee.without_parentheses().as_member_expression() {
            let obj = self.compact(m.object().span(), 40).to_ascii_lowercase();
            if !(name.starts_with('$')
                || obj.contains("i18n")
                || obj.ends_with("global")
                || obj == "this")
            {
                return;
            }
        }
        let Some(arg) = c.arguments.first().and_then(|a| a.as_expression()) else {
            return;
        };
        let line = self.line(c.span.start);
        // vue-i18n `t(key, 'Default text')`: the second argument is shown instead.
        let guarded = c
            .arguments
            .get(1)
            .and_then(|a| a.as_expression())
            .is_some_and(|a| matches!(strip_ts(a), Expression::StringLiteral(_)));
        if let Some(key) = string_value(arg) {
            if is_i18n_key(&key) {
                self.facts.i18n_uses.push(I18nUse {
                    key,
                    dynamic: false,
                    line,
                    guarded,
                });
            }
        } else if let Expression::TemplateLiteral(t) = strip_ts(arg)
            && let Some(q) = t.quasis.first()
        {
            let prefix = q.value.cooked.unwrap_or(q.value.raw).to_string();
            if !prefix.is_empty() && is_i18n_key(prefix.trim_end_matches('.')) {
                self.facts.i18n_uses.push(I18nUse {
                    key: prefix,
                    dynamic: true,
                    line,
                    guarded,
                });
            }
        }
    }

    // ------------------------------------------------------------ API calls

    fn scope(&self) -> (u32, u32) {
        self.scopes
            .last()
            .copied()
            .unwrap_or((0, self.src.len() as u32))
    }

    /// An HTTP call's method, URL argument and how it was called.
    fn api_call_parts<'b>(&self, c: &'b CallExpression<'a>) -> Option<CallParts<'b, 'a>> {
        const CLIENTS: &[&str] = &[
            "$fetch",
            "fetch",
            "useFetch",
            "useLazyFetch",
            "ofetch",
            "axios",
            "$axios",
            "request",
            "http",
            "api",
            "$http",
            "useApi",
            "apiFetch",
            "useHttp",
            "client",
            "apiClient",
        ];
        const VERBS: &[&str] = &[
            "get", "post", "put", "patch", "delete", "head", "$get", "$post", "$put", "$patch",
            "$delete", "request", "$request",
        ];
        let args: Vec<&'b Expression<'a>> = c
            .arguments
            .iter()
            .filter_map(|a| a.as_expression())
            .collect();
        let first = *args.first()?;
        let options = args.iter().skip(1).find_map(|a| match strip_ts(a) {
            Expression::ObjectExpression(o) => Some(&**o),
            _ => None,
        });
        let (via, verb, trusted) = match c.callee.without_parentheses() {
            Expression::Identifier(i) => {
                let n = i.name.as_str();
                let lower = n.to_ascii_lowercase();
                let trusted = CLIENTS.contains(&n);
                if !trusted
                    && !(lower.contains("api")
                        || lower.contains("request")
                        || lower.contains("fetch")
                        || lower.contains("http"))
                {
                    return None;
                }
                (n.to_owned(), None, trusted)
            }
            // `useApi()<T>('/x')`: a call on a client factory's result.
            Expression::CallExpression(inner) => {
                let n = callee_name(&inner.callee)?;
                let lower = n.to_ascii_lowercase();
                if !(lower.contains("api")
                    || lower.contains("http")
                    || lower.contains("fetch")
                    || lower.contains("client"))
                {
                    return None;
                }
                (format!("{n}()"), None, true)
            }
            other => {
                let m = other.as_member_expression()?;
                let verb = m.static_property_name()?;
                if !VERBS.contains(&verb) && !CLIENTS.contains(&verb) {
                    return None;
                }
                let obj = self.compact(m.object().span(), 40);
                let root = obj.rsplit('.').next().unwrap_or(&obj).to_owned();
                let trusted =
                    CLIENTS.contains(&root.as_str()) || root == "$axios" || obj.ends_with("$axios");
                let verb = (!CLIENTS.contains(&verb) || VERBS.contains(&verb))
                    .then(|| verb.trim_start_matches('$').to_ascii_uppercase());
                (
                    format!("{obj}.{}", m.static_property_name().unwrap_or_default()),
                    verb.filter(|v| v != "REQUEST"),
                    trusted,
                )
            }
        };
        // `axios({ url, method })` / `request({ url, method })`.
        let (url, options) = match strip_ts(first) {
            Expression::ObjectExpression(o) => {
                let url = o.properties.iter().find_map(|p| match p {
                    ObjectPropertyKind::ObjectProperty(p)
                        if p.key.static_name().as_deref() == Some("url") =>
                    {
                        Some(&p.value)
                    }
                    _ => None,
                })?;
                (url, Some(&**o))
            }
            _ => (first, options),
        };
        let method = options
            .and_then(|o| {
                o.properties.iter().find_map(|p| match p {
                    ObjectPropertyKind::ObjectProperty(p)
                        if p.key.static_name().as_deref() == Some("method") =>
                    {
                        string_value(&p.value).map(|m| m.to_ascii_uppercase())
                    }
                    _ => None,
                })
            })
            .or(verb)
            .or_else(|| {
                matches!(
                    via.as_str(),
                    "$fetch" | "fetch" | "useFetch" | "useLazyFetch" | "ofetch"
                )
                .then(|| "GET".to_owned())
            });
        Some(CallParts {
            method,
            url,
            via,
            trusted,
        })
    }

    /// Normalises a URL expression: `/orders/{}`; `None` when it is not URL-like.
    /// Returns `(url, base_var, optional_tail)`.
    fn url_of(&self, e: &Expression<'a>, trusted: bool) -> Option<(String, bool, bool)> {
        let mut parts: Vec<(bool, String)> = Vec::new(); // (is_static, text)
        self.url_parts(e, &mut parts, 0)?;
        parts.retain(|(is_static, t)| !(*is_static && t.is_empty()));
        // `/merchants${qs}` / `/orders${query ? '?' + query : ''}`: a query string tail;
        // `/procurement${id ? `/${id}` : ''}`: an optional segment.
        let mut optional_tail = false;
        if parts.len() > 1
            && let (Some((false, tail)), Some((true, before))) =
                (parts.last(), parts.get(parts.len() - 2))
            && !before.ends_with('/')
        {
            let t = tail.to_ascii_lowercase();
            let compact: String = t.chars().filter(|c| !c.is_whitespace()).collect();
            if compact.contains("?`/") || compact.contains("?'/") || compact.contains("?\"/") {
                optional_tail = true;
                parts.pop();
            } else if t.contains('?')
                || t.contains("query")
                || t == "qs"
                || t.contains("params")
                || t.contains("search")
            {
                parts.pop();
            }
        }
        let mut base_var = false;
        if let Some((false, name)) = parts.first() {
            let lower = name.to_ascii_lowercase();
            if lower.contains("base")
                || lower.contains("url")
                || lower.contains("api")
                || lower.contains("host")
                || lower.contains("prefix")
            {
                parts.remove(0);
                base_var = true;
            }
        }
        let mut url = String::new();
        for (is_static, t) in &parts {
            if *is_static {
                url.push_str(t);
            } else {
                url.push_str("{}");
            }
        }
        let has_static = parts
            .iter()
            .any(|(s, t)| *s && !t.trim_matches('/').is_empty());
        if !has_static
            || url.contains(char::is_whitespace)
            || url.starts_with('#')
            || url.starts_with('.')
        {
            return None;
        }
        if !trusted && !url.contains('/') {
            return None;
        }
        let url = url.split(['?', '#']).next().unwrap_or("").to_owned();
        Some((url, base_var, optional_tail))
    }

    fn url_parts(
        &self,
        e: &Expression<'a>,
        out: &mut Vec<(bool, String)>,
        depth: u8,
    ) -> Option<()> {
        if depth > 8 {
            return None;
        }
        match strip_ts(e) {
            Expression::StringLiteral(s) => out.push((true, s.value.to_string())),
            Expression::TemplateLiteral(t) => {
                for (i, q) in t.quasis.iter().enumerate() {
                    out.push((true, q.value.cooked.unwrap_or(q.value.raw).to_string()));
                    if let Some(x) = t.expressions.get(i) {
                        match self.eval_str(x, &[], 0) {
                            Some(v) => out.push((true, v)),
                            None => match strip_ts(x) {
                                Expression::CallExpression(c)
                                    if self.url_helper_of(c).is_some() =>
                                {
                                    self.expand_url_helper(c, out, depth + 1)?;
                                }
                                _ => out.push((false, self.compact(x.span(), 40))),
                            },
                        }
                    }
                }
            }
            Expression::BinaryExpression(b)
                if b.operator == oxc_ast::ast::BinaryOperator::Addition =>
            {
                self.url_parts(&b.left, out, depth + 1)?;
                self.url_parts(&b.right, out, depth + 1)?;
            }
            Expression::CallExpression(c) if self.url_helper_of(c).is_some() => {
                self.expand_url_helper(c, out, depth)?;
            }
            Expression::Identifier(id) => match self
                .consts
                .get(id.name.as_str())
                .and_then(|c| self.eval_str(c, &[], 0))
            {
                Some(v) => out.push((true, v)),
                None => out.push((false, id.name.to_string())),
            },
            other => out.push((false, self.compact(other.span(), 40))),
        }
        Some(())
    }

    fn url_helper_of(&self, c: &CallExpression<'_>) -> Option<&UrlHelper> {
        match c.callee.without_parentheses() {
            Expression::Identifier(i) => self.url_helpers.get(i.name.as_str()),
            _ => None,
        }
    }

    /// `url(outletId, \`/${id}\`)` → the helper's URL with the arguments in place.
    fn expand_url_helper(
        &self,
        c: &CallExpression<'a>,
        out: &mut Vec<(bool, String)>,
        depth: u8,
    ) -> Option<()> {
        let h = self.url_helper_of(c)?;
        for (is_static, t) in &h.parts {
            let param = (!is_static)
                .then(|| h.params.iter().position(|p| p == t))
                .flatten();
            match param {
                Some(i) => match c.arguments.get(i).and_then(|a| a.as_expression()) {
                    Some(arg) => self.url_parts(arg, out, depth + 1)?,
                    None => match &h.defaults[i] {
                        Some(d) => out.extend(d.iter().cloned()),
                        None => out.push((false, t.clone())),
                    },
                },
                None => out.push((*is_static, t.clone())),
            }
        }
        Some(())
    }

    /// `function RoomCard({ room }: Props)`, `export default function Page()`,
    /// `const Badge = ({ label }) => …`: capitalized functions are components.
    fn react_statement(&mut self, stmt: &'a Statement<'a>) {
        let mut found: Vec<(String, &'a FormalParameters<'a>, bool, bool, u32)> = Vec::new();
        let mut from_decl = |decl: &'a Declaration<'a>, exported: bool| match decl {
            Declaration::FunctionDeclaration(f) => {
                if let Some(id) = &f.id {
                    found.push((
                        id.name.to_string(),
                        &f.params,
                        exported,
                        false,
                        f.span.start,
                    ));
                }
            }
            Declaration::VariableDeclaration(v) => {
                for d in &v.declarations {
                    if let BindingPattern::BindingIdentifier(id) = &d.id
                        && let Some(init) = &d.init
                    {
                        let params = match strip_ts(init) {
                            Expression::ArrowFunctionExpression(f) => Some(&f.params),
                            Expression::FunctionExpression(f) => Some(&f.params),
                            Expression::CallExpression(c)
                                if matches!(
                                    callee_name(&c.callee),
                                    Some("memo" | "forwardRef")
                                ) =>
                            {
                                c.arguments
                                    .first()
                                    .and_then(|a| a.as_expression())
                                    .and_then(|a| match strip_ts(a) {
                                        Expression::ArrowFunctionExpression(f) => Some(&f.params),
                                        Expression::FunctionExpression(f) => Some(&f.params),
                                        _ => None,
                                    })
                            }
                            _ => None,
                        };
                        if let Some(params) = params {
                            found.push((
                                id.name.to_string(),
                                params,
                                exported,
                                false,
                                d.span.start,
                            ));
                        }
                    }
                }
            }
            _ => {}
        };
        match stmt {
            Statement::FunctionDeclaration(_) | Statement::VariableDeclaration(_) => {
                if let Some(decl) = stmt.as_declaration() {
                    from_decl(decl, false);
                }
            }
            Statement::ExportDeclaration(e) => from_decl(&e.declaration, true),
            Statement::ExportDefaultDeclaration(e) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(f) = &e.declaration {
                    let name =
                        f.id.as_ref()
                            .map(|i| i.name.to_string())
                            .unwrap_or_else(|| "default".into());
                    found.push((name, &f.params, true, true, f.span.start));
                }
            }
            _ => {}
        }
        for (name, params, exported, default_export, start) in found {
            let capital = name.chars().next().is_some_and(|c| c.is_ascii_uppercase());
            if !(capital || (default_export && name == "default"))
                || self.facts.react_components.len() >= 100
            {
                continue;
            }
            let mut props = Vec::new();
            if let Some(p) = params.items.first() {
                let mut unresolved = None;
                if let Some(t) = &p.type_annotation {
                    props = self.type_props(&t.type_annotation, &mut unresolved);
                }
                if let BindingPattern::ObjectPattern(o) = &p.pattern {
                    for prop in &o.properties {
                        if let Some(k) = prop.key.static_name()
                            && !props.iter().any(|x: &Prop| x.name == k.as_ref())
                        {
                            props.push(Prop {
                                name: k.to_string(),
                                line: self.line(prop.span.start),
                                ..Prop::default()
                            });
                        }
                    }
                }
            }
            let line = self.line(start);
            self.facts.react_components.push(ReactComponent {
                name,
                props,
                default_export,
                exported,
                line,
            });
        }
    }

    /// A type's text without comments, whitespace collapsed (at most 6000 chars).
    fn type_body(&self, span: Span) -> String {
        let t = strip_comments(self.text(span));
        let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
        t.chars().take(6000).collect()
    }

    fn push_type(&mut self, def: TsTypeDef) {
        if self.facts.types.len() < 300 && !self.facts.types.iter().any(|t| t.name == def.name) {
            self.facts.types.push(def);
        }
    }

    /// `x.from('rooms').select(…)` / `.insert` / `.update` / `.upsert` /
    /// `.delete`, and `x.rpc('book_room', …)`.
    fn record_db_table(&mut self, c: &CallExpression<'a>) {
        const OPS: &[&str] = &["select", "insert", "update", "upsert", "delete"];
        let Some(m) = c.callee.without_parentheses().as_member_expression() else {
            return;
        };
        let prop = m.static_property_name().unwrap_or_default();
        let line = self.line(c.span.start);
        if prop == "rpc"
            && let Some(name) = c
                .arguments
                .first()
                .and_then(|a| a.as_expression())
                .and_then(string_value)
        {
            if self.facts.db_tables.len() < 500 {
                self.facts.db_tables.push(NamedLine {
                    name: format!("rpc:{name}"),
                    line,
                });
            }
            return;
        }
        if !OPS.contains(&prop) {
            return;
        }
        let Expression::CallExpression(inner) = m.object().without_parentheses() else {
            return;
        };
        let Some(im) = inner.callee.without_parentheses().as_member_expression() else {
            return;
        };
        if im.static_property_name() != Some("from") {
            return;
        }
        let Some(table) = inner
            .arguments
            .first()
            .and_then(|a| a.as_expression())
            .and_then(string_value)
        else {
            return;
        };
        let ident = !table.is_empty()
            && table
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.');
        if ident && self.facts.db_tables.len() < 500 {
            self.facts.db_tables.push(NamedLine {
                name: format!("{table}:{prop}"),
                line,
            });
        }
    }

    /// Parameter names and the URL parts of their defaults.
    #[allow(clippy::type_complexity)]
    fn param_info(
        &self,
        params: &FormalParameters<'a>,
    ) -> (Vec<String>, Vec<Option<Vec<(bool, String)>>>) {
        let mut names = Vec::new();
        let mut defaults = Vec::new();
        for p in &params.items {
            let (name, default) = match &p.pattern {
                BindingPattern::AssignmentPattern(a) => {
                    (binding_names(&a.left).into_iter().next(), Some(&a.right))
                }
                other => (binding_names(other).into_iter().next(), None),
            };
            let default = p.initializer.as_deref().or(default);
            names.push(name.unwrap_or_default());
            defaults.push(default.and_then(|d| {
                let mut v = Vec::new();
                self.url_parts(d, &mut v, 0).map(|_| v)
            }));
        }
        (names, defaults)
    }

    /// An HTTP call whose URL uses the parameters of the function it sits in.
    fn record_url_template(
        &mut self,
        c: &CallExpression<'a>,
        url_expr: &Expression<'a>,
        method: Option<String>,
    ) {
        let at = c.span.start;
        let mut best: Option<WrapperFn> = self
            .wrappers
            .iter()
            .filter(|w| w.3.0 <= at && at < w.3.1)
            .min_by_key(|w| w.3.1 - w.3.0)
            .cloned();
        if best.is_none() {
            let top = self
                .functions
                .iter()
                .filter(|(_, f)| f.span.start <= at && at < f.span.end)
                .map(|(n, f)| (n.to_string(), *f))
                .next();
            if let Some((name, f)) = top {
                let (params, defaults) = self.param_info(&f.params);
                best = Some((name, params, defaults, (f.span.start, f.span.end)));
            }
        }
        let Some((key, params, defaults, _)) = best else {
            return;
        };
        let mut parts = Vec::new();
        if self.url_parts(url_expr, &mut parts, 0).is_none()
            || !parts
                .iter()
                .any(|(is_static, t)| !is_static && params.contains(t))
            || self.facts.url_templates.len() >= 200
        {
            return;
        }
        self.facts.url_templates.push(UrlTemplate {
            key,
            params,
            defaults,
            parts,
            method,
            line: self.line(at),
        });
    }

    fn record_api_call<'b>(&mut self, c: &'b CallExpression<'a>) {
        let Some(CallParts {
            method,
            url: url_expr,
            via,
            trusted,
        }) = self.api_call_parts(c)
        else {
            return;
        };
        let found = c
            .arguments
            .iter()
            .skip(1)
            .filter_map(|a| a.as_expression())
            .find_map(|a| self.spoof_of(a));
        // `@helper`: built by a function in another file, resolved later.
        let (found, mut spoof_via) = match found {
            Some(m) if m.starts_with('@') => (None, Some(m)),
            other => (other, None),
        };
        // `?_method=PUT` in the URL: Laravel reads it from the query string too.
        let found = found.or_else(|| {
            let text = self.compact(url_expr.span(), 300);
            let at = text.find("_method=")?;
            let m: String = text[at + 8..]
                .chars()
                .take_while(|ch| ch.is_ascii_alphabetic())
                .collect::<String>()
                .to_ascii_uppercase();
            matches!(m.as_str(), "PUT" | "PATCH" | "DELETE").then_some(m)
        });
        // The body is a parameter (or spreads one, or converts one:
        // `toFormData({ ...record })`): its callers decide.
        let body_param = c
            .arguments
            .get(1)
            .and_then(|a| a.as_expression())
            .and_then(|e| {
                body_params(e, 0)
                    .into_iter()
                    .find(|id| self.param_of(id.name.as_str(), id.span.start).is_some())
            });
        // Both may apply (`toFormData(payload)` with `payload` a parameter):
        // tried in order when files are linked.
        if found.is_none()
            && let Some(id) = body_param
            && let Some((key, idx)) = self.param_of(id.name.as_str(), id.span.start)
        {
            let param = format!("param:{key}#{idx}");
            spoof_via = Some(match spoof_via {
                Some(h) => format!("{h};{param}"),
                None => param,
            });
        }
        let spoofed = found
            .clone()
            .map(|m| either(method.as_deref(), m))
            .or(method.clone());
        self.record_url_template(c, url_expr, spoofed);
        // Through a project wrapper: keep the arguments to fill in its URL.
        let args: Vec<Option<Vec<(bool, String)>>> = if trusted {
            Vec::new()
        } else {
            c.arguments
                .iter()
                .take(6)
                .map(|a| {
                    a.as_expression().and_then(|e| {
                        let mut v = Vec::new();
                        self.url_parts(e, &mut v, 0).map(|_| v)
                    })
                })
                .collect()
        };
        // `ENDPOINT[kind]`: no text of its own, but known values.
        let mut options: Vec<String> = Vec::new();
        let parsed = self.url_of(url_expr, trusted).or_else(|| {
            let opts = self
                .string_options(url_expr, 0)?
                .into_iter()
                .filter(|o| trusted || o.contains('/'))
                .collect::<Vec<_>>();
            let first = opts.first()?.clone();
            options = opts;
            Some((first, false, false))
        });
        let Some((url, base_var, optional_tail)) = parsed else {
            return;
        };
        // Laravel method spoofing: `api.post(url, { …, _method: 'PUT' })`,
        // also through a variable, an assignment or a helper's object.
        let method = found.map(|m| either(method.as_deref(), m)).or(method);
        let external =
            (url.starts_with("http://") || url.starts_with("https://") || url.starts_with("//"))
                && !url.contains("localhost");
        let url = normalize_url(&url);
        let (prefixes, prefix_note) = if base_var {
            self.prefix_options(url_expr)
        } else {
            (Vec::new(), None)
        };
        let (variants, variants_loose) = if options.len() > 1 {
            (options.iter().map(|o| normalize_url(o)).collect(), false)
        } else if base_var {
            (Vec::new(), false)
        } else {
            self.url_variants(url_expr)
        };
        let sends = trusted.then(|| {
            let mut s = self.payload(c, method.as_deref());
            s.merge(self.url_query(url_expr));
            s
        });
        // `draftInvoiceApi.post`: the names of the spans around the call.
        let mut around: Vec<&(u32, u32, String)> = self
            .names
            .iter()
            .filter(|(a, b, _)| *a <= c.span.start && c.span.start <= *b)
            .collect();
        around.sort_by_key(|(a, _, _)| *a);
        let in_member = (!around.is_empty()).then(|| {
            around
                .iter()
                .map(|(_, _, n)| n.as_str())
                .collect::<Vec<_>>()
                .join(".")
        });
        let call = ApiCall {
            variants_loose,
            in_member,
            spoof_via,
            variants,
            sends,
            prefixes,
            prefix_note,
            method,
            raw: self.compact(url_expr.span(), 100),
            url,
            via,
            base_var,
            optional_tail,
            external,
            line: self.line(c.span.start),
            args,
            type_arg: c
                .type_arguments
                .as_ref()
                .and_then(|t| t.params.first())
                .map(|ty| self.type_body(ty.span())),
            ..ApiCall::default()
        };
        self.calls
            .push((call, c.span.start, c.span.end, self.scope()));
    }

    /// What happens to a call's result: paths read from it, state it is stored
    /// in, and how the enclosing function returns it.
    #[allow(clippy::type_complexity)]
    fn flow(
        &self,
        start: u32,
        end: u32,
        scope: (u32, u32),
    ) -> (Vec<String>, Vec<(String, String)>, Vec<(String, String)>) {
        let names: Vec<(String, (u32, u32), Vec<String>)> = self
            .result_bindings
            .iter()
            .filter(|(s, _, _, _)| *s == start)
            .map(|(_, n, sc, p)| (n.clone(), *sc, p.clone()))
            .collect();
        let _ = scope;
        let mut reads: Vec<String> = Vec::new();
        let mut stored: Vec<(String, String)> = Vec::new();
        let mut returned: Vec<(String, String)> = Vec::new();
        if self.direct_returns.contains(&(start, end)) {
            returned.push((String::new(), String::new()));
        }
        for (name, (s0, s1), prefix) in names {
            if name.is_empty() {
                continue;
            }
            let with_prefix = |path: &[String]| -> String {
                prefix
                    .iter()
                    .chain(path)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(".")
            };
            // The binding itself is state a template can read (`const { data: mall } = …`).
            let own = (name.clone(), prefix.join("."));
            if !stored.contains(&own) {
                stored.push(own);
            }
            for (root, path, at) in &self.chains {
                if *root == name && *at >= end.min(s1) && *at >= s0 && *at <= s1 && !path.is_empty()
                {
                    let p = with_prefix(path);
                    if !reads.contains(&p) {
                        reads.push(p);
                    }
                }
            }
            for (target, root, path, at) in &self.assigns {
                if *root == name && *at >= s0 && *at <= s1 {
                    let entry = (target.clone(), with_prefix(path));
                    if !stored.contains(&entry) {
                        stored.push(entry);
                    }
                }
            }
            for (out, root, path, at) in &self.returns {
                if *root == name && *at >= s0 && *at <= s1 {
                    let entry = (out.clone(), with_prefix(path));
                    if !returned.contains(&entry) {
                        returned.push(entry);
                    }
                }
            }
        }
        // Keep leaves: `data.meta.total` makes `data` and `data.meta` redundant.
        let all = reads.clone();
        reads.retain(|r| {
            !all.iter().any(|o| {
                o.len() > r.len() && o.starts_with(r.as_str()) && o.as_bytes()[r.len()] == b'.'
            })
        });
        reads.sort();
        reads.truncate(60);
        (reads, stored, returned)
    }

    /// The top-level function a line sits in.
    fn enclosing_fn(&self, line: u32) -> Option<String> {
        self.facts
            .decls
            .iter()
            .filter(|d| d.kind == DeclKind::Function && d.line <= line && line <= d.end_line)
            .min_by_key(|d| d.end_line - d.line)
            .map(|d| d.name.clone())
    }

    /// Fills `reads`, `stored_as` and `returned` from what the visitor saw.
    fn finish_api_calls(&mut self) {
        let calls = std::mem::take(&mut self.calls);
        for (mut call, start, end, scope) in calls {
            let (reads, stored, returned) = self.flow(start, end, scope);
            call.reads = reads;
            call.stored_as = stored;
            if !returned.is_empty() {
                call.in_fn = self.enclosing_fn(call.line);
                if call.in_fn.is_some() {
                    call.returned = returned;
                }
            }
            self.facts.api_calls.push(call);
        }
        let fn_calls = std::mem::take(&mut self.fn_calls);
        for (name, start, end, scope) in fn_calls {
            let (reads, stored, returned) = self.flow(start, end, scope);
            let line = self.line(start);
            let in_fn = if returned.is_empty() {
                None
            } else {
                self.enclosing_fn(line).filter(|f| *f != name)
            };
            if reads.is_empty() && stored.is_empty() && in_fn.is_none() {
                continue;
            }
            if self.facts.fn_calls.len() >= 300 {
                break;
            }
            self.facts.fn_calls.push(FnCall {
                name,
                line,
                reads,
                stored_as: stored,
                returned: if in_fn.is_some() {
                    returned
                } else {
                    Vec::new()
                },
                in_fn,
            });
        }
        self.finish_prop_reads();
    }

    /// Paths read from props in script: `props.mall.name`, destructured
    /// `mall.name`, Options API `this.mall.name`.
    fn finish_prop_reads(&mut self) {
        let option_props: Vec<String> = self
            .facts
            .options
            .iter()
            .flat_map(|o| o.props.iter().map(|p| p.name.clone()))
            .collect();
        let mut out: Vec<String> = Vec::new();
        for (root, path, _) in &self.chains {
            let full: Vec<String> =
                if let Some((_, prop)) = self.prop_bindings.iter().find(|(b, _)| b == root) {
                    if prop.is_empty() {
                        path.clone()
                    } else {
                        std::iter::once(prop.clone())
                            .chain(path.iter().cloned())
                            .collect()
                    }
                } else if root == "this" && path.first().is_some_and(|f| option_props.contains(f)) {
                    path.clone()
                } else {
                    continue;
                };
            if full.len() < 2 {
                continue;
            }
            let p = full.join(".").replace(".[]", "[]");
            if !out.contains(&p) {
                out.push(p);
            }
        }
        // Keep leaves.
        let all = out.clone();
        out.retain(|r| {
            !all.iter().any(|o| {
                o.len() > r.len() && o.starts_with(r.as_str()) && o.as_bytes()[r.len()] == b'.'
            })
        });
        out.sort();
        out.truncate(300);
        self.facts.prop_reads = out;
    }

    /// `return x.data`, `return { malls: r.data }`, `return api.get(…)` and
    /// arrow bodies (`r => ({ malls: r.data })`).
    fn on_return(&mut self, e: &Expression<'a>, at: u32) {
        // Only what the top-level function itself returns (`.then` callbacks
        // count as part of it), not returns of functions nested inside it.
        let depth = self
            .fn_stack
            .iter()
            .filter(|s| !self.then_cbs.contains(s))
            .count();
        if depth != 1 {
            return;
        }
        let mut e = strip_ts(e);
        while let Expression::AwaitExpression(a) = e {
            e = strip_ts(&a.argument);
        }
        let value = |v: &Expression<'a>| {
            let v = strip_ts(v);
            match v {
                Expression::LogicalExpression(l) => member_chain(&l.left),
                _ => member_chain(v),
            }
        };
        match e {
            Expression::CallExpression(c) => {
                // `.catch()` / `.finally()` pass the value through; `.then(cb)`
                // returns what `cb` returns (recorded from its own body).
                let mut c = &**c;
                while let Some(m) = c.callee.without_parentheses().as_member_expression() {
                    match m.static_property_name() {
                        Some("catch" | "finally") => match m.object().without_parentheses() {
                            Expression::CallExpression(inner) => c = inner,
                            _ => return,
                        },
                        Some("then") => return,
                        _ => break,
                    }
                }
                self.direct_returns.insert((c.span.start, c.span.end));
            }
            Expression::ObjectExpression(o) => {
                for p in &o.properties {
                    match p {
                        ObjectPropertyKind::ObjectProperty(p) => {
                            if let Some(k) = p.key.static_name()
                                && let Some((root, path)) = value(&p.value)
                            {
                                self.returns.push((k.to_string(), root, path, at));
                            }
                        }
                        ObjectPropertyKind::SpreadProperty(sp) => {
                            if let Some((root, path)) = member_chain(&sp.argument) {
                                self.returns.push((String::new(), root, path, at));
                            }
                        }
                    }
                }
            }
            other => {
                if let Some((root, path)) = value(other) {
                    self.returns.push((String::new(), root, path, at));
                }
            }
        }
    }

    // ------------------------------------------------------------ calls

    fn on_call<'b>(&mut self, c: &'b CallExpression<'a>) {
        if self.handled.contains(&c.span.start) {
            return;
        }
        self.record_api_call(c);
        self.record_i18n_use(c);
        self.record_db_table(c);
        let is_api = self
            .calls
            .last()
            .is_some_and(|(_, start, _, _)| *start == c.span.start);
        if let Expression::StaticMemberExpression(mm) = c.callee.without_parentheses()
            && matches!(mm.object.without_parentheses(), Expression::Identifier(_))
            && self.facts.member_calls.len() < 2000
        {
            let n = self.compact(mm.span, 80);
            if !self.facts.member_calls.contains(&n) {
                self.facts.member_calls.push(n);
            }
        }
        // A body that spoofs its method, passed to a project function.
        if !is_api && self.facts.spoof_calls.len() < 2000 {
            let callee = match c.callee.without_parentheses() {
                Expression::Identifier(i) => Some(i.name.to_string()),
                Expression::StaticMemberExpression(m) => match m.object.without_parentheses() {
                    Expression::Identifier(o) => Some(format!("{}.{}", o.name, m.property.name)),
                    _ => None,
                },
                _ => None,
            };
            if let Some(callee) =
                callee.filter(|c| !c.starts_with("Object.") && !c.starts_with("console."))
            {
                for (i, a) in c.arguments.iter().enumerate() {
                    // `@helper` (built by an imported helper) is resolved when
                    // files are linked.
                    if let Some(m) = a.as_expression().and_then(|e| self.spoof_of(e)) {
                        self.facts.spoof_calls.push(SpoofCall {
                            callee: callee.clone(),
                            index: i as u32,
                            method: m,
                        });
                    }
                }
            }
        }
        if !is_api
            && let Expression::Identifier(id) = c.callee.without_parentheses()
            && !NOT_DATA_FNS.contains(&id.name.as_str())
        {
            self.fn_calls
                .push((id.name.to_string(), c.span.start, c.span.end, self.scope()));
        }
        // `list.value.push(...res.data)`: the list now holds the result's items.
        if let Some(m) = c.callee.without_parentheses().as_member_expression()
            && matches!(m.static_property_name(), Some("push" | "unshift"))
        {
            let target = self.compact(m.object().span(), 60);
            for a in &c.arguments {
                let e = match a {
                    Argument::SpreadElement(sp) => &sp.argument,
                    other => match other.as_expression() {
                        Some(e) => e,
                        None => continue,
                    },
                };
                if let Some((root, path)) = member_chain(e) {
                    self.assigns
                        .push((target.clone(), root, path, c.span.start));
                }
            }
        }
        // `api.get(url).then((res) => …)`: bind the callback's parameter.
        if let Some(m) = c.callee.without_parentheses().as_member_expression()
            && m.static_property_name() == Some("then")
            && let Expression::CallExpression(inner) = m.object().without_parentheses()
            && let Some(cb) = c.arguments.first().and_then(|a| a.as_expression())
        {
            let (param, span, fn_span) = match strip_ts(cb) {
                Expression::ArrowFunctionExpression(f) => {
                    (f.params.items.first(), f.span, Some(f.span))
                }
                Expression::FunctionExpression(f) => (
                    f.params.items.first(),
                    f.span,
                    f.body.as_ref().map(|b| b.span),
                ),
                _ => (None, c.span, None),
            };
            if let Some(fs) = fn_span {
                self.then_cbs.insert((fs.start, fs.end));
            }
            if let Some(p) = param {
                for (n, path) in binding_paths(&p.pattern) {
                    self.result_bindings
                        .push((inner.span.start, n, (span.start, span.end), path));
                }
            }
        }
        let line = self.line(c.span.start);
        let first_str = || {
            c.arguments
                .first()
                .and_then(|a| a.as_expression())
                .and_then(string_value)
        };
        if let Some(name) = callee_name(&c.callee) {
            let is_member = !matches!(&c.callee, Expression::Identifier(_));
            if !is_member {
                match name {
                    "defineProps" if self.kind == ScriptKind::VueSetup => {
                        return self.define_props(c, None);
                    }
                    "withDefaults" if self.kind == ScriptKind::VueSetup => {
                        let inner = c
                            .arguments
                            .first()
                            .and_then(|a| a.as_expression())
                            .and_then(|e| match strip_ts(e) {
                                Expression::CallExpression(i)
                                    if callee_name(&i.callee) == Some("defineProps") =>
                                {
                                    Some(&**i)
                                }
                                _ => None,
                            });
                        if let Some(inner) = inner {
                            let defaults =
                                c.arguments.get(1).and_then(|a| a.as_expression()).map(|d| {
                                    match strip_ts(d) {
                                        Expression::ObjectExpression(o) => o
                                            .properties
                                            .iter()
                                            .filter_map(|p| match p {
                                                ObjectPropertyKind::ObjectProperty(p) => Some((
                                                    p.key.static_name()?.to_string(),
                                                    self.compact(p.value.span(), MAX_DEFAULT),
                                                )),
                                                _ => None,
                                            })
                                            .collect(),
                                        _ => Vec::new(),
                                    }
                                });
                            self.handled.insert(inner.span.start);
                            self.define_props(inner, defaults);
                        }
                        return;
                    }
                    "defineEmits" if self.kind == ScriptKind::VueSetup => {
                        let mut emits =
                            match c.type_arguments.as_ref().and_then(|t| t.params.first()) {
                                Some(ty) => self.type_emits(ty),
                                None => Vec::new(),
                            };
                        if let Some(arg) = c.arguments.first().and_then(|a| a.as_expression()) {
                            emits.extend(self.names_with_lines(self.resolve(arg)));
                        }
                        self.facts.setup.emits.extend(emits);
                        return;
                    }
                    "defineModel" if self.kind == ScriptKind::VueSetup => {
                        let name = first_str().unwrap_or_else(|| "modelValue".into());
                        let mut prop = Prop {
                            name,
                            r#type: c
                                .type_arguments
                                .as_ref()
                                .and_then(|t| t.params.first())
                                .map(|t| self.compact(t.span(), 60)),
                            line,
                            ..Prop::default()
                        };
                        let opts = c
                            .arguments
                            .iter()
                            .filter_map(|a| a.as_expression())
                            .find_map(|e| match strip_ts(e) {
                                Expression::ObjectExpression(o) => Some(&**o),
                                _ => None,
                            });
                        if let Some(o) = opts {
                            for p in &o.properties {
                                if let ObjectPropertyKind::ObjectProperty(p) = p {
                                    match p.key.static_name().as_deref() {
                                        Some("required") => {
                                            prop.required = matches!(&p.value, Expression::BooleanLiteral(b) if b.value)
                                        }
                                        Some("default") => {
                                            prop.default =
                                                Some(self.compact(p.value.span(), MAX_DEFAULT))
                                        }
                                        Some("type") if prop.r#type.is_none() => {
                                            prop.r#type = Some(self.compact(p.value.span(), 60))
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        self.facts.setup.models.push(prop);
                        return;
                    }
                    "defineSlots" if self.kind == ScriptKind::VueSetup => {
                        if let Some(ty) = c.type_arguments.as_ref().and_then(|t| t.params.first()) {
                            let slots = self.type_emits(ty);
                            let mut names: Vec<NamedLine> = slots;
                            if let TSType::TSTypeLiteral(l) = ty {
                                for m in &l.members {
                                    if let TSSignature::TSMethodSignature(m) = m
                                        && let Some(n) = m.key.static_name()
                                    {
                                        names.push(NamedLine {
                                            name: n.to_string(),
                                            line: self.line(m.span.start),
                                        });
                                    }
                                }
                            }
                            self.facts.setup.slots.extend(names);
                        }
                        return;
                    }
                    "defineExpose" if self.kind == ScriptKind::VueSetup => {
                        if let Some(arg) = c.arguments.first().and_then(|a| a.as_expression()) {
                            self.facts.setup.expose = self.keys(arg, false);
                        }
                        return;
                    }
                    "defineOptions" => {
                        if let Some(Expression::ObjectExpression(o)) = c
                            .arguments
                            .first()
                            .and_then(|a| a.as_expression())
                            .map(strip_ts)
                        {
                            for p in &o.properties {
                                if let ObjectPropertyKind::ObjectProperty(p) = p
                                    && p.key.static_name().as_deref() == Some("name")
                                {
                                    self.facts.setup.name = string_value(&p.value);
                                }
                            }
                        }
                        return;
                    }
                    "definePageMeta" => {
                        if let Some(Expression::ObjectExpression(o)) = c
                            .arguments
                            .first()
                            .and_then(|a| a.as_expression())
                            .map(strip_ts)
                        {
                            for p in &o.properties {
                                if let ObjectPropertyKind::ObjectProperty(p) = p
                                    && let Some(k) = p.key.static_name()
                                {
                                    let v = match string_value(&p.value) {
                                        Some(s) => s,
                                        None => self.compact(p.value.span(), 120),
                                    };
                                    self.facts.setup.page_meta.push((k.to_string(), v));
                                }
                            }
                        }
                        return;
                    }
                    "defineStore" => return self.define_store(c),
                    "createRouter" => {
                        if let Some(Expression::ObjectExpression(o)) = c
                            .arguments
                            .first()
                            .and_then(|a| a.as_expression())
                            .map(strip_ts)
                        {
                            self.router_options(o);
                        }
                        return;
                    }
                    "createStore" => {
                        // Vuex 4 `createStore({...})` (Pinia uses createPinia).
                        if let Some(Expression::ObjectExpression(o)) = c
                            .arguments
                            .first()
                            .and_then(|a| a.as_expression())
                            .map(strip_ts)
                            && self.facts.vuex_module.as_ref().is_none_or(|m| !m.root)
                        {
                            let mut m = self.vuex_object(o);
                            m.root = true;
                            self.facts.vuex_module = Some(m);
                        }
                        return;
                    }
                    "provide" | "inject" => {
                        let key = c
                            .arguments
                            .first()
                            .and_then(|a| a.as_expression())
                            .and_then(|e| match strip_ts(e) {
                                Expression::StringLiteral(s) => Some(s.value.to_string()),
                                Expression::Identifier(i) => Some(i.name.to_string()),
                                _ => None,
                            });
                        if let Some(key) = key {
                            if name == "provide" {
                                self.facts.setup.provide.push(key);
                            } else {
                                self.facts.setup.inject.push(key);
                            }
                        }
                    }
                    "mapState" | "mapGetters" | "mapActions" | "mapMutations" => {
                        let kind = match name {
                            "mapState" => "state",
                            "mapGetters" => "getter",
                            "mapActions" => "action",
                            _ => "mutation",
                        };
                        let ns = if c.arguments.len() > 1 {
                            first_str()
                        } else {
                            None
                        };
                        // Pinia's mapState(useStore, [...]) has an identifier first; skip.
                        let pinia = matches!(
                            c.arguments
                                .first()
                                .and_then(|a| a.as_expression())
                                .map(strip_ts),
                            Some(Expression::Identifier(_))
                        );
                        if !pinia {
                            for n in self.map_helper_names(c) {
                                let (namespace, n) = split_ns(ns.as_deref(), &n);
                                self.facts.vuex_uses.push(VuexUse {
                                    kind: kind.into(),
                                    namespace,
                                    name: n,
                                    line,
                                });
                            }
                        }
                    }
                    _ => {}
                }
                if is_composable_name(name) {
                    self.facts.composables.push(NamedLine {
                        name: name.to_owned(),
                        line,
                    });
                } else if !self.consts.contains_key(name)
                    && !self.functions.contains_key(name)
                    && !self.facts.imports.iter().any(|i| i.binds(name))
                    && !self.facts.unbound_calls.iter().any(|c| c.name == name)
                {
                    self.facts.unbound_calls.push(NamedLine {
                        name: name.to_owned(),
                        line,
                    });
                }
                if (self.emit_fns.contains(name)
                    || name == "emit" && self.kind != ScriptKind::Module)
                    && let Some(e) = first_str()
                {
                    self.facts.emitted.push(NamedLine { name: e, line });
                }
                return;
            }
            // Member calls.
            let Some(member) = c.callee.without_parentheses().as_member_expression() else {
                return;
            };
            let object = member.object().without_parentheses();
            match name {
                "$emit" => {
                    if let Some(e) = first_str() {
                        self.facts.emitted.push(NamedLine { name: e, line });
                    }
                }
                "emit" if matches!(object, Expression::Identifier(i) if matches!(i.name.as_str(), "ctx" | "context")) => {
                    if let Some(e) = first_str() {
                        self.facts.emitted.push(NamedLine { name: e, line });
                    }
                }
                "component" => {
                    self.saw_component_call = true;
                    if c.arguments.len() == 2
                        && let Some(reg_name) = first_str()
                    {
                        let second = c.arguments[1].as_expression().map(strip_ts);
                        let (local, source) = match second {
                            Some(Expression::Identifier(i)) => (Some(i.name.to_string()), None),
                            Some(other) => (None, lazy_import_source(other)),
                            None => (None, None),
                        };
                        self.facts.registrations.push(Registration {
                            name: reg_name,
                            local,
                            source,
                            plugin: false,
                            line,
                        });
                    }
                }
                "use" if c.arguments.len() <= 2 => {
                    if let Some(Expression::Identifier(i)) = c
                        .arguments
                        .first()
                        .and_then(|a| a.as_expression())
                        .map(strip_ts)
                    {
                        self.facts.registrations.push(Registration {
                            name: i.name.to_string(),
                            local: Some(i.name.to_string()),
                            source: None,
                            plugin: true,
                            line,
                        });
                    }
                }
                "context" if matches!(object, Expression::Identifier(i) if i.name == "require") => {
                    if let Some(dir) = first_str() {
                        let recursive = matches!(c.arguments.get(1).and_then(|a| a.as_expression()), Some(Expression::BooleanLiteral(b)) if b.value);
                        self.facts.registration_globs.push(RegistrationGlob {
                            pattern: dir,
                            recursive,
                            line,
                        });
                    }
                }
                "glob" | "globEager"
                    if matches!(object, Expression::ImportMeta(_))
                        || self.text(object.span()) == "import.meta" =>
                {
                    if let Some(p) = first_str() {
                        let recursive = p.contains("**");
                        self.facts.registration_globs.push(RegistrationGlob {
                            pattern: p,
                            recursive,
                            line,
                        });
                    }
                }
                "dispatch" | "commit" if is_store_object(object) => {
                    if let Some(full) = first_str() {
                        let (namespace, n) = split_ns(None, &full);
                        let kind = if name == "dispatch" {
                            "action"
                        } else {
                            "mutation"
                        };
                        self.facts.vuex_uses.push(VuexUse {
                            kind: kind.into(),
                            namespace,
                            name: n,
                            line,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn define_props<'b>(
        &mut self,
        c: &'b CallExpression<'a>,
        defaults: Option<Vec<(String, String)>>,
    ) {
        let mut unresolved = None;
        let mut props = match c.type_arguments.as_ref().and_then(|t| t.params.first()) {
            Some(ty) => self.type_props(ty, &mut unresolved),
            None => Vec::new(),
        };
        if let Some(arg) = c.arguments.first().and_then(|a| a.as_expression()) {
            props.extend(self.runtime_props(self.resolve(arg)));
        }
        let destructured = self
            .prop_defaults
            .get(&c.span.start)
            .cloned()
            .unwrap_or_default();
        for (name, value) in defaults.unwrap_or_default().into_iter().chain(destructured) {
            if let Some(p) = props.iter_mut().find(|p| p.name == name) {
                p.default = Some(value);
            }
        }
        self.facts.setup.props.extend(props);
        if unresolved.is_some() {
            self.facts.setup.props_unresolved = unresolved;
        }
    }
}

impl<'s, 'a> Visit<'a> for Analyzer<'s, 'a>
where
    's: 'a,
{
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        let in_copy = self
            .copied_spans
            .iter()
            .any(|(a, b)| *a <= it.span.start && it.span.start <= *b);
        // `Object.entries(payload).forEach(([k, v]) => form.append(k, v))`, also
        // through a local helper: the form gets payload's keys.
        if let Some(m) = it.callee.without_parentheses().as_member_expression()
            && m.static_property_name() == Some("forEach")
            && let Expression::CallExpression(inner) = m.object().without_parentheses()
            && let Some(src) = self.entries_source(inner)
            && let Some(cb) = it.arguments.first().and_then(|a| a.as_expression())
        {
            self.copy_into(src, cb, it.span.start);
        }
        // A body handed to other code may get keys codemap does not see.
        if !in_copy && self.api_call_parts(it).is_none() {
            let callee = self.compact(it.callee.span(), 40);
            let harmless = callee == "JSON.stringify"
                || callee.starts_with("console.")
                || callee.starts_with("Object.")
                || callee.ends_with(".append")
                || callee.ends_with(".set");
            if !harmless {
                for a in it.arguments.iter().filter_map(|a| a.as_expression()) {
                    if let Expression::Identifier(id) = strip_ts(a)
                        && let Some(body) = self.body_mut(id.name.as_str(), id.span.start)
                    {
                        body.merge(Sent::unknown(format!(
                            "`{}` is passed to `{callee}(…)`, which may add keys",
                            id.name
                        )));
                    }
                }
            }
        }
        // `form.append('name', …)` / `params.set('q', …)` on a body built here
        // (a copy loop's own `append(k, v)` is already counted).
        if !in_copy
            && let Some(m) = it.callee.without_parentheses().as_member_expression()
            && matches!(m.static_property_name(), Some("append" | "set"))
            && let Expression::Identifier(obj) = m.object().without_parentheses()
            && let Some(body) = self.body_mut(obj.name.as_str(), it.span.start)
        {
            match it
                .arguments
                .first()
                .and_then(|a| a.as_expression())
                .and_then(string_value)
            {
                Some(k) => body.push(top_key(&k)),
                None => body.merge(Sent::unknown(format!(
                    "`{}` gets a key that is not a string literal",
                    obj.name
                ))),
            }
        }
        // `form.append('_method', 'PUT')` on FormData.
        if let Some(m) = it.callee.without_parentheses().as_member_expression()
            && m.static_property_name() == Some("append")
            && let Expression::Identifier(obj) = m.object().without_parentheses()
            && let (Some(k), Some(v)) = (
                it.arguments
                    .first()
                    .and_then(|a| a.as_expression())
                    .and_then(string_value),
                it.arguments
                    .get(1)
                    .and_then(|a| a.as_expression())
                    .and_then(string_value),
            )
            && k == "_method"
        {
            let m = self.maybe(v.to_ascii_uppercase(), it.span.start);
            self.set_spoof(obj.name.as_str(), m, it.span.start);
        }
        self.on_call(it);
        walk::walk_call_expression(self, it);
    }

    fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
        // `new VueRouter({ routes })` / `new Router({ routes })`.
        if matches!(callee_name(&it.callee), Some("Router" | "VueRouter"))
            && let Some(Expression::ObjectExpression(o)) = it
                .arguments
                .first()
                .and_then(|a| a.as_expression())
                .map(strip_ts)
        {
            self.router_options(o);
        }
        // `new Vuex.Store({...})` / `new Store({...})`, exported or not.
        if callee_name(&it.callee) == Some("Store")
            && let Some(Expression::ObjectExpression(o)) = it
                .arguments
                .first()
                .and_then(|a| a.as_expression())
                .map(strip_ts)
            && self.facts.vuex_module.as_ref().is_none_or(|m| !m.root)
        {
            let mut m = self.vuex_object(o);
            m.root = true;
            self.facts.vuex_module = Some(m);
        }
        walk::walk_new_expression(self, it);
    }

    fn visit_function_body(&mut self, it: &FunctionBody<'a>) {
        self.scopes.push((it.span.start, it.span.end));
        // An arrow's block body is the same function as the arrow.
        let own = !self
            .fn_stack
            .last()
            .is_some_and(|(s, e)| *s < it.span.start && *e == it.span.end);
        if own {
            self.fn_stack.push((it.span.start, it.span.end));
        }
        walk::walk_function_body(self, it);
        if own {
            self.fn_stack.pop();
        }
        self.scopes.pop();
    }

    fn visit_jsx_opening_element(&mut self, it: &JSXOpeningElement<'a>) {
        let name = match &it.name {
            JSXElementName::IdentifierReference(r) => Some(r.name.to_string()),
            JSXElementName::Identifier(i) => Some(i.name.to_string()),
            JSXElementName::MemberExpression(m) => Some(self.compact(m.span, 80)),
            _ => None,
        };
        if let Some(n) = name
            && n.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && self.facts.jsx_tags.len() < 800
        {
            let line = self.line(it.span.start);
            self.facts.jsx_tags.push(NamedLine { name: n, line });
        }
        walk::walk_jsx_opening_element(self, it);
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        // `for (const [k, v] of Object.entries(payload)) form.append(k, v)`.
        if let Expression::CallExpression(inner) = it.right.without_parentheses()
            && let Some(src) = self.entries_source(inner)
        {
            let (start, end) = (it.body.span().start, it.body.span().end);
            let names = idents_in_statement(&it.body);
            self.copy_names(src, &names, (start, end), it.span.start);
        }
        walk::walk_for_of_statement(self, it);
    }

    fn visit_formal_parameters(&mut self, it: &FormalParameters<'a>) {
        let before = self.params_end;
        self.params_end = it.span.end;
        for (i, p) in it.items.iter().enumerate() {
            if let BindingPattern::BindingIdentifier(id) = &p.pattern {
                self.param_index
                    .push((id.name.to_string(), i as u32, (p.span.start, it.span.end)));
            }
        }
        walk::walk_formal_parameters(self, it);
        self.params_end = before;
    }

    fn visit_object_property(&mut self, it: &ObjectProperty<'a>) {
        // `seg() { return v ? 'a' : 'b' }` / `seg: () => …`: values of `this.seg`.
        if let Some(k) = it.key.static_name() {
            let returned: Option<&Expression<'a>> = match strip_ts(&it.value) {
                Expression::FunctionExpression(f) => {
                    f.body.as_ref().and_then(|b| match b.statements.as_slice() {
                        [Statement::ReturnStatement(r)] => r.argument.as_ref(),
                        _ => None,
                    })
                }
                Expression::ArrowFunctionExpression(a) => a.get_expression(),
                _ => None,
            };
            if let Some(v) = returned
                .and_then(|r| self.string_options(r, 1))
                .filter(|v| v.len() > 1 && v.len() <= MAX_PREFIXES)
            {
                self.member_options.insert(k.to_string(), v);
            }
        }
        if matches!(
            strip_ts(&it.value),
            Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
        ) && let Some(k) = it.key.static_name()
        {
            self.names
                .push((it.value.span().start, it.value.span().end, k.to_string()));
        }
        walk::walk_object_property(self, it);
    }

    fn visit_formal_parameter(&mut self, it: &FormalParameter<'a>) {
        if let BindingPattern::BindingIdentifier(id) = &it.pattern {
            let v = it
                .type_annotation
                .as_ref()
                .and_then(|t| self.type_options(&t.type_annotation, 0));
            self.param_types
                .push((id.name.to_string(), (it.span.start, self.params_end), v));
        }
        walk::walk_formal_parameter(self, it);
    }

    fn visit_if_statement(&mut self, it: &IfStatement<'a>) {
        self.cond_spans
            .push((it.consequent.span().start, it.consequent.span().end));
        if let Some(alt) = &it.alternate {
            self.cond_spans.push((alt.span().start, alt.span().end));
        }
        walk::walk_if_statement(self, it);
    }

    fn visit_conditional_expression(&mut self, it: &ConditionalExpression<'a>) {
        for e in [&it.consequent, &it.alternate] {
            self.cond_spans.push((e.span().start, e.span().end));
        }
        walk::walk_conditional_expression(self, it);
    }

    fn visit_logical_expression(&mut self, it: &LogicalExpression<'a>) {
        self.cond_spans
            .push((it.right.span().start, it.right.span().end));
        walk::walk_logical_expression(self, it);
    }

    fn visit_return_statement(&mut self, it: &ReturnStatement<'a>) {
        if let Some(e) = &it.argument {
            self.on_return(e, it.span.start);
        }
        walk::walk_return_statement(self, it);
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        self.fn_stack.push((it.span.start, it.span.end));
        if let Some(e) = it.get_expression() {
            self.on_return(e, e.span().start);
        }
        self.scopes.push((it.span.start, it.span.end));
        walk::walk_arrow_function_expression(self, it);
        self.scopes.pop();
        self.fn_stack.pop();
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if let BindingPattern::BindingIdentifier(id) = &it.id
            && let Some(init) = it.init.as_ref()
            && matches!(
                strip_ts(init),
                Expression::ArrowFunctionExpression(_)
                    | Expression::FunctionExpression(_)
                    | Expression::ObjectExpression(_)
            )
        {
            self.names
                .push((init.span().start, init.span().end, id.name.to_string()));
        }
        // A constant holding a small choice of strings (a URL prefix).
        if let BindingPattern::BindingIdentifier(id) = &it.id
            && let Some(init) = it.init.as_ref()
            && let Some(opts) = self.string_options(init, 0)
        {
            self.local_options.insert(id.name.to_string(), opts);
        }
        // A request body built in a variable, kept for the call that sends it;
        // any other value hides an outer one of the same name.
        if let BindingPattern::BindingIdentifier(id) = &it.id {
            let scope = self.scope();
            let body = it.init.as_ref().and_then(|init| self.body_init(init));
            let name = id.name.as_str();
            if body.is_some() || self.bodies.contains_key(name) {
                let body = body.unwrap_or_else(|| {
                    Sent::unknown(format!("the payload `{name}` is not an object built here"))
                });
                let list = self.bodies.entry(name.to_owned()).or_default();
                list.retain(|(s, _)| *s != scope);
                list.push((scope, body));
            }
        }
        // A request body that spoofs its method, kept for the call that sends it.
        if let BindingPattern::BindingIdentifier(id) = &it.id {
            let m = it.init.as_ref().and_then(|init| self.spoof_of(init));
            let name = id.name.as_str();
            if m.is_some() || self.spoofs.contains_key(name) {
                let scope = self.scope();
                let list = self.spoofs.entry(name.to_owned()).or_default();
                list.retain(|(s, _)| *s != scope);
                list.push((scope, m));
            }
        }
        // Possible HTTP wrappers: `const api = { get(endpoint, id) {…} }` and
        // `const getRecord = (endpoint, id) => …`.
        if let BindingPattern::BindingIdentifier(id) = &it.id
            && let Some(init) = it.init.as_ref().map(strip_ts)
        {
            let name = id.name.to_string();
            let mut found: Vec<WrapperFn> = Vec::new();
            match init {
                Expression::ObjectExpression(o) => {
                    for p in &o.properties {
                        if let ObjectPropertyKind::ObjectProperty(p) = p
                            && let Some(k) = p.key.static_name()
                        {
                            let (params, span) = match strip_ts(&p.value) {
                                Expression::FunctionExpression(f) => (&f.params, f.span),
                                Expression::ArrowFunctionExpression(f) => (&f.params, f.span),
                                _ => continue,
                            };
                            let (names, defaults) = self.param_info(params);
                            found.push((
                                format!("{name}.{k}"),
                                names,
                                defaults,
                                (span.start, span.end),
                            ));
                        }
                    }
                }
                Expression::ArrowFunctionExpression(f) => {
                    let (names, defaults) = self.param_info(&f.params);
                    found.push((name, names, defaults, (f.span.start, f.span.end)));
                }
                Expression::FunctionExpression(f) => {
                    let (names, defaults) = self.param_info(&f.params);
                    found.push((name, names, defaults, (f.span.start, f.span.end)));
                }
                _ => {}
            }
            self.wrappers.extend(found);
        }
        // `const url = (id) => \`/merchant/outlets/${id}\``: a URL builder.
        if let BindingPattern::BindingIdentifier(id) = &it.id
            && let Some(Expression::ArrowFunctionExpression(f)) = it.init.as_ref().map(strip_ts)
            && let Some(body) = f.get_expression()
            && matches!(
                strip_ts(body),
                Expression::TemplateLiteral(_)
                    | Expression::StringLiteral(_)
                    | Expression::BinaryExpression(_)
            )
        {
            let mut parts = Vec::new();
            if self.url_parts(body, &mut parts, 0).is_some()
                && parts.first().is_some_and(|(is_static, t)| {
                    // A path (`/orders/`, `admin/asset/`), not arbitrary text.
                    *is_static
                        && t.contains('/')
                        && !t.contains(char::is_whitespace)
                        && !t.starts_with(['.', '#'])
                })
            {
                let mut params = Vec::new();
                let mut defaults = Vec::new();
                for p in &f.params.items {
                    let (name, default) = match &p.pattern {
                        BindingPattern::AssignmentPattern(a) => {
                            (binding_names(&a.left).into_iter().next(), Some(&a.right))
                        }
                        other => (binding_names(other).into_iter().next(), None),
                    };
                    let default = p.initializer.as_deref().or(default);
                    params.push(name.unwrap_or_default());
                    defaults.push(default.and_then(|d| {
                        let mut v = Vec::new();
                        self.url_parts(d, &mut v, 0).map(|_| v)
                    }));
                }
                self.url_helpers.insert(
                    id.name.to_string(),
                    UrlHelper {
                        params,
                        defaults,
                        parts,
                    },
                );
            }
        }
        // `const props = defineProps(…)` / `const { mall } = withDefaults(defineProps(…), …)`.
        if let Some(Expression::CallExpression(c)) = it.init.as_ref().map(strip_ts)
            && matches!(
                c.callee.without_parentheses(),
                Expression::Identifier(i) if i.name == "defineProps" || i.name == "withDefaults"
            )
        {
            match &it.id {
                BindingPattern::BindingIdentifier(id) => {
                    self.prop_bindings
                        .push((id.name.to_string(), String::new()));
                }
                BindingPattern::ObjectPattern(o) => {
                    for p in &o.properties {
                        if let Some(k) = p.key.static_name()
                            && let Some(n) = binding_names(&p.value).into_iter().next()
                        {
                            self.prop_bindings.push((n, k.to_string()));
                        }
                    }
                }
                _ => {}
            }
        }
        // `const res = await api.get(…)` / `const { data } = await useFetch(…)`.
        if let Some(init) = &it.init {
            let mut e = strip_ts(init);
            while let Expression::AwaitExpression(a) = e {
                e = strip_ts(&a.argument);
            }
            if let Expression::CallExpression(c) = e {
                let scope = self.scope();
                let names: Vec<(String, Vec<String>)> = match &it.id {
                    BindingPattern::ObjectPattern(o) => {
                        // Prefer the `data` property (useFetch / axios: the body);
                        // else every name, with the key it was taken from.
                        let data = o
                            .properties
                            .iter()
                            .find(|p| p.key.static_name().as_deref() == Some("data"));
                        match data {
                            Some(p) => binding_names(&p.value)
                                .into_iter()
                                .map(|n| (n, Vec::new()))
                                .collect(),
                            None => binding_paths(&it.id),
                        }
                    }
                    other => binding_names(other)
                        .into_iter()
                        .map(|n| (n, Vec::new()))
                        .collect(),
                };
                for (n, path) in names {
                    self.result_bindings.push((c.span.start, n, scope, path));
                }
            }
        }
        walk::walk_variable_declarator(self, it);
    }

    fn visit_assignment_expression(&mut self, it: &AssignmentExpression<'a>) {
        let target = self.compact(it.left.span(), 60);
        // `payload.note = …` / `payload['note'] = …` on a body built here.
        if let Some((root, rest)) = target.split_once(['.', '['])
            && let Some(body) = self.body_mut(root, it.span.start)
        {
            let quoted = rest
                .strip_prefix(['\'', '"'])
                .and_then(|r| r.split_once(['\'', '"']))
                .map(|(k, _)| k);
            let key = if target.as_bytes()[root.len()] == b'.' {
                Some(top_key(rest))
            } else {
                quoted
            };
            match key {
                Some(k) if !k.is_empty() => body.push(k),
                _ => body.merge(Sent::unknown(format!("`{root}` gets a computed key"))),
            }
        }
        // `payload._method = 'PUT'` / `payload['_method'] = 'PUT'`.
        if let Some(root) = target
            .strip_suffix("._method")
            .or_else(|| target.strip_suffix("['_method']"))
            .or_else(|| target.strip_suffix("[\"_method\"]"))
            && let Some(m) = string_value(&it.right)
        {
            let m = self.maybe(m.to_ascii_uppercase(), it.span.start);
            self.set_spoof(root, m, it.span.start);
        }
        let mut right = strip_ts(&it.right);
        while let Expression::AwaitExpression(a) = right {
            right = strip_ts(&a.argument);
        }
        if let Some((root, path)) = member_chain(right) {
            self.assigns.push((target, root, path, it.span.start));
        } else {
            // `list = [...list, ...res.data]` / `list = list.concat(res.data)`.
            let parts: Vec<&Expression<'a>> = match right {
                Expression::ArrayExpression(a) => a
                    .elements
                    .iter()
                    .filter_map(|el| match el {
                        ArrayExpressionElement::SpreadElement(sp) => Some(&sp.argument),
                        _ => None,
                    })
                    .collect(),
                Expression::CallExpression(c)
                    if c.callee
                        .without_parentheses()
                        .as_member_expression()
                        .and_then(|m| m.static_property_name())
                        == Some("concat") =>
                {
                    c.arguments
                        .iter()
                        .filter_map(|a| a.as_expression())
                        .collect()
                }
                _ => Vec::new(),
            };
            for e in parts {
                if let Some((root, path)) = member_chain(e)
                    && self.compact(e.span(), 60) != target
                {
                    self.assigns
                        .push((target.clone(), root, path, it.span.start));
                }
            }
        }
        walk::walk_assignment_expression(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        if let Some(source) = string_value(&it.source) {
            self.facts.imports.push(Import {
                source,
                dynamic: true,
                line: self.line(it.span.start),
                ..Import::default()
            });
        }
        walk::walk_import_expression(self, it);
    }

    fn visit_member_expression(&mut self, it: &MemberExpression<'a>) {
        // `this.$store.getters['ns/x']` / `$store.getters.x` / `store.state.cart.items`.
        if let Some(prop) = it
            .static_property_name()
            .map(|p| p.to_string())
            .or_else(|| match it {
                MemberExpression::ComputedMemberExpression(c) => string_value(&c.expression),
                _ => None,
            })
            && let Some(parent) = it.object().without_parentheses().as_member_expression()
            && let Some(kind) = parent.static_property_name()
            && (kind == "getters" || kind == "state")
            && is_store_object(parent.object().without_parentheses())
        {
            let line = self.line(it.span().start);
            let (namespace, name) = if kind == "getters" {
                split_ns(None, &prop)
            } else {
                (None, prop)
            };
            let kind = if kind == "getters" { "getter" } else { "state" };
            self.facts.vuex_uses.push(VuexUse {
                kind: kind.into(),
                namespace,
                name,
                line,
            });
        }
        if let Some((root, path)) = member_chain_of(it) {
            self.chains.push((root, path, it.span().start));
        }
        walk::walk_member_expression(self, it);
    }
}

// ---------------------------------------------------------------- helpers

/// Plain calls whose result is never response data.
const NOT_DATA_FNS: &[&str] = &[
    "ref",
    "reactive",
    "computed",
    "watch",
    "watchEffect",
    "shallowRef",
    "toRef",
    "toRefs",
    "unref",
    "isRef",
    "readonly",
    "markRaw",
    "nextTick",
    "onMounted",
    "onBeforeMount",
    "onUnmounted",
    "onBeforeUnmount",
    "defineProps",
    "withDefaults",
    "defineEmits",
    "defineModel",
    "defineExpose",
    "defineSlots",
    "defineOptions",
    "definePageMeta",
    "defineComponent",
    "defineStore",
    "defineNuxtConfig",
    "defineNuxtPlugin",
    "defineNuxtRouteMiddleware",
    "h",
    "require",
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "clearInterval",
    "String",
    "Number",
    "Boolean",
    "Array",
    "Object",
    "Symbol",
    "Promise",
    "parseInt",
    "parseFloat",
    "isNaN",
    "encodeURIComponent",
    "decodeURIComponent",
    "t",
    "$t",
    "tc",
    "emit",
    "inject",
    "provide",
    "useI18n",
    "useRoute",
    "useRouter",
    "navigateTo",
    "alert",
    "confirm",
    "console",
];

/// A request body variable with the function (span) it is declared in.
type ScopedBody = ((u32, u32), Sent);
/// A spoofed method (or a hiding `None`) with the function it is declared in.
type ScopedSpoof = ((u32, u32), Option<String>);
/// A parameter, `(start, end of its list)`, and the strings its type allows.
type TypedParam = (String, (u32, u32), Option<Vec<String>>);

struct CallParts<'b, 'a> {
    method: Option<String>,
    url: &'b Expression<'a>,
    via: String,
    /// A well-known client: any string argument counts as a URL.
    trusted: bool,
}

/// Looks like a translation key (`orders.title`, `Save`), not arbitrary text.
pub fn is_i18n_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 200 && !k.starts_with('/') && !k.contains("://")
}

/// The most values a URL prefix is expanded into.
const MAX_PREFIXES: usize = 8;

impl<'a> Analyzer<'_, 'a> {
    /// The leading variable of a URL (`${prefix(kind)}/contacts`, `base + '/x'`),
    /// when it is a small choice of strings: its possible values.
    fn prefix_options(&self, url: &Expression<'a>) -> (Vec<String>, Option<String>) {
        let lead = match strip_ts(url).without_parentheses() {
            Expression::TemplateLiteral(t)
                if t.quasis.first().is_some_and(|q| q.value.raw.is_empty()) =>
            {
                t.expressions.first()
            }
            Expression::BinaryExpression(b) => {
                let mut left = &b.left;
                while let Expression::BinaryExpression(inner) = left.without_parentheses() {
                    left = &inner.left;
                }
                Some(left)
            }
            _ => None,
        };
        let Some(lead) = lead else {
            return (Vec::new(), None);
        };
        match self.string_options(lead, 0) {
            Some(v) if v.len() <= MAX_PREFIXES => (v, None),
            Some(v) => (
                Vec::new(),
                Some(format!(
                    "prefix not expanded: {} possible values, more than the limit of {MAX_PREFIXES}",
                    v.len()
                )),
            ),
            None => (Vec::new(), Some(self.why_not_expanded(lead))),
        }
    }

    /// Why a URL prefix has no fixed choice of values, in words.
    fn why_not_expanded(&self, e: &Expression<'a>) -> String {
        let text = self.compact(e.span(), 50);
        let why = match strip_ts(e).without_parentheses() {
            Expression::StaticMemberExpression(_) | Expression::ComputedMemberExpression(_) => {
                format!(
                    "it comes from an object property (`{text}`), which codemap does not follow"
                )
            }
            Expression::Identifier(_) => {
                format!("`{text}` is a parameter or a value set at run time")
            }
            Expression::CallExpression(c) => match c.callee.without_parentheses() {
                Expression::Identifier(f)
                    if self.functions.contains_key(f.name.as_str())
                        || self.consts.contains_key(f.name.as_str()) =>
                {
                    format!(
                        "the helper `{}` does not return a fixed choice of strings",
                        f.name
                    )
                }
                _ => format!("it comes from a call codemap cannot read (`{text}`)"),
            },
            Expression::ConditionalExpression(_) => {
                format!("a branch of `{text}` is not a fixed string")
            }
            _ => format!("`{text}` is not a fixed choice of strings"),
        };
        format!("prefix not expanded: {why}")
    }

    /// The strings an expression can be: literals, a condition between
    /// literals, a constant holding one, or a small function returning one.
    fn string_options(&self, e: &Expression<'a>, depth: u8) -> Option<Vec<String>> {
        if depth > 4 {
            return None;
        }
        let out = match strip_ts(e).without_parentheses() {
            Expression::StringLiteral(s) => vec![s.value.to_string()],
            Expression::TemplateLiteral(t) if t.expressions.is_empty() => {
                vec![
                    t.quasis
                        .first()
                        .map(|q| q.value.raw.to_string())
                        .unwrap_or_default(),
                ]
            }
            Expression::ConditionalExpression(c) => {
                let mut v = self.string_options(&c.consequent, depth + 1)?;
                for x in self.string_options(&c.alternate, depth + 1)? {
                    if !v.contains(&x) {
                        v.push(x);
                    }
                }
                v
            }
            Expression::Identifier(id) => {
                let name = id.name.as_str();
                let (start, _) = self.scope();
                // A parameter of this function: inside an arrow's span, or
                // just before a function's body (after its return type).
                let own_end = self
                    .param_types
                    .iter()
                    .map(|(_, (_, end), _)| *end)
                    .filter(|end| *end <= start && start - *end <= 300)
                    .max();
                let typed = self
                    .param_types
                    .iter()
                    .filter(|(n, (at, end), _)| {
                        n == name && *at <= id.span.start && (*at >= start || Some(*end) == own_end)
                    })
                    .max_by_key(|(_, (at, _), _)| *at);
                if let Some((_, _, v)) = typed {
                    v.clone()?
                } else if let Some(v) = self.local_options.get(name) {
                    v.clone()
                } else {
                    let init = *self.consts.get(name)?;
                    self.string_options(init, depth + 1)?
                }
            }
            Expression::CallExpression(c) => {
                let Expression::Identifier(f) = c.callee.without_parentheses() else {
                    return None;
                };
                self.returned_options(f.name.as_str(), depth)?
            }
            // `this.reportSegment`: a computed property of the component.
            Expression::StaticMemberExpression(m)
                if matches!(
                    m.object.without_parentheses(),
                    Expression::ThisExpression(_)
                ) =>
            {
                self.member_options.get(m.property.name.as_str())?.clone()
            }
            // `ENDPOINT.shops` on a constant object.
            Expression::StaticMemberExpression(m) => {
                let values =
                    self.object_values(&m.object, Some(m.property.name.as_str()), depth)?;
                values.into_iter().flatten().flatten().collect()
            }
            // `ENDPOINT[kind]` on a constant object: every value it holds.
            Expression::ComputedMemberExpression(m) => {
                let key = match strip_ts(&m.expression).without_parentheses() {
                    Expression::StringLiteral(k) => Some(k.value.as_str()),
                    _ => None,
                };
                let values = self.object_values(&m.object, key, depth)?;
                // All of them, or none: one unknown value makes the set unknown.
                values
                    .into_iter()
                    .collect::<Option<Vec<Vec<String>>>>()?
                    .concat()
            }
            _ => return None,
        };
        (!out.is_empty()).then_some(out)
    }

    /// The string values of a constant object's properties (`key` only, when
    /// given); `None` per value that is not a known string.
    fn object_values(
        &self,
        obj: &Expression<'a>,
        key: Option<&str>,
        depth: u8,
    ) -> Option<Vec<Option<Vec<String>>>> {
        let Expression::Identifier(id) = obj.without_parentheses() else {
            return None;
        };
        let init = *self.consts.get(id.name.as_str())?;
        let Expression::ObjectExpression(o) = strip_ts(init).without_parentheses() else {
            return None;
        };
        let mut out = Vec::new();
        for p in &o.properties {
            let oxc_ast::ast::ObjectPropertyKind::ObjectProperty(p) = p else {
                // A spread: values unknown.
                out.push(None);
                continue;
            };
            if let Some(k) = key
                && p.key.static_name().as_deref() != Some(k)
            {
                continue;
            }
            out.push(self.string_options(&p.value, depth + 1));
        }
        (!out.is_empty()).then_some(out)
    }

    /// The strings a type allows: `'a' | 'b'`, or an alias of such a union.
    fn type_options(&self, t: &TSType<'_>, depth: u8) -> Option<Vec<String>> {
        if depth > 3 {
            return None;
        }
        match t {
            TSType::TSLiteralType(l) => match &l.literal {
                TSLiteral::StringLiteral(s) => Some(vec![s.value.to_string()]),
                _ => None,
            },
            TSType::TSUnionType(u) => {
                let mut out: Vec<String> = Vec::new();
                for x in &u.types {
                    for v in self.type_options(x, depth + 1)? {
                        if !out.contains(&v) {
                            out.push(v);
                        }
                    }
                }
                (out.len() <= MAX_PREFIXES).then_some(out)
            }
            TSType::TSParenthesizedType(p) => self.type_options(&p.type_annotation, depth + 1),
            TSType::TSTypeReference(r) if r.type_arguments.is_none() => {
                let TSTypeName::IdentifierReference(id) = &r.type_name else {
                    return None;
                };
                match self.types.get(id.name.as_str())? {
                    TypeDef::Other(t) => self.type_options(t, depth + 1),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Every full URL of a template whose embedded values are a small known
    /// choice; values that are not known stay `{}`. The URLs are built per
    /// value of each variable the template depends on, so one variable that
    /// decides two parts (`${kind}${kind === 'rates' ? '/bulk' : ''}`) gives
    /// only the URLs it can make. `true` with them: some parts were combined
    /// without knowing they go together, so a URL may never be made.
    fn url_variants(&self, e: &Expression<'a>) -> (Vec<String>, bool) {
        let Expression::TemplateLiteral(t) = strip_ts(e).without_parentheses() else {
            return (Vec::new(), false);
        };
        // The variables the parts depend on, with their values.
        let mut drivers: Vec<(String, Vec<String>)> = Vec::new();
        for x in &t.expressions {
            let mut f = DriverFinder::default();
            f.visit_expression(x);
            for key in f.keys {
                if drivers.iter().any(|(k, _)| *k == key) {
                    continue;
                }
                if let Some(v) = self.driver_options(&key).filter(|v| v.len() > 1) {
                    drivers.push((key, v));
                }
            }
        }
        let combos: usize = drivers.iter().map(|(_, v)| v.len()).product();
        let mut out: Vec<String> = Vec::new();
        let mut loose = false;
        let mut per_part = drivers.is_empty() || combos > MAX_PREFIXES;
        if !per_part {
            let mut envs: Vec<HashMap<String, String>> = vec![HashMap::new()];
            for (k, vals) in &drivers {
                envs = envs
                    .iter()
                    .flat_map(|env| {
                        vals.iter().map(move |v| {
                            let mut e = env.clone();
                            e.insert(k.clone(), v.clone());
                            e
                        })
                    })
                    .collect();
            }
            for env in &envs {
                let mut urls = vec![String::new()];
                for (i, q) in t.quasis.iter().enumerate() {
                    let text = q.value.cooked.unwrap_or(q.value.raw).to_string();
                    for u in &mut urls {
                        u.push_str(&text);
                    }
                    let Some(x) = t.expressions.get(i) else {
                        continue;
                    };
                    if let Some(v) = self.eval_with(x, env, 0) {
                        for u in &mut urls {
                            u.push_str(&v);
                        }
                        continue;
                    }
                    let mut f = DriverFinder::default();
                    f.visit_expression(x);
                    let tied = f.keys.iter().any(|k| env.contains_key(k));
                    match self.string_options(x, 0).filter(|o| o.len() > 1) {
                        // A helper with known values (`prefix(kind)`): expand
                        // each part on its own instead.
                        Some(_) if tied => per_part = true,
                        // A choice not tied to the variables (`flag ? '/bulk' : ''`):
                        // every value, but only as a guess.
                        Some(opts) if urls.len() * opts.len() * envs.len() <= MAX_PREFIXES * 2 => {
                            loose = true;
                            urls = urls
                                .iter()
                                .flat_map(|u| opts.iter().map(move |o| format!("{u}{o}")))
                                .collect();
                            continue;
                        }
                        _ => {}
                    }
                    if tied {
                        loose = true;
                    }
                    for u in &mut urls {
                        u.push_str("{}");
                    }
                }
                out.extend(urls);
            }
        }
        if per_part {
            // Each part on its own (helpers whose values are known).
            out.clear();
            out.push(String::new());
            let mut chosen = 0;
            for (i, q) in t.quasis.iter().enumerate() {
                let text = q.value.cooked.unwrap_or(q.value.raw).to_string();
                for v in &mut out {
                    v.push_str(&text);
                }
                let Some(x) = t.expressions.get(i) else {
                    continue;
                };
                match self.string_options(x, 0).filter(|o| o.len() > 1) {
                    Some(opts) if out.len() * opts.len() <= MAX_PREFIXES => {
                        chosen += 1;
                        out = out
                            .iter()
                            .flat_map(|v| opts.iter().map(move |o| format!("{v}{o}")))
                            .collect();
                    }
                    _ => {
                        for v in &mut out {
                            v.push_str("{}");
                        }
                    }
                }
            }
            if chosen == 0 {
                return (Vec::new(), false);
            }
            // Two parts chosen independently may belong together.
            loose = chosen > 1;
        }
        let mut out: Vec<String> = out
            .iter()
            .map(|u| normalize_url(u.split(['?', '#']).next().unwrap_or("")))
            .collect();
        out.sort();
        out.dedup();
        if out.len() < 2 {
            return (Vec::new(), false);
        }
        (out, loose)
    }

    /// The values a template variable (`kind`, `this.segment`) can take.
    fn driver_options(&self, key: &str) -> Option<Vec<String>> {
        match key.strip_prefix("this.") {
            Some(prop) => self.member_options.get(prop).cloned(),
            None => {
                let (start, _) = self.scope();
                let own_end = self
                    .param_types
                    .iter()
                    .map(|(_, (_, end), _)| *end)
                    .filter(|end| *end <= start && start - *end <= 300)
                    .max();
                let typed = self
                    .param_types
                    .iter()
                    .filter(|(n, (at, end), _)| n == key && (*at >= start || Some(*end) == own_end))
                    .max_by_key(|(_, (at, _), _)| *at);
                if let Some((_, _, v)) = typed {
                    return v.clone();
                }
                if let Some(v) = self.local_options.get(key) {
                    return Some(v.clone());
                }
                let init = *self.consts.get(key)?;
                self.string_options(init, 1)
            }
        }
    }

    /// A string expression's value with some variables fixed.
    fn eval_with(
        &self,
        e: &Expression<'a>,
        env: &HashMap<String, String>,
        depth: u8,
    ) -> Option<String> {
        if depth > 6 {
            return None;
        }
        match strip_ts(e).without_parentheses() {
            Expression::StringLiteral(s) => Some(s.value.to_string()),
            Expression::TemplateLiteral(t) => {
                let mut out = String::new();
                for (i, q) in t.quasis.iter().enumerate() {
                    out.push_str(&q.value.cooked.unwrap_or(q.value.raw));
                    if let Some(x) = t.expressions.get(i) {
                        out.push_str(&self.eval_with(x, env, depth + 1)?);
                    }
                }
                Some(out)
            }
            Expression::Identifier(id) => env.get(id.name.as_str()).cloned().or_else(|| {
                let v = self.string_options(e, 1)?;
                (v.len() == 1).then(|| v[0].clone())
            }),
            Expression::StaticMemberExpression(m)
                if matches!(
                    m.object.without_parentheses(),
                    Expression::ThisExpression(_)
                ) =>
            {
                env.get(&format!("this.{}", m.property.name)).cloned()
            }
            Expression::ConditionalExpression(c) => {
                if self.test_with(&c.test, env, depth + 1)? {
                    self.eval_with(&c.consequent, env, depth + 1)
                } else {
                    self.eval_with(&c.alternate, env, depth + 1)
                }
            }
            Expression::BinaryExpression(b) if b.operator == BinaryOperator::Addition => {
                Some(format!(
                    "{}{}",
                    self.eval_with(&b.left, env, depth + 1)?,
                    self.eval_with(&b.right, env, depth + 1)?
                ))
            }
            _ => None,
        }
    }

    /// A condition's truth with some variables fixed (`kind === 'rates'`).
    fn test_with(
        &self,
        e: &Expression<'a>,
        env: &HashMap<String, String>,
        depth: u8,
    ) -> Option<bool> {
        if depth > 6 {
            return None;
        }
        match strip_ts(e).without_parentheses() {
            Expression::BinaryExpression(b) => {
                let eq = match b.operator {
                    BinaryOperator::StrictEquality | BinaryOperator::Equality => true,
                    BinaryOperator::StrictInequality | BinaryOperator::Inequality => false,
                    _ => return None,
                };
                let l = self.eval_with(&b.left, env, depth + 1)?;
                let r = self.eval_with(&b.right, env, depth + 1)?;
                Some((l == r) == eq)
            }
            Expression::LogicalExpression(l) => {
                let a = self.test_with(&l.left, env, depth + 1)?;
                match l.operator {
                    LogicalOperator::And => Some(a && self.test_with(&l.right, env, depth + 1)?),
                    LogicalOperator::Or => Some(a || self.test_with(&l.right, env, depth + 1)?),
                    LogicalOperator::Coalesce => None,
                }
            }
            Expression::UnaryExpression(u) if u.operator == UnaryOperator::LogicalNot => {
                self.test_with(&u.argument, env, depth + 1).map(|v| !v)
            }
            Expression::BooleanLiteral(b) => Some(b.value),
            _ => None,
        }
    }

    /// What a small helper returns: `(k) => (k === 'x' ? 'a' : 'b')` or a
    /// function whose only statement returns such a value.
    fn returned_options(&self, name: &str, depth: u8) -> Option<Vec<String>> {
        if let Some(init) = self.consts.get(name)
            && let Expression::ArrowFunctionExpression(a) = strip_ts(init).without_parentheses()
        {
            if let Some(body) = a.get_expression() {
                return self.string_options(body, depth + 1);
            }
            if let ArrowFunctionBody::FunctionBody(b) = &a.body
                && let [Statement::ReturnStatement(r)] = b.statements.as_slice()
            {
                return self.string_options(r.argument.as_ref()?, depth + 1);
            }
        }
        let f = self.functions.get(name)?;
        if let [Statement::ReturnStatement(r)] = f.body.as_ref()?.statements.as_slice() {
            return self.string_options(r.argument.as_ref()?, depth + 1);
        }
        None
    }

    /// The body variable `name` seen at `pos`: declared in the innermost
    /// function around it.
    fn body_at(&self, name: &str, pos: u32) -> Option<&Sent> {
        self.bodies
            .get(name)?
            .iter()
            .filter(|((a, b), _)| *a <= pos && pos <= *b)
            .max_by_key(|((a, _), _)| *a)
            .map(|(_, s)| s)
    }

    fn body_mut(&mut self, name: &str, pos: u32) -> Option<&mut Sent> {
        self.bodies
            .get_mut(name)?
            .iter_mut()
            .filter(|((a, b), _)| *a <= pos && pos <= *b)
            .max_by_key(|((a, _), _)| *a)
            .map(|(_, s)| s)
    }

    /// A variable's initial value, when it is a request body codemap can follow.
    fn body_init(&self, init: &Expression<'a>) -> Option<Sent> {
        match strip_ts(init) {
            Expression::ObjectExpression(_) | Expression::ConditionalExpression(_) => {
                Some(self.sent_keys(init, 0))
            }
            Expression::NewExpression(n)
                if matches!(callee_name(&n.callee), Some("FormData" | "URLSearchParams")) =>
            {
                Some(match n.arguments.first().and_then(|a| a.as_expression()) {
                    None => Sent {
                        complete: true,
                        ..Sent::default()
                    },
                    Some(a) => self.sent_keys(a, 0),
                })
            }
            _ => None,
        }
    }

    /// `x` in `Object.entries(x)` / `Object.keys(x)`.
    fn entries_source<'b>(&self, c: &'b CallExpression<'a>) -> Option<&'b Expression<'a>> {
        let callee = self.compact(c.callee.span(), 30);
        if matches!(callee.as_str(), "Object.entries" | "Object.keys") {
            c.arguments.first().and_then(|a| a.as_expression())
        } else {
            None
        }
    }

    /// Copies `src`'s keys into every body variable the callback names.
    fn copy_into(&mut self, src: &Expression<'a>, cb: &Expression<'a>, pos: u32) {
        let mut f = IdentFinder::default();
        f.visit_expression(cb);
        self.copy_names(src, &f.names, (cb.span().start, cb.span().end), pos);
    }

    fn copy_names(&mut self, src: &Expression<'a>, names: &[String], span: (u32, u32), pos: u32) {
        let keys = self.sent_keys(src, 0);
        let spoof = self.spoof_of(src);
        let mut hit = false;
        for n in names {
            if let Some(body) = self.body_mut(n, pos) {
                body.merge(keys.clone());
                hit = true;
                if let Some(m) = &spoof {
                    let m = m.clone();
                    self.set_spoof(n, m, pos);
                }
            }
        }
        if hit {
            self.copied_spans.push(span);
        }
    }

    /// The top-level keys of a payload expression.
    fn sent_keys(&self, e: &Expression<'a>, depth: u8) -> Sent {
        if depth > 4 {
            return Sent::unknown("the payload is nested too deeply");
        }
        match strip_ts(e) {
            Expression::ObjectExpression(o) => {
                let mut out = Sent {
                    complete: true,
                    ..Sent::default()
                };
                for p in &o.properties {
                    match p {
                        ObjectPropertyKind::ObjectProperty(p) => match p.key.static_name() {
                            Some(k) if !p.computed => out.push(&k),
                            _ => out.merge(Sent::unknown("the payload has a computed key")),
                        },
                        ObjectPropertyKind::SpreadProperty(sp) => {
                            out.merge(self.sent_keys(&sp.argument, depth + 1));
                        }
                    }
                }
                out
            }
            Expression::Identifier(id) if id.name == "undefined" => Sent {
                complete: true,
                ..Sent::default()
            },
            Expression::NullLiteral(_) => Sent {
                complete: true,
                ..Sent::default()
            },
            // `coords ? { lat, lng } : undefined`: either one.
            Expression::ConditionalExpression(c) => {
                let mut out = self.sent_keys(&c.consequent, depth + 1);
                out.merge(self.sent_keys(&c.alternate, depth + 1));
                out
            }
            Expression::Identifier(id) => match self.body_at(id.name.as_str(), id.span.start) {
                Some(b) => b.clone(),
                None => Sent::unknown(format!(
                    "the payload `{}` is not an object built in this file",
                    id.name
                )),
            },
            // `JSON.stringify(body)`.
            Expression::CallExpression(c)
                if self.compact(c.callee.span(), 40) == "JSON.stringify"
                    && c.arguments
                        .first()
                        .is_some_and(|a| a.as_expression().is_some()) =>
            {
                self.sent_keys(c.arguments[0].as_expression().unwrap(), depth + 1)
            }
            other => Sent::unknown(format!(
                "the payload is `{}`, which codemap does not follow",
                self.compact(other.span(), 40)
            )),
        }
    }

    /// Keys in a query string written in the URL: `/geocode?q=${q}&limit=5`.
    fn url_query(&self, e: &Expression<'a>) -> Sent {
        let mut out = Sent {
            complete: true,
            ..Sent::default()
        };
        let mut parts = Vec::new();
        if self.url_parts(e, &mut parts, 0).is_none() {
            return out;
        }
        // Dynamic parts as NUL, so a key built from a variable is visible.
        let text: String = parts
            .iter()
            .map(|(is_static, t)| if *is_static { t.as_str() } else { "\0" })
            .collect();
        match text.split_once('?') {
            Some((_, q)) => {
                for piece in q.split('&') {
                    let key = piece.split('=').next().unwrap_or("");
                    if key.is_empty() || key.contains('\0') {
                        if !piece.is_empty() {
                            out.merge(Sent::unknown("the query string is built from a variable"));
                        }
                    } else {
                        out.push(top_key(key));
                    }
                }
            }
            // `/merchants${qs}`: a query string in a variable.
            None if parts.last().is_some_and(|(is_static, t)| {
                let t = t.to_ascii_lowercase();
                !is_static
                    && (t.contains("query")
                        || t == "qs"
                        || t.contains("params")
                        || t.contains("search"))
            }) =>
            {
                out.merge(Sent::unknown("the query string is in a variable"));
            }
            None => {}
        }
        out
    }

    /// What a known client's call sends: the body (axios second argument,
    /// `data`, `body`) and the query (`params`, `query`).
    fn payload(&self, c: &CallExpression<'a>, method: Option<&str>) -> Sent {
        let args: Vec<&Expression<'a>> = c
            .arguments
            .iter()
            .filter_map(|a| a.as_expression())
            .collect();
        fn prop<'b, 'a>(
            o: &'b Expression<'a>,
            name: &str,
        ) -> Option<Result<&'b Expression<'a>, ()>> {
            match strip_ts(o) {
                Expression::ObjectExpression(o) => o.properties.iter().find_map(|p| match p {
                    ObjectPropertyKind::ObjectProperty(p)
                        if p.key.static_name().as_deref() == Some(name) =>
                    {
                        Some(Ok(&p.value))
                    }
                    _ => None,
                }),
                // Options in a variable: their keys are not visible.
                _ => Some(Err(())),
            }
        }
        let verb_style = c
            .callee
            .without_parentheses()
            .as_member_expression()
            .and_then(|m| m.static_property_name())
            .map(|v| v.trim_start_matches('$').to_ascii_lowercase())
            .is_some_and(|v| {
                matches!(
                    v.as_str(),
                    "get" | "post" | "put" | "patch" | "delete" | "head"
                )
            });
        let config_style = args
            .first()
            .is_some_and(|a| matches!(strip_ts(a), Expression::ObjectExpression(_)));
        let mut parts: Vec<Option<Result<&Expression<'a>, ()>>> = Vec::new();
        if config_style {
            parts.push(prop(args[0], "data"));
            parts.push(prop(args[0], "params"));
        } else if verb_style {
            let with_body = matches!(method, Some("POST" | "PUT" | "PATCH"));
            let options = if with_body { args.get(2) } else { args.get(1) };
            if with_body {
                parts.push(args.get(1).map(|b| Ok(*b)));
            } else if let Some(o) = options {
                parts.push(prop(o, "data"));
            }
            if let Some(o) = options {
                parts.push(prop(o, "params"));
            }
        } else if let Some(o) = args.get(1) {
            parts.push(prop(o, "body"));
            parts.push(prop(o, "query"));
            parts.push(prop(o, "params"));
        }
        let mut out = Sent {
            complete: true,
            ..Sent::default()
        };
        // A body that is not written out (a variable, a call) but gave no keys.
        let mut opaque = false;
        for part in parts.into_iter().flatten() {
            match part {
                Ok(e) => {
                    let got = self.sent_keys(e, 0);
                    opaque |= got.keys.is_empty()
                        && !matches!(
                            strip_ts(e),
                            Expression::ObjectExpression(_) | Expression::NullLiteral(_)
                        )
                        && !matches!(strip_ts(e), Expression::Identifier(i) if i.name == "undefined");
                    out.merge(got);
                }
                Err(()) => out.merge(Sent::unknown("the request options are in a variable")),
            }
        }
        if opaque && out.complete && out.keys.is_empty() {
            out.merge(Sent::unknown("the payload's keys could not be read"));
        }
        out
    }

    /// `name` as a parameter of the function around `pos`: that function's
    /// name (`api.save`, `saveOrder`) and the parameter's index.
    fn param_of(&self, name: &str, pos: u32) -> Option<(String, u32)> {
        let (start, _) = self.scope();
        let own_end = self
            .param_index
            .iter()
            .map(|(_, _, (_, end))| *end)
            .filter(|end| *end <= start && start - *end <= 300)
            .max();
        let (_, idx, (at, _)) = self
            .param_index
            .iter()
            .filter(|(n, _, (at, end))| {
                n == name && *at <= pos && (*at >= start || Some(*end) == own_end)
            })
            .max_by_key(|(_, _, (at, _))| *at)?;
        // The names of the spans around the parameter, outermost first.
        let mut around: Vec<&(u32, u32, String)> = self
            .names
            .iter()
            .filter(|(a, b, _)| *a <= *at && *at <= *b)
            .collect();
        around.sort_by_key(|(a, _, _)| *a);
        let key = if around.is_empty() {
            self.functions
                .iter()
                .find(|(_, f)| f.span.start <= *at && *at <= f.span.end)
                .map(|(n, _)| (*n).to_owned())?
        } else {
            around
                .iter()
                .map(|(_, _, n)| n.as_str())
                .collect::<Vec<_>>()
                .join(".")
        };
        Some((key, *idx))
    }

    /// Top-level functions whose body spoofs a method, for callers in other files.
    fn finish_fn_spoofs(&mut self) {
        let mut out = Vec::new();
        for (name, f) in &self.functions {
            let Some(body) = f.body.as_deref() else {
                continue;
            };
            let mut finder = SpoofFinder::default();
            finder.visit_function_body(body);
            if let Some(m) = spoof_summary(&finder.found) {
                out.push(((*name).to_owned(), m));
            }
        }
        for (name, e) in &self.consts {
            let mut finder = SpoofFinder::default();
            match strip_ts(e) {
                Expression::ArrowFunctionExpression(a) => match a.get_expression() {
                    Some(x) => {
                        finder.entered = true;
                        finder.visit_expression(x);
                    }
                    None => {
                        if let ArrowFunctionBody::FunctionBody(b) = &a.body {
                            finder.visit_function_body(b);
                        }
                    }
                },
                Expression::FunctionExpression(f) => {
                    if let Some(b) = f.body.as_deref() {
                        finder.visit_function_body(b);
                    }
                }
                _ => continue,
            }
            if let Some(m) = spoof_summary(&finder.found) {
                out.push(((*name).to_owned(), m));
            }
        }
        out.sort();
        self.facts.fn_spoofs = out;
    }

    /// `m`, marked `?` when the code at `pos` runs only on some paths.
    fn maybe(&self, m: String, pos: u32) -> String {
        let (start, _) = self.scope();
        if self
            .cond_spans
            .iter()
            .any(|(a, b)| *a >= start && *a <= pos && pos <= *b)
        {
            format!("?{m}")
        } else {
            m
        }
    }

    /// Records a `_method` set on `name` at `pos`, on the variable declared in
    /// the innermost function around it (or this one).
    fn set_spoof(&mut self, name: &str, m: String, pos: u32) {
        let scope = self.scope();
        let list = self.spoofs.entry(name.to_owned()).or_default();
        let at = list
            .iter_mut()
            .filter(|((a, b), _)| *a <= pos && pos <= *b)
            .max_by_key(|((a, _), _)| *a);
        match at {
            Some((_, v)) => *v = Some(m),
            None => list.push((scope, Some(m))),
        }
    }

    fn spoof_at(&self, name: &str, pos: u32) -> Option<String> {
        self.spoofs
            .get(name)?
            .iter()
            .filter(|((a, b), _)| *a <= pos && pos <= *b)
            .max_by_key(|((a, _), _)| *a)
            .and_then(|(_, m)| m.clone())
    }

    /// The method a request body spoofs: an object literal, a variable or
    /// object bound earlier in the same function, a variable passed through a
    /// converter (`toFormData(payload)`), or a helper function that sets it,
    /// decided by the argument when the helper sets it under a parameter.
    /// `?PUT`: only on some paths.
    fn spoof_of(&self, e: &Expression<'a>) -> Option<String> {
        if let Some(m) = spoofed_method(e, 0) {
            return Some(m);
        }
        match strip_ts(e) {
            Expression::Identifier(id) => self.spoof_at(id.name.as_str(), id.span.start),
            // `this.record` after `this.record._method = 'PATCH'`.
            Expression::StaticMemberExpression(m) => {
                self.spoof_at(&self.compact(m.span, 60), m.span.start)
            }
            // `{ ...record, reason }`: the spread value's own `_method`.
            Expression::ObjectExpression(o) => o.properties.iter().find_map(|p| match p {
                ObjectPropertyKind::SpreadProperty(sp) => self.spoof_of(&sp.argument),
                _ => None,
            }),
            Expression::CallExpression(c) => {
                let args: Vec<&Expression<'a>> = c
                    .arguments
                    .iter()
                    .filter_map(|a| a.as_expression())
                    .collect();
                // Spoofs carried by the arguments (`toFormData(payload)`):
                // known methods, and `@helper` markers resolved later.
                let carried: Vec<String> = args
                    .iter()
                    .filter_map(|a| match strip_ts(a) {
                        Expression::Identifier(id) => {
                            self.spoof_at(id.name.as_str(), id.span.start)
                        }
                        // `jsonFormData(this.record)`.
                        Expression::StaticMemberExpression(m) => {
                            self.spoof_at(&self.compact(m.span, 60), m.span.start)
                        }
                        _ => None,
                    })
                    .collect();
                if let Some(m) = carried.iter().find(|m| !m.starts_with('@')) {
                    return Some(m.clone());
                }
                let markers = || (!carried.is_empty()).then(|| carried.join(","));
                let Expression::Identifier(f) = c.callee.without_parentheses() else {
                    return markers();
                };
                let name = f.name.as_str();
                // A helper from the project's other files: it, then what its
                // arguments carry, resolved when files are linked.
                if !self.functions.contains_key(name) && !self.consts.contains_key(name) {
                    let project = self
                        .facts
                        .imports
                        .iter()
                        .any(|i| !i.type_only && i.binds(name) && is_project_source(&i.source));
                    return if project {
                        let mut all = vec![format!("@{name}")];
                        all.extend(carried.iter().cloned());
                        Some(all.join(","))
                    } else {
                        markers()
                    };
                }
                let mut finder = SpoofFinder::default();
                let params: &FormalParameters<'a> = if let Some(func) = self.functions.get(name) {
                    finder.visit_function_body(func.body.as_deref()?);
                    &func.params
                } else {
                    match self.consts.get(name).map(|e| strip_ts(e)) {
                        Some(Expression::ArrowFunctionExpression(a)) => {
                            match a.get_expression() {
                                Some(e) => {
                                    finder.entered = true;
                                    finder.visit_expression(e);
                                }
                                None => {
                                    if let ArrowFunctionBody::FunctionBody(b) = &a.body {
                                        finder.visit_function_body(b);
                                    }
                                }
                            }
                            &a.params
                        }
                        Some(Expression::FunctionExpression(func)) => {
                            finder.visit_function_body(func.body.as_deref()?);
                            &func.params
                        }
                        _ => return None,
                    }
                };
                let names: Vec<Option<&str>> = params
                    .items
                    .iter()
                    .map(|p| match &p.pattern {
                        BindingPattern::BindingIdentifier(id) => Some(id.name.as_str()),
                        _ => None,
                    })
                    .collect();
                let mut maybe = None;
                for (m, cond) in finder.found {
                    let on = match cond {
                        Cond::Always => Some(true),
                        Cond::Unknown => None,
                        Cond::Param(p, negated) => names
                            .iter()
                            .position(|n| *n == Some(p.as_str()))
                            .and_then(|i| match args.get(i).map(|a| strip_ts(a)) {
                                None => Some(false),
                                Some(a) => truthy(a),
                            })
                            .map(|v| v != negated),
                    };
                    match on {
                        Some(true) => return Some(m),
                        Some(false) => {}
                        None => maybe = maybe.or(Some(format!("?{m}"))),
                    }
                }
                maybe.or_else(markers)
            }
            _ => None,
        }
    }
}

/// Names a request body is built from: the body itself, the values it
/// spreads, and the arguments of a converter around it.
fn body_params<'b, 'a>(e: &'b Expression<'a>, depth: u8) -> Vec<&'b IdentifierReference<'a>> {
    if depth > 3 {
        return Vec::new();
    }
    match strip_ts(e) {
        Expression::Identifier(id) => vec![&**id],
        Expression::ObjectExpression(o) => o
            .properties
            .iter()
            .filter_map(|p| match p {
                ObjectPropertyKind::SpreadProperty(sp) => {
                    Some(body_params(&sp.argument, depth + 1))
                }
                _ => None,
            })
            .flatten()
            .collect(),
        Expression::CallExpression(c) => c
            .arguments
            .iter()
            .filter_map(|a| a.as_expression())
            .flat_map(|a| body_params(a, depth + 1))
            .collect(),
        _ => Vec::new(),
    }
}

/// An import of the project's own code (`./x`, `../x`, `@/x`, `~/x`, `#x`),
/// not a package.
fn is_project_source(src: &str) -> bool {
    src.starts_with('.')
        || src.starts_with('/')
        || src.starts_with("@/")
        || src.starts_with("~/")
        || src.starts_with("~~/")
        || src.starts_with('#')
}

/// The method of a call whose body spoofs `m`: `?PUT` on `POST` is "POST or PUT".
fn either(base: Option<&str>, m: String) -> String {
    match m.strip_prefix('?') {
        Some(s) if base == Some(s) => s.to_owned(),
        Some(s) => format!("{}|{s}", base.unwrap_or("POST")),
        None => m,
    }
}

/// The method a helper's `_method` writes give: set always, or only sometimes.
fn spoof_summary(found: &[(String, Cond)]) -> Option<String> {
    if let Some((m, _)) = found.iter().find(|(_, c)| matches!(c, Cond::Always)) {
        return Some(m.clone());
    }
    found.first().map(|(m, _)| format!("?{m}"))
}

/// A literal argument's truth (`true`, `0`, `''`, `null`); `None` when unknown.
fn truthy(e: &Expression<'_>) -> Option<bool> {
    match e {
        Expression::BooleanLiteral(b) => Some(b.value),
        Expression::NullLiteral(_) => Some(false),
        Expression::NumericLiteral(n) => Some(n.value != 0.0),
        Expression::StringLiteral(s) => Some(!s.value.is_empty()),
        Expression::Identifier(i) if i.name == "undefined" => Some(false),
        _ => None,
    }
}

/// The variables a template part depends on: names, and `this.x`.
#[derive(Default)]
struct DriverFinder {
    keys: Vec<String>,
}

impl<'a> Visit<'a> for DriverFinder {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        let n = it.name.to_string();
        if !self.keys.contains(&n) {
            self.keys.push(n);
        }
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        if matches!(
            it.object.without_parentheses(),
            Expression::ThisExpression(_)
        ) {
            let n = format!("this.{}", it.property.name);
            if !self.keys.contains(&n) {
                self.keys.push(n);
            }
        } else {
            walk::walk_static_member_expression(self, it);
        }
    }
}

/// Names referred to in some code.
#[derive(Default)]
struct IdentFinder {
    names: Vec<String>,
}

impl<'a> Visit<'a> for IdentFinder {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        let n = it.name.to_string();
        if !self.names.contains(&n) {
            self.names.push(n);
        }
    }
}

fn idents_in_statement(s: &Statement<'_>) -> Vec<String> {
    let mut f = IdentFinder::default();
    f.visit_statement(s);
    f.names
}

/// When a `_method` set inside a helper applies.
#[derive(Debug, Clone)]
enum Cond {
    Always,
    /// Under `if (param)` (`true`: `if (!param)`, or its `else`).
    Param(String, bool),
    Unknown,
}

/// Every `_method` a helper's body sets, with the condition around it.
#[derive(Default)]
struct SpoofFinder {
    entered: bool,
    conds: Vec<Cond>,
    found: Vec<(String, Cond)>,
}

impl SpoofFinder {
    fn cond(&self) -> Cond {
        match self.conds.as_slice() {
            [] => Cond::Always,
            [one] => one.clone(),
            _ => Cond::Unknown,
        }
    }

    fn test_of(e: &Expression<'_>) -> Cond {
        match strip_ts(e) {
            Expression::Identifier(i) => Cond::Param(i.name.to_string(), false),
            Expression::UnaryExpression(u) if u.operator == UnaryOperator::LogicalNot => {
                match strip_ts(&u.argument) {
                    Expression::Identifier(i) => Cond::Param(i.name.to_string(), true),
                    _ => Cond::Unknown,
                }
            }
            _ => Cond::Unknown,
        }
    }

    fn negate(c: Cond) -> Cond {
        match c {
            Cond::Param(p, n) => Cond::Param(p, !n),
            other => other,
        }
    }
}

impl<'a> Visit<'a> for SpoofFinder {
    fn visit_if_statement(&mut self, it: &IfStatement<'a>) {
        let t = Self::test_of(&it.test);
        self.conds.push(t.clone());
        self.visit_statement(&it.consequent);
        self.conds.pop();
        if let Some(alt) = &it.alternate {
            self.conds.push(Self::negate(t));
            self.visit_statement(alt);
            self.conds.pop();
        }
    }

    fn visit_conditional_expression(&mut self, it: &ConditionalExpression<'a>) {
        let t = Self::test_of(&it.test);
        self.conds.push(t.clone());
        self.visit_expression(&it.consequent);
        self.conds.pop();
        self.conds.push(Self::negate(t));
        self.visit_expression(&it.alternate);
        self.conds.pop();
    }

    fn visit_logical_expression(&mut self, it: &LogicalExpression<'a>) {
        self.visit_expression(&it.left);
        let t = if it.operator == LogicalOperator::And {
            Self::test_of(&it.left)
        } else {
            Cond::Unknown
        };
        self.conds.push(t);
        self.visit_expression(&it.right);
        self.conds.pop();
    }

    fn visit_object_property(&mut self, it: &ObjectProperty<'a>) {
        if it.key.static_name().as_deref() == Some("_method")
            && let Some(m) = string_value(&it.value)
        {
            self.found.push((m.to_ascii_uppercase(), self.cond()));
        }
        walk::walk_object_property(self, it);
    }

    fn visit_assignment_expression(&mut self, it: &AssignmentExpression<'a>) {
        let is_method = match &it.left {
            AssignmentTarget::StaticMemberExpression(m) => m.property.name == "_method",
            AssignmentTarget::ComputedMemberExpression(m) => {
                string_value(&m.expression).as_deref() == Some("_method")
            }
            _ => false,
        };
        if is_method && let Some(m) = string_value(&it.right) {
            self.found.push((m.to_ascii_uppercase(), self.cond()));
        }
        walk::walk_assignment_expression(self, it);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Some(m) = it.callee.without_parentheses().as_member_expression()
            && matches!(m.static_property_name(), Some("append" | "set"))
            && it
                .arguments
                .first()
                .and_then(|a| a.as_expression())
                .and_then(string_value)
                .as_deref()
                == Some("_method")
            && let Some(v) = it
                .arguments
                .get(1)
                .and_then(|a| a.as_expression())
                .and_then(string_value)
        {
            self.found.push((v.to_ascii_uppercase(), self.cond()));
        }
        walk::walk_call_expression(self, it);
    }

    // Nested functions are other code: only the helper's own body is read.
    fn visit_function_body(&mut self, it: &FunctionBody<'a>) {
        if !self.entered {
            self.entered = true;
            walk::walk_function_body(self, it);
        }
    }
}

/// `items[0][id]` / `address.city` → `items` / `address`: the key Laravel sees first.
fn top_key(k: &str) -> &str {
    k.split(['[', '.']).next().unwrap_or(k)
}

/// `_method: 'PUT'` in an object (or a call wrapping one, e.g. `formData({…})`).
fn spoofed_method(e: &Expression<'_>, depth: u8) -> Option<String> {
    if depth > 3 {
        return None;
    }
    match strip_ts(e) {
        Expression::ObjectExpression(o) => o.properties.iter().find_map(|p| match p {
            ObjectPropertyKind::ObjectProperty(p)
                if p.key.static_name().as_deref() == Some("_method") =>
            {
                string_value(&p.value).map(|m| m.to_ascii_uppercase())
            }
            ObjectPropertyKind::ObjectProperty(p) => spoofed_method(&p.value, depth + 1),
            ObjectPropertyKind::SpreadProperty(_) => None,
        }),
        Expression::CallExpression(c) => c
            .arguments
            .iter()
            .filter_map(|a| a.as_expression())
            .find_map(|a| spoofed_method(a, depth + 1)),
        _ => None,
    }
}

/// Names bound by a pattern with the path each one takes from the value:
/// `{ outcome, wallet: fresh }` → (`outcome`, [outcome]), (`fresh`, [wallet]).
fn binding_paths(p: &BindingPattern<'_>) -> Vec<(String, Vec<String>)> {
    match p {
        BindingPattern::BindingIdentifier(id) => vec![(id.name.to_string(), Vec::new())],
        BindingPattern::ObjectPattern(o) => o
            .properties
            .iter()
            .flat_map(|x| {
                let key = x.key.static_name().map(|k| k.to_string());
                binding_paths(&x.value)
                    .into_iter()
                    .map(move |(n, mut path)| {
                        if let Some(k) = &key {
                            path.insert(0, k.clone());
                        }
                        (n, path)
                    })
            })
            .collect(),
        BindingPattern::AssignmentPattern(a) => binding_paths(&a.left),
        // Array items and rest elements: names only, paths unknown.
        other => binding_names(other)
            .into_iter()
            .map(|n| (n, Vec::new()))
            .collect(),
    }
}

/// Names bound by a pattern (`a`, `{ a, b: c }` → `a`, `c`).
fn binding_names(p: &BindingPattern<'_>) -> Vec<String> {
    match p {
        BindingPattern::BindingIdentifier(id) => vec![id.name.to_string()],
        BindingPattern::ObjectPattern(o) => o
            .properties
            .iter()
            .flat_map(|x| binding_names(&x.value))
            .collect(),
        BindingPattern::ArrayPattern(a) => a
            .elements
            .iter()
            .flatten()
            .flat_map(binding_names)
            .collect(),
        BindingPattern::AssignmentPattern(a) => binding_names(&a.left),
    }
}

/// `res.data.items[0].name` → (`res`, [data, items, name]); `this.x.y` → (`this`, [x, y]).
fn member_chain(e: &Expression<'_>) -> Option<(String, Vec<String>)> {
    let mut path = Vec::new();
    let mut cur = strip_ts(e);
    loop {
        match cur {
            Expression::Identifier(i) => {
                path.reverse();
                return Some((i.name.to_string(), path));
            }
            Expression::ThisExpression(_) => {
                path.reverse();
                return Some(("this".into(), path));
            }
            Expression::ChainExpression(c) => match &c.expression {
                ChainElement::CallExpression(_) | ChainElement::TSNonNullExpression(_) => {
                    return None;
                }
                other => cur = return_member(other.as_member_expression()?, &mut path)?,
            },
            other => {
                let m = other.as_member_expression()?;
                cur = return_member(m, &mut path)?;
            }
        }
    }
}

fn return_member<'b, 'a>(
    m: &'b MemberExpression<'a>,
    path: &mut Vec<String>,
) -> Option<&'b Expression<'a>> {
    match m {
        MemberExpression::StaticMemberExpression(s) => path.push(s.property.name.to_string()),
        MemberExpression::ComputedMemberExpression(c) => match &c.expression {
            Expression::StringLiteral(s) => path.push(s.value.to_string()),
            _ => path.push("[]".into()),
        },
        MemberExpression::PrivateFieldExpression(_) => return None,
    }
    Some(strip_ts(m.object()))
}

fn member_chain_of(m: &MemberExpression<'_>) -> Option<(String, Vec<String>)> {
    let mut path = Vec::new();
    let obj = return_member(m, &mut path)?;
    let (root, mut rest) = member_chain(obj)?;
    rest.extend(path);
    Some((root, rest))
}

/// `/api//orders/{}/` → `/api/orders/{}`; adds the leading `/`.
pub fn normalize_url(url: &str) -> String {
    let url = match url.find("://") {
        // Absolute: keep the host for external detection, drop scheme.
        Some(i) if url.len() > i + 3 => {
            let rest = &url[i + 3..];
            match rest.find('/') {
                Some(j) => &rest[j..],
                None => "/",
            }
        }
        _ => url,
    };
    let segs: Vec<&str> = url.split('/').filter(|s| !s.is_empty()).collect();
    format!("/{}", segs.join("/"))
}

/// Drops `/* … */` and `// …` comments outside string literals.
fn strip_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut quote: Option<u8> = None;
    let mut last = 0;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' | b'"' | b'`' => quote = Some(c),
            b'/' if b.get(i + 1) == Some(&b'*') => {
                out.push_str(&s[last..i]);
                i = s[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                last = i;
                out.push(' ');
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                out.push_str(&s[last..i]);
                i = s[i..].find('\n').map_or(b.len(), |e| i + e);
                last = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&s[last.min(s.len())..]);
    out
}

fn dedupe(v: &mut Vec<NamedLine>) {
    let mut seen = HashSet::new();
    v.retain(|n| seen.insert(n.name.clone()));
}

pub fn is_composable_name(name: &str) -> bool {
    name.len() > 3
        && name.starts_with("use")
        && name[3..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn split_ns(ns: Option<&str>, name: &str) -> (Option<String>, String) {
    if let Some(ns) = ns {
        return (Some(ns.trim_end_matches('/').to_owned()), name.to_owned());
    }
    match name.rsplit_once('/') {
        Some((ns, n)) => (Some(ns.to_owned()), n.to_owned()),
        None => (None, name.to_owned()),
    }
}

/// `this.$store`, `$store`, `store`, `this.store`, `vm.$store`.
fn is_store_object(e: &Expression<'_>) -> bool {
    match e {
        Expression::Identifier(i) => matches!(i.name.as_str(), "$store" | "store"),
        _ => e
            .as_member_expression()
            .and_then(|m| m.static_property_name())
            .is_some_and(|p| p == "$store" || p == "store"),
    }
}

fn strip_ts<'b, 'a>(e: &'b Expression<'a>) -> &'b Expression<'a> {
    match e {
        Expression::TSAsExpression(t) => strip_ts(&t.expression),
        Expression::TSSatisfiesExpression(t) => strip_ts(&t.expression),
        Expression::TSNonNullExpression(t) => strip_ts(&t.expression),
        Expression::ParenthesizedExpression(p) => strip_ts(&p.expression),
        _ => e,
    }
}

/// The function's name: `foo` for `foo()`, `bar` for `a.b.bar()`.
pub fn callee_name<'b>(callee: &'b Expression<'_>) -> Option<&'b str> {
    match callee.without_parentheses() {
        Expression::Identifier(i) => Some(i.name.as_str()),
        other => other
            .as_member_expression()
            .and_then(|m| m.static_property_name()),
    }
}

fn string_value(e: &Expression<'_>) -> Option<String> {
    match strip_ts(e) {
        Expression::StringLiteral(s) => Some(s.value.to_string()),
        Expression::TemplateLiteral(t) if t.expressions.is_empty() => t
            .quasis
            .first()
            .map(|q| q.value.cooked.unwrap_or(q.value.raw).to_string()),
        _ => None,
    }
}

/// `() => import('./X.vue')`, `defineAsyncComponent(() => import('./X.vue'))`, `import('./X.vue')`.
fn lazy_import_source(e: &Expression<'_>) -> Option<String> {
    match strip_ts(e) {
        Expression::ImportExpression(i) => string_value(&i.source),
        Expression::ArrowFunctionExpression(f) => match &f.body {
            ArrowFunctionBody::FunctionBody(b) => b.statements.iter().find_map(|s| match s {
                Statement::ReturnStatement(r) => r.argument.as_ref().and_then(lazy_import_source),
                _ => None,
            }),
            other => other.as_expression().and_then(lazy_import_source),
        },
        Expression::CallExpression(c) => c
            .arguments
            .first()
            .and_then(|a| a.as_expression())
            .and_then(lazy_import_source),
        _ => None,
    }
}

/// `require('./X.vue')` / `require('./X.vue').default` / `resolve => require([...], resolve)`.
fn require_source(e: &Expression<'_>) -> Option<String> {
    match strip_ts(e) {
        Expression::CallExpression(c) if matches!(&c.callee, Expression::Identifier(i) if i.name == "require") => {
            c.arguments
                .first()
                .and_then(|a| a.as_expression())
                .and_then(|a| match strip_ts(a) {
                    Expression::ArrayExpression(arr) => arr
                        .elements
                        .first()
                        .and_then(|x| x.as_expression())
                        .and_then(string_value),
                    other => string_value(other),
                })
        }
        Expression::ArrowFunctionExpression(f) => match &f.body {
            ArrowFunctionBody::FunctionBody(b) => b.statements.iter().find_map(|s| match s {
                Statement::ExpressionStatement(x) => require_source(&x.expression),
                Statement::ReturnStatement(r) => r.argument.as_ref().and_then(require_source),
                _ => None,
            }),
            other => other.as_expression().and_then(require_source),
        },
        other => other
            .as_member_expression()
            .and_then(|m| require_source(m.object())),
    }
}

fn find_call<'b, 'a>(e: &'b Expression<'a>, name: &str) -> Option<&'b CallExpression<'a>> {
    match strip_ts(e) {
        Expression::CallExpression(c) if callee_name(&c.callee) == Some(name) => Some(c),
        Expression::CallExpression(c) => c
            .arguments
            .iter()
            .filter_map(|a| a.as_expression())
            .find_map(|a| find_call(a, name)),
        _ => None,
    }
}

fn arrow_returned_object<'b, 'a>(
    body: &'b ArrowFunctionBody<'a>,
) -> Option<&'b ObjectExpression<'a>> {
    match body {
        ArrowFunctionBody::FunctionBody(b) => returned_object(b),
        other => match other.as_expression().map(strip_ts) {
            Some(Expression::ObjectExpression(o)) => Some(o),
            _ => None,
        },
    }
}

fn returned_object<'b, 'a>(body: &'b FunctionBody<'a>) -> Option<&'b ObjectExpression<'a>> {
    body.statements.iter().rev().find_map(|s| match s {
        Statement::ReturnStatement(r) => match r.argument.as_ref().map(strip_ts) {
            Some(Expression::ObjectExpression(o)) => Some(&**o),
            _ => None,
        },
        Statement::ExpressionStatement(x) => match strip_ts(&x.expression) {
            // Arrow functions with an expression body are a single ExpressionStatement.
            Expression::ObjectExpression(o) => Some(&**o),
            _ => None,
        },
        _ => None,
    })
}

const VUEX_KEYS: &[&str] = &["mutations", "namespaced", "modules"];
const COMPONENT_KEYS: &[&str] = &[
    "template",
    "render",
    "props",
    "data",
    "computed",
    "methods",
    "components",
    "mixins",
    "setup",
    "emits",
    "watch",
    "mounted",
    "created",
];

fn has_key(o: &ObjectExpression<'_>, keys: &[&str]) -> bool {
    o.properties.iter().any(|p| match p {
        ObjectPropertyKind::ObjectProperty(p) => p
            .key
            .static_name()
            .is_some_and(|k| keys.contains(&k.as_ref())),
        _ => false,
    })
}

fn is_vuex_shape(o: &ObjectExpression<'_>) -> bool {
    has_key(o, VUEX_KEYS)
        || has_key(o, &["state"])
            && has_key(o, &["getters", "actions"])
            && !has_key(o, COMPONENT_KEYS)
}

fn is_component_shape(o: &ObjectExpression<'_>) -> bool {
    has_key(o, COMPONENT_KEYS)
}
