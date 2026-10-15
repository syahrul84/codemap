use pidx_js::{DeclKind, ScriptKind, analyze, sfc};

fn names<T>(v: &[T], f: impl Fn(&T) -> String) -> Vec<String> {
    v.iter().map(f).collect()
}

#[test]
fn options_api_component() {
    let src = r#"
import BaseCard from '@/components/BaseCard.vue'
import formMixin from '@/mixins/form'
import { mapState, mapActions } from 'vuex'

export default {
  name: 'OrderForm',
  mixins: [formMixin],
  components: { BaseCard, 'order-line': () => import('./OrderLine.vue') },
  model: { prop: 'order', event: 'change' },
  props: {
    order: { type: Object, required: true },
    mode: { type: String, default: 'create' },
    tags: [Array, String],
  },
  emits: ['saved'],
  layout: 'admin',
  middleware: ['auth', 'admin'],
  data() {
    return { saving: false, errors: {} }
  },
  computed: {
    ...mapState('cart', ['items', 'total']),
    title() { return this.order.name },
  },
  methods: {
    ...mapActions('cart', { add: 'addItem' }),
    async save() {
      await this.$store.dispatch('orders/save', this.order)
      this.$emit('saved', this.order)
      return this.$store.getters['orders/count'] + this.$store.getters.total
    },
  },
}
"#;
    let f = analyze(src, "js", 1, ScriptKind::VueScript);
    assert!(f.errors.is_empty(), "{:?}", f.errors);
    let o = f.options.as_ref().unwrap();
    assert_eq!(o.style, "object");
    assert_eq!(o.name.as_deref(), Some("OrderForm"));
    assert_eq!(o.mixins, ["formMixin"]);
    assert_eq!(
        o.components,
        [
            ("BaseCard".to_owned(), "BaseCard".to_owned()),
            ("order-line".to_owned(), "import:./OrderLine.vue".to_owned())
        ]
    );
    assert_eq!(o.model, Some(("order".into(), "change".into())));
    let props: Vec<(String, Option<String>, bool, Option<String>)> = o
        .props
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                p.r#type.clone(),
                p.required,
                p.default.clone(),
            )
        })
        .collect();
    assert_eq!(
        props,
        [
            ("order".into(), Some("Object".into()), true, None),
            (
                "mode".into(),
                Some("String".into()),
                false,
                Some("'create'".into())
            ),
            ("tags".into(), Some("[Array, String]".into()), false, None),
        ]
    );
    assert_eq!(o.props[0].line, 12);
    assert_eq!(names(&o.emits, |e| e.name.clone()), ["saved"]);
    assert_eq!(o.layout.as_deref(), Some("admin"));
    assert_eq!(o.middleware, ["auth", "admin"]);
    assert_eq!(o.data, ["saving", "errors"]);
    assert_eq!(o.computed, ["items", "total", "title"]);
    assert_eq!(o.methods, ["add", "save"]);
    assert_eq!(
        names(&f.emitted, |e| format!("{}:{}", e.name, e.line)),
        ["saved:30"]
    );
    let uses = names(&f.vuex_uses, |u| {
        format!(
            "{} {}/{} :{}",
            u.kind,
            u.namespace.as_deref().unwrap_or("-"),
            u.name,
            u.line
        )
    });
    assert_eq!(
        uses,
        [
            "state cart/items :23",
            "state cart/total :23",
            "action cart/add :27",
            "action orders/save :29",
            "getter orders/count :31",
            "getter -/total :31",
        ]
    );
    let dynamic: Vec<&str> = f
        .imports
        .iter()
        .filter(|i| i.dynamic)
        .map(|i| i.source.as_str())
        .collect();
    assert_eq!(dynamic, ["./OrderLine.vue"]);
}

