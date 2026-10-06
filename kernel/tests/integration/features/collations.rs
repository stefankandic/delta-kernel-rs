//! Collation table-feature passthrough integration tests.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use delta_kernel::arrow::array::{Array as _, ArrayRef, Int32Array, StringArray, StructArray};
use delta_kernel::arrow::datatypes::{
    DataType as ArrowDataType, Field as ArrowField, Schema as ArrowSchema,
};
use delta_kernel::arrow::record_batch::RecordBatch;
use delta_kernel::committer::FileSystemCommitter;
use delta_kernel::expressions::{col, lit};
use delta_kernel::schema::{
    schema_ref, ArrayType, ColumnMetadataKey, DataType, MapType, MetadataValue, StructField,
};
use delta_kernel::snapshot::{Snapshot, SnapshotRef};
use delta_kernel::table_features::TableFeature;
use delta_kernel::transaction::create_table::create_table;
use delta_kernel::DeltaResult;
use rstest::rstest;
use serde_json::{Deserializer, Map, Value};
use test_utils::delta_kernel_default_engine::executor::TaskExecutor;
use test_utils::delta_kernel_default_engine::DefaultEngine;
use test_utils::{read_add_infos, read_scan, test_table_setup, write_batch_to_table};

const COLLATION_NAME: &str = "spark.UTF8_LCASE";

fn collation_metadata(path: &str) -> MetadataValue {
    MetadataValue::Other(Value::Object(Map::from_iter([(
        path.to_string(),
        Value::String(COLLATION_NAME.to_string()),
    )])))
}

fn collated_field(name: &str) -> StructField {
    StructField::nullable(name, DataType::STRING).with_metadata([(
        ColumnMetadataKey::Collations.as_ref(),
        collation_metadata(name),
    )])
}

fn committer() -> Box<FileSystemCommitter> {
    Box::new(FileSystemCommitter::new())
}

fn assert_field_collation(field: &StructField, expected: &MetadataValue) {
    assert_eq!(
        field.get_config_value(&ColumnMetadataKey::Collations),
        Some(expected),
    );
}

fn assert_arrow_field_collation(field: &ArrowField, expected: &str) {
    assert_eq!(
        field
            .metadata()
            .get(ColumnMetadataKey::Collations.as_ref())
            .map(String::as_str),
        Some(expected),
    );
}

fn inject_collation_stats(
    table_path: &str,
    version: u64,
    stats_with_collation: Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let commit_path = Path::new(table_path).join(format!("_delta_log/{version:020}.json"));
    let mut actions = Deserializer::from_reader(File::open(&commit_path)?)
        .into_iter::<Value>()
        .collect::<Result<Vec<_>, _>>()?;
    let mut add_count = 0;
    for action in &mut actions {
        if let Some(add) = action.get_mut("add") {
            let mut stats: Value = serde_json::from_str(add["stats"].as_str().unwrap())?;
            assert!(stats.get("statsWithCollation").is_none());
            stats["statsWithCollation"] = stats_with_collation.clone();
            add["stats"] = Value::String(serde_json::to_string(&stats)?);
            add_count += 1;
        }
    }
    assert_eq!(add_count, 1);
    let mut file = File::create(commit_path)?;
    for action in actions {
        serde_json::to_writer(&mut file, &action)?;
        file.write_all(b"\n")?;
    }
    Ok(())
}

