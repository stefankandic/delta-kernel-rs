# Collation Usage Examples

This document demonstrates how to use collation identifiers with Delta Kernel expressions.

## Basic Usage

### Creating Collation Identifiers

```rust
use delta_kernel::collation::{CollationIdentifier, CollationProvider};
use delta_kernel::expressions::ExprContext;

// Spark UTF8 binary collation (default, case-sensitive)
let binary = CollationIdentifier::spark("UTF8_BINARY");

// Spark case-insensitive collation
let case_insensitive = CollationIdentifier::spark("UTF8_LCASE");

// ICU German locale collation with version
let german = CollationIdentifier::icu("de_DE", Some("75.1".to_string()));

// ICU English locale without specific version
let english = CollationIdentifier::icu("en_US", None);

// Custom collation provider
let custom = CollationIdentifier::custom("my_provider", "my_collation");
```

### Creating Expression Contexts

```rust
use delta_kernel::expressions::ExprContext;
use delta_kernel::collation::CollationIdentifier;

// Default context (no special settings)
let ctx = ExprContext::new();

// Context with collation
let collation = CollationIdentifier::spark("UTF8_LCASE");
let ctx = ExprContext::with_collation(collation);

// You can also access the collation from a context
if let Some(coll) = ctx.collation() {
    println!("Using collation: {}", coll);
}
```

### Using Collations in Predicates

There are several ways to create collation-aware predicates:

#### 1. Using ExprContext (Recommended for Future Extensions)

```rust
use delta_kernel::expressions::{column_expr, ExprContext};
use delta_kernel::collation::CollationIdentifier;

let collation = CollationIdentifier::spark("UTF8_LCASE");
let context = ExprContext::with_collation(collation);

// Use with_context to attach the full context
let pred = column_expr!(\"name\")
    .eq(\"Alice\")
    .with_context(context);
```

#### 2. Builder Pattern with Expression (Convenience Method)

```rust
use delta_kernel::expressions::{column_expr, Expression};
use delta_kernel::collation::CollationIdentifier;

let collation = CollationIdentifier::spark("UTF8_LCASE");

// Case-insensitive name comparison
let pred = column_expr!("name")
    .eq_collated("Alice", collation.clone());

// Case-insensitive range check
let pred = column_expr!("city")
    .gt_collated("Berlin", collation.clone());
```

#### 3. Builder Pattern with with_collation()

```rust
use delta_kernel::expressions::{column_expr, Predicate};
use delta_kernel::collation::CollationIdentifier;

let collation = CollationIdentifier::spark("UTF8_LCASE");

// Create predicate then attach collation
let pred = column_expr!("email")
    .eq("user@example.com")
    .with_collation(collation);
```

#### 4. Static Constructors

```rust
use delta_kernel::expressions::{column_expr, Predicate};
use delta_kernel::collation::CollationIdentifier;

let collation = CollationIdentifier::icu("de_DE", Some("75.1".to_string()));

// Direct construction
let pred = Predicate::eq_collated(
    column_expr!("name"),
    "München",
    collation
);
```

## Complete Example

```rust
use delta_kernel::expressions::{column_expr, Predicate};
use delta_kernel::collation::CollationIdentifier;

fn create_case_insensitive_filter(name: &str) -> Predicate {
    let collation = CollationIdentifier::spark("UTF8_LCASE");

    // This will match "alice", "Alice", "ALICE", etc.
    column_expr!("name").eq_collated(name, collation)
}

fn create_locale_aware_filter(city: &str) -> Predicate {
    // Use German collation rules for sorting
    let collation = CollationIdentifier::icu("de_DE", Some("75.1".to_string()));

    column_expr!("city").lt_collated(city, collation)
}

// Combining predicates
fn create_complex_filter() -> Predicate {
    let case_insensitive = CollationIdentifier::spark("UTF8_LCASE");
    let german = CollationIdentifier::icu("de_DE", Some("75.1".to_string()));

    // (name = 'alice' COLLATE UTF8_LCASE) AND (city < 'München' COLLATE de_DE:75.1)
    Predicate::and(
        column_expr!("name").eq_collated("alice", case_insensitive),
        column_expr!("city").lt_collated("München", german)
    )
}
```

## Available Collation-Aware Operations

All comparison operations support collations:

- `eq_collated` - Equality (=)
- `ne_collated` - Inequality (!=)
- `lt_collated` - Less than (<)
- `le_collated` - Less than or equal (<=)
- `gt_collated` - Greater than (>)
- `ge_collated` - Greater than or equal (>=)
- `distinct_collated` - DISTINCT comparison (NULL-safe)

## Collation Display

When printing predicates with collations, the collation is shown:

```rust
let pred = column_expr!("name")
    .eq_collated("Alice", CollationIdentifier::spark("UTF8_LCASE"));

println!("{}", pred);
// Output: name = 'Alice' COLLATE spark:UTF8_LCASE

let pred = column_expr!("name")
    .eq_collated("Berlin", CollationIdentifier::icu("de_DE", Some("75.1".to_string())));

println!("{}", pred);
// Output: name = 'Berlin' COLLATE icu:de_DE:75.1
```

## JSON Serialization

CollationIdentifier implements Serialize/Deserialize:

```rust
use delta_kernel::collation::CollationIdentifier;
use serde_json;

let collation = CollationIdentifier::icu("de_DE", Some("75.1".to_string()));
let json = serde_json::to_string(&collation).unwrap();
// {"provider":"icu","name":"de_DE","version":"75.1"}

// Version is omitted when None
let collation = CollationIdentifier::spark("UTF8_LCASE");
let json = serde_json::to_string(&collation).unwrap();
// {"provider":"spark","name":"UTF8_LCASE"}
```

