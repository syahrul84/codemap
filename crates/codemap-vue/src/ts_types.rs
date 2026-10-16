//! The fields a TypeScript type promises, for comparing the type an API
//! response is cast to with what the endpoint sends. Types are read from the
//! text codemap-js records (`interface`, `type`, the call's `<T>`), resolved
//! across imports and generics (`Paginated<Mall>` → `data[].id`, `data[].name`,
//! `meta.total`). Anything not understood is "unknown" and never reported.

use std::collections::HashMap;

use codemap_js::TsTypeDef;

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// `(field, optional, shape)`; `open` when an index signature allows any key.
    Obj(Vec<(String, bool, Shape)>, bool),
    Arr(Box<Shape>),
    Prim,
    Unknown,
}

/// A parsed type expression, before names are resolved.
#[derive(Debug, Clone, PartialEq)]
enum Ty {
    Obj(Vec<(String, bool, Ty)>, bool),
    Arr(Box<Ty>),
    Named(String, Vec<Ty>),
    Union(Vec<Ty>),
    Inter(Vec<Ty>),
    Prim,
    Unknown,
}

struct Parser<'s> {
    s: &'s [u8],
    i: usize,
}

impl<'s> Parser<'s> {
    fn new(s: &'s str) -> Self {
        Self {
            s: s.as_bytes(),
            i: 0,
        }
    }

    fn ws(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else if self.s[self.i..].starts_with(b"/*") {
                match self.s[self.i + 2..].windows(2).position(|w| w == b"*/") {
                    Some(end) => self.i += end + 4,
                    None => self.i = self.s.len(),
                }
            } else {
                return;
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Option<String> {
        self.ws();
        let start = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric()
                || matches!(self.s[self.i], b'_' | b'$' | b'.'))
        {
            self.i += 1;
        }
        (self.i > start).then(|| String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    /// Skips a balanced `(…)`, `[…]`, `{…}` or `<…>` group starting at `open`.
    fn skip_group(&mut self, open: u8, close: u8) {
        let mut depth = 0;
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    self.i += 1;
                    return;
                }
            } else if c == b'\'' || c == b'"' || c == b'`' {
                self.skip_string(c);
                continue;
            }
            self.i += 1;
        }
    }

    fn skip_string(&mut self, q: u8) {
        self.i += 1;
        while self.i < self.s.len() && self.s[self.i] != q {
            if self.s[self.i] == b'\\' {
                self.i += 1;
            }
            self.i += 1;
        }
        self.i += 1;
    }

    fn ty(&mut self, depth: u8) -> Ty {
        if depth > 30 {
            return Ty::Unknown;
        }
        // Leading `|` / `&` in multi-line unions.
        if self.peek() == Some(b'|') || self.peek() == Some(b'&') {
            self.i += 1;
        }
        let mut parts = vec![self.inter(depth)];
        while self.eat(b'|') {
            parts.push(self.inter(depth));
        }
        if parts.len() == 1 {
            parts.pop().unwrap_or(Ty::Unknown)
        } else {
            Ty::Union(parts)
        }
    }

    fn inter(&mut self, depth: u8) -> Ty {
        let mut parts = vec![self.postfix(depth)];
        while self.peek() == Some(b'&') && self.s.get(self.i + 1) != Some(&b'&') {
            self.i += 1;
            parts.push(self.postfix(depth));
        }
        if parts.len() == 1 {
            parts.pop().unwrap_or(Ty::Unknown)
        } else {
            Ty::Inter(parts)
        }
    }

    fn postfix(&mut self, depth: u8) -> Ty {
        let mut t = self.primary(depth);
        loop {
            self.ws();
            if self.s.get(self.i) == Some(&b'[') && self.s.get(self.i + 1) == Some(&b']') {
                self.i += 2;
                t = Ty::Arr(Box::new(t));
            } else if self.s.get(self.i) == Some(&b'[') {
                // Indexed access `T['x']`: not followed.
                self.skip_group(b'[', b']');
                t = Ty::Unknown;
            } else {
                return t;
            }
        }
    }

