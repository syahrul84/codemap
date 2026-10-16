//! Models, schema replay and Form Requests on the fixtures, checked against
//! hand-written expectations before snapshots.

mod common;

use codemap_core::envelope::Detail;
use codemap_laravel::models;
use codemap_laravel::requests;
use codemap_laravel::schema::{self, Column};
use common::{fixture, indexed, write};
use serde_json::Value;

fn cols(store: &codemap_core::Store, table: &str) -> Vec<Column> {
    schema::load_columns(store.conn(), table).unwrap()
}

fn col<'a>(cols: &'a [Column], name: &str) -> &'a Column {
    cols.iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no column {name}"))
}

fn model<'a>(all: &'a [models::ModelInfo], fqn: &str) -> &'a models::ModelInfo {
    all.iter()
        .find(|m| m.fqn == fqn)
        .unwrap_or_else(|| panic!("no model {fqn}"))
}

#[test]
fn legacy_models() {
    let (store, _) = indexed(&fixture("laravel10-legacy"));
    let all = models::load(store.conn()).unwrap();
    assert_eq!(all.len(), 4);
    let order = model(&all, "App\\Models\\Order");
    assert_eq!(
        (order.table.as_str(), order.table_inferred),
        ("orders", true)
    );
    assert_eq!(
        order.fillable.as_deref().unwrap(),
        ["customer_id", "status", "total_cents", "paid_at"]
    );
    assert_eq!(order.hidden.as_deref().unwrap(), ["deleted_at"]);
    assert_eq!(
        order.casts.as_deref().unwrap(),
        [
            ("paid_at".to_owned(), "datetime".to_owned()),
            ("total_cents".to_owned(), "integer".to_owned())
        ]
    );
    assert_eq!(order.appends.as_deref().unwrap(), ["total_formatted"]);
    assert!(order.uses_trait("SoftDeletes"));
    assert!(order.notes.iter().any(|n| n.contains("deleted_at")));
    let rels: Vec<String> = order
        .relations
        .iter()
        .map(|r| {
            format!(
                "{} {} {}",
                r.method,
                r.r#type,
                r.related.as_deref().unwrap_or("?")
            )
        })
        .collect();
    assert_eq!(
        rels,
        [
            "customer belongsTo App\\Models\\Customer",
            "items hasMany App\\Models\\OrderItem"
        ]
    );
    let acc: Vec<String> = order
        .accessors
        .iter()
        .map(|a| format!("{} {}", a.attribute, a.style))
        .collect();
    assert_eq!(acc, ["total_formatted legacy_get", "status legacy_set"]);
    assert_eq!(order.scopes[0].name, "paid");

    let item = model(&all, "App\\Models\\OrderItem");
    assert_eq!(
        (item.table.as_str(), item.table_inferred),
        ("order_items", false)
    );
    assert_eq!(item.guarded.as_deref().unwrap(), [] as [&str; 0]);
    assert_eq!(item.relations[0].foreign_key.as_deref(), Some("order_id"));
    assert_eq!(item.relations[0].owner_key.as_deref(), Some("id"));

    let user = model(&all, "App\\Models\\User");
    assert_eq!(user.kind, "user");
    assert_eq!(user.table, "users");
}

