//! The made-up examples from GitHub issues #16–#24 (reports sent with
//! `codemap reports --send`), each with the answer that was wrong.

use codemap_core::{Config, ScanOptions, Store};
use serde_json::{Value, json};

/// Writes a project into a temp folder, indexes it and calls one tool.
fn call(files: &[(&str, &str)], tool: &str, args: Value) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let root = dir.path().canonicalize().unwrap();
    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root: root.clone(),
        id: "i".into(),
        index_path: data.path().join("i").join("index.db"),
    };
    let mut store = Store::open(&project.index_path).unwrap();
    codemap_mcp::refresh(&mut store, &root, &project.config, ScanOptions::default()).unwrap();
    codemap_mcp::call(
        &store,
        &project,
        tool,
        args,
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data
}

/// #16: verify {"claims":{"methods":["App\\Models\\Widget::refreshh"]}}
const ISSUE_16: &[(&str, &str)] = &[
    (
        "composer.json",
        r##"{"name":"acme/shop","require":{"laravel/framework":"^11.0"},"autoload":{"psr-4":{"App\\":"app/"}}}
"##,
    ),
    (
        "app/Models/Widget.php",
        r##"<?php
namespace App\Models;

use Illuminate\Database\Eloquent\Model;

class Widget extends Model
{
    public function total(): int { return 1; }
}
"##,
    ),
    (
        "vendor/laravel/framework/src/Illuminate/Database/Eloquent/Model.php",
        r##"<?php
namespace Illuminate\Database\Eloquent;

abstract class Model
{
    public function refresh() { return $this; }
    public function save(array $options = []) { return true; }
}
"##,
    ),
];

/// #17: find_usages {"changed_files":["app/Panel/WidgetActions.php"]}
const ISSUE_17: &[(&str, &str)] = &[
    (
        "composer.json",
        r##"{"name":"acme/shop","require":{"laravel/framework":"^11.0"},"autoload":{"psr-4":{"App\\":"app/"}},"autoload-dev":{"psr-4":{"Tests\\":"tests/"}}}
"##,
    ),
    (
        "app/Panel/WidgetActions.php",
        r##"<?php
namespace App\Panel;

class WidgetActions
{
    public static function all(): array { return ['archive']; }
}
"##,
    ),
    (
        "app/Panel/ViewWidget.php",
        r##"<?php
namespace App\Panel;

class ViewWidget
{
    public function actions(): array { return WidgetActions::all(); }
}
"##,
    ),
    (
        "tests/Feature/ViewWidgetTest.php",
        r##"<?php
namespace Tests\Feature;

use App\Panel\ViewWidget;
use PHPUnit\Framework\TestCase;

class ViewWidgetTest extends TestCase
{
    public function test_it_offers_archive(): void
    {
        $this->assertSame(['archive'], (new ViewWidget)->actions());
    }
}
"##,
    ),
];

/// #18: get_schema {"table":"labels"}
const ISSUE_18: &[(&str, &str)] = &[
    (
        "composer.json",
        r##"{"name":"acme/shop","require":{"laravel/framework":"^11.0"},"autoload":{"psr-4":{"App\\":"app/"}}}
"##,
    ),
    (
        "database/migrations/2024_01_01_000000_create_labels_table.php",
        r##"<?php

use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        Schema::create('labels', function (Blueprint $table) {
            $table->id();
            $table->foreignId('shelf_id');
            $table->string('slug')->index();
            $table->unique(['shelf_id', 'slug']);
        });
    }
};
"##,
    ),
];

/// #19: verify {"claims":{"classes":["App\\Models\\Gadget"]}}
const ISSUE_19: &[(&str, &str)] = &[
    (
        "composer.json",
        r##"{"name":"acme/shop","require":{"laravel/framework":"^11.0"},"autoload":{"psr-4":{"App\\":"app/"}}}
"##,
    ),
    (
        "app/Models/Widget.php",
        r##"<?php
namespace App\Models;

class Widget
{
}
"##,
    ),
    (
        "vendor/composer/autoload_classmap.php",
        r##"<?php

$vendorDir = dirname(__DIR__);
$baseDir = dirname($vendorDir);

return array(
    'App\\Models\\Widget' => $baseDir . '/app/Models/Widget.php',
    'App\\Models\\Gadget' => $baseDir . '/app/Models/Gadget.php',
);
"##,
    ),
];

