//! Blade templates, read as text: what a view extends, includes and shows,
//! the sections and slots it has, its forms and the variables it expects.
//!
//! Only ASCII bytes are matched, so every slice starts and ends on a
//! character boundary whatever the template's text is.

use serde::{Deserialize, Serialize};

/// A name with the line it is written on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Named {
    pub name: String,
    pub line: u32,
}

/// One `<form>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Form {
    pub method: String,
    /// `route:orders.store`, or the attribute as written.
    pub action: String,
    pub line: u32,
}

/// What one template says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BladeFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<Named>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub yields: Vec<String>,
    /// `@include` and its relatives, `@each`, `@component`. A name ending in
    /// `*` was built at run time and only its start is known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<Named>,
    /// `x-alert`, `x-forms.input`, `livewire:cart`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<Named>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stacks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pushes: Vec<String>,
    /// Variables read and never set in the template: what it must be given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vars: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forms: Vec<Form>,
}

/// Variables Blade or Laravel put in every view.
const GIVEN: &[&str] = &[
    "loop",
    "errors",
    "slot",
    "attributes",
    "component",
    "this",
    "message",
    "app",
    "__env",
    "__data",
    "__path",
];

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

struct Scan<'a> {
    src: &'a str,
    b: &'a [u8],
    line_starts: Vec<usize>,
    facts: BladeFacts,
    used: Vec<String>,
    local: Vec<String>,
}

impl<'a> Scan<'a> {
    fn line(&self, at: usize) -> u32 {
        self.line_starts.partition_point(|s| *s <= at) as u32
    }

    /// Position of `needle` at or after `from`.
    fn find(&self, from: usize, needle: &str) -> Option<usize> {
        self.src.get(from..)?.find(needle).map(|i| from + i)
    }