#[test]
fn slim_models_casts_method_attribute_scope() {
    let (store, _) = indexed(&fixture("laravel11-slim"));
    let all = models::load(store.conn()).unwrap();
    let order = model(&all, "App\\Models\\Order");
    assert_eq!(
        order.casts.as_deref().unwrap(),
        [
            (
                "status".to_owned(),
                "App\\Enums\\OrderStatus::class".to_owned()
            ),
            ("total".to_owned(), "decimal:2".to_owned()),
            ("meta".to_owned(), "array".to_owned()),
        ]
    );
    let tags = order.relations.iter().find(|r| r.method == "tags").unwrap();
    assert_eq!(
        (tags.r#type.as_str(), tags.pivot_table.as_deref()),
        ("belongsToMany", Some("order_tag"))
    );
    assert_eq!(order.accessors[0].attribute, "total_formatted");
    assert_eq!(order.accessors[0].style, "attribute_make");
    assert_eq!(order.scopes[0].name, "paid");
    assert_eq!(order.scopes[0].method, "paid");
    assert!(model(&all, "App\\Models\\Customer").uses_trait("HasUuids"));
}

#[test]
fn legacy_orders_table_matches_migration() {
    let (store, _) = indexed(&fixture("laravel10-legacy"));
    let c = cols(&store, "orders");
    let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "id",
            "customer_id",
            "status",
            "total_cents",
            "paid_at",
            "created_at",
            "updated_at",
            "deleted_at"
        ]
    );
    let id = col(&c, "id");
    assert!(id.primary && id.auto_increment && id.unsigned && id.r#type == "bigint");
    let customer = col(&c, "customer_id");
    assert_eq!(
        (
            customer.fk_table.as_deref(),
            customer.fk_column.as_deref(),
            customer.nullable
        ),
        (Some("customers"), Some("id"), false)
    );
    let status = col(&c, "status");
    assert_eq!(
        (
            status.r#type.as_str(),
            status.default.as_deref(),
            status.indexed
        ),
        ("varchar(20)", Some("'pending'"), true)
    );
    let total = col(&c, "total_cents");
    assert_eq!(
        (
            total.r#type.as_str(),
            total.unsigned,
            total.default.as_deref()
        ),
        ("int", true, Some("0"))
    );
    assert!(col(&c, "paid_at").nullable);
    assert!(col(&c, "deleted_at").nullable);
    assert_eq!(
        status.created_in.as_deref(),
        Some("database/migrations/2023_01_01_000002_create_orders_table.php:14")
    );
}

