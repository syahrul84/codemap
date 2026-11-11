//! Blade templates: what a view extends, includes and expects, and which
//! controller, mailable, component class or route shows it.

use codemap_core::{Config, ScanOptions, Store};
use serde_json::{Value, json};

const FILES: &[(&str, &str)] = &[
    (
        "composer.json",
        r##"{"name":"acme/shop","require":{"laravel/framework":"^11.0"},"autoload":{"psr-4":{"App\\":"app/"}}}
"##,
    ),
    ("artisan", "#!/usr/bin/env php\n<?php\n"),
    (
        "routes/web.php",
        r##"<?php

use App\Http\Controllers\OrderController;
use Illuminate\Support\Facades\Route;

Route::get('/orders', [OrderController::class, 'index'])->name('orders.index');
Route::put('/orders/{order}', [OrderController::class, 'update'])->name('orders.update');
Route::view('/about', 'pages.about')->name('about');
"##,
    ),
    (
        "app/Http/Controllers/OrderController.php",
        r##"<?php

namespace App\Http\Controllers;

class OrderController
{
    public function index()
    {
        $orders = [];

        return view('orders.index', [
            'orders' => $orders,
            'total' => count($orders),
        ]);
    }

    public function update($order)
    {
        return view('pages.' . $order->kind);
    }

    public function slip($data)
    {
        $template = $data ? 'pdf.slip' : 'pdf.none';
        $viewName = 'pdf/sl' . $data;

        return \PDF::loadView('pdf/slip', $data)->download();
    }
}
"##,
    ),
    (
        "app/View/Components/Alert.php",
        r##"<?php

namespace App\View\Components;

class Alert
{
    public function render()
    {
        return view('components.alert');
    }
}
"##,
    ),
    (
        "app/Mail/OrderPaid.php",
        r##"<?php

namespace App\Mail;

class OrderPaid
{
    public function build()
    {
        return $this->markdown('emails.orders.paid')->with('order', $this->order);
    }
}
"##,
    ),
    (
        "resources/views/layouts/app.blade.php",
        "<html><body>@yield('content') @stack('scripts')</body></html>\n",
    ),
    (
        "resources/views/orders/index.blade.php",
        r##"@extends('layouts.app')
@section('content')
    <x-alert type="info" :message="$notice" />
    @foreach ($orders as $order)
        @include('orders.partials.row', ['order' => $order])
    @endforeach
    <p>{{ $total }} — orders</p>
    <form method="POST" action="{{ route('orders.update', $current) }}">
        @csrf
        @method('PUT')
    </form>
    @include('orders.partials.gone')
@endsection
"##,
    ),
    (
        "resources/views/orders/partials/row.blade.php",
        "<tr><td>{{ $order->id }}</td></tr>\n",
    ),
    (
        "resources/views/components/alert.blade.php",
        "@props(['type' => 'info', 'message'])\n<div class=\"alert-{{ $type }}\">{{ $message }} {{ $slot }}</div>\n",
    ),
    ("resources/views/pages/about.blade.php", "<h1>About</h1>\n"),
    (
        "resources/views/emails/orders/paid.blade.php",
        "Paid: {{ $order->id }}\n",
    ),
    (
        "resources/views/orphan.blade.php",
        "<p>nobody shows this</p>\n",
    ),
    (
        "resources/views/pdf/slip.blade.php",
        "<p>{{ $number }}</p>\n",
    ),
    (
        "resources/views/filament/pages/reports.blade.php",
        "<p>reports</p>\n",
    ),
    (
        "app/Filament/Pages/Reports.php",
        r##"<?php

namespace App\Filament\Pages;

class Reports
{
    protected string $view = 'filament.pages.reports';
}
"##,
    ),
];