async fn create_collation_table(
    table_path: &str,
    engine: &DefaultEngine<impl TaskExecutor>,
    nested: bool,
) -> Result<SnapshotRef, Box<dyn std::error::Error>> {
    let (schema, files, collation_version) = if nested {
        (
            schema_ref! {
                nullable "id": INTEGER,
                nullable "name": {
                    (collated_field("first")),
                    (collated_field("last")),
                },
                (collated_field("email")),
                nullable "department": STRING,
            },
            vec![
                (
                    complex_batch(&[
                        (
                            1,
                            "Alice",
                            "Johnson",
                            "Alice.Johnson@example.com",
                            "Engineering",
                        ),
                        (2, "Bob", "Smith", "bob.smith@example.com", "Marketing"),
                        (3, "Charlie", "Brown", "charlie@example.com", "Engineering"),
                    ]),
                    serde_json::json!({
                        "minValues": {
                            "name": { "first": "Alice", "last": "Brown" },
                            "email": "Alice.Johnson@example.com",
                        },
                        "maxValues": {
                            "name": { "first": "Charlie", "last": "Smith" },
                            "email": "charlie@example.com",
                        },
                    }),
                ),
                (
                    complex_batch(&[
                        (
                            4,
                            "alice",
                            "johnson",
                            "alice.johnson@example.com",
                            "Engineering",
                        ),
                        (5, "bob", "dylan", "Bob.Dylan@Example.Com", "Sales"),
                    ]),
                    serde_json::json!({
                        "minValues": {
                            "name": { "first": "alice", "last": "dylan" },
                            "email": "alice.johnson@example.com",
                        },
                        "maxValues": {
                            "name": { "first": "bob", "last": "johnson" },
                            "email": "Bob.Dylan@Example.Com",
                        },
                    }),
                ),
            ],
            "spark.UTF8_LCASE.75.1",
        )
    } else {
        (
            schema_ref! {
                nullable "id": INTEGER,
                (StructField::nullable("name", DataType::STRING).with_metadata([(
                    ColumnMetadataKey::Collations.as_ref(),
                    MetadataValue::Other(serde_json::json!({ "name": "icu.UNICODE_CI" })),
                )])),
            },
            [(1, "Müller"), (2, "MÜLLER"), (3, "müller")]
                .into_iter()
                .map(|(id, name)| {
                    (
                        simple_batch(vec![id], vec![name]),
                        serde_json::json!({
                            "minValues": { "name": name },
                            "maxValues": { "name": name },
                        }),
                    )
                })
                .collect(),
            "icu.UNICODE_CI.75.1",
        )
    };
    let mut snapshot = create_table(table_path, schema, "test")
        .with_table_properties([
            ("delta.enableDeletionVectors", "true"),
            ("delta.feature.invariants", "supported"),
            ("delta.feature.appendOnly", "supported"),
        ])
        .build(engine, committer())?
        .commit(engine)?
        .unwrap_post_commit_snapshot();
    for (batch, bounds) in files {
        snapshot = write_batch_to_table(&snapshot, engine, batch, HashMap::new()).await?;
        inject_collation_stats(
            table_path,
            snapshot.version(),
            serde_json::json!({ collation_version: bounds }),
        )?;
        snapshot = Snapshot::builder_for(table_path).build(engine)?;
    }
    Ok(snapshot)
}

fn simple_rows(batches: &[RecordBatch]) -> Vec<(i32, String)> {
    let mut rows = Vec::new();
    for batch in batches {
        let ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        let names = batch
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for row in 0..batch.num_rows() {
            rows.push((ids.value(row), names.value(row).to_string()));
        }
    }
    rows.sort();
    rows
}

fn nested_first_values(batches: &[RecordBatch]) -> Vec<String> {
    let mut values = Vec::new();
    for batch in batches {
        let names = batch
            .column(1)
            .as_any()
            .downcast_ref::<StructArray>()
            .unwrap();
        let first = names
            .column_by_name("first")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        values.extend(first.iter().flatten().map(str::to_string));
    }
    values.sort();
    values
}

fn simple_batch(ids: Vec<i32>, names: Vec<&str>) -> RecordBatch {
    RecordBatch::try_new(
        Arc::new(ArrowSchema::new(vec![
            ArrowField::new("id", ArrowDataType::Int32, true),
            ArrowField::new("name", ArrowDataType::Utf8, true),
        ])),
        vec![
            Arc::new(Int32Array::from(ids)),
            Arc::new(StringArray::from(names)),
        ],
    )
    .unwrap()
}