## ICU Builder API (New)

The builder API provides a convenient way to create ICU collations with case and accent sensitivity options.

### Case-Insensitive Collation

```rust
// Case-insensitive English collation (a == A)
let collation = CollationIdentifier::icu_builder("en_US")
    .case_sensitive(false)
    .build();

// Result: "icu:en_US_CI"
```

### Accent-Insensitive Collation

```rust
// Accent-insensitive French collation (é == e)
let collation = CollationIdentifier::icu_builder("fr_CAN")
    .accent_sensitive(false)
    .build();

// Result: "icu:fr_CAN_AI"
```

### Case and Accent Insensitive

```rust
// Both case and accent insensitive German collation
// So 'Ä', 'A', and 'a' are all considered equal
let collation = CollationIdentifier::icu_builder("de")
    .case_sensitive(false)
    .accent_sensitive(false)
    .build();

// Result: "icu:de_CI_AI"
```

### Root Unicode Collation

```rust
// Root/universal collation
let unicode = CollationIdentifier::icu_builder("unicode").build();
// Result: "icu:unicode"

// Case-insensitive unicode
let unicode_ci = CollationIdentifier::icu_builder("unicode")
    .case_sensitive(false)
    .build();
// Result: "icu:unicode_CI"

// Case and accent insensitive unicode
let unicode_ci_ai = CollationIdentifier::icu_builder("unicode")
    .case_sensitive(false)
    .accent_sensitive(false)
    .build();
// Result: "icu:unicode_CI_AI"
```

### Multiple Locales

```rust
// Japanese case-insensitive
let jp_collation = CollationIdentifier::icu_builder("ja_JP")
    .case_sensitive(false)
    .build();
// Result: "icu:ja_JP_CI"

// Traditional Chinese (Macao) with modifiers
let zh_collation = CollationIdentifier::icu_builder("zh_Hant_MAC")
    .case_sensitive(false)
    .accent_sensitive(false)
    .build();
// Result: "icu:zh_Hant_MAC_CI_AI"
```

### Builder with Predicates

```rust
use delta_kernel::expressions::{column_expr, Predicate};
use delta_kernel::collation::CollationIdentifier;

// Create case-insensitive search
let collation = CollationIdentifier::icu_builder("en_US")
    .case_sensitive(false)
    .build();

let pred = column_expr!("email").eq_collated("user@example.com", collation);
// Matches: "USER@EXAMPLE.COM", "User@Example.Com", etc.
```

## Collation Name Format

The builder generates Spark/Databricks format names with `_CI` and `_AI` suffixes:

| Configuration | Spark/Databricks Format | ICU Conversion | Behavior |
|--------------|------------------------|----------------|----------|
| Default | `en_US` | `en_US` | Case-sensitive, accent-sensitive |
| Case-insensitive | `en_US_CI` | `en_US@colStrength=secondary` | Case-insensitive, accent-sensitive |
| Accent-insensitive | `en_US_AI` | `en_US@colStrength=primary` | Case-insensitive, accent-insensitive |
| Both insensitive | `en_US_CI_AI` | `en_US@colStrength=primary` | Case-insensitive, accent-insensitive |
| Unicode root | `unicode` | `unicode` | Root collation |
| Unicode CI | `unicode_CI` | `unicode@colStrength=secondary` | Case-insensitive root |

The Spark format (`_CI`/`_AI`) is used for storage and display. When creating ICU collators,
the format is converted to ICU keywords automatically via `to_icu_locale_string()`.

## Supported Locales

Collation names follow Spark/Databricks semantics using ICU locale identifiers:

**Format**: `language[_Script][_Country]`

- **language**: 2-letter ISO 639-1 code (e.g., `en`, `fr`, `zh`)
- **Script**: 4-letter ISO 15924 code (optional, e.g., `Hant`, `Cyrl`)
- **Country**: 2-letter ISO 3166-1 alpha-2 OR 3-letter ISO 3166-1 alpha-3 (e.g., `US`, `CAN`, `MAC`)

**Case insensitive**: `en_US`, `EN_US`, `en_us` are all equivalent

### Examples:

```rust
// Root/universal collation
CollationIdentifier::icu_builder("unicode")      // Universal/root collation
CollationIdentifier::icu_builder("unicode_CI")   // Case-insensitive universal

// Language only
CollationIdentifier::icu_builder("en")
CollationIdentifier::icu_builder("en_CI")        // Case-insensitive English

// Language + 2-letter country
CollationIdentifier::icu_builder("en_US")        // English (United States)
CollationIdentifier::icu_builder("de_DE_CI_AI")  // German (Germany), case + accent insensitive

// Language + 3-letter country (Databricks format)
CollationIdentifier::icu_builder("fr_CAN")       // French (Canada)
CollationIdentifier::icu_builder("en_GBR_CI")    // English (Great Britain), case-insensitive

// Language + Script + Country
CollationIdentifier::icu_builder("zh_Hant_MAC")  // Traditional Chinese (Macao)
CollationIdentifier::icu_builder("sr_Cyrl_RS")   // Serbian (Cyrillic, Serbia)
CollationIdentifier::icu_builder("sr_Latn_RS")   // Serbian (Latin, Serbia)
```

See [Databricks Collation Reference](https://docs.databricks.com/aws/en/sql/language-manual/sql-ref-collation) for full specification.

## Notes

- **Collation is optional**: If no collation is specified, binary (byte-wise) comparison is used
- **String operations only**: Collation only affects string comparisons; numeric/date comparisons are unaffected
- **Backward compatible**: Existing code without collations continues to work unchanged
- **Provider + name + version**: The three-part identifier ensures precise collation semantics across systems
- **Builder API**: Use `icu_builder()` for fine-grained control over case/accent sensitivity
