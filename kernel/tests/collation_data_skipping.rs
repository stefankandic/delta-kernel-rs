//! Integration tests for collation-aware data skipping.
//!
//! These tests use a Delta table with 3 parquet files, each containing one row:
//! - File 1: (id=1, name='Müller')
//! - File 2: (id=2, name='MÜLLER')
//! - File 3: (id=3, name='müller')
//!
//! Each file has statsWithCollation["icu.UNICODE_CI.75.1"] containing min/max values
//! for the `name` column using the UNICODE_CI (case-insensitive) collation.
//!
//! Tests verify:
//! 1. Data skipping works with matching collation versions
//! 2. Version mismatches cause predicates to be dropped (conservative approach)
//! 3. Version-agnostic collations (version: None) always work

use std::path::PathBuf;
use std::sync::Arc;

use delta_kernel::collation::{CollationIdentifier, CollationProvider};
use delta_kernel::expressions::{column_expr, Expression as Expr, Predicate as Pred};
use delta_kernel::{Engine, Snapshot};

// ============================================================================
// Helper Functions
// ============================================================================

/// Sets up the test table and returns the engine and snapshot (wrapped in Arc)
fn setup_test_table() -> Result<(Arc<dyn Engine>, Arc<Snapshot>), Box<dyn std::error::Error>> {
    let table_path = std::fs::canonicalize(PathBuf::from("./tests/data/collations"))?;
    let url = url::Url::from_directory_path(table_path).unwrap();
    let engine = test_utils::create_default_engine(&url)?;
    let snapshot = Snapshot::builder_for(url).build(engine.as_ref())?;
    Ok((engine, snapshot))
}

/// Executes a scan with the given predicate and returns the number of files read
fn count_files_with_predicate(
    snapshot: Arc<Snapshot>,
    engine: Arc<dyn Engine>,
    predicate: Pred,
) -> Result<usize, Box<dyn std::error::Error>> {
    let scan = snapshot
        .scan_builder()
        .with_predicate(Arc::new(predicate))
        .build()?;

    let mut num_read_files = 0;
    for _ in scan.execute(engine)? {
        num_read_files += 1;
    }
    Ok(num_read_files)
}

/// Creates an ICU collation with the specified name and version
/// - `name`: The collation name (e.g., "UNICODE_CI", "en_US")
/// - `version: Some(v)` creates version-specific collation
/// - `version: None` creates version-agnostic collation
/// - Pass `Some(delta_kernel::collation_factory::ICU_VERSION.to_string())` for current kernel version
fn create_icu_collation(name: &str, version: Option<String>) -> CollationIdentifier {
    CollationIdentifier {
        provider: CollationProvider::Icu,
        name: name.to_string(),
        version,
    }
}