fn complex_batch(rows: &[(i32, &str, &str, &str, &str)]) -> RecordBatch {
    let name = StructArray::from(vec![
        (
            Arc::new(ArrowField::new("first", ArrowDataType::Utf8, true)),
            Arc::new(StringArray::from_iter_values(rows.iter().map(|row| row.1))) as ArrayRef,
        ),
        (
            Arc::new(ArrowField::new("last", ArrowDataType::Utf8, true)),
            Arc::new(StringArray::from_iter_values(rows.iter().map(|row| row.2))) as ArrayRef,
        ),
    ]);
    RecordBatch::try_new(
        Arc::new(ArrowSchema::new(vec![
            ArrowField::new("id", ArrowDataType::Int32, true),
            ArrowField::new("name", name.data_type().clone(), true),
            ArrowField::new("email", ArrowDataType::Utf8, true),
            ArrowField::new("department", ArrowDataType::Utf8, true),
        ])),
        vec![
            Arc::new(Int32Array::from_iter_values(rows.iter().map(|row| row.0))),
            Arc::new(name),
            Arc::new(StringArray::from_iter_values(rows.iter().map(|row| row.3))),
            Arc::new(StringArray::from_iter_values(rows.iter().map(|row| row.4))),
        ],
    )
    .unwrap()
}

fn simple_append_batch() -> RecordBatch {
    simple_batch(vec![4], vec!["test"])
}

fn complex_append_batch() -> RecordBatch {
    complex_batch(&[(6, "Test", "User", "test.user@example.com", "Engineering")])
}

