# Collation-Aware Data Skipping Implementation Plan

## Objective

Implement collation-aware data skipping using statsWithCollation from the Delta collation RFC. This enables correct file skipping when predicates use ICU collations (e.g., WHERE name < 'Bob' COLLATE icu.de_DE.75.1).

## Requirements (User-Specified)

- **Strict version matching**: Predicate collation version must match kernel's ICU version, otherwise FAIL query
- **UTF8_LCASE versioning**: UTF8_LCASE should use ICU version. If version not specified, use kernel's ICU_VERSION automatically
- **Missing stats handling**: If statsWithCollation is missing → NULL propagation → keep file (conservative, same as regular stats)
- **Fail on version mismatch**: Any collation version mismatch in predicate → FAIL whole query
- **Backward compatibility**: UTF8_BINARY predicates continue using regular minValues/maxValues

## Implementation Steps

### 1. Extend Stats Schema with statsWithCollation

File: `/home/stefan.kandic/delta-kernel-rs/kernel/src/scan/data_skipping.rs` (lines 119-124)

Add new field to stats schema:
```rust
StructField::nullable("statsWithCollation",
    DataType::Map(Box::new(MapType::new(
        DataType::STRING,  // key: "icu.en_US.75.1"
        DataType::Struct(Arc::new(StructType::new_unchecked([
            StructField::nullable("minValues", stats_schema.clone()),
            StructField::nullable("maxValues", stats_schema.clone()),
        ]))),
        true
    )))
)
```

### 2. Add Collation Key Methods

File: `/home/stefan.kandic/delta-kernel-rs/kernel/src/collation.rs` (after line 160)

Implement three methods on `CollationIdentifier`:

```rust
/// Creates statsWithCollation lookup key: "icu.en_US.75.1"
pub fn to_stats_key(&self) -> DeltaResult<String>

/// Parses stats key: returns (provider, name, version)
pub fn from_stats_key(key: &str) -> DeltaResult<(String, String, String)>

/// Returns true if this collation needs statsWithCollation (anything except UTF8_BINARY)
pub fn requires_stats_with_collation(&self) -> bool
```

### 3. Add Error Types

File: `/home/stefan.kandic/delta-kernel-rs/kernel/src/error.rs`

Add enum variants (after line 212):
```rust
CollationVersionMismatch(String),
MissingCollationStats(String),
```

Add constructors (after line 295):
```rust
pub fn collation_version_mismatch(msg: impl ToString) -> Self
pub fn missing_collation_stats(msg: impl ToString) -> Self
```

### 4. Extract Required Collations from Predicate

File: `/home/stefan.kandic/delta-kernel-rs/kernel/src/scan/data_skipping.rs` (new function)

Add helper function:
```rust
/// Walks predicate tree and extracts all CollationIdentifiers that require statsWithCollation
fn extract_required_collations(pred: &Pred) -> HashSet<CollationIdentifier>
```

Add field to `DataSkippingFilter` struct (line 53):
```rust
required_collations: Option<HashSet<CollationIdentifier>>
```

Update constructor `DataSkippingFilter::new()` (around line 61) to extract and store collations.

### 5. Implement Version Validation

File: `/home/stefan.kandic/delta-kernel-rs/kernel/src/scan/data_skipping.rs` (new method)

Add validation method:
```rust
fn validate_collation_stats(
    &self,
    stats: &dyn EngineData,
    required: &HashSet<CollationIdentifier>,
) -> DeltaResult<()>
```

Call from `DataSkippingFilter::apply()` (around line 179) after parsing stats, before predicate evaluation:
```rust
if let Some(ref required) = self.required_collations {
    self.validate_collation_stats(&*parsed_stats, required)?;
}
```

Validation logic:
- Compare predicate's `collation.version` against `collation_factory::ICU_VERSION`
- If mismatch → return `Error::collation_version_mismatch()`
- Note: Checking `statsWithCollation[key]` existence deferred to Phase 2 (complex, requires EngineData traversal)