#[test]
fn slim_schema_uuid_fk_pivot_and_users() {
    let (store, _) = indexed(&fixture("laravel11-slim"));
    let orders = cols(&store, "orders");
    let customer = col(&orders, "customer_id");
    assert_eq!(
        (customer.r#type.as_str(), customer.fk_table.as_deref()),
        ("uuid", Some("customers"))
    );
    assert_eq!(col(&orders, "total").r#type, "decimal(10,2)");
    assert!(col(&orders, "meta").nullable);
    let customers = cols(&store, "customers");
    assert!(col(&customers, "id").primary);
    assert_eq!(col(&customers, "id").r#type, "uuid");
    let pivot = cols(&store, "order_tag");
    assert!(col(&pivot, "order_id").primary && col(&pivot, "tag_id").primary);
    assert_eq!(col(&pivot, "tag_id").fk_table.as_deref(), Some("tags"));
    let users = cols(&store, "users");
    assert_eq!(col(&users, "remember_token").r#type, "varchar(100)");
    assert!(col(&users, "email").unique);
}

#[test]
fn schema_dump_then_rename_and_change() {
    let (store, report) = indexed(&fixture("schema-dump"));
    assert_eq!(report.laravel.data.migrations_replayed, 2);
    let orders = cols(&store, "orders");
    let names: Vec<&str> = orders.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "id",
            "customer_id",
            "status",
            "shipped_at",
            "total",
            "notes",
            "created_at",
            "updated_at"
        ]
    );
    let status = col(&orders, "status");
    assert_eq!(
        (status.default.as_deref(), status.indexed),
        (Some("'pending'"), true)
    );
    assert_eq!(
        status.last_changed_in.as_deref(),
        Some("database/migrations/2024_02_01_000000_rename_state_on_orders_table.php:12")
    );
    // Laravel 10: change() keeps NOT NULL that was not restated.
    let total = col(&orders, "total");
    assert_eq!(
        (total.r#type.as_str(), total.nullable),
        ("decimal(10,2)", false)
    );
    assert_eq!(col(&orders, "notes").r#type, "varchar(500)");
    assert_eq!(
        col(&orders, "customer_id").fk_table.as_deref(),
        Some("customers")
    );

    let v = schema::describe(store.conn(), Some("state"), Detail::Brief).unwrap();
    assert_eq!(v["found"], false);
    let tables = schema::describe(store.conn(), None, Detail::Brief).unwrap();
    let list: Vec<&str> = tables["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["table"].as_str().unwrap())
        .collect();
    assert_eq!(list, ["customers", "orders", "users"]);
}

fn migration_project(major: &str, body: &str) -> (tempfile::TempDir, codemap_core::Store) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "composer.lock",
        &format!(r#"{{"packages":[{{"name":"laravel/framework","version":"v{major}.0.0"}}]}}"#),
    );
    write(
        root,
        "database/migrations/2024_01_01_000000_create_things.php",
        "<?php\nuse Illuminate\\Database\\Schema\\Blueprint;\nuse Illuminate\\Support\\Facades\\Schema;\nreturn new class extends Migration {\n    public function up(): void\n    {\n        Schema::create('things', function (Blueprint $table) {\n            $table->id();\n            $table->string('name')->nullable()->default('x');\n            $table->morphs('owner');\n            $table->foreignIdFor(App\\Models\\Widget::class)->constrained();\n        });\n    }\n};\n",
    );
    write(
        root,
        "database/migrations/2024_01_02_000000_change_things.php",
        body,
    );
    let (store, _) = indexed(root);
    (dir, store)
}

const CHANGE: &str = "<?php\nuse Illuminate\\Support\\Facades\\Schema;\nreturn new class extends Migration {\n    public function up(): void\n    {\n        Schema::table('things', function ($t) {\n            $t->string('name', 50)->change();\n        });\n        if (! Schema::hasColumn('things', 'flag')) {\n            Schema::table('things', fn ($t) => $t->boolean('flag'));\n        }\n        DB::statement('ALTER TABLE things SET UNLOGGED');\n        Schema::rename('things', 'items');\n        Schema::table('items', function ($t) {\n            $t->dropColumn(['owner_type']);\n            $t->renameColumn('owner_id', 'holder_id');\n            $t->weirdColumn('odd');\n        });\n    }\n};\n";

#[test]
fn change_semantics_follow_laravel_version() {
    let (_d, store11) = migration_project("11", CHANGE);
    let items = cols(&store11, "items");
    let name = col(&items, "name");
    assert_eq!(
        (name.r#type.as_str(), name.nullable, name.default.as_deref()),
        ("varchar(50)", false, None)
    );

    let (_d, store10) = migration_project("10", CHANGE);
    let name = cols(&store10, "items")
        .into_iter()
        .find(|c| c.name == "name")
        .unwrap();
    assert_eq!(
        (name.r#type.as_str(), name.nullable, name.default.as_deref()),
        ("varchar(50)", true, Some("'x'"))
    );
}

#[test]
fn conditional_raw_rename_drop_and_unknown() {
    let (_d, store) = migration_project("11", CHANGE);
    assert_eq!(
        schema::describe(store.conn(), Some("things"), Detail::Brief).unwrap()["found"],
        false
    );
    let items = cols(&store, "items");
    let names: Vec<&str> = items.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["id", "name", "holder_id", "widget_id", "flag", "odd"]
    );
    assert_eq!(col(&items, "flag").confidence, schema::Confidence::Low);
    assert_eq!(col(&items, "odd").r#type, "unknown(weirdColumn)");
    // foreignIdFor of an unknown model: inferred table, low confidence.
    let widget = col(&items, "widget_id");
    assert_eq!(widget.fk_table.as_deref(), Some("widgets"));
    assert_eq!(widget.confidence, schema::Confidence::Low);
    let warnings: Vec<String> = store
        .warnings()
        .unwrap()
        .into_iter()
        .map(|w| w.message)
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("raw SQL")),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.contains("->weirdColumn()")),
        "{warnings:?}"
    );
    let renamed = schema::describe(store.conn(), Some("things"), Detail::Brief).unwrap();
    assert!(
        renamed["hint"]
            .as_str()
            .unwrap()
            .contains("renamed to items"),
        "{renamed}"
    );
}

#[test]
fn form_requests_and_controller_links() {
    let (store, _) = indexed(&fixture("laravel10-legacy"));
    let reqs = requests::load(store.conn(), None).unwrap();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.fqn, "App\\Http\\Requests\\StoreOrderRequest");
    let rules: Vec<String> = r
        .rules
        .iter()
        .map(|f| format!("{}: {}", f.field, f.rules.join(" | ")))
        .collect();
    assert_eq!(
        rules,
        [
            "customer_id: required | exists:customers,id",
            "items: required | array | min:1",
            "items.*.sku: required | string | max:64",
            "items.*.quantity: required | integer | min:1",
            "status: sometimes | Rule::in(['pending', 'paid'])",
        ]
    );
    assert_eq!(r.authorize.as_deref(), Some("$this->user() !== null"));
    let uses =
        requests::used_by_controller(store.conn(), "App\\Http\\Controllers\\OrderController")
            .unwrap();
    assert_eq!(uses.len(), 1);
    assert_eq!(
        (uses[0].method.as_str(), uses[0].param.as_str()),
        ("store", "$request")
    );
}

#[test]
fn data_snapshots() {
    for fx in [
        "laravel10-legacy",
        "laravel11-slim",
        "laravel11-upgraded",
        "schema-dump",
    ] {
        let (store, _) = indexed(&fixture(fx));
        let tables = schema::describe(store.conn(), None, Detail::Brief).unwrap();
        let mut schema_json = serde_json::Map::new();
        for t in tables["tables"].as_array().into_iter().flatten() {
            let name = t["table"].as_str().unwrap();
            schema_json.insert(
                name.into(),
                schema::describe(store.conn(), Some(name), Detail::Full).unwrap(),
            );
        }
        let snapshot = serde_json::json!({
            "models": models::load(store.conn()).unwrap(),
            "schema": Value::Object(schema_json),
            "form_requests": requests::load(store.conn(), None).unwrap(),
        });
        insta::assert_json_snapshot!(format!("data-{fx}"), snapshot);
    }
}

#[test]
fn config_driven_table_names_like_spatie_permission() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v12.0.0"}]}"#,
    );
    write(
        root,
        "config/permission.php",
        "<?php\nreturn [\n    'table_names' => ['roles' => 'roles', 'model_has_roles' => 'model_has_roles'],\n    'column_names' => ['model_morph_key' => 'model_id'],\n];\n",
    );
    write(
        root,
        "config/activitylog.php",
        "<?php\nreturn ['table_name' => env('ACTIVITY_LOGGER_TABLE_NAME', 'activity_log')];\n",
    );
    write(
        root,
        "database/migrations/2024_01_01_000000_create_permission_tables.php",
        r#"<?php
use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        $tableNames = config('permission.table_names');
        $columnNames = config('permission.column_names');
        $pivotRole = $columnNames['role_pivot_key'] ?? 'role_id';

        Schema::create($tableNames['roles'], static function (Blueprint $table) {
            $table->bigIncrements('id');
            $table->string('name');
        });

        Schema::create($tableNames['model_has_roles'], static function (Blueprint $table) use ($tableNames, $columnNames, $pivotRole) {
            $table->unsignedBigInteger($pivotRole);
            $table->unsignedBigInteger($columnNames['model_morph_key']);
            $table->unsignedBigInteger($columnNames['missing_key']);
        });

        Schema::create(config('activitylog.table_name'), function (Blueprint $table) {
            $table->id();
            $table->foreignIdFor(Passkeys::userModel(), 'user_id')->constrained();
        });
    }
};
"#,
    );
    let (store, _) = indexed(root);
    let names = |t: &str| -> Vec<String> { cols(&store, t).into_iter().map(|c| c.name).collect() };
    assert_eq!(names("roles"), ["id", "name"]);
    assert_eq!(names("model_has_roles"), ["role_id", "model_id"]);
    assert_eq!(names("activity_log"), ["id", "user_id"]);
    let user_id = col(&cols(&store, "activity_log"), "user_id").clone();
    assert_eq!(user_id.fk_table.as_deref(), Some("users"));
    assert_eq!(user_id.confidence, schema::Confidence::Low);
    let warnings: Vec<String> = store
        .warnings()
        .unwrap()
        .into_iter()
        .map(|w| w.message)
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("->unsignedBigInteger() column name is not a literal")),
        "{warnings:?}"
    );
    assert!(
        !warnings
            .iter()
            .any(|w| w.contains("unknown Blueprint method")),
        "{warnings:?}"
    );
}