/// #20: get_schema {"cache":true}
const ISSUE_20: &[(&str, &str)] = &[
    (
        "composer.json",
        r##"{"name":"acme/shop","require":{"laravel/framework":"^11.0"},"autoload":{"psr-4":{"App\\":"app/"}}}
"##,
    ),
    (
        "app/Http/Controllers/PageController.php",
        r##"<?php
namespace App\Http\Controllers;

use Illuminate\Support\Facades\Cache;

class PageController
{
    public function show(int $id)
    {
        $key = 'page_body:'.$id.':'.app()->getLocale();

        return Cache::remember($key, 300, fn () => ['id' => $id]);
    }

    public function cards(array $ids)
    {
        $keys = array_map(fn ($id) => 'card_row:v1:'.$id, $ids);
        $hit = Cache::many($keys);
        Cache::putMany(array_fill_keys($keys, 1), 60);

        return $hit;
    }
}
"##,
    ),
];

/// #21: search_symbol {"query":"percentOff"}
const ISSUE_21: &[(&str, &str)] = &[
    (
        "package.json",
        r##"{"name":"acme-app","dependencies":{"nuxt":"^4.0.0","vue":"^3.5.0"}}
"##,
    ),
    (
        "nuxt.config.ts",
        r##"export default defineNuxtConfig({})
"##,
    ),
    (
        "app/components/PriceTag.vue",
        r##"<script setup lang="ts">
const props = defineProps<{ was: number, now: number }>()

function percentOff(was: number, now: number): number {
  return Math.round((1 - now / was) * 100)
}
</script>

<template>
  <span>{{ percentOff(props.was, props.now) }}%</span>
</template>
"##,
    ),
];

/// #22: search_symbol {"query":"visits_7d"}
const ISSUE_22: &[(&str, &str)] = &[
    (
        "package.json",
        r##"{"name":"acme-app","dependencies":{"nuxt":"^4.0.0","vue":"^3.5.0"}}
"##,
    ),
    (
        "nuxt.config.ts",
        r##"export default defineNuxtConfig({})
"##,
    ),
    (
        "app/composables/api/shops.ts",
        r##"export interface ShopCard {
  id: number
  name: string
  visits_7d: number
}

export function label(card: ShopCard): string {
  return `${card.name} (${card.visits_7d})`
}
"##,
    ),
];

/// #23: api_usage {"file":"app/composables/api/saved.ts"}
const ISSUE_23: &[(&str, &str)] = &[
    (
        "package.json",
        r##"{"name":"acme-app","dependencies":{"nuxt":"^4.0.0","vue":"^3.5.0"}}
"##,
    ),
    (
        "nuxt.config.ts",
        r##"export default defineNuxtConfig({})
"##,
    ),
    (
        "app/composables/useApi.ts",
        r##"export function useApi() {
  return $fetch.create({ baseURL: '/api/v1' })
}
"##,
    ),
    (
        "app/composables/api/saved.ts",
        r##"type Kind = 'shops' | 'items'

const ENDPOINT: Record<Kind, string> = { shops: '/saved-shops', items: '/saved-items' }

export function fetchOverview() {
  return useApi()<{ data: unknown }>('/saved', { query: {} })
}

export function fetchSavedPage(kind: Kind, page: number) {
  return useApi()<{ data: unknown[] }>(ENDPOINT[kind], { query: { page } })
}
"##,
    ),
];

/// #24: verify {"claims":{"functions":["useShelf"]}}
const ISSUE_24: &[(&str, &str)] = &[
    (
        "package.json",
        r##"{"name":"acme-app","dependencies":{"nuxt":"^4.0.0","vue":"^3.5.0"}}
"##,
    ),
    (
        "nuxt.config.ts",
        r##"export default defineNuxtConfig({})
"##,
    ),
    (
        "app/composables/useShelf.ts",
        r##"export function provideShelf() {
  return { size: ref(0) }
}

export function useShelf() {
  return inject('shelf') as { size: Ref<number> }
}
"##,
    ),
];

#[test]
fn issue_16_method_missing_on_project_and_vendor_parent() {
    let v = call(
        ISSUE_16,
        "verify",
        json!({"claims": {"methods": ["App\\Models\\Widget::refreshh"]}}),
    );
    assert_eq!(v["results"][0]["status"], "not_found", "{v}");
    assert_eq!(v["all_exist"], false, "{v}");
    // A method the vendor parent has still counts.
    let ok = call(
        ISSUE_16,
        "verify",
        json!({"claims": {"methods": ["App\\Models\\Widget::refresh"]}}),
    );
    assert_ne!(ok["results"][0]["status"], "not_found", "{ok}");
}