#[test]
fn script_setup_typescript_macros() {
    let src = r#"
import type { User } from '~/types'
import { useCartStore } from '~/stores/cart'

interface Props {
  /** The user */
  user: User
  size?: 'sm' | 'lg'
  onPick?(id: number): void
}
const props = withDefaults(defineProps<Props>(), { size: 'sm' })
const emit = defineEmits<{
  (e: 'update', id: number): void
  (e: 'close'): void
}>()
const title = defineModel<string>('title', { required: true })
const open = defineModel()
defineSlots<{ default(props: { item: User }): any; footer: any }>()
defineOptions({ name: 'UserCard' })
definePageMeta({ layout: 'dashboard', middleware: ['auth'] })
const cart = useCartStore()
const { t } = useI18n()
function pick() { emit('update', props.user.id) }
provide('card', 1)
defineExpose({ pick })
"#;
    let f = analyze(src, "ts", 1, ScriptKind::VueSetup);
    assert!(f.errors.is_empty(), "{:?}", f.errors);
    let s = &f.setup;
    let props: Vec<String> = names(&s.props, |p| {
        format!(
            "{}:{}:{}:{}",
            p.name,
            p.r#type.clone().unwrap_or_default(),
            p.required,
            p.default.clone().unwrap_or_default()
        )
    });
    assert_eq!(
        props,
        [
            "user:User:true:",
            "size:'sm' | 'lg':false:'sm'",
            "onPick:Function:false:"
        ]
    );
    assert_eq!(s.props[0].line, 7);
    assert_eq!(names(&s.emits, |e| e.name.clone()), ["update", "close"]);
    let models = names(&s.models, |m| {
        format!(
            "{}:{}:{}",
            m.name,
            m.r#type.clone().unwrap_or_default(),
            m.required
        )
    });
    assert_eq!(models, ["title:string:true", "modelValue::false"]);
    assert_eq!(names(&s.slots, |e| e.name.clone()), ["footer", "default"]);
    assert_eq!(s.name.as_deref(), Some("UserCard"));
    assert_eq!(
        s.page_meta,
        [
            ("layout".to_owned(), "dashboard".to_owned()),
            ("middleware".to_owned(), "['auth']".to_owned())
        ]
    );
    assert_eq!(s.provide, ["card"]);
    assert_eq!(s.expose, ["pick"]);
    assert_eq!(
        names(&f.composables, |c| c.name.clone()),
        ["useCartStore", "useI18n"]
    );
    assert_eq!(names(&f.emitted, |e| e.name.clone()), ["update"]);
    assert!(f.options.is_none());
}

#[test]
fn setup_props_from_literal_destructure_and_imported_type() {
    let f = analyze(
        "const { size = 'md', label } = defineProps<{ size?: string; label: string }>()",
        "ts",
        1,
        ScriptKind::VueSetup,
    );
    let props = names(&f.setup.props, |p| {
        format!("{}={}", p.name, p.default.clone().unwrap_or_default())
    });
    assert_eq!(props, ["size='md'", "label="]);

    let f = analyze(
        "defineProps<{\n  /** The row */\n  row: { id: number /* pk */; name: string } // note\n}>()",
        "ts",
        1,
        ScriptKind::VueSetup,
    );
    assert_eq!(
        f.setup.props[0].r#type.as_deref(),
        Some("{ id: number ; name: string }")
    );

    let f = analyze(
        "import type { ButtonProps } from './types'\ndefineProps<ButtonProps>()",
        "ts",
        1,
        ScriptKind::VueSetup,
    );
    assert!(f.setup.props.is_empty());
    assert_eq!(
        f.setup.props_unresolved.as_deref(),
        Some("props type `ButtonProps` is imported from './types'")
    );

    let f = analyze(
        "defineProps(['a', 'b'])\ndefineEmits(['x'])",
        "js",
        1,
        ScriptKind::VueSetup,
    );
    assert_eq!(names(&f.setup.props, |p| p.name.clone()), ["a", "b"]);
    assert_eq!(names(&f.setup.emits, |p| p.name.clone()), ["x"]);
    // Vue 3.3 named-tuple emits.
    let f = analyze(
        "defineEmits<{ change: [id: number]; close: [] }>()",
        "ts",
        1,
        ScriptKind::VueSetup,
    );
    assert_eq!(
        names(&f.setup.emits, |p| p.name.clone()),
        ["change", "close"]
    );
}

#[test]
fn define_component_with_setup() {
    let src = "import { defineComponent } from 'vue'\nexport default defineComponent({\n  props: ['id'],\n  emits: { done: null },\n  setup(props, { emit }) {\n    const api = useApi()\n    emit('done')\n    return {}\n  }\n})\n";
    let f = analyze(src, "ts", 1, ScriptKind::VueScript);
    let o = f.options.unwrap();
    assert_eq!(o.style, "defineComponent");
    assert!(o.has_setup);
    assert_eq!(names(&o.emits, |e| e.name.clone()), ["done"]);
    assert_eq!(names(&f.emitted, |e| e.name.clone()), ["done"]);
    assert_eq!(names(&f.composables, |e| e.name.clone()), ["useApi"]);
}