    fn primary(&mut self, depth: u8) -> Ty {
        match self.peek() {
            Some(b'{') => {
                self.i += 1;
                self.object(depth)
            }
            Some(b'(') => {
                self.i += 1;
                let t = self.ty(depth + 1);
                // `(a: X) => Y`: a function type.
                if !self.eat(b')') {
                    return Ty::Unknown;
                }
                if self.peek() == Some(b'=') && self.s.get(self.i + 1) == Some(&b'>') {
                    self.i += 2;
                    let _ = self.ty(depth + 1);
                    return Ty::Prim;
                }
                t
            }
            Some(b'[') => {
                self.skip_group(b'[', b']');
                Ty::Unknown
            }
            Some(q @ (b'\'' | b'"' | b'`')) => {
                self.skip_string(q);
                Ty::Prim
            }
            Some(c) if c.is_ascii_digit() || c == b'-' => {
                self.i += 1;
                while self.i < self.s.len()
                    && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'.')
                {
                    self.i += 1;
                }
                Ty::Prim
            }
            Some(_) => {
                let Some(name) = self.ident() else {
                    self.i += 1;
                    return Ty::Unknown;
                };
                if name == "readonly" {
                    // `readonly Mall[]`: the same items.
                    return self.postfix(depth + 1);
                }
                if matches!(name.as_str(), "typeof" | "keyof" | "infer" | "unique") {
                    let _ = self.postfix(depth + 1);
                    return Ty::Unknown;
                }
                let mut args = Vec::new();
                if self.eat(b'<') {
                    loop {
                        args.push(self.ty(depth + 1));
                        if !self.eat(b',') {
                            break;
                        }
                    }
                    if !self.eat(b'>') {
                        return Ty::Unknown;
                    }
                }
                Ty::Named(name, args)
            }
            None => Ty::Unknown,
        }
    }

    /// After `{`: fields until the matching `}`.
    fn object(&mut self, depth: u8) -> Ty {
        let mut fields = Vec::new();
        let mut open = false;
        loop {
            match self.peek() {
                None => return Ty::Unknown,
                Some(b'}') => {
                    self.i += 1;
                    return Ty::Obj(fields, open);
                }
                Some(b';' | b',') => {
                    self.i += 1;
                    continue;
                }
                Some(b'[') => {
                    // `[key: string]: T` or a mapped type.
                    self.skip_group(b'[', b']');
                    open = true;
                    self.skip_member();
                    continue;
                }
                _ => {}
            }
            let key = match self.peek() {
                Some(q @ (b'\'' | b'"')) => {
                    let start = self.i + 1;
                    self.skip_string(q);
                    String::from_utf8_lossy(&self.s[start..self.i.saturating_sub(1)]).into_owned()
                }
                _ => {
                    let Some(k) = self.ident() else {
                        self.skip_member();
                        continue;
                    };
                    if k == "readonly"
                        && self
                            .peek()
                            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'\'' || c == b'"')
                    {
                        continue;
                    }
                    k
                }
            };
            let optional = self.eat(b'?');
            match self.peek() {
                Some(b':') => {
                    self.i += 1;
                    let t = self.ty(depth + 1);
                    fields.push((key, optional, t));
                }
                Some(b'(') | Some(b'<') => {
                    // A method signature: not data.
                    self.skip_member();
                }
                _ => {
                    self.skip_member();
                }
            }
        }
    }

    /// Skips to the end of the current member (`;`, `,` or the closing `}`).
    fn skip_member(&mut self) {
        while self.i < self.s.len() {
            match self.s[self.i] {
                b';' | b',' => {
                    self.i += 1;
                    return;
                }
                b'}' => return,
                b'{' => self.skip_group(b'{', b'}'),
                b'(' => self.skip_group(b'(', b')'),
                b'<' => self.skip_group(b'<', b'>'),
                b'[' => self.skip_group(b'[', b']'),
                q @ (b'\'' | b'"' | b'`') => self.skip_string(q),
                _ => self.i += 1,
            }
        }
    }
}