struct App {
    store: Store,
    project: codemap_mcp::Project,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn app() -> App {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    for (path, body) in FILES {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let root = dir.path().canonicalize().unwrap();
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root: root.clone(),
        id: "b".into(),
        index_path: data.path().join("b").join("index.db"),
    };
    let mut store = Store::open(&project.index_path).unwrap();
    codemap_mcp::refresh(&mut store, &root, &project.config, ScanOptions::default()).unwrap();
    App {
        store,
        project,
        _dirs: (dir, data),
    }
}

impl App {
    fn call(&self, tool: &str, args: Value) -> Value {
        codemap_mcp::call(
            &self.store,
            &self.project,
            tool,
            args,
            codemap_mcp::Freshness::default(),
        )
        .unwrap()
        .data
    }
}

fn has(v: &Value, key: &str, part: &str) -> bool {
    v[key].as_array().is_some_and(|a| {
        a.iter()
            .any(|x| x.as_str().is_some_and(|s| s.contains(part)))
    })
}

#[test]
fn a_page_says_what_it_needs_and_who_renders_it() {
    let app = app();
    let v = app.call("get_component", json!({ "name": "orders.index" }));
    assert_eq!(v["framework"], "blade", "{v}");
    assert_eq!(v["file"], "resources/views/orders/index.blade.php", "{v}");
    assert_eq!(v["type"], "page", "{v}");
    assert_eq!(
        v["extends"], "layouts.app → resources/views/layouts/app.blade.php  :1",
        "{v}"
    );
    assert_eq!(v["sections"], json!(["content"]), "{v}");
    assert_eq!(
        v["expects"],
        json!(["current", "notice", "orders", "total"]),
        "{v}"
    );
    assert!(
        has(
            &v,
            "includes",
            "orders.partials.row → resources/views/orders/partials/row.blade.php  :5"
        ),
        "{v}"
    );
    assert!(
        has(
            &v,
            "includes",
            "orders.partials.gone (no such view in this project)"
        ),
        "{v}"
    );
    assert!(
        has(
            &v,
            "components",
            "x-alert → resources/views/components/alert.blade.php  :3"
        ),
        "{v}"
    );
    assert!(
        has(
            &v,
            "forms",
            "PUT route:orders.update → PUT orders/{order}  :8"
        ),
        "{v}"
    );
    assert!(
        has(
            &v,
            "rendered_by",
            "App\\Http\\Controllers\\OrderController::index  app/Http/Controllers/OrderController.php:11  passes: orders, total"
        ),
        "{v}"
    );
    assert!(has(&v, "routes", "GET orders  name:orders.index"), "{v}");

    // The same view by file and by a slash name.
    for name in ["resources/views/orders/index.blade.php", "orders/index"] {
        let again = app.call("get_component", json!({ "name": name }));
        assert_eq!(again["view"], "orders.index", "{again}");
    }
}

#[test]
fn components_layouts_mail_and_route_views() {
    let app = app();
    let alert = app.call("get_component", json!({ "name": "x-alert" }));
    assert_eq!(alert["view"], "components.alert", "{alert}");
    assert_eq!(alert["type"], "component", "{alert}");
    assert_eq!(alert["tag"], "<x-alert>", "{alert}");
    assert_eq!(alert["props"], json!(["type", "message"]), "{alert}");
    assert!(alert.get("expects").is_none(), "{alert}");
    assert!(
        has(
            &alert,
            "shown_in",
            "orders.index  resources/views/orders/index.blade.php:3"
        ),
        "{alert}"
    );
    assert!(
        has(
            &alert,
            "rendered_by",
            "App\\View\\Components\\Alert::render"
        ),
        "{alert}"
    );

    let layout = app.call("get_component", json!({ "name": "layouts.app" }));
    assert_eq!(layout["type"], "layout", "{layout}");
    assert_eq!(layout["yields"], json!(["content"]), "{layout}");
    assert_eq!(layout["extended_by_count"], 1, "{layout}");

    let row = app.call("get_component", json!({ "name": "orders.partials.row" }));
    assert_eq!(row["type"], "partial", "{row}");

    let mail = app.call("get_component", json!({ "name": "emails.orders.paid" }));
    assert!(
        has(
            &mail,
            "rendered_by",
            "App\\Mail\\OrderPaid::build  app/Mail/OrderPaid.php:9  passes: order"
        ),
        "{mail}"
    );

    // `Route::view`, and a name built at run time (`'pages.' . $kind`).
    let about = app.call("get_component", json!({ "name": "pages.about" }));
    assert!(has(&about, "rendered_by", "routes/web.php:8"), "{about}");
    assert!(
        has(&about, "rendered_by", "OrderController::update"),
        "{about}"
    );
    assert!(has(&about, "routes", "GET about  name:about"), "{about}");

    // A PDF library's `loadView`, a view named in a variable, and a Filament
    // page's `$view` property.
    let slip = app.call("get_component", json!({ "name": "pdf.slip" }));
    assert!(
        has(
            &slip,
            "rendered_by",
            "OrderController::slip  app/Http/Controllers/OrderController.php:27  passes: $data"
        ),
        "{slip}"
    );
    assert!(
        has(&slip, "rendered_by", "OrderController.php:24")
            && has(
                &slip,
                "rendered_by",
                "OrderController.php:25  (name built at run time"
            ),
        "{slip}"
    );
    let reports = app.call("get_component", json!({ "name": "filament.pages.reports" }));
    assert!(
        has(&reports, "rendered_by", "app/Filament/Pages/Reports.php:7"),
        "{reports}"
    );

    let orphan = app.call("get_component", json!({ "name": "orphan" }));
    assert!(
        orphan["note"]
            .as_str()
            .unwrap()
            .contains("nothing in the project names this view"),
        "{orphan}"
    );
}

#[test]
fn other_tools_know_views() {
    let app = app();
    let missing = app.call("get_component", json!({ "name": "orders.indx" }));
    assert_eq!(missing["found"], false, "{missing}");
    let near = app.call("get_component", json!({ "name": "order.index" }));
    assert_eq!(near["found"], false, "{near}");
    assert!(has(&near, "suggestions", "orders.index"), "{near}");

    let v = app.call(
        "verify",
        json!({ "claims": { "components": ["emails.orders.paid", "emails.orders.unpaid"] } }),
    );
    let results = v["results"].as_array().unwrap();
    assert_eq!(results[0]["status"], "exists", "{v}");
    assert_eq!(results[1]["status"], "not_found", "{v}");

    let o = app.call("project_overview", json!({}));
    assert_eq!(o["blade"]["views"], 9, "{o}");
    assert_eq!(o["blade"]["kinds"]["component"], 1, "{o}");

    let u = app.call(
        "find_usages",
        json!({ "target": "resources/views/orders/partials/row.blade.php" }),
    );
    assert!(u.get("unused").is_none(), "{u}");
    assert!(
        has(
            &u,
            "usages",
            "resources/views/orders/index.blade.php:5  includes it"
        ),
        "{u}"
    );
    let u = app.call("find_usages", json!({ "target": "emails.orders.paid" }));
    assert!(has(&u, "usages", "app/Mail/OrderPaid.php:9"), "{u}");
    let u = app.call(
        "find_usages",
        json!({ "changed_files": ["resources/views/orders/partials/row.blade.php", "resources/views/pages/about.blade.php"] }),
    );
    let row = &u["changed"][0];
    assert_eq!(row["is"], "Blade view orders.partials.row", "{u}");
    assert_eq!(row["used_in_files"], 1, "{u}");
    assert!(has(row, "routes", "GET|HEAD orders → "), "{u}");
    assert!(has(&u["changed"][1], "routes", "GET|HEAD about"), "{u}");
}

/// After an edit only the links that changed are redone: a controller that
/// starts rendering another view, a template that drops an include, and an
/// unrelated file that changes nothing.
#[test]
fn links_follow_edits() {
    let mut app = app();
    let root = app.project.root.clone();
    let edit = |file: &str, from: &str, to: &str| {
        let path = root.join(file);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{file} has no `{from}`");
        std::fs::write(&path, text.replacen(from, to, 1)).unwrap();
    };
    edit(
        "app/Http/Controllers/OrderController.php",
        "return view('orders.index', [",
        "return view('orphan', [",
    );
    edit(
        "resources/views/orders/index.blade.php",
        "@include('orders.partials.row', ['order' => $order])",
        "",
    );
    edit(
        "app/Mail/OrderPaid.php",
        "class OrderPaid",
        "final class OrderPaid",
    );
    codemap_mcp::refresh(
        &mut app.store,
        &root,
        &app.project.config,
        ScanOptions::default(),
    )
    .unwrap();

    let orphan = app.call("get_component", json!({ "name": "orphan" }));
    assert!(
        has(&orphan, "rendered_by", "OrderController::index"),
        "{orphan}"
    );
    let index = app.call("get_component", json!({ "name": "orders.index" }));
    assert!(index.get("rendered_by").is_none(), "{index}");
    assert!(!has(&index, "includes", "orders.partials.row"), "{index}");
    let row = app.call("get_component", json!({ "name": "orders.partials.row" }));
    assert!(row.get("shown_in").is_none(), "{row}");
    let mail = app.call("get_component", json!({ "name": "emails.orders.paid" }));
    assert!(has(&mail, "rendered_by", "OrderPaid::build"), "{mail}");
}