/// Creates a UNICODE_CI collation with a version that doesn't match the kernel's ICU version
fn create_wrong_version_collation() -> CollationIdentifier {
    let kernel_version = delta_kernel::collation_factory::ICU_VERSION.to_string();
    let wrong_version = if kernel_version == "75.1" {
        "76.0"
    } else {
        "75.1"
    };

    create_icu_collation("UNICODE_CI", Some(wrong_version.to_string()))
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn test_collation_data_skipping_unicode_ci() -> Result<(), Box<dyn std::error::Error>> {
    // Table has 3 files with UNICODE_CI collation stats:
    // File 1: (1, 'Müller') with statsWithCollation["icu.UNICODE_CI.75.1"]
    // File 2: (2, 'MÜLLER') with statsWithCollation["icu.UNICODE_CI.75.1"]
    // File 3: (3, 'müller') with statsWithCollation["icu.UNICODE_CI.75.1"]

    let (engine, snapshot) = setup_test_table()?;
    let collation = create_icu_collation(
        "UNICODE_CI",
        Some(delta_kernel::collation_factory::ICU_VERSION.to_string()),
    );

    // Test 1: Query with matching collation version
    // Query: name < 'A' (should skip all files since all values start with 'M' or 'm')
    let predicate = column_expr!("name").lt_collated(Expr::literal("A"), collation.clone());
    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    // With collation-aware data skipping, all files should be skipped
    assert_eq!(
        num_read_files, 0,
        "Expected 0 files (all skipped) when name < 'A' with UNICODE_CI collation, but got {}",
        num_read_files
    );

    // Test 2: Query that matches all files
    // name >= 'MÜLLER' with UNICODE_CI (case-insensitive)
    // Should match all files since 'Müller', 'MÜLLER', 'müller' are all equal under UNICODE_CI
    let predicate = column_expr!("name").ge_collated(Expr::literal("MÜLLER"), collation.clone());
    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    // With UNICODE_CI, all three files should be included (can't skip any)
    assert_eq!(
        num_read_files, 3,
        "Expected 3 files (all included) when name >= 'MÜLLER' with UNICODE_CI (case-insensitive), but got {}",
        num_read_files
    );

    // Test 3: Query with name > 'Z'
    // With UNICODE_CI collation, 'Müller', 'MÜLLER', and 'müller' are all treated the same
    // All have max value that equals 'M' (case-insensitive), which is < 'Z'
    // Therefore all files should be skipped
    let predicate = column_expr!("name").gt_collated(Expr::literal("Z"), collation);
    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;
    assert_eq!(
        num_read_files, 0,
        "Expected 0 files (all skipped) when name > 'Z' with UNICODE_CI collation (all values start with 'M' < 'Z'), but got {}",
        num_read_files
    );


    Ok(())
}

#[test]
fn test_collation_version_mismatch_data_skipping() -> Result<(), Box<dyn std::error::Error>> {
    // Test that version mismatch causes predicate to be dropped (read all files)
    // Using name > 'M' gives THREE different outcomes based on which logic path is taken

    let (engine, snapshot) = setup_test_table()?;
    let collation = create_wrong_version_collation();

    // Query: name > 'M'
    //
    // Three possible outcomes:
    // 1. UTF8_BINARY: 'Müller' byte value (0xC3...) > 'M' (0x4D) → would skip some files
    // 2. UNICODE_CI correct version: 'Müller' normalizes to 'M', and 'M' NOT > 'M' → skip all files → 0 files
    // 3. Wrong version (THIS TEST): predicate dropped → no skipping → 3 files
    //
    // This way, if we get 0 or some other number, we know which code path ran!
    let predicate = column_expr!("name").gt_collated(Expr::literal("M"), collation);

    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    // With version mismatch, predicate is dropped for data skipping
    // → all files are read (conservative approach)
    // If we get 0: incorrect - used UNICODE_CI logic (should have been dropped)
    // If we get 3: correct - predicate was dropped, read all files
    assert_eq!(
        num_read_files, 3,
        "Expected 3 files (all files, no skipping) when collation version mismatches and predicate is dropped, but got {}. \
         Got 0? Bug: predicate used UNICODE_CI instead of being dropped. Got other? Bug in data skipping logic.",
        num_read_files
    );

    Ok(())
}

#[test]
fn test_collation_version_agnostic() -> Result<(), Box<dyn std::error::Error>> {
    // Test that version-agnostic collations (version: None) always work

    let (engine, snapshot) = setup_test_table()?;

    // Create version-agnostic collation (no version specified)
    let collation = create_icu_collation("UNICODE_CI", None);

    // Query: name < 'A' (should skip all files)
    let predicate = column_expr!("name").lt_collated(Expr::literal("A"), collation);
    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    // Version-agnostic should use kernel's ICU version and skip files
    assert_eq!(
        num_read_files, 0,
        "Expected 0 files (all skipped) with version-agnostic collation, but got {}",
        num_read_files
    );

    Ok(())
}

#[test]
fn test_complex_predicate_with_collation() -> Result<(), Box<dyn std::error::Error>> {
    // Test complex predicates combining collated and non-collated columns

    let (engine, snapshot) = setup_test_table()?;
    let collation = create_icu_collation(
        "UNICODE_CI",
        Some(delta_kernel::collation_factory::ICU_VERSION.to_string()),
    );

    // Test 1: AND predicate - id > 1 AND name < 'Z'
    // With UNICODE_CI collation working properly:
    // - id > 1: skips file 1 (id=1), keeps files 2,3 (id=2,3)
    // - name < 'Z' with UNICODE_CI: 'M'/'m' treated equal, all < 'Z', doesn't skip any
    // - AND: intersection = keeps files 2,3 = 2 files
    let predicate = Pred::and(
        column_expr!("id").gt(Expr::literal(1i32)),
        column_expr!("name").lt_collated(Expr::literal("Z"), collation.clone()),
    );
    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    assert_eq!(
        num_read_files, 2,
        "Expected 2 files (files 2,3): id > 1 skips file 1, name < 'Z' with UNICODE_CI keeps all (M < Z), but got {}",
        num_read_files
    );

    // Test 2: AND predicate - id < 2 AND name > 'A'
    // Verifies that AND predicates with mixed collated/non-collated columns work
    // - id < 2: keeps file 1 (id=1), skips files 2,3 (id=2,3)
    // - name > 'A': all names start with 'M'/'m' > 'A', so doesn't skip any
    // - AND: intersection = keeps only file 1 = 1 file
    let predicate = Pred::and(
        column_expr!("id").lt(Expr::literal(2i32)),
        column_expr!("name").gt_collated(Expr::literal("A"), collation.clone()),
    );
    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    assert_eq!(
        num_read_files, 1,
        "Expected 1 file (file 1): id < 2 keeps file 1, name > 'A' keeps all, AND = file 1, but got {}",
        num_read_files
    );

    // Test 3: OR predicate - id = 1 OR name > 'N'
    // Verifies that OR predicates with mixed collated/non-collated columns work
    // With UNICODE_CI working properly:
    // - File 1: id=1 → first operand true → keep
    // - File 2: max(id)=2 (≠1), min(name)='MÜLLER', under UNICODE_CI 'M' < 'N' → both false → skip
    // - File 3: max(id)=3 (≠1), min(name)='müller', under UNICODE_CI 'm'='M' < 'N' → both false → skip
    // Result: Keep file 1 only = 1 file
    let predicate = Pred::or(
        column_expr!("id").eq(Expr::literal(1i32)),
        column_expr!("name").gt_collated(Expr::literal("N"), collation),
    );
    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    assert_eq!(
        num_read_files, 1,
        "Expected 1 file (file 1): id=1 OR name>'N', only file 1 has id=1, files 2,3 have M<N under UNICODE_CI, but got {}",
        num_read_files
    );

    Ok(())
}

#[test]
fn test_complex_predicate_version_mismatch_partial_skip() -> Result<(), Box<dyn std::error::Error>> {
    // Test that with wrong collation version, we can still skip on non-collated columns

    let table_path = std::fs::canonicalize(PathBuf::from("./tests/data/collations"))?;
    let url = url::Url::from_directory_path(table_path).unwrap();
    let engine = test_utils::create_default_engine(&url)?;

    let snapshot = Snapshot::builder_for(url).build(engine.as_ref())?;

    // Create collation with wrong version
    let kernel_version = delta_kernel::collation_factory::ICU_VERSION.to_string();
    let wrong_version = if kernel_version == "75.1" {
        "76.0"
    } else {
        "75.1"
    };

    let wrong_collation = CollationIdentifier {
        provider: CollationProvider::Icu,
        name: "UNICODE_CI".to_string(),
        version: Some(wrong_version.to_string()),
    };

    // First, test just the name predicate with wrong version alone
    // Note: This table has no partition columns, so predicates are only for data skipping
    // Data skipping handles version mismatch gracefully by dropping the incompatible predicate
    let predicate = column_expr!("name").lt_collated(Expr::literal("Z"), wrong_collation.clone());
    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;
    // With wrong version, predicate is dropped, so all 3 files are read
    assert_eq!(
        num_read_files, 3,
        "Expected 3 files (all files): With wrong version, collation predicate dropped by data skipping, but got {}",
        num_read_files
    );

    // Predicate: id > 1 AND name < 'Z'
    // With wrong collation version:
    // - id > 1 predicate is still evaluated (not collated) → can skip based on id
    // - name < 'Z' predicate is dropped (wrong version) → no collation-based skipping
    // Important: AND predicate continues with remaining operand (doesn't fail query)
    let predicate = Pred::and(
        column_expr!("id").gt(Expr::literal(1i32)),
        column_expr!("name").lt_collated(Expr::literal("Z"), wrong_collation),
    );

    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    // With wrong collation version, the name predicate is DROPPED (returns None)
    // For AND predicate:
    // - When one operand is dropped, AND continues with remaining operands
    // - Only id > 1 predicate remains: skips file 1 (id=1), keeps files 2,3 (id=2,3)
    // Result: 2 files
    assert_eq!(
        num_read_files, 2,
        "Expected 2 files (files 2,3): With wrong version, name predicate dropped, only id > 1 applies, but got {}",
        num_read_files
    );

    Ok(())
}

#[test]
fn test_complex_predicate_version_mismatch_or() -> Result<(), Box<dyn std::error::Error>> {
    // Test OR predicate with wrong collation version

    let (engine, snapshot) = setup_test_table()?;
    let collation = create_wrong_version_collation();

    // Predicate: id > 2 OR name < 'A'
    // With wrong collation version:
    // - id > 2 predicate is evaluated normally
    // - name < 'A' predicate is dropped (wrong version) → returns None
    // OR with None: entire OR is dropped (conservative: read all files)
    let predicate = Pred::or(
        column_expr!("id").gt(Expr::literal(2i32)),
        column_expr!("name").lt_collated(Expr::literal("A"), collation),
    );

    let num_read_files = count_files_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    // With wrong collation version and OR predicate:
    // - name < 'A' predicate is dropped (returns None) due to version mismatch
    // - OR with None operand: entire OR is dropped (conservative approach)
    // - No data skipping predicate remains → read all 3 files
    //
    // This is different from correct version:
    // - Correct: id > 2 OR name < 'A' with UNICODE_CI
    //   - File 1: max(id)=1 (not > 2), min(name)='Müller' ('M' >= 'A') → both false → skip
    //   - File 2: max(id)=2 (not > 2), min(name)='MÜLLER' ('M' >= 'A') → both false → skip
    //   - File 3: min(id)=3 (> 2) → first operand true → keep
    //   - Result: 1 file (file 3)
    assert_eq!(
        num_read_files, 3,
        "Expected 3 files (all): With wrong version, name predicate dropped, entire OR dropped (conservative), but got {}",
        num_read_files
    );

    Ok(())
}

// ============================================================================
// Partition Filtering Tests
// ============================================================================

/// Sets up the partitioned test table and returns the engine and snapshot (wrapped in Arc)
fn setup_partitioned_table() -> Result<(Arc<dyn Engine>, Arc<Snapshot>), Box<dyn std::error::Error>> {
    let table_path = std::fs::canonicalize(PathBuf::from("./tests/data/collations-partitioned"))?;
    let url = url::Url::from_directory_path(table_path).unwrap();
    let engine = test_utils::create_default_engine(&url)?;
    let snapshot = Snapshot::builder_for(url).build(engine.as_ref())?;
    Ok((engine, snapshot))
}

/// Counts total rows from a scan with the given predicate
fn count_rows_with_predicate(
    snapshot: Arc<Snapshot>,
    engine: Arc<dyn Engine>,
    predicate: Pred,
) -> Result<usize, Box<dyn std::error::Error>> {
    use delta_kernel::DeltaResult;

    let scan = snapshot
        .scan_builder()
        .with_predicate(Arc::new(predicate))
        .build()?;

    let total_rows: usize = scan.execute(engine)?
        .map(|result: DeltaResult<Box<dyn delta_kernel::EngineData>>| -> DeltaResult<usize> {
            Ok(result?.len())
        })
        .collect::<DeltaResult<Vec<usize>>>()?
        .into_iter()
        .sum();

    Ok(total_rows)
}

#[test]
fn test_partition_filtering_with_collation() -> Result<(), Box<dyn std::error::Error>> {
    // Partitioned table by name (UTF8_LCASE collation):
    // Partitions: 'Apple', 'apple', 'APPLE', 'Banana', 'banana', 'ananas', 'PEAR'
    //
    // Test that querying with case-insensitive collation finds all case variations
    //
    // Data:
    // - INSERT INTO t2 VALUES (1, 'Apple'), (2, 'Banana')  -- 2 rows
    // - INSERT INTO t2 VALUES (1, 'apple'), (2, 'banana'), (1, 'APPLE')  -- 3 rows
    // - INSERT INTO t2 VALUES (3, 'ananas'), (4, 'PEAR')  -- 2 rows
    //
    // Query: name = 'apple' with UTF8_LCASE
    // Should match partitions: 'Apple' (1 row), 'apple' (1 row), 'APPLE' (1 row) = 3 rows total

    let (engine, snapshot) = setup_partitioned_table()?;
    let collation = CollationIdentifier::spark("UTF8_LCASE");

    // Query: name = 'apple' with UTF8_LCASE collation
    // Should match partitions: 'Apple', 'apple', 'APPLE'
    let predicate = column_expr!("name").eq_collated(Expr::literal("apple"), collation);

    let num_rows = count_rows_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    // Should find all rows where name matches 'apple' (case-insensitive)
    // Expected: 3 rows from 'Apple', 'apple', and 'APPLE' partitions
    assert_eq!(
        num_rows, 3,
        "Expected 3 rows (from Apple, apple, APPLE partitions), got {}",
        num_rows
    );

    Ok(())
}

#[test]
fn test_partition_filtering_wrong_version_fails() -> Result<(), Box<dyn std::error::Error>> {
    // Test that partition filtering with wrong collation version FAILS the query
    // (unlike data skipping which drops the predicate)
    //
    // This is important for security: partition filtering must fail on version mismatch
    // to prevent data leakage from incorrect filtering

    let (engine, snapshot) = setup_partitioned_table()?;
    let collation = create_wrong_version_collation();

    // Query: name = 'apple' with wrong collation version
    let predicate = column_expr!("name").eq_collated(Expr::literal("apple"), collation);

    let result = count_rows_with_predicate(snapshot.clone(), engine.clone(), predicate);

    // Should fail with a CollationVersionMismatch error
    assert!(
        result.is_err(),
        "Expected error for partition filtering with wrong collation version, but query succeeded"
    );

    let error = result.unwrap_err();
    let error_msg = error.to_string();

    assert!(
        error_msg.contains("Collation version mismatch") || error_msg.contains("version"),
        "Expected CollationVersionMismatch error, got: {}",
        error_msg
    );

    Ok(())
}

#[test]
fn test_partition_filtering_binary_collation() -> Result<(), Box<dyn std::error::Error>> {
    // Test that UTF8_BINARY collation uses exact matching (case-sensitive)
    //
    // Query: name = 'apple' with UTF8_BINARY (default, no collation specified)
    // Should only match the exact 'apple' partition, not 'Apple' or 'APPLE'
    //
    // Expected: Only 1 row from 'apple' partition

    let (engine, snapshot) = setup_partitioned_table()?;

    // Query: name = 'apple' with UTF8_BINARY (default)
    let predicate = column_expr!("name").eq(Expr::literal("apple"));

    let num_rows = count_rows_with_predicate(snapshot.clone(), engine.clone(), predicate)?;

    // Should only find rows from 'apple' partition (case-sensitive)
    // Expected: 1 row (only exact match 'apple')
    assert_eq!(
        num_rows, 1,
        "Expected 1 row (only 'apple' partition with binary collation), got {}",
        num_rows
    );

    Ok(())
}