fn parse(text: &str) -> Ty {
    Parser::new(text).ty(0)
}

/// Looks a type name up as seen from one module: `(definition, module key)`.
pub trait TypeLookup {
    fn find(&self, module: &str, name: &str) -> Option<(TsTypeDef, String)>;
}

struct Eval<'l> {
    lookup: &'l dyn TypeLookup,
    budget: std::cell::Cell<u32>,
}

impl Eval<'_> {
    fn eval(&self, t: &Ty, module: &str, env: &HashMap<String, Shape>, depth: u8) -> Shape {
        if depth > 12 || self.budget.get() == 0 {
            return Shape::Unknown;
        }
        self.budget.set(self.budget.get() - 1);
        match t {
            Ty::Prim => Shape::Prim,
            Ty::Unknown => Shape::Unknown,
            Ty::Arr(x) => Shape::Arr(Box::new(self.eval(x, module, env, depth + 1))),
            Ty::Obj(fields, open) => Shape::Obj(
                fields
                    .iter()
                    .map(|(k, o, ft)| (k.clone(), *o, self.eval(ft, module, env, depth + 1)))
                    .collect(),
                *open,
            ),
            Ty::Union(parts) => {
                let shapes: Vec<Shape> = parts
                    .iter()
                    .filter(|p| !matches!(p, Ty::Named(n, a) if a.is_empty() && matches!(n.as_str(), "null" | "undefined" | "void")))
                    .map(|p| self.eval(p, module, env, depth + 1))
                    .collect();
                match shapes.as_slice() {
                    [one] => one.clone(),
                    [] => Shape::Prim,
                    many if many.iter().all(|s| *s == Shape::Prim) => Shape::Prim,
                    _ => Shape::Unknown,
                }
            }
            Ty::Inter(parts) => {
                let mut fields: Vec<(String, bool, Shape)> = Vec::new();
                let mut open = false;
                for p in parts {
                    match self.eval(p, module, env, depth + 1) {
                        Shape::Obj(f, o) => {
                            open |= o;
                            for x in f {
                                if !fields.iter().any(|y| y.0 == x.0) {
                                    fields.push(x);
                                }
                            }
                        }
                        _ => return Shape::Unknown,
                    }
                }
                Shape::Obj(fields, open)
            }
            Ty::Named(name, args) => self.named(name, args, module, env, depth),
        }
    }

    fn named(
        &self,
        name: &str,
        args: &[Ty],
        module: &str,
        env: &HashMap<String, Shape>,
        depth: u8,
    ) -> Shape {
        if args.is_empty()
            && let Some(s) = env.get(name)
        {
            return s.clone();
        }
        let arg = |i: usize| {
            args.get(i)
                .map(|a| self.eval(a, module, env, depth + 1))
                .unwrap_or(Shape::Unknown)
        };
        match name {
            "string" | "number" | "boolean" | "bigint" | "symbol" | "null" | "undefined"
            | "void" | "never" | "true" | "false" | "Date" | "File" | "Blob" | "object"
            | "Function" | "RegExp" => return Shape::Prim,
            "any" | "unknown" | "Record" | "Map" | "Set" | "Pick" | "Omit" | "Exclude"
            | "Extract" | "ReturnType" | "Parameters" | "Awaited" => return Shape::Unknown,
            "Array" | "ReadonlyArray" => return Shape::Arr(Box::new(arg(0))),
            "Promise" | "Readonly" | "Required" | "NonNullable" | "Ref" | "ComputedRef" => {
                return arg(0);
            }
            "Partial" => {
                return match arg(0) {
                    Shape::Obj(f, o) => {
                        Shape::Obj(f.into_iter().map(|(k, _, s)| (k, true, s)).collect(), o)
                    }
                    other => other,
                };
            }
            _ => {}
        }
        let Some((def, def_module)) = self.lookup.find(module, name) else {
            return Shape::Unknown;
        };
        let mut env2: HashMap<String, Shape> = HashMap::new();
        for (i, p) in def.params.iter().enumerate() {
            env2.insert(p.clone(), arg(i));
        }
        let mut shape = self.eval(&parse(&def.body), &def_module, &env2, depth + 1);
        // `interface X extends Base, Other<T>`: their fields too.
        for e in &def.extends {
            if let Shape::Obj(base, open) = self.eval(&parse(e), &def_module, &env2, depth + 1)
                && let Shape::Obj(fields, o) = &mut shape
            {
                *o |= open;
                for b in base {
                    if !fields.iter().any(|f| f.0 == b.0) {
                        fields.push(b);
                    }
                }
            } else if let Shape::Obj(_, o) = &mut shape {
                *o = true; // an unknown base: other fields may come from it
            }
        }
        shape
    }
}

