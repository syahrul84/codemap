//! Version and layout detection, with reasons.

mod common;

use common::{fixture, indexed, write};
use pidx_laravel::Layout;
use pidx_laravel::refresh::stored_detection;

#[test]
fn legacy_fixture() {
    let (store, _) = indexed(&fixture("laravel10-legacy"));
    let d = stored_detection(&store).unwrap().unwrap();
    assert_eq!(d.version.as_deref(), Some("10.48.4"));
    assert_eq!(d.major, Some(10));
    assert_eq!(d.version_source.as_deref(), Some("composer.lock"));
    assert!(!d.version_inferred);
    assert_eq!(d.layout, Layout::Legacy);
    assert!(
        d.reasons
            .iter()
            .any(|r| r.contains("App\\Http\\Kernel extends the HTTP Kernel"))
    );
    assert!(d.reasons.iter().any(|r| r.contains("RouteServiceProvider")));
    assert_eq!(d.composer_name.as_deref(), Some("acme/legacy-shop"));
    assert_eq!(d.php_constraint.as_deref(), Some("^8.1"));
    assert_eq!(d.psr4[0].namespace, "App");
    assert_eq!(d.psr4[0].path, "app");
    assert!(d.warnings.is_empty());
}

#[test]
fn slim_and_upgraded_fixtures() {
    let (store, _) = indexed(&fixture("laravel11-slim"));
    let d = stored_detection(&store).unwrap().unwrap();
    assert_eq!((d.layout, d.major), (Layout::Slim, Some(11)));
    assert!(d.with_routing);
    assert!(d.reasons[1].contains("withRouting"));

    let (store, _) = indexed(&fixture("laravel11-upgraded"));
    let d = stored_detection(&store).unwrap().unwrap();
    assert_eq!((d.layout, d.major), (Layout::Legacy, Some(11)));
    assert!(d.reasons.iter().any(|r| r.contains("upgraded app")));
}

#[test]
fn version_from_constraint_is_inferred() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "composer.json",
        r#"{"require":{"laravel/framework":"^10.10"}}"#,
    );
    let (store, _) = indexed(dir.path());
    let d = stored_detection(&store).unwrap().unwrap();
    assert!(d.laravel && d.version_inferred);
    assert_eq!(d.version.as_deref(), Some("10.10"));
    assert_eq!(d.layout, Layout::Unknown);
}

#[test]
fn not_laravel() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "composer.json",
        r#"{"require":{"symfony/console":"^7.0"}}"#,
    );
    write(dir.path(), "app/A.php", "<?php class A {}");
    let (store, report) = indexed(dir.path());
    assert!(!report.laravel.detected);
    let d = stored_detection(&store).unwrap().unwrap();
    assert!(!d.laravel);
    assert!(d.reasons[0].contains("not in composer.lock or composer.json"));
}

#[test]
fn mixed_layout_warns() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v11.0.0"}]}"#,
    );
    write(
        root,
        "bootstrap/app.php",
        "<?php\nreturn Application::configure(basePath: dirname(__DIR__))\n    ->withRouting(web: __DIR__.'/../routes/web.php')->create();\n",
    );
    write(
        root,
        "app/Http/Kernel.php",
        "<?php\nnamespace App\\Http;\nuse Illuminate\\Foundation\\Http\\Kernel as HttpKernel;\nclass Kernel extends HttpKernel {}\n",
    );
    write(
        root,
        "routes/web.php",
        "<?php\nRoute::get('/', fn () => 'ok');\n",
    );
    let (store, report) = indexed(root);
    let d = stored_detection(&store).unwrap().unwrap();
    assert_eq!(d.layout, Layout::Slim);
    assert!(d.warnings[0].starts_with("mixed layout"));
    assert_eq!(report.laravel.routes.items, 1);
    assert!(
        store
            .warnings()
            .unwrap()
            .iter()
            .any(|w| w.message.starts_with("mixed layout"))
    );
}

#[test]
fn convention_fallback_without_any_source() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v9.52.0"}]}"#,
    );
    write(
        root,
        "routes/api.php",
        "<?php\nRoute::get('/ping', fn () => 'pong');\n",
    );
    let (store, _) = indexed(root);
    let all = common::routes(&store);
    assert_eq!(all[0].uri, "api/ping");
    assert_eq!(all[0].middleware, ["api"]);
    assert!(
        store
            .warnings()
            .unwrap()
            .iter()
            .any(|w| w.message.contains("by convention"))
    );
}
