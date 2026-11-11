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
    // What class another value holds is a guess: only a class's calls on
    // itself are checked.
    assert!(!told.contains("sumary"), "{told}");
    assert!(
        !told.contains("OrderController::index` does not exist"),
        "{told}"
    );
    // In a closure `$this` may be another object, and every enum has
    // `cases()` and `tryFrom()`: neither is a mistake.
    let quiet = f.edit(
        controller,
        "$this->index();",
        "$this->index();\n        collect([1])->each(function ($n) {\n            $this->boundElsewhere($n);\n        });\n        \\App\\Enums\\OrderStatus::cases();\n        \\App\\Enums\\OrderStatus::tryFrom('paid');",
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