#[test]
fn pinia_stores() {
    let src = r#"
import { defineStore } from 'pinia'
export const useCartStore = defineStore('cart', {
  state: () => ({ items: [], coupon: null }),
  getters: { count: (s) => s.items.length },
  actions: { add(item) { this.items.push(item) }, ...other },
})
const ID = 'auth'
export const useAuthStore = defineStore(ID, () => {
  const user = ref(null)
  const loggedIn = computed(() => !!user.value)
  async function login() {}
  const logout = () => {}
  return { user, loggedIn, login, logout }
})
export const useLegacy = defineStore({ id: 'legacy', state: () => ({ a: 1 }) })
"#;
    let f = analyze(src, "ts", 1, ScriptKind::Module);
    let stores = names(&f.pinia_stores, |s| {
        format!(
            "{} {} {} s={:?} g={:?} a={:?} :{}-{}",
            s.id,
            s.export.clone().unwrap_or_default(),
            s.style,
            s.state,
            s.getters,
            s.actions,
            s.line,
            s.end_line
        )
    });
    assert_eq!(
        stores,
        [
            r#"cart useCartStore options s=["items", "coupon"] g=["count"] a=["add"] :3-7"#,
            r#"auth useAuthStore setup s=["user"] g=["loggedIn"] a=["login", "logout"] :9-15"#,
            r#"legacy useLegacy options s=["a"] g=[] a=[] :16-16"#,
        ]
    );
    let decls = names(&f.decls, |d| {
        format!("{} {:?} {}", d.name, d.kind, d.exported)
    });
    assert_eq!(
        decls,
        [
            "useCartStore Const true",
            "ID Const false",
            "useAuthStore Const true",
            "useLegacy Const true"
        ]
    );
}

#[test]
fn vuex_modules_and_root_store() {
    let module = r#"
const state = () => ({ items: [], total: 0 })
const getters = { count: (s) => s.items.length }
export default {
  namespaced: true,
  state,
  getters,
  mutations: { SET_ITEMS(s, v) { s.items = v } },
  actions: { async load({ commit }) { commit('SET_ITEMS', []) } },
}
"#;
    let f = analyze(module, "js", 1, ScriptKind::Module);
    let m = f.vuex_module.unwrap();
    assert!(m.namespaced && !m.root);
    assert_eq!(m.state, ["items", "total"]);
    assert_eq!(m.getters, ["count"]);
    assert_eq!(m.mutations, ["SET_ITEMS"]);
    assert_eq!(m.actions, ["load"]);
    assert!(f.options.is_none());

    let root = "import Vue from 'vue'\nimport Vuex from 'vuex'\nimport cart from './modules/cart'\nimport user from './modules/user'\nVue.use(Vuex)\nexport default new Vuex.Store({ modules: { cart, account: user }, state: { ready: false } })\n";
    let m = analyze(root, "js", 1, ScriptKind::Module)
        .vuex_module
        .unwrap();
    assert!(m.root);
    assert_eq!(
        m.modules,
        [
            ("cart".to_owned(), "cart".to_owned()),
            ("account".to_owned(), "user".to_owned())
        ]
    );
    assert_eq!(m.state, ["ready"]);

    // Named export, as in many Vue 2 templates.
    let named = "import Vuex from 'vuex'\nimport auth from './modules/auth'\nexport const store = new Vuex.Store({ modules: { auth } })\n";
    let m = analyze(named, "js", 1, ScriptKind::Module)
        .vuex_module
        .unwrap();
    assert!(m.root);
    assert_eq!(m.modules, [("auth".to_owned(), "auth".to_owned())]);
    let v4 = "import { createStore } from 'vuex'\nexport default createStore({ state: { a: 1 }, modules: { cart } })\n";
    assert!(
        analyze(v4, "js", 1, ScriptKind::Module)
            .vuex_module
            .unwrap()
            .root
    );

    // Nuxt 2 store/ files use named exports.
    let nuxt2 = "export const state = () => ({ list: [] })\nexport const mutations = { add(s, x) { s.list.push(x) } }\nexport const actions = { async fetch({ commit }) {} }\n";
    let m = analyze(nuxt2, "js", 1, ScriptKind::Module)
        .vuex_module
        .unwrap();
    assert_eq!(
        (m.state, m.mutations, m.actions),
        (
            vec!["list".to_owned()],
            vec!["add".to_owned()],
            vec!["fetch".to_owned()]
        )
    );
}