#[test]
fn issue_17_change_impact_follows_callers_to_their_tests() {
    let v = call(
        ISSUE_17,
        "find_usages",
        json!({"changed_files": ["app/Panel/WidgetActions.php"]}),
    );
    let text = v.to_string();
    assert!(text.contains("tests/Feature/ViewWidgetTest.php"), "{v}");
}

#[test]
fn issue_18_composite_unique_index_is_listed() {
    let v = call(ISSUE_18, "get_schema", json!({"table": "labels"}));
    let text = v.to_string();
    assert!(text.contains("shelf_id") && text.contains("unique"), "{v}");
    let idx = v["indexes"].as_array().expect("indexes listed");
    assert!(
        idx.iter()
            .any(|i| i["columns"] == json!(["shelf_id", "slug"]) && i["unique"] == true),
        "{v}"
    );
}

#[test]
fn issue_19_stale_classmap_entry_into_app_is_not_external() {
    let v = call(
        ISSUE_19,
        "verify",
        json!({"claims": {"classes": ["App\\Models\\Gadget"]}}),
    );
    assert_eq!(v["results"][0]["status"], "not_found", "{v}");
}

#[test]
fn issue_20_cache_keys_through_variables_and_many() {
    let v = call(ISSUE_20, "get_schema", json!({"cache": true}));
    let text = v.to_string();
    assert!(text.contains("page_body:{}"), "{v}");
    assert!(text.contains("card_row:v1:{}"), "{v}");
}

#[test]
fn issue_21_function_in_script_setup_is_found() {
    let v = call(ISSUE_21, "search_symbol", json!({"query": "percentOff"}));
    assert_eq!(v["found"], true, "{v}");
    assert!(v.to_string().contains("app/components/PriceTag.vue"), "{v}");
}

#[test]
fn issue_22_interface_property_is_found() {
    let v = call(ISSUE_22, "search_symbol", json!({"query": "visits_7d"}));
    assert_eq!(v["found"], true, "{v}");
    assert!(v.to_string().contains("ShopCard"), "{v}");
}

#[test]
fn issue_23_call_with_url_from_lookup_object_is_listed() {
    let v = call(
        ISSUE_23,
        "api_usage",
        json!({"file": "app/composables/api/saved.ts"}),
    );
    let text = v.to_string();
    assert!(text.contains("saved.ts:10"), "{v}");
    assert!(
        text.contains("saved-shops") && text.contains("saved-items"),
        "{v}"
    );
}

#[test]
fn issue_24_second_export_of_a_composable_file_exists() {
    let v = call(
        ISSUE_24,
        "verify",
        json!({"claims": {"functions": ["useShelf"]}}),
    );
    assert_eq!(v["results"][0]["status"], "exists", "{v}");
}

/// #28: a static helper on another Resource returns what it returns; only
/// `::make` / `::collection` / `new` give that Resource's fields.
const ISSUE_28: &[(&str, &str)] = &[
    (
        "composer.json",
        r#"{"name":"acme/shop","require":{"laravel/framework":"^11.0"},"autoload":{"psr-4":{"App\\":"app/"}}}"#,
    ),
    (
        "app/Http/Resources/PhotoResource.php",
        r#"<?php
namespace App\Http\Resources;

use Illuminate\Http\Resources\Json\JsonResource;

class PhotoResource extends JsonResource
{
    public static function gallery(array $paths, string $host): array
    {
        return array_map(fn ($p) => $host.$p, $paths);
    }

    public function toArray($request): array
    {
        return ['id' => $this->id, 'title' => $this->title];
    }
}
"#,
    ),
    (
        "app/Http/Resources/ShopResource.php",
        r#"<?php
namespace App\Http\Resources;

use Illuminate\Http\Resources\Json\JsonResource;

class ShopResource extends JsonResource
{
    public function toArray($request): array
    {
        return [
            'photo_urls' => PhotoResource::gallery($this->paths, $this->host),
            'cover' => PhotoResource::make($this->cover),
            'photos' => PhotoResource::collection($this->photos),
        ];
    }
}
"#,
    ),
];