#[tokio::test]
async fn reads_collation_table_with_binary_predicate_semantics(
) -> Result<(), Box<dyn std::error::Error>> {
    let (_temp_dir, table_path, engine) = test_table_setup()?;
    let snapshot = create_collation_table(&table_path, engine.as_ref(), false).await?;

    let table_config = snapshot.table_configuration();
    assert!(table_config.is_feature_supported(&TableFeature::Collations));
    assert!(!table_config.is_feature_supported(&TableFeature::CollationsPreview));
    assert!(table_config.is_feature_supported(&TableFeature::DomainMetadata));
    assert_field_collation(
        snapshot.schema().field("name").unwrap(),
        &MetadataValue::Other(serde_json::json!({
            "name": "icu.UNICODE_CI"
        })),
    );

    let batches = read_scan(&snapshot.clone().scan_builder().build()?, engine.clone())?;
    let add_infos = read_add_infos(&snapshot, engine.as_ref())?;
    assert_eq!(add_infos.len(), 3);
    for add in &add_infos {
        let stats = add.stats.as_ref().unwrap();
        assert_eq!(stats["numRecords"], 1);
        assert!(stats["statsWithCollation"]["icu.UNICODE_CI.75.1"].is_object());
    }
    assert_eq!(
        simple_rows(&batches),
        vec![
            (1, "Müller".to_string()),
            (2, "MÜLLER".to_string()),
            (3, "müller".to_string()),
        ]
    );
    for batch in &batches {
        assert_arrow_field_collation(batch.schema().field(1), r#"{"name":"icu.UNICODE_CI"}"#);
    }

    let scan = snapshot
        .scan_builder()
        .with_predicate(Arc::new(col!("name").eq(lit("Müller"))))
        .build()?;
    assert_eq!(
        simple_rows(&read_scan(&scan, engine)?),
        vec![(1, "Müller".to_string())]
    );
    Ok(())
}

#[tokio::test]
async fn reads_nested_collations_with_binary_predicate_semantics(
) -> Result<(), Box<dyn std::error::Error>> {
    let (_temp_dir, table_path, engine) = test_table_setup()?;
    let snapshot = create_collation_table(&table_path, engine.as_ref(), true).await?;
    let schema = snapshot.schema();
    let DataType::Struct(name) = schema.field("name").unwrap().data_type() else {
        panic!("name must be a struct")
    };
    assert_field_collation(name.field("first").unwrap(), &collation_metadata("first"));
    assert_field_collation(name.field("last").unwrap(), &collation_metadata("last"));
    assert_field_collation(schema.field("email").unwrap(), &collation_metadata("email"));

    let batches = read_scan(&snapshot.clone().scan_builder().build()?, engine.clone())?;
    let add_infos = read_add_infos(&snapshot, engine.as_ref())?;
    assert_eq!(add_infos.len(), 2);
    for add in &add_infos {
        let stats = add.stats.as_ref().unwrap();
        assert!(stats["statsWithCollation"]["spark.UTF8_LCASE.75.1"].is_object());
        if stats["numRecords"] == 2 {
            assert_eq!(stats["minValues"]["email"], "Bob.Dylan@Example.Com");
            assert_eq!(stats["maxValues"]["email"], "alice.johnson@example.com");
            let collation_stats = &stats["statsWithCollation"]["spark.UTF8_LCASE.75.1"];
            assert_eq!(
                collation_stats["minValues"]["email"],
                "alice.johnson@example.com"
            );
            assert_eq!(
                collation_stats["maxValues"]["email"],
                "Bob.Dylan@Example.Com"
            );
        } else {
            assert_eq!(stats["numRecords"], 3);
        }
    }
    assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 5);
    assert_eq!(
        nested_first_values(&batches),
        vec![
            "Alice".to_string(),
            "Bob".to_string(),
            "Charlie".to_string(),
            "alice".to_string(),
            "bob".to_string(),
        ]
    );
    for batch in &batches {
        let batch_schema = batch.schema();
        let ArrowDataType::Struct(name_fields) = batch_schema.field(1).data_type() else {
            panic!("name must be a struct")
        };
        for (field_name, expected) in [
            ("first", r#"{"first":"spark.UTF8_LCASE"}"#),
            ("last", r#"{"last":"spark.UTF8_LCASE"}"#),
        ] {
            assert_arrow_field_collation(
                name_fields
                    .iter()
                    .find(|field| field.name() == field_name)
                    .unwrap(),
                expected,
            );
        }
        assert_arrow_field_collation(batch_schema.field(2), r#"{"email":"spark.UTF8_LCASE"}"#);
    }
    let scan = snapshot
        .clone()
        .scan_builder()
        .with_predicate(Arc::new(col!("name.first").eq(lit("charlie"))))
        .build()?;
    assert_eq!(
        read_scan(&scan, engine.clone())?
            .iter()
            .map(RecordBatch::num_rows)
            .sum::<usize>(),
        0
    );
    let scan = snapshot
        .scan_builder()
        .with_predicate(Arc::new(col!("name.first").eq(lit("alice"))))
        .build()?;
    assert_eq!(
        nested_first_values(&read_scan(&scan, engine)?),
        vec!["alice".to_string(), "bob".to_string()]
    );
    Ok(())
}

#[rstest]
#[case::simple(false, 3, simple_append_batch)]
#[case::nested(true, 5, complex_append_batch)]
#[tokio::test]
async fn appends_to_collation_table_without_changing_schema_or_protocol(
    #[case] nested: bool,
    #[case] initial_rows: usize,
    #[case] batch: fn() -> RecordBatch,
) -> Result<(), Box<dyn std::error::Error>> {
    let (_temp_dir, table_path, engine) = test_table_setup()?;
    let snapshot = create_collation_table(&table_path, engine.as_ref(), nested).await?;
    let initial_version = snapshot.version();
    let initial_protocol = snapshot.table_configuration().protocol().clone();
    let initial_schema = snapshot.schema();

    write_batch_to_table(&snapshot, engine.as_ref(), batch(), HashMap::new()).await?;

    let reloaded = Snapshot::builder_for(&table_path).build(engine.as_ref())?;
    assert_eq!(reloaded.version(), initial_version + 1);
    assert_eq!(reloaded.table_configuration().protocol(), &initial_protocol);
    assert_eq!(reloaded.schema().as_ref(), initial_schema.as_ref());
    assert_eq!(
        read_scan(&reloaded.clone().scan_builder().build()?, engine)?
            .iter()
            .map(RecordBatch::num_rows)
            .sum::<usize>(),
        initial_rows + 1
    );
    Ok(())
}

#[rstest]
#[case::stable(None, TableFeature::Collations, TableFeature::CollationsPreview)]
#[case::stable_signal(
    Some("collations"),
    TableFeature::Collations,
    TableFeature::CollationsPreview
)]
#[case::preview(
    Some("collations-preview"),
    TableFeature::CollationsPreview,
    TableFeature::Collations
)]
#[tokio::test]
async fn collations_read_write_passthrough(
    #[case] feature_signal: Option<&str>,
    #[case] expected_feature: TableFeature,
    #[case] absent_feature: TableFeature,
) -> Result<(), Box<dyn std::error::Error>> {
    let (_temp_dir, table_path, engine) = test_table_setup()?;
    let expected_metadata = collation_metadata("value");
    let schema = schema_ref! { (collated_field("value")) };
    let mut builder = create_table(&table_path, schema, "test");
    if let Some(feature) = feature_signal {
        builder = builder
            .with_table_properties([(format!("delta.feature.{feature}"), "supported".to_string())]);
    }
    let snapshot = builder
        .build(engine.as_ref(), committer())?
        .commit(engine.as_ref())?
        .unwrap_post_commit_snapshot();

    let table_config = snapshot.table_configuration();
    assert!(table_config.is_feature_supported(&expected_feature));
    assert!(!table_config.is_feature_supported(&absent_feature));
    assert!(table_config.is_feature_supported(&TableFeature::DomainMetadata));
    assert_eq!(table_config.protocol().writer_features().unwrap().len(), 2);
    assert_field_collation(
        snapshot.schema().field("value").unwrap(),
        &expected_metadata,
    );

    let input_schema = Arc::new(ArrowSchema::new(vec![ArrowField::new(
        "value",
        ArrowDataType::Utf8,
        true,
    )]));
    let batch = RecordBatch::try_new(
        input_schema,
        vec![Arc::new(StringArray::from(vec!["a", "A"]))],
    )?;
    let initial_protocol = snapshot.table_configuration().protocol().clone();
    let snapshot = write_batch_to_table(&snapshot, engine.as_ref(), batch, HashMap::new()).await?;

    let reloaded = Snapshot::builder_for(&table_path).build(engine.as_ref())?;
    assert_eq!(reloaded.version(), snapshot.version());
    assert_eq!(reloaded.table_configuration().protocol(), &initial_protocol);
    assert_field_collation(
        reloaded.schema().field("value").unwrap(),
        &expected_metadata,
    );

    let batches = read_scan(&reloaded.clone().scan_builder().build()?, engine.clone())?;
    assert_eq!(batches.len(), 1);
    let values = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(
        values.iter().collect::<Vec<_>>(),
        vec![Some("a"), Some("A")]
    );
    assert_arrow_field_collation(
        batches[0].schema().field(0),
        r#"{"value":"spark.UTF8_LCASE"}"#,
    );

    let add_infos = read_add_infos(&reloaded, engine.as_ref())?;
    let stats = add_infos[0].stats.as_ref().unwrap();
    assert_eq!(stats["minValues"]["value"], "A");
    assert_eq!(stats["maxValues"]["value"], "a");
    assert!(stats.get("statsWithCollation").is_none());
    Ok(())
}