#[test]
fn foreach_over_literal_arrays_expands() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v12.0.0"}]}"#,
    );
    write(
        root,
        "database/migrations/2024_01_01_000000_create.php",
        "<?php\nuse Illuminate\\Support\\Facades\\Schema;\nreturn new class extends Migration {\n    public function up(): void\n    {\n        Schema::create('orders', fn ($t) => $t->id());\n        Schema::create('reservations', fn ($t) => $t->id());\n    }\n};\n",
    );
    write(
        root,
        "database/migrations/2024_01_02_000000_vouchers.php",
        "<?php\nuse Illuminate\\Support\\Facades\\Schema;\nreturn new class extends Migration {\n    public function up(): void\n    {\n        foreach (['reservations', 'orders'] as $tableName) {\n            Schema::table($tableName, function ($table) {\n                $table->foreignId('voucher_id')->nullable()->constrained();\n            });\n        }\n        foreach (['orders' => 'note', 'reservations' => 'memo'] as $t => $column) {\n            Schema::table($t, fn ($table) => $table->text($column)->nullable());\n        }\n        foreach ($dynamic as $x) {\n            Schema::table($x, fn ($table) => $table->string('y'));\n        }\n    }\n};\n",
    );
    let (store, _) = indexed(root);
    let names = |t: &str| -> Vec<String> { cols(&store, t).into_iter().map(|c| c.name).collect() };
    assert_eq!(names("orders"), ["id", "voucher_id", "note"]);
    assert_eq!(names("reservations"), ["id", "voucher_id", "memo"]);
    assert_eq!(
        col(&cols(&store, "orders"), "voucher_id")
            .fk_table
            .as_deref(),
        Some("vouchers")
    );
    assert!(col(&cols(&store, "orders"), "voucher_id").confidence == schema::Confidence::High);
}