    /// The end of the bracket opened at `open` (index of its closing
    /// bracket), skipping strings.
    fn close_of(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut i = open;
        while i < self.b.len() {
            match self.b[i] {
                b'\'' | b'"' => i = skip_string(self.b, i),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// PHP written in the template: its variables, and the ones it sets.
    fn php(&mut self, code: &str) {
        let b = code.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\'' => {
                    i = skip_string(b, i) + 1;
                    continue;
                }
                b'$' if b
                    .get(i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') =>
                {
                    let start = i + 1;
                    let mut end = start;
                    while end < b.len() && is_ident(b[end]) {
                        end += 1;
                    }
                    let name = &code[start..end];
                    // `$x = …` (not `==`, `=>`): set here.
                    let mut k = end;
                    while k < b.len() && b[k] == b' ' {
                        k += 1;
                    }
                    let assigned = b.get(k) == Some(&b'=')
                        && !matches!(b.get(k + 1), Some(b'=' | b'>'))
                        && (i == 0 || b[i - 1] != b'>' && b[i - 1] != b':');
                    let list = if assigned {
                        &mut self.local
                    } else {
                        &mut self.used
                    };
                    if !list.iter().any(|n| n == name) {
                        list.push(name.to_owned());
                    }
                    i = end;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        // `foreach ($orders as $id => $order)`, `function ($q)`, `fn ($q) =>`,
        // `catch (Throwable $e)`: names the code gives itself.
        for word in [" as ", "function", "fn", "catch"] {
            let mut from = 0;
            while let Some(at) = code
                .get(from..)
                .and_then(|c| c.find(word))
                .map(|x| x + from)
            {
                from = at + word.len();
                let rest = &code[from..];
                let rest = if word == " as " {
                    rest
                } else {
                    match rest.trim_start().strip_prefix('(') {
                        Some(r) => r,
                        None => continue,
                    }
                };
                let end = rest
                    .bytes()
                    .position(|c| c == b')' || c == b'{')
                    .unwrap_or(rest.len());
                for part in rest[..end].split('$').skip(1) {
                    let name: String = part
                        .bytes()
                        .take_while(|c| is_ident(*c))
                        .map(char::from)
                        .collect();
                    if !name.is_empty() && !self.local.contains(&name) {
                        self.local.push(name);
                    }
                }
            }
        }
    }

    fn directive(&mut self, name: &str, args: &str, at: usize) {
        let line = self.line(at);
        let top = top_level_args(args);
        let lit = |i: usize| top.get(i).and_then(|a| view_literal(a));
        let plain = |i: usize| top.get(i).and_then(|a| plain_literal(a));
        match name {
            "extends" | "layout" => {
                if let Some(n) = lit(0) {
                    self.facts.extends = Some(Named { name: n, line });
                }
            }
            "section" | "hasSection" | "sectionMissing" => {
                if name == "section"
                    && let Some(n) = plain(0)
                {
                    push_unique(&mut self.facts.sections, n);
                }
            }
            "yield" => {
                if let Some(n) = plain(0) {
                    push_unique(&mut self.facts.yields, n);
                }
            }
            "include" | "includeIf" | "each" | "component" => {
                if let Some(n) = lit(0) {
                    self.facts.includes.push(Named { name: n, line });
                }
            }
            "includeWhen" | "includeUnless" => {
                if let Some(n) = lit(1) {
                    self.facts.includes.push(Named { name: n, line });
                }
            }
            "includeFirst" => {
                if let Some(list) = top.first() {
                    let inner = list.trim().trim_start_matches('[').trim_end_matches(']');
                    for a in top_level_args(inner) {
                        if let Some(n) = view_literal(&a) {
                            self.facts.includes.push(Named { name: n, line });
                        }
                    }
                }
            }
            "livewire" => {
                if let Some(n) = lit(0) {
                    self.facts.components.push(Named {
                        name: format!("livewire:{n}"),
                        line,
                    });
                }
            }
            "props" | "aware" => {
                if let Some(list) = top.first() {
                    let inner = list.trim().trim_start_matches('[').trim_end_matches(']');
                    for a in top_level_args(inner) {
                        let key = a.split("=>").next().unwrap_or(&a);
                        if let Some(n) = plain_literal(key) {
                            if name == "props" {
                                push_unique(&mut self.facts.props, n.clone());
                            }
                            push_unique(&mut self.local, n);
                        }
                    }
                }
            }
            "slot" => {
                if let Some(n) = plain(0) {
                    push_unique(&mut self.facts.slots, n);
                }
            }
            "stack" => {
                if let Some(n) = plain(0) {
                    push_unique(&mut self.facts.stacks, n);
                }
            }
            "push" | "pushOnce" | "pushIf" | "prepend" | "prependOnce" => {
                let i = usize::from(name == "pushIf");
                if let Some(n) = plain(i) {
                    push_unique(&mut self.facts.pushes, n);
                }
            }
            _ => {}
        }
        self.php(args);
    }

    /// `<x-name …>` / `<livewire:name …>` starting at `at` (the `<`).
    fn tag(&mut self, at: usize, name_start: usize, prefix: &str) {
        let mut end = name_start;
        while end < self.b.len()
            && (is_ident(self.b[end]) || matches!(self.b[end], b'.' | b'-' | b':'))
        {
            end += 1;
        }
        let name = self.src[name_start..end].trim_end_matches(['.', '-', ':']);
        if name.is_empty() {
            return;
        }
        // The tag's attributes, up to its `>` (quotes may hold one).
        let mut close = end;
        while close < self.b.len() && self.b[close] != b'>' {
            if matches!(self.b[close], b'"' | b'\'') {
                close = skip_string(self.b, close);
            }
            close += 1;
        }
        let close = close.min(self.b.len());
        let attrs = &self.src[end..close];
        let line = self.line(at);
        if prefix == "x-" && (name == "slot" || name.starts_with("slot:")) {
            let slot = name
                .strip_prefix("slot:")
                .map(str::to_owned)
                .or_else(|| attr_value(attrs, "name"));
            if let Some(s) = slot {
                push_unique(&mut self.facts.slots, s);
            }
        } else if name != "dynamic-component" {
            self.facts.components.push(Named {
                name: format!("{prefix}{name}"),
                line,
            });
        }
        // `:order="$order"`: PHP in a bound attribute.
        let ab = attrs.as_bytes();
        let mut i = 0;
        while i + 1 < ab.len() {
            if ab[i].is_ascii_whitespace() && ab[i + 1] == b':' {
                let mut k = i + 2;
                while k < ab.len() && (is_ident(ab[k]) || matches!(ab[k], b'-' | b'.' | b':')) {
                    k += 1;
                }
                if ab.get(k) == Some(&b'=') && matches!(ab.get(k + 1), Some(b'"' | b'\'')) {
                    let q = k + 1;
                    let e = skip_string(ab, q).min(ab.len());
                    if q < e {
                        let code = attrs[q + 1..e].to_owned();
                        self.php(&code);
                    }
                    i = e;
                } else if k > i + 2 && ab.get(k) != Some(&b'=') {
                    // `:$order` shorthand.
                    let short = &attrs[i + 2..k];
                    if let Some(v) = short.strip_prefix('$') {
                        push_unique(&mut self.used, v.to_owned());
                    }
                    i = k;
                }
            }
            i += 1;
        }
    }

    fn form(&mut self, at: usize) {
        let mut close = at;
        while close < self.b.len() && self.b[close] != b'>' {
            if matches!(self.b[close], b'"' | b'\'') {
                close = skip_string(self.b, close);
            }
            close += 1;
        }
        let close = close.min(self.b.len());
        let attrs = &self.src[at..close];
        let mut method = attr_value(attrs, "method")
            .unwrap_or_else(|| "GET".into())
            .to_uppercase();
        // `@method('PUT')` before the form ends.
        let body_end = self
            .find(close, "</form")
            .unwrap_or(self.b.len())
            .min(close + 4000);
        if let Some(body) = self.src.get(close..floor_ascii(self.b, body_end))
            && let Some(m) = body.find("@method(")
            && let Some(v) = plain_literal(body[m + 8..].split(')').next().unwrap_or(""))
        {
            method = v.to_uppercase();
        }
        let action = attr_value(attrs, "action")
            .or_else(|| attr_value(attrs, "wire:submit"))
            .or_else(|| attr_value(attrs, "wire:submit.prevent"))
            .unwrap_or_default();
        let action = match action.find("route(") {
            Some(r) => {
                match plain_literal(action[r + 6..].split([',', ')']).next().unwrap_or("")) {
                    Some(name) => format!("route:{name}"),
                    None => squeeze(&action),
                }
            }
            None => squeeze(&action),
        };
        self.facts.forms.push(Form {
            method,
            action,
            line: self.line(at),
        });
    }

    fn run(mut self) -> BladeFacts {
        let b = self.b;
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'{' if self.src[i..].starts_with("{{--") => {
                    i = self.find(i, "--}}").map_or(b.len(), |e| e + 4);
                    continue;
                }
                b'{' if self.src[i..].starts_with("{{") || self.src[i..].starts_with("{!!") => {
                    let (open, shut) = if b[i + 1] == b'{' {
                        (2, "}}")
                    } else {
                        (3, "!!}")
                    };
                    let end = self.find(i + open, shut).unwrap_or(b.len());
                    let code = self.src[i + open..end].to_owned();
                    self.php(&code);
                    i = end + shut.len();
                    continue;
                }
                b'<' if self.src[i..].starts_with("<?php") => {
                    let end = self.find(i, "?>").unwrap_or(b.len());
                    let code = self.src[i + 5..end].to_owned();
                    self.php(&code);
                    i = end + 2;
                    continue;
                }
                b'<' if self.src[i..].starts_with("<x-") => self.tag(i, i + 3, "x-"),
                b'<' if self.src[i..].starts_with("<livewire:") => self.tag(i, i + 10, "livewire:"),
                b'<' if self.src[i..].starts_with("<form")
                    && b.get(i + 5)
                        .is_some_and(|c| c.is_ascii_whitespace() || *c == b'>') =>
                {
                    self.form(i);
                }
                b'@' if i + 1 < b.len()
                    && b[i + 1].is_ascii_alphabetic()
                    && (i == 0
                        || !(is_ident(b[i - 1]) || b[i - 1] == b'@' || b[i - 1] == b'.')) =>
                {
                    let mut end = i + 1;
                    while end < b.len() && is_ident(b[end]) {
                        end += 1;
                    }
                    let name = &self.src[i + 1..end];
                    if name == "verbatim" {
                        i = self.find(end, "@endverbatim").map_or(b.len(), |e| e + 12);
                        continue;
                    }
                    let mut open = end;
                    while open < b.len() && b[open] == b' ' {
                        open += 1;
                    }
                    if b.get(open) == Some(&b'(') {
                        if let Some(close) = self.close_of(open) {
                            let args = self.src[open + 1..close].to_owned();
                            let name = name.to_owned();
                            self.directive(&name, &args, i);
                            i = close + 1;
                            continue;
                        }
                    } else if name == "php" {
                        let stop = self.find(end, "@endphp").unwrap_or(b.len());
                        let code = self.src[end..stop].to_owned();
                        self.php(&code);
                        i = stop + 7;
                        continue;
                    }
                    i = end;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        let mut vars: Vec<String> = self
            .used
            .into_iter()
            .filter(|v| !self.local.contains(v) && !GIVEN.contains(&v.as_str()))
            .collect();
        vars.sort();
        self.facts.vars = vars;
        self.facts
    }
}

/// The largest index `<= at` that is not inside a multi-byte character.
fn floor_ascii(b: &[u8], at: usize) -> usize {
    let mut at = at.min(b.len());
    while at > 0 && at < b.len() && (b[at] & 0xC0) == 0x80 {
        at -= 1;
    }
    at
}

/// Index of the quote closing the string opened at `open`.
fn skip_string(b: &[u8], open: usize) -> usize {
    let q = b[open];
    let mut i = open + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            c if c == q => return i,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

fn push_unique(list: &mut Vec<String>, v: String) {
    if !list.contains(&v) {
        list.push(v);
    }
}

fn squeeze(s: &str) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match t.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &t[..i]),
        None => t,
    }
}

/// `name="value"` in a tag's attributes.
fn attr_value(attrs: &str, name: &str) -> Option<String> {
    let b = attrs.as_bytes();
    let mut from = 0;
    while let Some(at) = attrs.get(from..)?.find(name).map(|i| i + from) {
        from = at + name.len();
        let before_ok = at == 0 || b[at - 1].is_ascii_whitespace();
        let mut k = from;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if !before_ok || b.get(k) != Some(&b'=') {
            continue;
        }
        k += 1;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        return match b.get(k) {
            Some(b'"' | b'\'') => {
                let end = skip_string(b, k).min(b.len());
                Some(attrs[k + 1..end].to_owned())
            }
            Some(_) => Some(
                attrs[k..]
                    .split(|c: char| c.is_whitespace() || c == '>')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            None => None,
        };
    }
    None
}

/// The arguments of a call, split at its top-level commas.
pub fn top_level_args(args: &str) -> Vec<String> {
    let b = args.as_bytes();
    let mut out = Vec::new();
    let (mut depth, mut start, mut i) = (0usize, 0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' => i = skip_string(b, i),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                out.push(args[start..i].trim().to_owned());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let last = args.get(start..).unwrap_or_default().trim();
    if !last.is_empty() {
        out.push(last.to_owned());
    }
    out
}

/// `'orders.index'` → `orders.index`; nothing for anything but one plain string.
pub fn plain_literal(arg: &str) -> Option<String> {
    let t = arg.trim();
    let q = t.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let inner = t.get(1..)?.strip_suffix(q)?;
    (!inner.contains([q, '$', '{'])).then(|| inner.to_owned())
}

/// A view name as written in a call: `'orders.index'`, `"orders/index"`, or
/// `'pages.' . $slug` / `"pages.{$slug}"` as `pages.*` (only its start is known).
pub fn view_literal(arg: &str) -> Option<String> {
    let t = arg.trim();
    let q = t.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let rest = t.get(1..)?;
    let stop = rest
        .char_indices()
        .find(|(_, c)| *c == q || (q == '"' && matches!(c, '$' | '{')))
        .map_or(rest.len(), |(i, _)| i);
    let name = rest[..stop].replace('/', ".");
    let whole = rest
        .get(stop..)
        .is_some_and(|r| r.len() == 1 && r.starts_with(q));
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'));
    match (ok, whole) {
        (true, true) => Some(name),
        (true, false) => Some(format!("{name}*")),
        _ => None,
    }
}

/// Reads one Blade template.
pub fn facts(src: &str) -> BladeFacts {
    let mut line_starts = vec![0];
    line_starts.extend(
        src.bytes()
            .enumerate()
            .filter(|(_, c)| *c == b'\n')
            .map(|(i, _)| i + 1),
    );
    Scan {
        src,
        b: src.as_bytes(),
        line_starts,
        facts: BladeFacts::default(),
        used: Vec::new(),
        local: Vec::new(),
    }
    .run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[Named]) -> Vec<(&str, u32)> {
        v.iter().map(|n| (n.name.as_str(), n.line)).collect()
    }

    #[test]
    fn page_with_layout_includes_and_form() {
        let f = facts(
            r#"@extends('layouts.app')
{{-- @include('never.this') --}}
@section('title', 'Orders — Résumé')
@section('content')
    @include('orders.partials.filters', ['status' => $status])
    @includeWhen($user->isAdmin('x'), 'orders.partials.admin')
    @foreach ($orders as $id => $order)
        <x-order.card :order="$order" class="mt-2" />
        {{ $loop->index }} {{ $order->total }} {!! $note !!}
    @endforeach
    @php $shown = count($orders); @endphp
    <p>{{ $shown }} of {{ $total }}</p>
    <form method="POST" action="{{ route('orders.update', $current) }}">
        @csrf @method('PUT')
    </form>
    <livewire:cart-badge />
    @include('pages.' . $slug)
    <a href="mailto:a@example.com">x</a>
@endsection
@push('scripts')
<script>const total = $('#total'); let n = `${total}`;</script>
@endpush
"#,
        );
        assert_eq!(
            f.extends.as_ref().map(|e| e.name.as_str()),
            Some("layouts.app")
        );
        assert_eq!(f.sections, ["title", "content"]);
        assert_eq!(
            names(&f.includes),
            [
                ("orders.partials.filters", 5),
                ("orders.partials.admin", 6),
                ("pages.*", 17)
            ]
        );
        assert_eq!(
            names(&f.components),
            [("x-order.card", 8), ("livewire:cart-badge", 16)]
        );
        assert_eq!(
            f.vars,
            [
                "current", "note", "orders", "slug", "status", "total", "user"
            ]
        );
        assert_eq!(
            f.forms,
            [Form {
                method: "PUT".into(),
                action: "route:orders.update".into(),
                line: 13
            }]
        );
        assert_eq!(f.pushes, ["scripts"]);
    }

    #[test]
    fn component_with_props_and_slots() {
        let f = facts(
            r#"@props(['type' => 'info', 'message'])
<div {{ $attributes->merge(['class' => 'alert alert-'.$type]) }}>
    {{ $title ?? '' }} {{ $message }} {{ $slot }}
    <x-slot:footer>ok</x-slot:footer>
    <x-slot name="icon">i</x-slot>
    @yield('extra') @stack('modals')
</div>
@verbatim {{ $not_blade }} @endverbatim
"#,
        );
        assert_eq!(f.props, ["type", "message"]);
        assert_eq!(f.vars, ["title"]);
        assert_eq!(f.slots, ["footer", "icon"]);
        assert_eq!(f.yields, ["extra"]);
        assert_eq!(f.stacks, ["modals"]);
        assert!(f.components.is_empty());
    }

    #[test]
    fn view_names_as_written() {
        assert_eq!(
            view_literal("'orders.index'").as_deref(),
            Some("orders.index")
        );
        assert_eq!(
            view_literal("\"mail/orders/paid\"").as_deref(),
            Some("mail.orders.paid")
        );
        assert_eq!(view_literal("'pages.' . $slug").as_deref(), Some("pages.*"));
        assert_eq!(
            view_literal("\"pages.{$slug}\"").as_deref(),
            Some("pages.*")
        );
        assert_eq!(view_literal("$view"), None);
        assert_eq!(view_literal("'Hello world'"), None);
        assert_eq!(
            top_level_args("$a, ['x', 'y'], f(1, 2)"),
            ["$a", "['x', 'y']", "f(1, 2)"]
        );
    }

    #[test]
    fn odd_text_does_not_break_it() {
        for src in [
            "",
            "@",
            "{{",
            "{!! $x",
            "<x-",
            "@include(",
            "<form",
            "é@é{{é}}",
            "@php",
        ] {
            let _ = facts(src);
        }
    }
}