#[tokio::test]
async fn create_preserves_container_collation_metadata() -> DeltaResult<()> {
    let (_temp_dir, table_path, engine) = test_table_setup()?;
    let array_metadata = collation_metadata("array_value.element");
    let map_metadata = collation_metadata("map_value.value");
    let schema = schema_ref! {
        (StructField::nullable(
            "array_value",
            ArrayType::new(DataType::STRING, true),
        )
        .with_metadata([(
            ColumnMetadataKey::Collations.as_ref(),
            array_metadata.clone(),
        )])),
        (StructField::nullable(
            "map_value",
            MapType::new(DataType::STRING, DataType::STRING, true),
        )
        .with_metadata([(
            ColumnMetadataKey::Collations.as_ref(),
            map_metadata.clone(),
        )])),
    };
    let snapshot = create_table(&table_path, schema, "test")
        .build(engine.as_ref(), committer())?
        .commit(engine.as_ref())?
        .unwrap_post_commit_snapshot();

    assert!(snapshot
        .table_configuration()
        .is_feature_supported(&TableFeature::Collations));
    assert!(snapshot
        .table_configuration()
        .is_feature_supported(&TableFeature::DomainMetadata));
    assert_eq!(
        snapshot
            .table_configuration()
            .protocol()
            .writer_features()
            .unwrap()
            .len(),
        2
    );
    assert_field_collation(
        snapshot.schema().field("array_value").unwrap(),
        &array_metadata,
    );
    assert_field_collation(snapshot.schema().field("map_value").unwrap(), &map_metadata);
    Ok(())
}