#[test]
fn class_constants_properties_and_interpolation_in_migrations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v12.0.0"}]}"#,
    );
    write(
        root,
        "database/migrations/2024_01_01_000000_x.php",
        r#"<?php
use Illuminate\Support\Facades\Schema;
return new class extends Migration
{
    private const TABLES = ['cart_items', 'order_items'];
    private array $extra = ['notes' => 'text'];

    public function up(): void
    {
        foreach (self::TABLES as $table) {
            Schema::create($table, fn ($t) => $t->id());
        }
        foreach ($this->extra as $name => $type) {
            Schema::create("tbl_{$name}", fn ($t) => $t->id());
        }
    }
};
"#,
    );
    let (store, _) = indexed(root);
    let tables = schema::describe(store.conn(), None, Detail::Brief).unwrap();
    let list: Vec<&str> = tables["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["table"].as_str().unwrap())
        .collect();
    assert_eq!(list, ["cart_items", "order_items", "tbl_notes"]);
    assert!(
        store.warnings().unwrap().is_empty(),
        "{:?}",
        store.warnings().unwrap()
    );
}

#[test]
fn raw_sql_ddl_and_array_interpolation_in_loops() {
    // Real-project cases: PostGIS column via raw SQL, a later raw DROP COLUMN,
    // and per-field columns dropped with interpolated names inside nested loops.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v13.0.0"}]}"#,
    );
    write(
        root,
        "database/migrations/2024_01_01_000000_create.php",
        r#"<?php
use Illuminate\Support\Facades\DB;
use Illuminate\Support\Facades\Schema;
return new class extends Migration {
    public function up(): void
    {
        Schema::create('outlets', function ($t) {
            $t->id();
            $t->decimal('lat', 10, 7);
            $t->foreignId('cuisine_type_id')->nullable();
        });
        Schema::create('tags', function ($t) {
            $t->id();
            $t->string('name_en');
            $t->string('name_ms');
        });
        DB::statement('ALTER TABLE outlets ADD COLUMN geom geometry(Point, 4326)');
        DB::unprepared(<<<'SQL'
            CREATE OR REPLACE FUNCTION outlets_geom_sync() RETURNS trigger AS $$
            BEGIN NEW.geom := ST_SetSRID(ST_MakePoint(NEW.lng, NEW.lat), 4326); RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            SQL);
        DB::statement('ALTER TABLE outlets ALTER COLUMN geom SET NOT NULL');
    }
};
"#,
    );
    write(
        root,
        "database/migrations/2024_02_01_000000_translations.php",
        r#"<?php
use Illuminate\Support\Facades\DB;
use Illuminate\Support\Facades\Schema;
return new class extends Migration {
    private const FIELDS = ['tags' => ['name']];

    public function up(): void
    {
        foreach (self::FIELDS as $table => $fields) {
            foreach ($fields as $field) {
                if (! Schema::hasColumn($table, "{$field}_en")) {
                    continue;
                }
                Schema::table($table, fn ($t) => $t->jsonb($field)->nullable());
                Schema::table($table, fn ($t) => $t->dropColumn(["{$field}_en", "{$field}_ms"]));
            }
        }
        DB::statement('ALTER TABLE outlets DROP COLUMN IF EXISTS cuisine_type_id');
    }
};
"#,
    );
    let (store, _) = indexed(root);
    let names = |t: &str| -> Vec<String> { cols(&store, t).into_iter().map(|c| c.name).collect() };
    assert_eq!(names("outlets"), ["id", "lat", "geom"]);
    assert_eq!(names("tags"), ["id", "name"]);
    let geom = col(&cols(&store, "outlets"), "geom").clone();
    assert!(!geom.nullable);
    let warnings: Vec<String> = store
        .warnings()
        .unwrap()
        .into_iter()
        .map(|w| w.message)
        .collect();
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn known_conditions_and_helper_methods_in_migrations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v13.0.0"}]}"#,
    );
    write(
        root,
        "config/permission.php",
        "<?php\nreturn ['teams' => false, 'table_names' => ['roles' => 'roles']];\n",
    );
    write(
        root,
        "app/Models/Template.php",
        "<?php\nnamespace App\\Models;\nuse Illuminate\\Database\\Eloquent\\Model;\nclass Template extends Model { protected $table = 'item_templates'; }\n",
    );
    write(
        root,
        "database/migrations/2024_01_01_000000_roles.php",
        r#"<?php
use Illuminate\Support\Facades\Schema;
return new class extends Migration {
    public function up(): void
    {
        $teams = config('permission.teams');
        Schema::create('roles', function ($table) use ($teams) {
            $table->id();
            if ($teams || config('permission.testing')) {
                $table->unsignedBigInteger('team_id')->nullable();
            }
            $table->string('name');
        });
        if (! $teams) {
            Schema::create('no_teams_marker', fn ($t) => $t->id());
        } else {
            Schema::create('teams_marker', fn ($t) => $t->id());
        }
        Schema::create('item_templates', fn ($t) => $t->id());
    }
};
"#,
    );
    write(
        root,
        "database/migrations/2024_02_01_000000_translations.php",
        r#"<?php
use App\Models\Template;
use Illuminate\Support\Facades\Schema;
return new class extends Migration {
    public function up(): void
    {
        foreach ($this->tables() as $table) {
            if (! Schema::hasColumn($table, 'translations')) {
                Schema::table($table, fn ($t) => $t->jsonb('translations')->nullable());
            }
        }
    }

    private function tables(): array
    {
        return [(new Template)->getTable()];
    }
};
"#,
    );
    let (store, _) = indexed(root);
    let names = |t: &str| -> Vec<String> { cols(&store, t).into_iter().map(|c| c.name).collect() };
    assert_eq!(names("roles"), ["id", "name"], "teams is false in config");
    assert_eq!(names("no_teams_marker"), ["id"]);
    assert!(cols(&store, "teams_marker").is_empty());
    assert_eq!(names("item_templates"), ["id", "translations"]);
}

