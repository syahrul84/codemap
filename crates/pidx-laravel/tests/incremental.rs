//! Routes are re-extracted only when something they depend on changes.

mod common;

use std::path::Path;

use common::write;
use filetime::{FileTime, set_file_mtime};
use pidx_core::{Config, ScanOptions, Store};

fn touch(root: &Path, rel: &str, content: &str, t: i64) {
    write(root, rel, content);
    set_file_mtime(
        root.join(rel),
        FileTime::from_unix_time(1_760_000_000 + t, 0),
    )
    .unwrap();
}

fn refresh(store: &mut Store, root: &Path) -> pidx_laravel::RefreshReport {
    pidx_laravel::refresh(store, root, &Config::default(), ScanOptions::default()).unwrap()
}

#[test]
fn routes_rerun_only_for_route_relevant_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    touch(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v11.0.0"}]}"#,
        0,
    );
    touch(
        root,
        "routes/web.php",
        "<?php\nRoute::get('/', fn () => 'a');\nrequire __DIR__.'/../app/Routes/extra.php';\n",
        0,
    );
    touch(
        root,
        "app/Routes/extra.php",
        "<?php\nRoute::get('/extra', fn () => 'b');\n",
        0,
    );
    touch(
        root,
        "app/Models/User.php",
        "<?php\nnamespace App\\Models;\nclass User {}\n",
        0,
    );
    let mut store = Store::open_in_memory().unwrap();

    let r = refresh(&mut store, root);
    assert!(r.laravel.routes.ran);
    assert_eq!(r.laravel.routes.items, 2);

    let r = refresh(&mut store, root);
    assert!(!r.laravel.routes.ran, "nothing changed");
    assert_eq!(r.laravel.routes.items, 2);

    touch(
        root,
        "app/Models/User.php",
        "<?php\nnamespace App\\Models;\nclass User { function x() {} }\n",
        10,
    );
    let r = refresh(&mut store, root);
    assert_eq!(r.php.parsed, 1);
    assert!(
        !r.laravel.routes.ran,
        "a model change does not affect routes"
    );

    // A file outside routes/ that a route file requires is tracked too.
    touch(
        root,
        "app/Routes/extra.php",
        "<?php\nRoute::get('/extra', fn () => 'b');\nRoute::get('/more', fn () => 'c');\n",
        20,
    );
    let r = refresh(&mut store, root);
    assert!(r.laravel.routes.ran);
    assert_eq!(r.laravel.routes.items, 3);

    touch(
        root,
        "routes/web.php",
        "<?php\nRoute::get('/', fn () => 'a');\n",
        30,
    );
    let r = refresh(&mut store, root);
    assert!(r.laravel.routes.ran);
    assert_eq!(r.laravel.routes.items, 1);
}

#[test]
fn data_reruns_only_for_models_schema_and_requests() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    touch(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v11.0.0"}]}"#,
        0,
    );
    let model = "<?php\nnamespace App\\Models;\nuse Illuminate\\Database\\Eloquent\\Model;\nclass Order extends Model {}\n";
    touch(root, "app/Models/Order.php", model, 0);
    touch(
        root,
        "app/Services/Pricing.php",
        "<?php\nnamespace App\\Services;\nclass Pricing {}\n",
        0,
    );
    let mut store = Store::open_in_memory().unwrap();
    assert!(refresh(&mut store, root).laravel.data.ran);

    touch(
        root,
        "app/Services/Pricing.php",
        "<?php\nnamespace App\\Services;\nclass Pricing { function x() {} }\n",
        10,
    );
    assert!(
        !refresh(&mut store, root).laravel.data.ran,
        "a service change does not touch models"
    );

    touch(
        root,
        "app/Models/Order.php",
        &model.replace("{}", "{ protected $table = 'sales'; }"),
        20,
    );
    let r = refresh(&mut store, root);
    assert!(r.laravel.data.ran);
    assert_eq!(
        pidx_laravel::models::load(store.conn()).unwrap()[0].table,
        "sales"
    );

    touch(
        root,
        "app/Models/Invoice.php",
        &model.replace("Order", "Invoice"),
        30,
    );
    assert_eq!(
        refresh(&mut store, root).laravel.data.models,
        2,
        "a new model file is picked up"
    );

    touch(
        root,
        "database/migrations/2024_01_01_000000_x.php",
        "<?php\n",
        40,
    );
    assert!(refresh(&mut store, root).laravel.data.ran);
}
