//! The check after each edit (`codemap hook edited`): the assistant is told
//! about names an edit brought in that the project does not have, once, and
//! not about what was already wrong.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const FILES: &[(&str, &str)] = &[
    (
        "composer.json",
        r##"{"name":"acme/shop","require":{"laravel/framework":"^11.0","laravel/mcp":"^0.3"},"autoload":{"psr-4":{"App\\":"app/"}}}
"##,
    ),
    ("artisan", "#!/usr/bin/env php\n<?php\n"),
    (
        "routes/web.php",
        r##"<?php

use App\Http\Controllers\OrderController;
use Illuminate\Support\Facades\Route;

Route::get('/orders', [OrderController::class, 'index'])->name('orders.index');

// A registrar kept in a variable, given its prefix one way or the other.
$admin = Route::middleware(['auth'])->name('admin.');
$admin = config('app.admin_domain') ? $admin->domain(config('app.admin_domain')) : $admin->prefix('admin');
$admin->get('/reports', [OrderController::class, 'index'])->name('reports');
$admin->group(function () {
    Route::get('/audit', [OrderController::class, 'index'])->name('audit');
});

// A package's own call that registers a route.
\Acme\Mcp\Facades\Mcp::web('/mcp/shop', OrderController::class)->middleware(['api'])->name('mcp.shop');

// The same, decided in an if / else.
$staff = Route::middleware(['auth'])->name('staff.');
if (config('app.staff_domain')) {
    $staff = $staff->domain(config('app.staff_domain'));
} else {
    $staff = $staff->prefix('staff');
}
$staff->get('/rota', [OrderController::class, 'index'])->name('rota');
"##,
    ),
    // The file Laravel MCP loads itself.
    (
        "routes/ai.php",
        "<?php\n\nuse App\\Http\\Controllers\\OrderController;\nuse Laravel\\Mcp\\Facades\\Mcp;\n\nMcp::web('/mcp/ai', OrderController::class)->name('mcp.ai');\n",
    ),
    // Routes a service provider registers itself.
    (
        "app/Providers/AppServiceProvider.php",
        r##"<?php

namespace App\Providers;

use App\Http\Controllers\OrderController;
use Illuminate\Support\Facades\Route;
use Illuminate\Support\ServiceProvider;

class AppServiceProvider extends ServiceProvider
{
    public function boot(): void
    {
        Route::group([
            'as' => 'partner.',
            'prefix' => config('shop.partner_path', 'partner'),
        ], function () {
            Route::get('/ping', [OrderController::class, 'index'])->name('ping');
        });
    }
}
"##,
    ),
    // No vendor folder (it lives in a container): the lock file is what says
    // which packages are installed. PHPUnit names no namespace in it.
    (
        "composer.lock",
        r##"{"packages":[],"packages-dev":[{"name":"phpunit/phpunit","autoload":{"classmap":["src/"]}},{"name":"mockery/mockery","autoload":{"psr-4":{"Mockery\\":"library/Mockery"},"files":["library/Mockery.php"]}}]}
"##,
    ),
    (
        "database/migrations/2024_01_01_000000_create_orders_table.php",
        r##"<?php

use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        Schema::create('orders', function (Blueprint $table) {
            $table->id();
            $table->integer('total');
        });
        Schema::create('legacy_notes', function (Blueprint $table) {
            $table->id();
        });
    }
};
"##,
    ),
    (
        "database/migrations/2024_02_01_000000_drop_legacy_notes.php",
        r##"<?php

use Illuminate\Database\Migrations\Migration;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        Schema::drop('legacy_notes');
    }
};
"##,
    ),
    (
        "app/Models/Order.php",
        r##"<?php

namespace App\Models;

class Order
{
    public static function total(): int
    {
        return 1;
    }
}
"##,
    ),
    (
        "app/Http/Controllers/OrderController.php",
        r##"<?php

namespace App\Http\Controllers;

use App\Models\Order;

class OrderController
{
    public function index()
    {
        Order::legacyMissing();

        return view('orders.index');
    }
}
"##,
    ),
    (
        "resources/views/orders/index.blade.php",
        "<h1>Orders</h1>\n@include('orders.partials.row')\n",
    ),
    (
        "resources/views/orders/partials/row.blade.php",
        "<tr></tr>\n",
    ),
    (
        "app/Enums/OrderStatus.php",
        "<?php\n\nnamespace App\\Enums;\n\nenum OrderStatus: string\n{\n    case Paid = 'paid';\n}\n",
    ),
    (
        "app/Support/Anything.php",
        "<?php\n\nnamespace App\\Support;\n\nclass Anything\n{\n    public function __call($name, $arguments)\n    {\n    }\n}\n",
    ),
    (
        "app/Support/helpers.php",
        "<?php\n\nfunction format_money(int $cents): string\n{\n    return (string) $cents;\n}\n",
    ),
    // A test's stand-in for Laravel's global `Log`.
    (
        "tests/Fixtures/LogStub.php",
        "<?php\n\nclass Log\n{\n    public static function write(): void {}\n}\n",
    ),
];

