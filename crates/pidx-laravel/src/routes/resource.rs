//! Resource route expansion, mirroring `Illuminate\Routing\ResourceRegistrar`.

use std::collections::BTreeMap;

use super::{ActionKind, GET, RouteDef};
use crate::inflector;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Resource,
    Api,
    Singleton,
    ApiSingleton,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Names {
    #[default]
    None,
    /// `->names('photo')`: replaces the base name.
    Base(String),
    /// `->names(['index' => 'photos.all'])` / `->name('index', ...)`: exact names.
    Map(BTreeMap<String, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceSpec {
    pub kind: ResourceKind,
    pub name: String,
    pub controller: Option<String>,
    pub only: Option<Vec<String>>,
    pub except: Vec<String>,
    pub names: Names,
    /// Option `as`: prefix for all route names of this resource.
    pub as_prefix: Option<String>,
    pub parameters: BTreeMap<String, String>,
    pub shallow: bool,
    pub creatable: bool,
    pub destroyable: bool,
    pub middleware: Vec<String>,
}

impl ResourceSpec {
    pub fn new(kind: ResourceKind, name: &str, controller: Option<String>) -> Self {
        Self {
            kind,
            name: name.to_owned(),
            controller,
            only: None,
            except: Vec::new(),
            names: Names::None,
            as_prefix: None,
            parameters: BTreeMap::new(),
            shallow: false,
            creatable: false,
            destroyable: false,
            middleware: Vec::new(),
        }
    }
}

const RESOURCE_DEFAULTS: &[&str] = &[
    "index", "create", "store", "show", "edit", "update", "destroy",
];
const API_DEFAULTS: &[&str] = &["index", "show", "store", "update", "destroy"];
const SINGLETON_DEFAULTS: &[&str] = &["show", "edit", "update"];

/// One expanded route before group attributes are applied: methods, URI
/// (relative to any resource prefix), route name, controller method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    pub methods: Vec<String>,
    pub uri: String,
    pub name: String,
    pub action: String,
}

fn methods_of(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

/// `str_replace('-', '_', parameters[v] ?? Str::singular(v))`
fn wildcard(spec: &ResourceSpec, segment: &str) -> String {
    let value = spec
        .parameters
        .get(segment)
        .cloned()
        .unwrap_or_else(|| inflector::singular(segment));
    value.replace('-', "_")
}

/// `photos.comments` → `photos/{photo}/comments`.
fn resource_uri(spec: &ResourceSpec, name: &str) -> String {
    if !name.contains('.') {
        return name.to_owned();
    }
    let segments: Vec<&str> = name.split('.').collect();
    let full: Vec<String> = segments
        .iter()
        .map(|s| format!("{s}/{{{}}}", wildcard(spec, s)))
        .collect();
    let last = segments.last().copied().unwrap_or_default();
    full.join("/")
        .replace(&format!("/{{{}}}", wildcard(spec, last)), "")
}

fn route_name(spec: &ResourceSpec, resource: &str, method: &str) -> String {
    let mut name = resource.to_owned();
    match &spec.names {
        Names::Base(base) => name = base.clone(),
        Names::Map(map) => {
            if let Some(exact) = map.get(method) {
                return exact.clone();
            }
        }
        Names::None => {}
    }
    let prefix = spec
        .as_prefix
        .as_deref()
        .map(|a| format!("{a}."))
        .unwrap_or_default();
    format!("{prefix}{name}.{method}")
        .trim_matches('.')
        .to_owned()
}

/// Expands a resource into its routes, in Laravel's registration order.
/// Returns the URI prefix from a `/` in the name (e.g. `admin/photos` → `admin`).
pub fn expand(spec: &ResourceSpec) -> (String, Vec<Expanded>) {
    let (prefix, name) = match spec.name.rsplit_once('/') {
        Some((prefix, name)) => (prefix.to_owned(), name.to_owned()),
        None => (String::new(), spec.name.clone()),
    };
    let last = name.rsplit('.').next().unwrap_or(&name).to_owned();
    let base = wildcard(spec, &last);
    let shallow_name = if spec.shallow {
        last.clone()
    } else {
        name.clone()
    };

    let defaults: Vec<&str> = match spec.kind {
        ResourceKind::Resource => RESOURCE_DEFAULTS.to_vec(),
        // apiResource passes `only` to resource(), which filters the full defaults
        // (so the order stays index, store, show, update, destroy).
        ResourceKind::Api => RESOURCE_DEFAULTS.to_vec(),
        ResourceKind::Singleton | ResourceKind::ApiSingleton => {
            let mut d = SINGLETON_DEFAULTS.to_vec();
            if spec.creatable {
                d.extend(["create", "store", "destroy"]);
            } else if spec.destroyable {
                d.push("destroy");
            }
            d
        }
    };
    // apiSingleton merges ['only' => [store, show, update, destroy]] unless `only` is given.
    let api_singleton_only: Option<Vec<String>> =
        (spec.kind == ResourceKind::ApiSingleton && spec.only.is_none()).then(|| {
            ["store", "show", "update", "destroy"]
                .map(String::from)
                .to_vec()
        });
    // apiResource merges ['only' => API_DEFAULTS] first, so an explicit `only` wins.
    let api_only: Option<Vec<String>> = (spec.kind == ResourceKind::Api && spec.only.is_none())
        .then(|| API_DEFAULTS.iter().map(|s| (*s).to_owned()).collect());
    let only = spec
        .only
        .as_ref()
        .or(api_singleton_only.as_ref())
        .or(api_only.as_ref());
    let methods: Vec<&str> = defaults
        .into_iter()
        .filter(|m| only.is_none_or(|o| o.iter().any(|x| x == m)))
        .filter(|m| !spec.except.iter().any(|x| x == m))
        .collect();

    let singleton = matches!(
        spec.kind,
        ResourceKind::Singleton | ResourceKind::ApiSingleton
    );
    let mut out = Vec::new();
    for m in methods {
        let (http, uri, resource): (&[&str], String, &str) = if singleton {
            let uri = resource_uri(spec, &name);
            match m {
                "create" => (GET, format!("{uri}/create"), &name),
                "store" => (&["POST"], uri, &name),
                "show" => (GET, uri, &name),
                "edit" => (GET, format!("{uri}/edit"), &name),
                "update" => (&["PUT", "PATCH"], uri, &name),
                _ => (&["DELETE"], uri, &name),
            }
        } else {
            let collection = resource_uri(spec, &name);
            let member = format!("{}/{{{base}}}", resource_uri(spec, &shallow_name));
            match m {
                "index" => (GET, collection, &name),
                "create" => (GET, format!("{collection}/create"), &name),
                "store" => (&["POST"], collection, &name),
                "show" => (GET, member, &shallow_name),
                "edit" => (GET, format!("{member}/edit"), &shallow_name),
                "update" => (&["PUT", "PATCH"], member, &shallow_name),
                _ => (&["DELETE"], member, &shallow_name),
            }
        };
        out.push(Expanded {
            methods: methods_of(http),
            uri,
            name: route_name(spec, resource, m),
            action: m.to_owned(),
        });
    }
    (prefix, out)
}

/// Turns an expansion into route definitions (group attributes applied by the caller).
pub fn to_routes(spec: &ResourceSpec, expanded: Vec<Expanded>) -> Vec<RouteDef> {
    expanded
        .into_iter()
        .map(|e| RouteDef {
            methods: e.methods,
            uri: e.uri,
            name: Some(e.name),
            action_kind: if spec.controller.is_some() {
                ActionKind::Controller
            } else {
                ActionKind::Unresolved
            },
            action_raw: spec
                .controller
                .as_ref()
                .map(|c| format!("{c}@{}", e.action)),
            controller_fqn: spec.controller.clone(),
            controller_method: Some(e.action),
            middleware: spec.middleware.clone(),
            unresolved_reason: spec
                .controller
                .is_none()
                .then(|| "resource controller is not a literal class".to_owned()),
            ..RouteDef::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(spec: &ResourceSpec) -> Vec<String> {
        let (prefix, routes) = expand(spec);
        routes
            .iter()
            .map(|r| {
                let uri = if prefix.is_empty() {
                    r.uri.clone()
                } else {
                    format!("{prefix}/{}", r.uri)
                };
                format!("{} {} {} {}", r.methods.join("|"), uri, r.name, r.action)
            })
            .collect()
    }

    fn spec(kind: ResourceKind, name: &str) -> ResourceSpec {
        ResourceSpec::new(kind, name, Some("PhotoController".into()))
    }

    #[test]
    fn full_resource() {
        assert_eq!(
            table(&spec(ResourceKind::Resource, "photos")),
            [
                "GET|HEAD photos photos.index index",
                "GET|HEAD photos/create photos.create create",
                "POST photos photos.store store",
                "GET|HEAD photos/{photo} photos.show show",
                "GET|HEAD photos/{photo}/edit photos.edit edit",
                "PUT|PATCH photos/{photo} photos.update update",
                "DELETE photos/{photo} photos.destroy destroy",
            ]
        );
    }

    #[test]
    fn api_resource_with_only_and_except() {
        assert_eq!(
            table(&spec(ResourceKind::Api, "photos")),
            [
                "GET|HEAD photos photos.index index",
                "POST photos photos.store store",
                "GET|HEAD photos/{photo} photos.show show",
                "PUT|PATCH photos/{photo} photos.update update",
                "DELETE photos/{photo} photos.destroy destroy",
            ]
        );
        let mut s = spec(ResourceKind::Api, "photos");
        s.only = Some(vec!["index".into(), "show".into()]);
        assert_eq!(
            table(&s),
            [
                "GET|HEAD photos photos.index index",
                "GET|HEAD photos/{photo} photos.show show"
            ]
        );
        let mut s = spec(ResourceKind::Resource, "photos");
        s.except = vec!["create".into(), "edit".into(), "destroy".into()];
        assert_eq!(table(&s).len(), 4);
    }

    #[test]
    fn nested_and_shallow() {
        let s = spec(ResourceKind::Resource, "photos.comments");
        let t = table(&s);
        assert_eq!(
            t[0],
            "GET|HEAD photos/{photo}/comments photos.comments.index index"
        );
        assert_eq!(
            t[3],
            "GET|HEAD photos/{photo}/comments/{comment} photos.comments.show show"
        );
        let mut s = spec(ResourceKind::Resource, "photos.comments");
        s.shallow = true;
        let t = table(&s);
        assert_eq!(
            t[0],
            "GET|HEAD photos/{photo}/comments photos.comments.index index"
        );
        assert_eq!(
            t[2],
            "POST photos/{photo}/comments photos.comments.store store"
        );
        assert_eq!(t[3], "GET|HEAD comments/{comment} comments.show show");
        assert_eq!(t[6], "DELETE comments/{comment} comments.destroy destroy");
    }

    #[test]
    fn names_parameters_and_prefix() {
        let mut s = spec(ResourceKind::Api, "admin/user-profiles");
        s.only = Some(vec!["index".into(), "show".into()]);
        assert_eq!(
            table(&s),
            [
                "GET|HEAD admin/user-profiles user-profiles.index index",
                "GET|HEAD admin/user-profiles/{user_profile} user-profiles.show show",
            ]
        );
        let mut s = spec(ResourceKind::Api, "photos");
        s.only = Some(vec!["index".into(), "show".into()]);
        s.parameters.insert("photos".into(), "picture".into());
        s.names = Names::Map(BTreeMap::from([("index".into(), "gallery".into())]));
        assert_eq!(
            table(&s),
            [
                "GET|HEAD photos gallery index",
                "GET|HEAD photos/{picture} photos.show show",
            ]
        );
        let mut s = spec(ResourceKind::Api, "photos");
        s.only = Some(vec!["index".into()]);
        s.names = Names::Base("pics".into());
        s.as_prefix = Some("admin".into());
        assert_eq!(table(&s), ["GET|HEAD photos admin.pics.index index"]);
    }

    #[test]
    fn singletons() {
        assert_eq!(
            table(&spec(ResourceKind::Singleton, "profile")),
            [
                "GET|HEAD profile profile.show show",
                "GET|HEAD profile/edit profile.edit edit",
                "PUT|PATCH profile profile.update update",
            ]
        );
        let mut s = spec(ResourceKind::Singleton, "photos.thumbnail");
        s.creatable = true;
        assert_eq!(
            table(&s),
            [
                "GET|HEAD photos/{photo}/thumbnail photos.thumbnail.show show",
                "GET|HEAD photos/{photo}/thumbnail/edit photos.thumbnail.edit edit",
                "PUT|PATCH photos/{photo}/thumbnail photos.thumbnail.update update",
                "GET|HEAD photos/{photo}/thumbnail/create photos.thumbnail.create create",
                "POST photos/{photo}/thumbnail photos.thumbnail.store store",
                "DELETE photos/{photo}/thumbnail photos.thumbnail.destroy destroy",
            ]
        );
        assert_eq!(
            table(&spec(ResourceKind::ApiSingleton, "profile")),
            [
                "GET|HEAD profile profile.show show",
                "PUT|PATCH profile profile.update update"
            ]
        );
        let mut s = spec(ResourceKind::ApiSingleton, "profile");
        s.destroyable = true;
        assert_eq!(
            table(&s).last().unwrap(),
            "DELETE profile profile.destroy destroy"
        );
    }
}