#[test]
fn global_registrations() {
    let src = r#"
import Vue from 'vue'
import BaseButton from './components/BaseButton.vue'
Vue.component('BaseButton', BaseButton)
Vue.component('LazyChart', () => import('./components/Chart.vue'))
app.component('async-thing', defineAsyncComponent(() => import('./Thing.vue')))
const ctx = require.context('./components/base', true, /Base[A-Z]\w+\.vue$/)
ctx.keys().forEach((f) => Vue.component(name(f), ctx(f).default))
"#;
    let f = analyze(src, "js", 1, ScriptKind::Module);
    let regs = names(&f.registrations, |r| {
        format!(
            "{} {} {} :{}",
            r.name,
            r.local.clone().unwrap_or_default(),
            r.source.clone().unwrap_or_default(),
            r.line
        )
    });
    assert_eq!(
        regs,
        [
            "BaseButton BaseButton  :4",
            "LazyChart  ./components/Chart.vue :5",
            "async-thing  ./Thing.vue :6"
        ]
    );
    assert_eq!(f.registration_globs.len(), 1);
    assert_eq!(f.registration_globs[0].pattern, "./components/base");
    assert!(f.registration_globs[0].recursive);
}

#[test]
fn module_declarations_docs_and_exports() {
    let src = r#"
/**
 * Formats a price in cents.
 * @param cents number
 */
export function formatPrice(cents: number, currency = 'MYR'): string { return '' }

/** Shared API client. */
export const useApi = <T>(url: string, opts: Opts = {}) => {}
const internal = 1, other = function (a) {}
export { internal as publicName, other }
export { default as OrderCard } from './OrderCard.vue'
export * from './money'
export default function (x) { return x }
export interface Opts { a: number }
export type Id = string
export class Client extends Base {}
"#;
    let f = analyze(src, "ts", 1, ScriptKind::Module);
    assert!(f.errors.is_empty(), "{:?}", f.errors);
    let decls = names(&f.decls, |d| {
        format!(
            "{} {:?} exp={} def={} [{}] {}-{} {}",
            d.name,
            d.kind,
            d.exported,
            d.default_export,
            d.signature,
            d.line,
            d.end_line,
            d.doc.clone().unwrap_or_default()
        )
    });
    assert_eq!(
        decls,
        [
            "formatPrice Function exp=true def=false [formatPrice(cents: number, currency = 'MYR')] 6-6 Formats a price in cents.",
            "useApi Function exp=true def=false [useApi(url: string, opts: Opts = {})] 9-9 Shared API client.",
            "internal Const exp=true def=false [const internal] 10-10 ",
            "other Function exp=true def=false [other(a)] 10-10 ",
            "default Function exp=true def=true [default(x)] 14-14 ",
            "Opts Interface exp=true def=false [interface Opts] 15-15 ",
            "Id Type exp=true def=false [type Id] 16-16 ",
            "Client Class exp=true def=false [class Client extends Base] 17-17 ",
        ]
    );
    assert!(f.decls.iter().any(|d| d.kind == DeclKind::Function));
    let re = names(&f.reexports, |r| {
        format!(
            "{}<-{}@{}",
            r.exported,
            r.local,
            r.from.clone().unwrap_or_default()
        )
    });
    assert_eq!(
        re,
        [
            "publicName<-internal@",
            "OrderCard<-default@./OrderCard.vue",
            "*<-*@./money"
        ]
    );
}

#[test]
fn class_components() {
    let src = "import { Component, Prop, Vue } from 'vue-property-decorator'\n@Component\nexport default class UserList extends Vue {\n  @Prop({ required: true }) readonly users!: User[]\n  @Prop() title?: string\n  loading = false\n  get count() { return this.users.length }\n  refresh() {}\n}\n";
    let f = analyze(src, "ts", 1, ScriptKind::VueScript);
    let o = f.options.unwrap();
    assert_eq!(o.style, "class");
    assert_eq!(o.name.as_deref(), Some("UserList"));
    assert_eq!(
        names(&o.props, |p| format!("{}:{}", p.name, p.required)),
        ["users:true", "title:false"]
    );
    assert_eq!(o.data, ["loading"]);
    assert_eq!(o.computed, ["count"]);
    assert_eq!(o.methods, ["refresh"]);
}