#[test]
fn laravel13_model_attributes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "composer.lock",
        r#"{"packages":[{"name":"laravel/framework","version":"v13.0.0"}]}"#,
    );
    write(
        root,
        "app/Models/Merchant.php",
        r#"<?php
namespace App\Models;

use Illuminate\Database\Eloquent\Attributes\Fillable;
use Illuminate\Database\Eloquent\Attributes\Hidden;
use Illuminate\Database\Eloquent\Attributes\Table;
use Illuminate\Database\Eloquent\Attributes\Unguarded;
use Illuminate\Database\Eloquent\Model;

#[Fillable(['name', 'popular_until'])]
#[Hidden(['secret'])]
#[Table('brands', key: 'uuid')]
class Merchant extends Model
{
    protected $fillable = ['slug'];
}

#[Unguarded]
class Loose extends Model {}
"#,
    );
    let (store, _) = indexed(root);
    let all = models::load(store.conn()).unwrap();
    let m = model(&all, "App\\Models\\Merchant");
    assert_eq!(
        m.fillable.as_deref().unwrap(),
        ["slug", "name", "popular_until"]
    );
    assert_eq!(m.hidden.as_deref().unwrap(), ["secret"]);
    assert_eq!((m.table.as_str(), m.table_inferred), ("brands", false));
    assert_eq!(m.primary_key.as_deref(), Some("uuid"));
    let loose = model(&all, "App\\Models\\Loose");
    assert_eq!(loose.guarded.as_deref().unwrap(), [] as [&str; 0]);
}