#[test]
fn issue_28_static_helper_of_a_resource_is_not_that_resource() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    for (path, body) in ISSUE_28 {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let root = dir.path().canonicalize().unwrap();
    let config = Config::load(&root).unwrap();
    let mut store = Store::open(&data.path().join("index.db")).unwrap();
    codemap_mcp::refresh(&mut store, &root, &config, ScanOptions::default()).unwrap();
    let shape = codemap_laravel::responses::resource_shape(
        store.conn(),
        &root,
        "App\\Http\\Resources\\ShopResource",
    );
    let field = |n: &str| {
        shape
            .fields
            .iter()
            .find(|f| f.name == n)
            .unwrap()
            .resource
            .clone()
    };
    assert_eq!(field("photo_urls"), None, "{shape:?}");
    assert_eq!(
        field("cover").as_deref(),
        Some("App\\Http\\Resources\\PhotoResource")
    );
    assert_eq!(
        field("photos").as_deref(),
        Some("App\\Http\\Resources\\PhotoResource")
    );
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

/// #26: the release check lists unused code added on the branch: an exported
/// constant and type nothing imports, and a new translation key nothing reads.
#[test]
fn issue_26_release_check_lists_unused_additions() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let write = |path: &str, body: &str| {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    write(
        "package.json",
        r#"{"name":"acme-app","dependencies":{"nuxt":"^4.0.0","vue":"^3.5.0","@nuxtjs/i18n":"^9.0.0"}}"#,
    );
    write(
        "nuxt.config.ts",
        "export default defineNuxtConfig({ modules: ['@nuxtjs/i18n'] })\n",
    );
    write("i18n/locales/en.json", "{\n  \"hello\": \"Hi\"\n}\n");
    write(
        "app/pages/index.vue",
        "<script setup lang=\"ts\">\nimport { label } from '~/utils/labels'\n</script>\n<template><p>{{ $t('hello') }} {{ label() }}</p></template>\n",
    );
    write(
        "app/utils/labels.ts",
        "export function label() { return 'x' }\n",
    );
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "base"]);
    git(&root, &["checkout", "-q", "-b", "feature"]);
    write(
        "app/utils/labels.ts",
        "export const MAX_LABELS = 3\nexport interface LabelBox { size: number }\nexport const PREFIX = '#'\nexport function label() { return PREFIX + 'x' }\n",
    );
    write(
        "i18n/locales/en.json",
        "{\n  \"hello\": \"Hi\",\n  \"unused_key\": \"Nobody\"\n}\n",
    );
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "feature"]);

    let project = codemap_mcp::Project {
        app: None,
        config: Config::load(&root).unwrap(),
        root: root.clone(),
        id: "r".into(),
        index_path: data.path().join("r").join("index.db"),
    };
    let mut store = Store::open(&project.index_path).unwrap();
    codemap_mcp::refresh(&mut store, &root, &project.config, ScanOptions::default()).unwrap();
    let v = codemap_mcp::call(
        &store,
        &project,
        "verify",
        json!({"release_check": true, "base": "main"}),
        codemap_mcp::Freshness::default(),
    )
    .unwrap()
    .data;
    let text = v.to_string();
    assert_ne!(v["clean"], true, "{v}");
    assert!(text.contains("MAX_LABELS is not used"), "{v}");
    assert!(text.contains("LabelBox is not used"), "{v}");
    assert!(
        text.contains("PREFIX is exported but only used in its own file"),
        "{v}"
    );
    assert!(text.contains("unused_key"), "{v}");
    assert!(!text.contains("hello is not used"), "{v}");
    assert!(!text.contains("label is not used"), "{v}");
}

/// #29: find_usages on a database column.
const ISSUE_29: &[(&str, &str)] = &[
    (
        "composer.json",
        r#"{"name":"acme/shop","require":{"laravel/framework":"^11.0"},"autoload":{"psr-4":{"App\\":"app/","Database\\Factories\\":"database/factories/"}}}"#,
    ),
    (
        "database/migrations/2024_01_01_000000_create_templates_table.php",
        r#"<?php
use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        Schema::create('templates', function (Blueprint $table) {
            $table->id();
            $table->timestamp('notified_at')->nullable();
        });
    }
};
"#,
    ),
    (
        "app/Models/Template.php",
        r#"<?php
namespace App\Models;

use Illuminate\Database\Eloquent\Model;

class Template extends Model
{
    protected $fillable = [
        'notified_at',
    ];

    protected function casts(): array
    {
        return ['notified_at' => 'datetime'];
    }
}
"#,
    ),
    (
        "app/Jobs/Remind.php",
        r#"<?php
namespace App\Jobs;

use App\Models\Template;

class Remind
{
    public function handle(): void
    {
        Template::whereNull('notified_at')->update(['notified_at' => now()]);
    }
}
"#,
    ),
    (
        "app/Other/Unrelated.php",
        r#"<?php
namespace App\Other;

class Unrelated
{
    public function x(array $row): mixed
    {
        return $row['notified_at'] ?? null;
    }
}
"#,
    ),
];