#[test]
fn broken_code_reports_errors_without_panicking() {
    let f = analyze(
        "export default {\n  props: [\n",
        "js",
        5,
        ScriptKind::VueScript,
    );
    assert!(!f.errors.is_empty());
    assert!(f.errors[0].line >= 5);
}

#[test]
fn lines_inside_vue_files() {
    let src = "<template>\n  <div/>\n</template>\n\n<script setup lang=\"ts\">\nimport { ref } from 'vue'\nconst count = ref(0)\nconst data = useFetch('/api/x')\n</script>\n";
    let sfc = sfc::split(src);
    let b = sfc.script_setup().unwrap();
    let f = analyze(b.content, b.lang(), b.line, ScriptKind::VueSetup);
    assert_eq!(f.imports[0].line, 6);
    assert_eq!(f.composables[0].name, "useFetch");
    assert_eq!(f.composables[0].line, 8);
}

#[test]
fn vue_router_route_groups() {
    use pidx_js::RouteItem;
    let index = r#"
import Router from 'vue-router'
import adminRoutes from './admin'
import { reportRoutes } from '@/modules/reportRoutes'
const Login = () => import('Views/session/Login')
export default new Router({
  mode: 'history',
  routes: [
    { path: '/', redirect: { name: 'home' } },
    adminRoutes,
    ...reportRoutes,
    { path: '/login', name: 'login', component: Login, meta: { guest: true } },
    { path: '/old', component: require('./Old.vue').default },
    workbench('/wb', 'wb'),
  ],
})
"#;
    let f = analyze(index, "js", 1, ScriptKind::Module);
    assert_eq!(f.route_groups.len(), 1);
    let g = &f.route_groups[0];
    assert_eq!(g.binding, "#router");
    let kinds: Vec<String> = g
        .items
        .iter()
        .map(|i| match i {
            RouteItem::Node(n) => format!(
                "node {} {} {} {}",
                n.path.clone().unwrap_or_default(),
                n.name.clone().unwrap_or_default(),
                n.component.clone().unwrap_or_default(),
                n.redirect.clone().unwrap_or_default()
            ),
            RouteItem::Ref { name, .. } => format!("ref {name}"),
            RouteItem::Dynamic { text, .. } => format!("dyn {text}"),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "node /   { name: 'home' }",
            "ref adminRoutes",
            "ref reportRoutes",
            "node /login login import:Views/session/Login ",
            "node /old  import:./Old.vue ",
            "dyn workbench('/wb', 'wb')",
        ]
    );

    let module = r#"
import Layout from '@/layouts/Admin.vue'
export default {
  path: '/admin',
  component: Layout,
  children: [
    { path: 'users', name: 'admin.users', component: () => import('@/views/Users.vue') },
    { path: 'users/:id', component: () => import('@/views/User.vue'), children: userTabs },
  ],
}
export const userTabs = [{ path: 'roles', component: () => import('@/views/Roles.vue') }]
"#;
    let f = analyze(module, "ts", 1, ScriptKind::Module);
    let names: Vec<(&str, bool)> = f
        .route_groups
        .iter()
        .map(|g| (g.binding.as_str(), g.exported))
        .collect();
    assert_eq!(names, [("default", true), ("userTabs", true)]);
    let RouteItem::Node(root) = &f.route_groups[0].items[0] else {
        panic!()
    };
    assert_eq!(root.component.as_deref(), Some("local:Layout"));
    assert_eq!(root.children.len(), 2);
    let RouteItem::Node(user) = &root.children[1] else {
        panic!()
    };
    assert!(matches!(&user.children[0], RouteItem::Ref { name, .. } if name == "userTabs"));

    // Arrays of references only count when the name says so.
    let f = analyze(
        "export const things = [a, b]\nexport const appRoutes = [a, ...b]",
        "js",
        1,
        ScriptKind::Module,
    );
    assert_eq!(f.route_groups.len(), 1);
    assert_eq!(f.route_groups[0].binding, "appRoutes");
    let f = analyze(
        "const routes = [{ path: '/', component: Home }]\nexport const router = createRouter({ history: h, routes })",
        "ts",
        1,
        ScriptKind::Module,
    );
    assert_eq!(
        f.route_groups
            .iter()
            .map(|g| g.binding.as_str())
            .collect::<Vec<_>>(),
        ["routes", "#router"]
    );

    // Helper functions that return one record.
    let helpers = r#"
import ListPage from '@/pages/ListPage.vue'
const BASE = 'finance'
const list = (path: string, name: string, kind: string): RouteRecordRaw => ({
  path: `${BASE}/${path}`,
  name,
  component: ListPage,
  props: { kind },
})
function detail(path, page) { return { path: path + '/:id', component: page } }
export const financeRoutes = [list('payments', 'finance.payments', 'p'), detail('payments', ListPage), ...extra.map(x => x)]
"#;
    let f = analyze(helpers, "ts", 1, ScriptKind::Module);
    let g = f
        .route_groups
        .iter()
        .find(|g| g.binding == "financeRoutes")
        .unwrap();
    let got: Vec<String> = g
        .items
        .iter()
        .map(|i| match i {
            RouteItem::Node(n) => format!(
                "{} {} {}",
                n.path.clone().unwrap_or_default(),
                n.name.clone().unwrap_or_default(),
                n.component.clone().unwrap_or_default()
            ),
            RouteItem::Ref { name, .. } => format!("ref {name}"),
            RouteItem::Dynamic { text, .. } => format!("dyn {text}"),
        })
        .collect();
    assert_eq!(
        got,
        [
            "finance/payments finance.payments local:ListPage",
            "payments/:id  local:ListPage",
            "dyn extra.map(x => x)",
        ]
    );
}

