//! What the script analyzer reports. Everything is serialisable so adapters
//! can store it as JSON.

use serde::{Deserialize, Serialize};

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScriptFacts {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<Import>,
    /// Top-level declarations (exported or not).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decls: Vec<Decl>,
    /// `export { a as b }`, `export { default as X } from './X.vue'`, `export * from`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reexports: Vec<Reexport>,
    /// Options API object (`export default {…}`, `defineComponent({…})`, `Vue.extend({…})`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ComponentOptions>,
    /// `<script setup>` compiler macros.
    #[serde(default, skip_serializing_if = "SetupMacros::is_empty")]
    pub setup: SetupMacros,
    /// `use*()` calls: composables, Pinia stores, router, i18n.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub composables: Vec<NamedLine>,
    /// Calls to names neither imported nor declared in the file (auto-imports,
    /// globals): candidates for project functions used without an import.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unbound_calls: Vec<NamedLine>,
    /// Methods called on a named object (`ordersApi.cancel`), so an object's
    /// method counts as used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_calls: Vec<String>,
    /// `$emit('x')` / `emit('x')` calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emitted: Vec<NamedLine>,
    /// `Vue.component('Name', X)` / `app.component('Name', X)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registrations: Vec<Registration>,
    /// `require.context(dir, …)` / `import.meta.glob(pattern)` in a file that registers components.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registration_globs: Vec<RegistrationGlob>,
    /// Pinia `defineStore`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinia_stores: Vec<PiniaStore>,
    /// A Vuex module shape (default export or Nuxt 2 named exports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vuex_module: Option<VuexModule>,
    /// Vuex use: `mapState('ns', [...])`, `$store.dispatch('ns/x')`, ….
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vuex_uses: Vec<VuexUse>,
    /// Capitalised top-level bindings (including destructured ones) of a
    /// `<script setup>`; usable as component tags in its template.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup_bindings: Vec<String>,
    /// vue-router route records: top-level arrays/objects that look like routes,
    /// and the `routes` given to `createRouter` / `new VueRouter` (`#router`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_groups: Vec<RouteGroup>,
    /// HTTP calls: known clients (axios, `$fetch`, `useFetch`, `fetch`) and
    /// wrappers recognised by a URL-like first argument.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_calls: Vec<ApiCall>,
    /// Translation keys used: `$t('a.b')`, `t('a.b')`, `i18n.global.t(...)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub i18n_uses: Vec<I18nUse>,
    /// Calls to plain functions whose result is kept or returned
    /// (`const res = await fetchMalls()`), so response fields can be followed
    /// through helper functions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fn_calls: Vec<FnCall>,
    /// Calls whose argument spoofs a method (`save(id, { _method: 'PUT' })`),
    /// for a function elsewhere that posts that argument.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spoof_calls: Vec<SpoofCall>,
    /// Top-level functions whose returned object spoofs a method:
    /// `(name, method)`, the method starting with `?` when set only sometimes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fn_spoofs: Vec<(String, String)>,
    /// Property paths read from this component's props in script
    /// (`props.mall.name`, `this.mall.name` → `mall.name`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prop_reads: Vec<String>,
    /// HTTP calls inside functions / object methods whose URL is built from
    /// the function's parameters (`get(endpoint, id) { api.get(\`${endpoint}/${id}\`) }`):
    /// callers' arguments fill them in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub url_templates: Vec<UrlTemplate>,
    /// Top-level TypeScript interfaces and type aliases (`interface Mall {…}`),
    /// as written, to resolve the types API responses are cast to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<TsTypeDef>,
    /// React function components: capitalized top-level functions, with the
    /// props they take (destructured names, or the fields of their type).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub react_components: Vec<ReactComponent>,
    /// JSX elements rendered with a component name (`<RoomCard />`, `<Tabs.Item>`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jsx_tags: Vec<NamedLine>,
    /// Database tables used through a query builder: `supabase.from('rooms').select()`
    /// → `rooms` (name `table:op`), and `.rpc('fn')` → `rpc:fn`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub db_tables: Vec<NamedLine>,
    /// File directives: `use client`, `use server`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directives: Vec<String>,
    /// Parse errors (first few).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<NamedLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedLine {
    pub name: String,
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// `(imported, local)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub type_only: bool,
    /// `import('…')` / `() => import('…')` (lazy).
    #[serde(default, skip_serializing_if = "is_false")]
    pub dynamic: bool,
    pub line: u32,
}