struct Fixture {
    root: std::path::PathBuf,
    data: tempfile::TempDir,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    for (path, body) in FILES {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let root = dir.path().canonicalize().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_codemap"))
        .args([
            "--data-dir",
            data.path().to_str().unwrap(),
            "scan",
            "--project",
        ])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Fixture {
        root,
        data,
        _dir: dir,
    }
}

impl Fixture {
    /// Runs the hook for a tool call; what the assistant is told ("" for nothing).
    fn hook(&self, tool: &str, input: Value) -> String {
        let mut child = Command::new(env!("CARGO_BIN_EXE_codemap"))
            .args([
                "--data-dir",
                self.data.path().to_str().unwrap(),
                "hook",
                "edited",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // The Write tool says whether it made the file or replaced one.
        let response = match tool {
            "Write" => input["_response"].clone(),
            _ => Value::Null,
        };
        let call = json!({
            "session_id": "s1", "cwd": self.root, "hook_event_name": "PostToolUse",
            "tool_name": tool, "tool_input": input, "tool_response": response,
        });
        child
            .stdin
            .take()
            .unwrap()
            .write_all(call.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "the hook must exit 0");
        let text = String::from_utf8(out.stdout).unwrap();
        if text.trim().is_empty() {
            return String::new();
        }
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Edits a file the way the Edit tool does, then runs the hook.
    fn edit(&self, file: &str, old: &str, new: &str) -> String {
        let path = self.root.join(file);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(old), "{file} has no `{old}`");
        std::fs::write(&path, text.replacen(old, new, 1)).unwrap();
        self.hook(
            "Edit",
            json!({ "file_path": path, "old_string": old, "new_string": new }),
        )
    }
}

#[test]
fn an_edit_is_told_what_it_uses_that_does_not_exist() {
    let f = fixture();
    let controller = "app/Http/Controllers/OrderController.php";
    let told = f.edit(
        controller,
        "return view('orders.index');",
        "$n = Order::totl();\n\n        return redirect()->route('orders.indx');",
    );
    assert!(told.contains(controller), "{told}");
    assert!(
        told.contains("method `App\\Models\\Order::totl` does not exist"),
        "{told}"
    );
    assert!(
        told.contains("route `orders.indx` does not exist"),
        "{told}"
    );
    assert!(told.contains("orders.index"), "the closest name: {told}");
    // A method called on the class itself, or on a typed value.
    let told = f.edit(
        controller,
        "public function index()\n    {",
        "public function show(Order $order)\n    {\n        $this->indx();\n        $order->sumary();\n        $this->index();\n    }\n\n    public function index()\n    {",
    );
    assert!(
        told.contains("method `App\\Http\\Controllers\\OrderController::indx` does not exist"),
        "{told}"
    );
    // The parameter's type says what class `$order` is.
    assert!(
        told.contains("method `App\\Models\\Order::sumary` does not exist"),
        "{told}"
    );
    assert!(
        !told.contains("OrderController::index` does not exist"),
        "{told}"
    );
    // A closure handed to a call that only runs it keeps `$this`. One given
    // to `macro` is bound to another object, one kept in a variable may be;
    // and every enum has `cases()` and `tryFrom()`.
    let told = f.edit(
        controller,
        "$this->index();",
        "$this->index();\n        collect([1])->each(function ($n) {\n            $this->inEach($n);\n        });\n        \\Illuminate\\Support\\Collection::macro('odd', function () {\n            return $this->boundElsewhere();\n        });\n        $later = fn () => $this->keptForLater();\n        \\App\\Enums\\OrderStatus::cases();\n        \\App\\Enums\\OrderStatus::tryFrom('paid');",
    );
    assert!(
        told.contains("OrderController::inEach` does not exist"),
        "{told}"
    );
    for quiet in ["boundElsewhere", "keptForLater", "OrderStatus"] {
        assert!(!told.contains(quiet), "{quiet}: {told}");
    }
    // A parameter given another value is no longer what its type said; a
    // class that answers any call through `__call` has no missing method.
    let quiet = f.edit(
        controller,
        "public function index()\n    {",
        "public function swap(Order $order, \\App\\Support\\Anything $any)\n    {\n        $order = $this->other();\n        $order->notOnOrder();\n        $any->whatever();\n    }\n\n    public function other()\n    {\n    }\n\n    public function index()\n    {",
    );
    assert_eq!(quiet, "");
    // What was wrong before the edit is not this edit's news.
    assert!(!told.contains("legacyMissing"), "{told}");

    // The same finding is said once in a session.
    let again = f.edit(
        controller,
        "$n = Order::totl();",
        "$n = Order::totl(); // count",
    );
    assert_eq!(again, "");

    // An edit that only uses what exists says nothing; nor do Laravel's
    // global names, even when a test fixture has a class called the same.
    let fine = f.edit(
        controller,
        "Order::totl(); // count",
        "Order::total();\n        \\Log::info('listed');\n        \\Validator::make([], []);",
    );
    assert_eq!(fine, "");
}

#[test]
fn templates_and_new_files_are_checked_too() {
    let f = fixture();
    let told = f.edit(
        "resources/views/orders/index.blade.php",
        "@include('orders.partials.row')",
        "@include('orders.partials.row')\n@include('orders.partials.rwo')\n<a href=\"{{ route('orders.shw') }}\">x</a>",
    );
    assert!(
        told.contains("view `orders.partials.rwo` does not exist"),
        "{told}"
    );
    assert!(told.contains("route `orders.shw` does not exist"), "{told}");

    // A whole new file (the Write tool).
    let path = f.root.join("app/Services/Billing.php");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = "<?php\n\nnamespace App\\Services;\n\nuse App\\Models\\Invoice;\nuse App\\Models\\Order;\n\nclass Billing\n{\n    public function run()\n    {\n        Order::total();\n\n        return Invoice::latest();\n    }\n}\n";
    std::fs::write(&path, body).unwrap();
    let told = f.hook(
        "Write",
        json!({ "file_path": path, "content": body, "_response": { "type": "create" } }),
    );
    assert!(
        told.contains("class `App\\Models\\Invoice` does not exist"),
        "{told}"
    );
    assert!(!told.contains("Order"), "{told}");

    // An existing file replaced whole, with no record of what it held and
    // nothing in git: what the edit added cannot be known, so nothing is said.
    let replaced = f.hook(
        "Write",
        json!({ "file_path": path, "content": body, "_response": { "type": "update" } }),
    );
    assert_eq!(replaced, "");

    // A view that may be missing is not a mistake; one that must exist is.
    let told = f.edit(
        "resources/views/orders/partials/row.blade.php",
        "<tr></tr>",
        "<tr></tr>\n@includeIf('orders.partials.promo')\n@includeFirst(['orders.custom', 'orders.partials.row'])",
    );
    assert_eq!(told, "");

    // A session started in a subfolder still finds the project.
    let mut child = Command::new(env!("CARGO_BIN_EXE_codemap"))
        .args([
            "--data-dir",
            f.data.path().to_str().unwrap(),
            "hook",
            "edited",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let controller = f.root.join("app/Http/Controllers/OrderController.php");
    let text = std::fs::read_to_string(&controller).unwrap();
    std::fs::write(
        &controller,
        text.replacen(
            "Order::legacyMissing();",
            "Order::legacyMissing();\n        Order::fromSubfolder();",
            1,
        ),
    )
    .unwrap();
    let call = json!({
        "session_id": "s2", "cwd": f.root.join("app/Http"), "tool_name": "Edit",
        "tool_input": { "file_path": controller, "old_string": "Order::legacyMissing();", "new_string": "Order::legacyMissing();\n        Order::fromSubfolder();" },
    });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(call.to_string().as_bytes())
        .unwrap();
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    assert!(out.contains("Order::fromSubfolder"), "{out}");
    assert!(!out.contains("legacyMissing"), "{out}");

    // Switched off in the project's settings: nothing is said.
    std::fs::write(
        f.root.join(".codemap.toml"),
        "[agent]\ncheck_edits = false\n",
    )
    .unwrap();
    let off = f.edit(
        "app/Http/Controllers/OrderController.php",
        "Order::legacyMissing();",
        "Order::alsoMissing();",
    );
    assert_eq!(off, "");

    // Bad input never fails.
    let _ = Path::new("");
    assert_eq!(f.hook("Edit", json!({})), "");
    assert_eq!(f.hook("Bash", json!({ "command": "ls" })), "");
}

#[test]
fn what_the_code_cannot_tell_is_not_reported() {
    let f = fixture();
    let controller = "app/Http/Controllers/OrderController.php";
    // Routes of a registrar kept in a variable are routes. A name the code
    // asks about first, or one a package adds when the app runs, is not a
    // finding; a misspelt one still is.
    let told = f.edit(
        controller,
        "return view('orders.index');",
        "route('admin.reports');\n        route('admin.audit');\n        route('admin.reprots');\n        route('order.index');\n        route('partner.ping');\n        route('partner.pong');\n        route('staff.rota');\n        route('mcp.shop');\n        route('mcp.ai');\n        route('mcp.shp');\n        route('filament.admin.pages.dashboard');\n        if (\\Route::has('login')) {\n            route('login');\n        }\n\n        return view('orders.index');",
    );
    assert!(
        told.contains("route `admin.reprots` does not exist"),
        "{told}"
    );
    assert!(
        told.contains("route `order.index` does not exist"),
        "{told}"
    );
    assert!(
        told.contains("route `partner.pong` does not exist"),
        "{told}"
    );
    assert!(told.contains("route `mcp.shp` does not exist"), "{told}");
    for quiet in [
        "admin.reports",
        "admin.audit",
        "partner.ping",
        "staff.rota",
        "mcp.shop",
        "mcp.ai",
        "filament.admin.pages.dashboard",
        "login",
    ] {
        assert!(
            !told.contains(&format!("route `{quiet}`")),
            "{quiet}: {told}"
        );
    }

    // A namespace, PHP's own classes, and classes of installed packages
    // (by the lock file: no vendor folder here).
    let told = f.edit(
        controller,
        "use App\\Models\\Order;\n\nclass OrderController\n{",
        "use Acme\\Missing\\Thing;\nuse App\\Models;\nuse App\\Models\\Order;\nuse Mockery;\nuse PHPUnit\\Framework\\Attributes\\DataProvider;\n\nclass OrderController\n{\n    private ?Models\\Order $last = null;\n",
    );
    assert!(
        told.contains("class `Acme\\Missing\\Thing` does not exist"),
        "{told}"
    );
    for quiet in ["App\\Models`", "Mockery", "PHPUnit"] {
        assert!(!told.contains(quiet), "{quiet}: {told}");
    }
    let told = f.edit(
        controller,
        "Order::legacyMissing();",
        "Order::legacyMissing();\n        Mockery::mock(Order::class);\n        new \\DOMXPath(new \\DOMDocument());\n        Models\\Order::total();",
    );
    assert_eq!(told, "");

    // `orders as o` is the table orders; the database's own catalogs are
    // nobody's migration; what follows `get()` reads rows, not columns.
    let told = f.edit(
        controller,
        "Mockery::mock(Order::class);",
        "\\DB::table('orders as o')->where('o.total', 1)->where('total', 2)->get()->pluck('nope')->firstWhere('kind', 1);\n        \\DB::table('ordrs as o')->count();\n        \\DB::table('pg_class')->count();",
    );
    assert!(told.contains("table `ordrs` does not exist"), "{told}");
    for quiet in [" as o", "pg_class", "nope", "kind", "total"] {
        assert!(!told.contains(quiet), "{quiet}: {told}");
    }

    // A table the file makes for itself, or is ready to find missing.
    let told = f.edit(
        controller,
        "\\DB::table('pg_class')->count();",
        "\\DB::statement('CREATE TEMPORARY TABLE scratch_rows (id INT)');\n        \\DB::table('scratch_rows')->count();\n        \\Schema::dropIfExists('old_rows');\n        \\DB::table('old_rows')->count();\n        \\DB::table('scratch_row')->count();",
    );
    assert!(
        told.contains("table `scratch_row` does not exist"),
        "{told}"
    );
    for quiet in ["scratch_rows", "old_rows"] {
        assert!(
            !told.contains(&format!("table `{quiet}`")),
            "{quiet}: {told}"
        );
    }

    // A migration may work on a table a later one dropped; a table no
    // migration ever named is still a mistake. (`Schema::table` on a table
    // nobody created is recorded as a table made outside the migrations.)
    let path = f
        .root
        .join("database/migrations/2024_01_15_000000_note_orders.php");
    let body = "<?php\n\nuse Illuminate\\Database\\Migrations\\Migration;\nuse Illuminate\\Database\\Schema\\Blueprint;\nuse Illuminate\\Support\\Facades\\Schema;\n\nreturn new class extends Migration\n{\n    public function up(): void\n    {\n        Schema::table('legacy_notes', function (Blueprint $table) {\n            $table->string('body')->nullable();\n        });\n        \\Illuminate\\Support\\Facades\\DB::table('ordrs')->delete();\n    }\n};\n";
    std::fs::write(&path, body).unwrap();
    let told = f.hook(
        "Write",
        json!({ "file_path": path, "content": body, "_response": { "type": "create" } }),
    );
    assert!(told.contains("table `ordrs` does not exist"), "{told}");
    assert!(!told.contains("legacy_notes"), "{told}");

    // `Schema::table` on a name a letter away from a real table is a slip;
    // on a table made outside the migrations (nothing like it here) it is not.
    let path = f
        .root
        .join("database/migrations/2024_03_01_000000_note_orders_again.php");
    let body = "<?php\n\nuse Illuminate\\Database\\Migrations\\Migration;\nuse Illuminate\\Database\\Schema\\Blueprint;\nuse Illuminate\\Support\\Facades\\Schema;\n\nreturn new class extends Migration\n{\n    public function up(): void\n    {\n        Schema::table('order', function (Blueprint $table) {\n            $table->string('note')->nullable();\n        });\n        Schema::table('wp_posts', function (Blueprint $table) {\n            $table->string('note')->nullable();\n        });\n        Schema::table('orders', function (Blueprint $table) {\n            $table->string('memo')->nullable();\n        });\n    }\n};\n";
    std::fs::write(&path, body).unwrap();
    let told = f.hook(
        "Write",
        json!({ "file_path": path, "content": body, "_response": { "type": "create" } }),
    );
    assert!(
        told.contains("table `order` does not exist (closest: `orders`)"),
        "{told}"
    );
    // Every table here is made by a migration, so one that is not is
    // missing, close to a real name or not.
    assert!(told.contains("table `wp_posts` does not exist"), "{told}");
    assert!(!told.contains("table `orders`"), "{told}");

    // Once the project has a table made outside the migrations (that
    // `wp_posts`), another such table may be one more of them: only a name
    // close to a real table is reported.
    let path = f
        .root
        .join("database/migrations/2024_03_02_000000_note_contacts.php");
    let body = "<?php\n\nuse Illuminate\\Database\\Migrations\\Migration;\nuse Illuminate\\Database\\Schema\\Blueprint;\nuse Illuminate\\Support\\Facades\\Schema;\n\nreturn new class extends Migration\n{\n    public function up(): void\n    {\n        Schema::table('crm_contacts', function (Blueprint $table) {\n            $table->string('note')->nullable();\n        });\n    }\n};\n";
    std::fs::write(&path, body).unwrap();
    let told = f.hook(
        "Write",
        json!({ "file_path": path, "content": body, "_response": { "type": "create" } }),
    );
    assert_eq!(told, "");
}

#[test]
fn typed_values_joins_and_near_miss_functions() {
    let f = fixture();
    // A typed property reached through `$this`, and a joined query: a
    // column neither table has is a finding, one the joined table has is not.
    let path = f.root.join("app/Services/Report.php");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = "<?php\n\nnamespace App\\Services;\n\nuse App\\Models\\Order;\nuse Illuminate\\Support\\Facades\\DB;\n\nclass Report\n{\n    public function __construct(private Order $order)\n    {\n    }\n\n    public function again(): self\n    {\n        return $this;\n    }\n\n    public function run()\n    {\n        $this->order->totl();\n        Order::total();\n        $made = new Order();\n        $made->madeUp();\n        $twice = new Order();\n        $twice = other();\n        $twice->neverChecked();\n        $again = $this->again();\n        $again->notOnReport();\n        $again->run();\n        array_map(function (Order $each) {\n            return $each->inClosure();\n        }, []);\n\n        return DB::table('orders')\n            ->join('order_lines', 'order_lines.order_id', '=', 'orders.id')\n            ->where('sku', 'A1')\n            ->where('colour', 'red')\n            ->get();\n    }\n}\n";
    let lines = f
        .root
        .join("database/migrations/2024_01_02_000000_create_order_lines_table.php");
    std::fs::write(
        &lines,
        "<?php\n\nuse Illuminate\\Database\\Migrations\\Migration;\nuse Illuminate\\Database\\Schema\\Blueprint;\nuse Illuminate\\Support\\Facades\\Schema;\n\nreturn new class extends Migration\n{\n    public function up(): void\n    {\n        Schema::create('order_lines', function (Blueprint $table) {\n            $table->id();\n            $table->integer('order_id');\n            $table->string('sku');\n        });\n    }\n};\n",
    )
    .unwrap();
    std::fs::write(&path, body).unwrap();
    let told = f.hook(
        "Write",
        json!({ "file_path": path, "content": body, "_response": { "type": "create" } }),
    );
    assert!(
        told.contains("method `App\\Models\\Order::totl` does not exist"),
        "{told}"
    );
    assert!(
        told.contains("column `orders.colour` does not exist"),
        "{told}"
    );
    // A value made with `new` and never given anything else, and a
    // closure's typed parameter; not a variable written twice.
    assert!(told.contains("Order::madeUp` does not exist"), "{told}");
    assert!(told.contains("Order::inClosure` does not exist"), "{told}");
    assert!(!told.contains("neverChecked"), "{told}");
    // A value a method returned, by the type the method declares.
    assert!(
        told.contains("Report::notOnReport` does not exist"),
        "{told}"
    );
    assert!(!told.contains("Report::run`"), "{told}");
    assert!(!told.contains("sku"), "{told}");

    // Functions: only with the packages installed, and only a name close to
    // one the project or Laravel has. A package's own helper is not reported.
    let composer = f.root.join("vendor/composer");
    std::fs::create_dir_all(f.root.join("vendor/acme/kit/src")).unwrap();
    std::fs::create_dir_all(&composer).unwrap();
    std::fs::write(
        composer.join("autoload_files.php"),
        "<?php\n$vendorDir = dirname(__DIR__);\nreturn array(\n    'abc' => $vendorDir . '/acme/kit/src/helpers.php',\n);\n",
    )
    .unwrap();
    std::fs::write(
        f.root.join("vendor/acme/kit/src/helpers.php"),
        "<?php\n\nfunction format_monex()\n{\n}\n",
    )
    .unwrap();
    let controller = "app/Http/Controllers/OrderController.php";
    let told = f.edit(
        controller,
        "return view('orders.index');",
        "format_mony(1);\n        format_monex();\n        reponse();\n        some_unknown_thing();\n        strlen('x');\n        array_map('trim', []);\n        collect([]);\n\n        return view('orders.index');",
    );
    assert!(
        told.contains("function `format_mony` does not exist (closest: `format_money`)"),
        "{told}"
    );
    assert!(
        told.contains("function `reponse` does not exist (closest: `response`)"),
        "{told}"
    );
    // One nothing defines, near no known name.
    assert!(
        told.contains("function `some_unknown_thing` does not exist"),
        "{told}"
    );
    for quiet in ["format_monex", "strlen", "array_map", "collect"] {
        assert!(!told.contains(quiet), "{quiet}: {told}");
    }
}