#[test]
fn api_calls_and_reads() {
    let src = r#"
import api from '@/services/http'
const ORDERS = '/orders'
export async function load(id) {
  const res = await api.get(`/orders/${id}/items?with=product`)
  this.items = res.data.data
  return res.data.meta.total
}
export function save(order) {
  return api.post(ORDERS, order).then((r) => r.data.id)
}
export function cart(merchantId) {
  return useApi()<Cart>(`/carts/${merchantId}/items`, { method: 'POST', body: {} })
}
const { data: mall } = await useFetch(`${config.public.apiBase}/malls/${slug}`)
console.log(mall.value.name, mall.value.address.city)
await $fetch('https://maps.googleapis.com/maps/api/geocode/json')
axios({ url: '/reports/export', method: 'post' })
fetchOrders('abc')
requestMap.get('pending')
router.push('/orders/1')
api.delete('/orders/' + id)
http.get('settings')
"#;
    let f = analyze(src, "ts", 1, ScriptKind::Module);
    let got: Vec<String> = f
        .api_calls
        .iter()
        .map(|c| {
            format!(
                "{} {} via={} base={} ext={} reads={:?} stored={:?}",
                c.method.clone().unwrap_or("?".into()),
                c.url,
                c.via,
                c.base_var,
                c.external,
                c.reads,
                c.stored_as
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            r#"GET /orders/{}/items via=api.get base=false ext=false reads=["data.data", "data.meta.total"] stored=[("res", ""), ("this.items", "data.data")]"#,
            r#"POST /orders via=api.post base=false ext=false reads=["data.id"] stored=[("r", "")]"#,
            r#"POST /carts/{}/items via=useApi() base=false ext=false reads=[] stored=[]"#,
            r#"GET /malls/{} via=useFetch base=true ext=false reads=["value.address.city", "value.name"] stored=[("mall", "")]"#,
            r#"GET /maps/api/geocode/json via=$fetch base=false ext=true reads=[] stored=[]"#,
            r#"POST /reports/export via=axios base=false ext=false reads=[] stored=[]"#,
            r#"DELETE /orders/{} via=api.delete base=false ext=false reads=[] stored=[]"#,
            r#"GET /settings via=http.get base=false ext=false reads=[] stored=[]"#,
        ]
    );
}

#[test]
fn api_url_query_tail_is_dropped() {
    let f = analyze(
        "const qs = params.toString()\nuseApi()<X>(`/merchants${qs ? `?${qs}` : ''}`)",
        "ts",
        1,
        ScriptKind::Module,
    );
    assert_eq!(f.api_calls[0].url, "/merchants");
}

#[test]
fn api_method_spoofing_and_optional_tail() {
    let src = "api.post(`admin/x/${id}/approve`, toForm({ ...payload, _method: 'PUT' }))\napi.post(`${base}/${a}/procurement${record.id ? `/${record.id}` : ''}`, record)";
    let f = analyze(src, "ts", 1, ScriptKind::Module);
    assert_eq!(f.api_calls[0].method.as_deref(), Some("PUT"));
    assert_eq!(f.api_calls[1].url, "/{}/procurement");
    assert!(f.api_calls[1].optional_tail && f.api_calls[1].base_var);
}
