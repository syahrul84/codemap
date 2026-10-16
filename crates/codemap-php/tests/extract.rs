//! Extraction on hand-written PHP covering each construct.

use codemap_core::symbols::{NewSymbol, SymbolKind};
use codemap_php::extract;

fn symbols(src: &str) -> Vec<NewSymbol> {
    let parsed = extract(src.as_bytes());
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    parsed.symbols
}

fn get<'a>(syms: &'a [NewSymbol], fqn: &str) -> &'a NewSymbol {
    syms.iter().find(|s| s.fqn == fqn).unwrap_or_else(|| {
        let all: Vec<_> = syms.iter().map(|s| &s.fqn).collect();
        panic!("{fqn} not found in {all:#?}")
    })
}

const MODEL: &str = r#"<?php

namespace App\Models;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\{BelongsTo, HasMany as Many};
use Illuminate\Database\Eloquent\SoftDeletes;
use App\Contracts\Payable as PayableContract;
use function App\Support\money;
use const App\Support\MAX_ITEMS;

/**
 * An order placed by a customer.
 *
 * @property int $id
 */
#[\Illuminate\Database\Eloquent\Attributes\ObservedBy(OrderObserver::class)]
final class Order extends Model implements PayableContract, \JsonSerializable
{
    use SoftDeletes, HasFactory;

    public const STATUS_PAID = 'paid';
    final protected const LIMIT = 10, OFFSET = -1;

    /** Mass-assignable attributes. */
    protected $fillable = ['customer_id', 'status'];
    protected static ?Order $latest = null;
    public readonly array $tags;
    private $computed = self::LIMIT * 2;

    public function __construct(
        private readonly OrderService $service,
        protected int $priority = 1,
        string $plain = 'x',
    ) {}

    /**
     * Customer who placed the order. Second sentence.
     */
    public function customer(): BelongsTo
    {
        return $this->belongsTo(Customer::class);
    }

    public function items(): Many
    {
        return $this->hasMany(OrderItem::class);
    }

    abstract protected static function build(?Customer $c, int|string ...$ids): ?static;

    function legacy(array &$rows, $untyped = [1, 2]) {}

    #[Scope]
    protected function paid($query): void {}
}
"#;

#[test]
fn class_with_imports_modifiers_and_members() {
    let syms = symbols(MODEL);
    let class = get(&syms, "App\\Models\\Order");
    assert_eq!(class.kind, SymbolKind::Class);
    assert!(class.is_final);
    assert_eq!(class.start_line, 17);
    // The docblock sits above the attribute, which belongs to the class node.
    assert_eq!(
        class.doc_summary.as_deref(),
        Some("An order placed by a customer.")
    );
    assert_eq!(
        class.meta.extends,
        ["Illuminate\\Database\\Eloquent\\Model"]
    );
    assert_eq!(
        class.meta.implements,
        ["App\\Contracts\\Payable", "JsonSerializable"]
    );
    assert_eq!(
        class.meta.traits,
        [
            "Illuminate\\Database\\Eloquent\\SoftDeletes",
            "App\\Models\\HasFactory"
        ]
    );
    assert_eq!(
        class.meta.attributes,
        ["Illuminate\\Database\\Eloquent\\Attributes\\ObservedBy"]
    );
    assert_eq!(
        class.signature,
        "final class Order extends Model implements PayableContract, \\JsonSerializable"
    );
}