#[test]
fn issue_29_find_usages_of_a_column() {
    let v = call(
        ISSUE_29,
        "find_usages",
        json!({"target": "templates.notified_at"}),
    );
    assert_eq!(v["resolved_as"], "column templates.notified_at", "{v}");
    let usages = v["usages"].to_string();
    assert!(
        usages.contains("app/Models/Template.php:9  model $fillable"),
        "{v}"
    );
    assert!(
        usages.contains("app/Models/Template.php:14  model $casts"),
        "{v}"
    );
    assert!(usages.contains("app/Jobs/Remind.php:10"), "{v}");
    assert!(!usages.contains("database/migrations"), "{v}");
    assert!(
        v["possible"]
            .to_string()
            .contains("app/Other/Unrelated.php:8"),
        "{v}"
    );
}

/// #35: routes in a closure held in a variable and passed to `->group($shared)`.
const ISSUE_35: &[(&str, &str)] = &[
    (
        "composer.json",
        r#"{"require":{"laravel/framework":"^13.0"},"autoload":{"psr-4":{"App\\":"app/"}}}"#,
    ),
    (
        "bootstrap/app.php",
        r#"<?php
use Illuminate\Foundation\Application;
return Application::configure(basePath: dirname(__DIR__))
    ->withRouting(web: __DIR__.'/../routes/web.php')
    ->create();
"#,
    ),
    (
        "routes/web.php",
        r#"<?php
use Illuminate\Support\Facades\Route;

$shared = function (): void {
    Route::get('/widgets/{id}', fn () => 'ok')->name('widgets.show');
};

Route::prefix('{lang}')->group($shared);
Route::group([], $shared);
"#,
    ),
];

#[test]
fn issue_35_routes_in_a_closure_variable() {
    let v = call(
        ISSUE_35,
        "verify",
        json!({"claims": {"routes": ["widgets.show"]}}),
    );
    assert_eq!(v["results"][0]["status"], "exists", "{v}");
    let r = call(ISSUE_35, "list_routes", json!({}));
    let text = r.to_string();
    assert!(
        text.contains("{lang}/widgets/{id}") && text.contains("\"widgets/{id}"),
        "{r}"
    );
}

/// #32: two migrations guarded by `Schema::hasTable`: one creates the table.
const ISSUE_32: &[(&str, &str)] = &[
    (
        "composer.json",
        r#"{"require":{"laravel/framework":"^11.0"},"autoload":{"psr-4":{"App\\":"app/"}}}"#,
    ),
    (
        "database/migrations/2025_11_28_000000_update_items_table.php",
        r#"<?php
use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        if (! Schema::hasTable('items')) {
            Schema::create('items', function (Blueprint $t) { $t->id(); $t->json('data'); });
            return;
        }
        Schema::table('items', function (Blueprint $t) { $t->json('data')->nullable(); });
    }
};
"#,
    ),
    (
        "database/migrations/2025_12_01_000000_create_items_table.php",
        r#"<?php
use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        if (Schema::hasTable('items')) {
            return;
        }
        Schema::create('items', function (Blueprint $t) { $t->id(); $t->json('data'); });
    }
};
"#,
    ),
];

#[test]
fn issue_32_has_table_guards_are_followed() {
    let v = call(
        ISSUE_32,
        "get_schema",
        json!({"table": "items", "detail": "full"}),
    );
    assert_eq!(v["found"], true, "{v}");
    assert!(
        v["created_in"].as_str().unwrap().contains("2025_11_28"),
        "{v}"
    );
    assert!(!v.to_string().contains("\"confidence\":\"low\""), "{v}");
    let o = call(ISSUE_32, "project_overview", json!({}));
    assert_eq!(o["health"]["warnings"], 0, "{o}");
}