impl Import {
    /// The import that binds local name `local`.
    pub fn binds(&self, local: &str) -> bool {
        self.default.as_deref() == Some(local)
            || self.namespace.as_deref() == Some(local)
            || self.named.iter().any(|(_, l)| l == local)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclKind {
    Function,
    Class,
    Const,
    Let,
    Type,
    Interface,
    Enum,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decl {
    pub name: String,
    pub kind: DeclKind,
    /// Arrow functions / function expressions assigned to a const count as functions.
    pub exported: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub default_export: bool,
    /// e.g. `useApi(url: string, opts = {})` or `const API_BASE`.
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    pub line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reexport {
    /// Exported name (`default` for default).
    pub exported: String,
    /// Local or imported name (`*` for `export *`).
    pub local: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Prop {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComponentOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<Prop>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emits: Vec<NamedLine>,
    /// Local registrations: `(registered name, local identifier)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mixins: Vec<String>,
    /// Vue 2 `model: { prop, event }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub computed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inject: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provide: Vec<String>,
    /// Nuxt 2 page options: `layout`, `middleware`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub middleware: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_setup: bool,
    /// Has `template` or `render`: a component rather than a mixin.
    #[serde(default, skip_serializing_if = "is_false")]
    pub renders: bool,
    /// Which wrapper: `object`, `defineComponent`, `Vue.extend`, `class`.
    pub style: String,
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SetupMacros {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<Prop>,
    /// Set when props come from a type we could not see (imported type).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props_unresolved: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emits: Vec<NamedLine>,
    /// `defineModel()` names (`modelValue` when unnamed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<Prop>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<NamedLine>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expose: Vec<String>,
    /// `defineOptions({ name })`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Nuxt `definePageMeta({...})` as raw key → source text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_meta: Vec<(String, String)>,
    /// `provide('key', …)` / `inject('key')`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provide: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inject: Vec<String>,
}

impl SetupMacros {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    /// Registered name, e.g. `BaseButton` or `base-button`.
    pub name: String,
    /// Local identifier of the component, when it is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    /// Import source when registered with `() => import('…')`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `Vue.use(Plugin)` / `app.use(Plugin)`: a plugin that may register components.
    #[serde(default, skip_serializing_if = "is_false")]
    pub plugin: bool,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationGlob {
    /// Directory (relative to the file) for `require.context`, or the glob pattern.
    pub pattern: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub recursive: bool,
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PiniaStore {
    pub id: String,
    /// Exported binding, e.g. `useCartStore`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<String>,
    /// `options` or `setup`.
    pub style: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub getters: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    pub line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VuexModule {
    #[serde(default, skip_serializing_if = "is_false")]
    pub namespaced: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub getters: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    /// `modules: { cart, user: userModule }` → `(name, local identifier)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<(String, String)>,
    /// `new Vuex.Store({...})` / `createStore({...})` rather than a plain module.
    #[serde(default, skip_serializing_if = "is_false")]
    pub root: bool,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VuexUse {
    /// `state`, `getter`, `action`, `mutation`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub name: String,
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteGroup {
    /// Const name, `default` for the default export, `#router` for router options.
    pub binding: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub exported: bool,
    pub items: Vec<RouteItem>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteItem {
    Node(RouteNode),
    /// An identifier (possibly spread) naming more routes, here or imported.
    Ref {
        name: String,
        line: u32,
    },
    /// A call or expression whose routes can't be read statically.
    Dynamic {
        text: String,
        line: u32,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteNode {
    /// `None` when the path is not a literal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `import:<spec>` (lazy or const lazy) or `local:<identifier>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect: Option<String>,
    /// `meta` as written (compacted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<RouteItem>,
    pub line: u32,
}

/// A call passing a body that spoofs `method` as argument `index`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoofCall {
    /// As written: `save`, `ordersApi.update`.
    pub callee: String,
    pub index: u32,
    pub method: String,
}

/// The top-level keys of a request's body and query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    pub keys: Vec<String>,
    /// Every key is known (no spread, variable or helper codemap cannot see into).
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Sent {
    fn add(&mut self, k: &str) {
        if k != "_method" && !self.keys.iter().any(|x| x == k) {
            self.keys.push(k.to_owned());
        }
    }

    pub fn push(&mut self, k: &str) {
        self.add(k);
    }

    pub fn unknown(note: impl Into<String>) -> Self {
        Self {
            keys: Vec::new(),
            complete: false,
            note: Some(note.into()),
        }
    }

    /// Adds another part's keys; one unknown part makes the whole unknown.
    pub fn merge(&mut self, o: Sent) {
        for k in &o.keys {
            self.add(k);
        }
        if !o.complete {
            self.complete = false;
            if self.note.is_none() {
                self.note = o.note;
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiCall {
    /// The object method this call sits in (`ordersApi.cancel` for
    /// `ordersApi = { cancel() {…} }`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_member: Option<String>,
    /// Where a spoofed method may come from in another file: `@helper` (an
    /// imported function building the body) or `param:<function>#<index>`
    /// (the body is a parameter; its callers pass it). Several, `;`-separated,
    /// are tried in order; `@a,@b` tries helpers in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoof_via: Option<String>,
    /// The keys the call sends (body and query), when its payload is visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends: Option<Sent>,
    /// `GET`, `POST`, …; `None` when the wrapper's method is not visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Normalised path: `/orders/{}/items` (query string dropped).
    pub url: String,
    /// The URL argument as written.
    pub raw: String,
    /// The call as written: `api.get`, `$fetch`, `useApi()`.
    pub via: String,
    /// The URL starts with a variable (a base URL) that was dropped.
    #[serde(default, skip_serializing_if = "is_false")]
    pub base_var: bool,
    /// The URL ends with an optional segment (`${id ? `/${id}` : ''}`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub optional_tail: bool,
    /// The values the leading variable can take, when it is a small choice of
    /// strings (`kind === 'x' ? 'area/x' : 'area'`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefixes: Vec<String>,
    /// Every URL the call can have when its embedded values are a small
    /// known choice (`/${kind}-permit/${id}` with `kind: 'loading' | 'unloading'`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// Some parts of `variants` were combined without knowing they go
    /// together: they are tried, but none is reported as missing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub variants_loose: bool,
    /// Why the leading variable could not be expanded (a limit, or a value
    /// codemap does not follow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_note: Option<String>,
    /// Absolute URL to another host.
    #[serde(default, skip_serializing_if = "is_false")]
    pub external: bool,
    pub line: u32,
    /// Property paths read from the result in the same function (heuristic).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
    /// State the result is stored in: `orders`, `this.orders`, `items.value`
    /// (normalised to the bare name), with the path taken from the result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stored_as: Vec<(String, String)>,
    /// Top-level function this call sits in, when that function returns its result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_fn: Option<String>,
    /// How the function's return value is built from the result:
    /// `(returned path, result path)`, e.g. `("malls", "data")` for
    /// `.then(r => ({ malls: r.data }))`, `("", "")` for `return api.get(…)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returned: Vec<(String, String)>,
    /// URL parts of each argument (`(static, text)`; `None` when not readable),
    /// for calls through project wrappers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<Option<Vec<(bool, String)>>>,
    /// The response type given to the call (`useApi()<Paginated<Mall>>(…)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_arg: Option<String>,
}

/// `interface Paginated<T> extends Base { data: T[]; meta?: Meta }` or
/// `type Mall = { … }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TsTypeDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<String>,
    /// The body (`{ … }`) or the aliased type, as written.
    pub body: String,
    pub line: u32,
}

/// A React function component.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReactComponent {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<Prop>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub default_export: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub exported: bool,
    pub line: u32,
}

/// A URL built from a function's parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlTemplate {
    /// `generalLedgerApi.get` (object method) or `getRecord` (function).
    pub key: String,
    pub params: Vec<String>,
    /// URL parts of each parameter's default value.
    pub defaults: Vec<Option<Vec<(bool, String)>>>,
    /// URL parts; a non-static part naming a parameter takes the argument.
    pub parts: Vec<(bool, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub line: u32,
}

/// A call to a plain function (`fetchMalls(…)`), with what is done with its result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FnCall {
    pub name: String,
    pub line: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stored_as: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_fn: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returned: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct I18nUse {
    /// The key, or its static prefix when `dynamic` (`status.` for `` `status.${s}` ``).
    pub key: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub dynamic: bool,
    pub line: u32,
    /// A default text is given (`t('key', 'Fallback')`): a missing key is not shown raw.
    #[serde(default, skip_serializing_if = "is_false")]
    pub guarded: bool,
}