#[test]
fn methods_params_and_return_types() {
    let syms = symbols(MODEL);
    let customer = get(&syms, "App\\Models\\Order::customer");
    assert_eq!(customer.kind, SymbolKind::Method);
    assert_eq!(customer.parent_fqn.as_deref(), Some("App\\Models\\Order"));
    assert_eq!(customer.visibility.as_deref(), Some("public"));
    assert_eq!(customer.signature, "public function customer(): BelongsTo");
    assert_eq!(
        customer.doc_summary.as_deref(),
        Some("Customer who placed the order.")
    );
    assert_eq!(
        customer.meta.return_fqns,
        ["Illuminate\\Database\\Eloquent\\Relations\\BelongsTo"]
    );
    assert_eq!(
        get(&syms, "App\\Models\\Order::items").meta.return_fqns,
        ["Illuminate\\Database\\Eloquent\\Relations\\HasMany"]
    );

    let build = get(&syms, "App\\Models\\Order::build");
    assert!(build.is_static && build.is_abstract);
    assert_eq!(build.visibility.as_deref(), Some("protected"));
    assert_eq!(build.meta.params.len(), 2);
    assert_eq!(build.meta.params[0].type_fqns, ["App\\Models\\Customer"]);
    assert!(build.meta.params[1].variadic);
    assert_eq!(build.meta.params[1].r#type.as_deref(), Some("int|string"));
    assert!(build.meta.params[1].type_fqns.is_empty());
    assert_eq!(build.meta.return_fqns, ["App\\Models\\Order"]);

    let legacy = get(&syms, "App\\Models\\Order::legacy");
    assert_eq!(legacy.visibility.as_deref(), Some("public"));
    assert!(legacy.meta.params[0].by_ref);
    assert_eq!(legacy.meta.params[0].name, "$rows");
    assert_eq!(legacy.meta.params[1].default.as_deref(), Some("[1, 2]"));

    let paid = get(&syms, "App\\Models\\Order::paid");
    assert_eq!(paid.meta.attributes, ["App\\Models\\Scope"]);
}

#[test]
fn properties_constants_and_promotion() {
    let syms = symbols(MODEL);
    let fillable = get(&syms, "App\\Models\\Order::$fillable");
    assert_eq!(fillable.kind, SymbolKind::Property);
    assert_eq!(fillable.name, "fillable");
    assert_eq!(
        fillable.meta.value.as_deref(),
        Some("['customer_id', 'status']")
    );
    assert_eq!(
        fillable.doc_summary.as_deref(),
        Some("Mass-assignable attributes.")
    );
    assert_eq!(
        fillable.signature,
        "protected $fillable = ['customer_id', 'status']"
    );

    let latest = get(&syms, "App\\Models\\Order::$latest");
    assert!(latest.is_static);
    assert_eq!(latest.meta.r#type.as_deref(), Some("?Order"));
    assert_eq!(latest.meta.value.as_deref(), Some("null"));

    assert!(get(&syms, "App\\Models\\Order::$tags").meta.readonly);
    // Non-literal defaults are not stored.
    assert_eq!(get(&syms, "App\\Models\\Order::$computed").meta.value, None);

    let paid = get(&syms, "App\\Models\\Order::STATUS_PAID");
    assert_eq!(paid.kind, SymbolKind::Const);
    assert_eq!(paid.meta.value.as_deref(), Some("'paid'"));
    assert_eq!(paid.signature, "public const STATUS_PAID = 'paid'");
    let offset = get(&syms, "App\\Models\\Order::OFFSET");
    assert!(offset.is_final);
    assert_eq!(offset.visibility.as_deref(), Some("protected"));
    assert_eq!(offset.meta.value.as_deref(), Some("-1"));

    let service = get(&syms, "App\\Models\\Order::$service");
    assert_eq!(service.visibility.as_deref(), Some("private"));
    assert!(service.meta.readonly);
    assert_eq!(service.signature, "private readonly OrderService $service");
    let priority = get(&syms, "App\\Models\\Order::$priority");
    assert_eq!(priority.meta.value.as_deref(), Some("1"));
    assert!(syms.iter().all(|s| s.fqn != "App\\Models\\Order::$plain"));

    let ctor = get(&syms, "App\\Models\\Order::__construct");
    assert_eq!(ctor.meta.params.len(), 3);
    assert!(ctor.meta.params[0].promoted);
    assert_eq!(ctor.meta.params[0].type_fqns, ["App\\Models\\OrderService"]);
    assert_eq!(
        ctor.signature,
        "public function __construct(private readonly OrderService $service, protected int $priority = 1, string $plain = 'x')"
    );
}

#[test]
fn interfaces_traits_enums() {
    let syms = symbols(
        r#"<?php
namespace App\Contracts;

interface Payable extends \Countable, Billable
{
    public function pay(int $cents): bool;
}

trait HasTotals
{
    abstract public function items(): array;
}

enum Status: string implements HasLabel
{
    use HasTotals;
    case Pending = 'pending';
    case Paid = 'paid';
    const DEFAULT = self::Pending;

    public function label(): string { return ucfirst($this->value); }
}

enum Flag { case On; }
"#,
    );
    let payable = get(&syms, "App\\Contracts\\Payable");
    assert_eq!(payable.kind, SymbolKind::Interface);
    assert_eq!(
        payable.meta.extends,
        ["Countable", "App\\Contracts\\Billable"]
    );
    assert_eq!(
        get(&syms, "App\\Contracts\\Payable::pay").signature,
        "public function pay(int $cents): bool"
    );
    assert_eq!(
        get(&syms, "App\\Contracts\\HasTotals").kind,
        SymbolKind::Trait
    );
    assert!(get(&syms, "App\\Contracts\\HasTotals::items").is_abstract);

    let status = get(&syms, "App\\Contracts\\Status");
    assert_eq!(status.kind, SymbolKind::Enum);
    assert_eq!(status.meta.backed_type.as_deref(), Some("string"));
    assert_eq!(status.meta.traits, ["App\\Contracts\\HasTotals"]);
    assert_eq!(status.signature, "enum Status: string implements HasLabel");
    let paid = get(&syms, "App\\Contracts\\Status::Paid");
    assert_eq!(paid.kind, SymbolKind::EnumCase);
    assert_eq!(paid.meta.value.as_deref(), Some("'paid'"));
    assert_eq!(paid.signature, "case Paid = 'paid'");
    assert_eq!(
        get(&syms, "App\\Contracts\\Status::DEFAULT")
            .meta
            .value
            .as_deref(),
        Some("self::Pending")
    );
    assert_eq!(get(&syms, "App\\Contracts\\Flag::On").signature, "case On");
}

#[test]
fn helpers_inside_function_exists_guards() {
    let syms = symbols(
        r#"<?php

if (! function_exists('money')) {
    /**
     * Formats cents as money.
     */
    function money(int $cents, string $currency = 'USD'): string
    {
        return number_format($cents / 100, 2);
    }
}

function plain() {}
const VERSION = '1.0';
"#,
    );
    let money = get(&syms, "money");
    assert_eq!(money.kind, SymbolKind::Function);
    assert_eq!(
        money.doc_summary.as_deref(),
        Some("Formats cents as money.")
    );
    assert_eq!(
        money.signature,
        "function money(int $cents, string $currency = 'USD'): string"
    );
    assert_eq!(money.start_line, 7);
    assert_eq!(get(&syms, "plain").kind, SymbolKind::Function);
    assert_eq!(get(&syms, "VERSION").kind, SymbolKind::Const);
}

#[test]
fn namespaces_braced_and_multiple() {
    let syms = symbols(
        r#"<?php
namespace First;
use Shared\Thing;
class A extends Thing {}

namespace Second;
class B extends Thing {}

namespace Third {
    use Other\Base as Parent_;
    class C extends Parent_ {}
}
namespace {
    function global_helper() {}
}
"#,
    );
    assert_eq!(get(&syms, "First\\A").meta.extends, ["Shared\\Thing"]);
    // Imports do not carry into the next namespace.
    assert_eq!(get(&syms, "Second\\B").meta.extends, ["Second\\Thing"]);
    assert_eq!(get(&syms, "Third\\C").meta.extends, ["Other\\Base"]);
    assert_eq!(get(&syms, "global_helper").kind, SymbolKind::Function);
}

#[test]
fn anonymous_classes_and_closures_are_skipped() {
    let syms = symbols(
        r#"<?php
use Illuminate\Database\Migrations\Migration;

return new class extends Migration
{
    public function up(): void
    {
        $fn = function () {};
    }
};
"#,
    );
    assert!(syms.is_empty(), "{syms:#?}");
}

#[test]
fn syntax_error_warns_and_keeps_what_parsed() {
    let parsed = extract(
        br#"<?php
namespace App;

class Good
{
    public function ok() {}
}

class Broken
{
    public function oops( {
}
"#,
    );
    assert_eq!(parsed.warnings.len(), 1, "{:?}", parsed.warnings);
    assert!(parsed.warnings[0].message.starts_with("could not parse"));
    assert!(parsed.warnings[0].line >= 9);
    assert!(parsed.symbols.iter().any(|s| s.fqn == "App\\Good::ok"));
}

#[test]
fn html_before_php_tag_is_fine() {
    let syms = symbols("<html><?php\nfunction view_helper() {}\n?></html>");
    assert_eq!(get(&syms, "view_helper").kind, SymbolKind::Function);
}
