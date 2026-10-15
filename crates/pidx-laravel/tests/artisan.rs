//! Artisan mode with a fake `artisan_command` (a shell script).

mod common;

use std::path::Path;

use common::{indexed_with, routes, write};
use pidx_core::Config;
use pidx_core::config::ArtisanMode;
use pidx_laravel::routes::RouteSource;

fn project(root: &Path) {
    write(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v11.0.0"}]}"#,
    );
    write(
        root,
        "routes/web.php",
        "<?php\nuse App\\X;\nuse Illuminate\\Support\\Facades\\Route;\n\nRoute::get('/a', [X::class, 'a'])->name('a');\nRoute::get('/gone', fn () => 'x');\n",
    );
}

fn config(script: &Path, timeout_ms: u64) -> Config {
    let mut c = Config::default();
    c.laravel.artisan = ArtisanMode::On;
    c.laravel.artisan_command = format!("sh {}", script.display());
    c.laravel.artisan_timeout_ms = timeout_ms;
    c
}

#[test]
fn artisan_routes_are_merged_with_static_locations() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = tempfile::tempdir().unwrap();
    project(dir.path());
    let script = scripts.path().join("artisan.sh");
    std::fs::write(
        &script,
        r#"echo 'Deprecated: something noisy'
cat <<'JSON'
[{"domain":null,"method":"GET|HEAD","uri":"a","name":"a","action":"App\\X@a","middleware":["web","auth"]},
 {"domain":null,"method":"GET|HEAD","uri":"sanctum/csrf-cookie","name":"sanctum.csrf-cookie","action":"Laravel\\Sanctum\\Http\\Controllers\\CsrfCookieController@show","middleware":["web"]}]
JSON
"#,
    )
    .unwrap();
    let (store, report) = indexed_with(dir.path(), &config(&script, 10_000));
    assert_eq!(report.laravel.route_source.as_deref(), Some("artisan"));
    let all = routes(&store);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|r| r.source == RouteSource::Artisan));
    assert_eq!(all[0].location().as_deref(), Some("routes/web.php:5"));
    // artisan reports the resolved middleware
    assert_eq!(all[0].middleware, ["web", "auth"]);
    assert_eq!(all[1].file, None);
    assert_eq!(
        all[1].controller_fqn.as_deref(),
        Some("Laravel\\Sanctum\\Http\\Controllers\\CsrfCookieController")
    );
    // The static `/gone` route was not reported by artisan.
    assert!(
        store
            .warnings()
            .unwrap()
            .iter()
            .any(|w| w.message.contains("1 statically found route"))
    );
}

#[test]
fn failing_artisan_falls_back_to_static() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = tempfile::tempdir().unwrap();
    project(dir.path());
    let script = scripts.path().join("artisan.sh");
    std::fs::write(
        &script,
        "echo 'Could not open input file: artisan' >&2\nexit 1\n",
    )
    .unwrap();
    let (store, report) = indexed_with(dir.path(), &config(&script, 10_000));
    assert_eq!(
        report.laravel.route_source.as_deref(),
        Some("static (artisan failed)")
    );
    assert_eq!(routes(&store).len(), 2);
    let w = store.warnings().unwrap();
    assert!(
        w.iter()
            .any(|w| w.message.contains("artisan route:list failed")
                && w.message.contains("Could not open input file"))
    );
}

#[test]
fn slow_artisan_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = tempfile::tempdir().unwrap();
    project(dir.path());
    let script = scripts.path().join("artisan.sh");
    std::fs::write(&script, "sleep 5\necho '[]'\n").unwrap();
    let started = std::time::Instant::now();
    let (store, _) = indexed_with(dir.path(), &config(&script, 300));
    assert!(started.elapsed().as_secs() < 4, "timeout was not enforced");
    assert!(
        store
            .warnings()
            .unwrap()
            .iter()
            .any(|w| w.message.contains("timed out after 300 ms"))
    );
    assert_eq!(routes(&store).len(), 2);
}

#[test]
fn auto_mode_without_vendor_stays_static() {
    let dir = tempfile::tempdir().unwrap();
    project(dir.path());
    let mut c = Config::default();
    c.laravel.artisan = ArtisanMode::Auto;
    let (_, report) = indexed_with(dir.path(), &c);
    assert_eq!(report.laravel.route_source.as_deref(), Some("static"));
    assert!(
        report
            .laravel
            .routes
            .notes
            .iter()
            .any(|n| n.starts_with("auto:"))
    );
}