/// Paths of the fields a type requires (optional fields left out):
/// `data[].name`, `meta.total`. Empty when the type is not understood.
pub fn required_paths(type_text: &str, module: &str, lookup: &dyn TypeLookup) -> Vec<String> {
    let ev = Eval {
        lookup,
        budget: std::cell::Cell::new(4000),
    };
    let shape = ev.eval(&parse(type_text), module, &HashMap::new(), 0);
    let mut out = Vec::new();
    paths(&shape, "", 0, &mut out);
    out.truncate(150);
    out
}

fn paths(shape: &Shape, prefix: &str, depth: u8, out: &mut Vec<String>) {
    if depth > 4 {
        return;
    }
    match shape {
        Shape::Obj(fields, _) => {
            for (k, optional, s) in fields {
                if *optional {
                    continue;
                }
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match s {
                    Shape::Obj(..) | Shape::Arr(_) => {
                        let before = out.len();
                        paths(s, &p, depth + 1, out);
                        if out.len() == before {
                            out.push(p);
                        }
                    }
                    _ => out.push(p),
                }
            }
        }
        Shape::Arr(inner) => paths(inner, &format!("{prefix}[]"), depth + 1, out),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Defs(Vec<TsTypeDef>);

    impl TypeLookup for Defs {
        fn find(&self, _module: &str, name: &str) -> Option<(TsTypeDef, String)> {
            self.0
                .iter()
                .find(|d| d.name == name)
                .cloned()
                .map(|d| (d, String::new()))
        }
    }

    fn def(name: &str, params: &[&str], body: &str) -> TsTypeDef {
        TsTypeDef {
            name: name.into(),
            params: params.iter().map(|p| (*p).to_owned()).collect(),
            body: body.into(),
            ..TsTypeDef::default()
        }
    }

    #[test]
    fn generic_envelopes_resolve_to_fields() {
        let defs = Defs(vec![
            def(
                "Paginated",
                &["T"],
                "{ data: T[]; meta: { total: number; page?: number } }",
            ),
            def(
                "Mall",
                &[],
                "{ id: number; /** shown */ name: string; city: string | null; image_card?: string | null; impact: MallImpact; on: () => void }",
            ),
            def("MallImpact", &[], "{ order_count: number }"),
        ]);
        let p = required_paths("Paginated<Mall>", "", &defs);
        assert_eq!(
            p,
            [
                "data[].id",
                "data[].name",
                "data[].city",
                "data[].impact.order_count",
                "data[].on",
                "meta.total"
            ]
        );
        assert_eq!(required_paths("{ data: Mall | null }", "", &defs).len(), 5);
        assert!(required_paths("Record<string, Mall>", "", &defs).is_empty());
        assert_eq!(
            required_paths(
                "{ data: { outcome: 'landed' | 'failed', wallet: Partial<Mall> } }",
                "",
                &defs
            ),
            ["data.outcome", "data.wallet"]
        );
    }
}