### 6. Route Predicates Based on Collation

File: `/home/stefan.kandic/delta-kernel-rs/kernel/src/scan/data_skipping.rs`

Current behavior (lines ~800-850):
- `DataSkippingPredicateCreator` implements `DataSkippingPredicateEvaluator`
- Methods like `get_min_stat()` and `get_max_stat()` return `joined_column_expr!("minValues", col)`

New behavior:

Add context tracking to `DataSkippingPredicateCreator`:
```rust
struct DataSkippingPredicateCreator {
    current_context: RefCell<Option<ExprContext>>,
}
```

Update trait implementations to store context:
```rust
fn eval_pred_lt(&self, col: &ColumnName, val: &Scalar, context: Option<&ExprContext>, inverted: bool) -> Option<Pred> {
    *self.current_context.borrow_mut() = context.cloned();
    // ... rest of implementation
}
```

Update `get_min_stat()` routing:
```rust
fn get_min_stat(&self, col: &ColumnName, _data_type: &DataType) -> Option<Expr> {
    let context = self.current_context.borrow();

    if let Some(ref ctx) = *context {
        if let Some(ref collation) = ctx.collation {
            if collation.requires_stats_with_collation() {
                let stats_key = collation.to_stats_key().ok()?;
                return Some(joined_column_expr!("statsWithCollation", stats_key, "minValues", col));
            }
        }
    }

    // Default: UTF8_BINARY path
    Some(joined_column_expr!("minValues", col))
}
```

Update `get_max_stat()` similarly (with timestamp check preserved).

### 7. Testing

Unit tests (in `/home/stefan.kandic/delta-kernel-rs/kernel/src/scan/data_skipping.rs`):
- `test_stats_schema_includes_collation()`
- `test_collation_key_format()`
- `test_predicate_transform_utf8_binary()`
- `test_predicate_transform_icu_collation()`
- `test_version_validation_mismatch()`
- `test_extract_required_collations()`
- `test_complex_predicate_multiple_collations()`

Integration tests:
- `test_e2e_file_skipping_with_collation()`
- `test_version_mismatch_fails_query()`
- `test_backward_compatibility_no_collation()`

## Critical Files

1. `/home/stefan.kandic/delta-kernel-rs/kernel/src/scan/data_skipping.rs` - Core logic (stats schema, routing, validation)
2. `/home/stefan.kandic/delta-kernel-rs/kernel/src/collation.rs` - Collation key methods
3. `/home/stefan.kandic/delta-kernel-rs/kernel/src/error.rs` - Error types
4. `/home/stefan.kandic/delta-kernel-rs/kernel/src/collation_factory.rs` - Reference ICU_VERSION (no changes, read-only)

## Implementation Phases

**Phase 1 (MVP)**: Steps 1-7 above
- Extends schema
- Routes predicates correctly
- Validates versions
- Unit tests

**Phase 2 (Future)**:
- Check `statsWithCollation[key]` existence (requires EngineData traversal)
- Emit `Error::missing_collation_stats()` when key missing
- Handle nested field collations
- Performance optimization

## Edge Cases

- Nested fields: `address.city` with collation → `statsWithCollation["icu.de_DE.75.1"].minValues.address.city`
- Mixed collations: UTF8_BINARY and ICU in same query → route independently
- UTF8_LCASE: Phase 1 treats like UTF8_BINARY (uses regular minValues/maxValues)
- Complex predicates: OR requires all branches eligible; any version issue fails entire query

## Key Design Decisions

1. Collation key format: `{provider}.{name}.{version}` (e.g., "icu.en_US.75.1")
2. Strict validation: Version mismatch = query failure (no fallback to binary stats)
3. Context threading: Store in `RefCell` for trait method access
4. Conservative Phase 1: Don't check `statsWithCollation` key existence yet (NULL propagation handles it)
