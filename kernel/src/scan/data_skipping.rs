use std::borrow::Cow;
use std::cmp::Ordering;
use std::sync::{Arc, LazyLock};

use tracing::{debug, error};

use crate::actions::get_log_add_schema;
use crate::actions::visitors::SelectionVectorVisitor;
use crate::error::DeltaResult;
use crate::expressions::{
    column_expr, joined_column_expr, BinaryPredicateOp, ColumnName, Expression as Expr,
    ExpressionRef, JunctionPredicateOp, OpaquePredicateOpRef, Predicate as Pred, PredicateRef,
    Scalar,
};
use crate::kernel_predicates::{
    DataSkippingPredicateEvaluator, KernelPredicateEvaluator, KernelPredicateEvaluatorDefaults,
};
use crate::schema::{DataType, MapType, PrimitiveType, SchemaRef, SchemaTransform, StructField, StructType};
use crate::{
    Engine, EngineData, ExpressionEvaluator, JsonHandler, PredicateEvaluator, RowVisitor as _,
};

#[cfg(test)]
mod tests;

/// Rewrites a predicate to a predicate that can be used to skip files based on their stats.
/// Returns `None` if the predicate is not eligible for data skipping.
///
/// We normalize each binary operation to a comparison between a column and a literal value and
/// rewrite that in terms of the min/max values of the column.
/// For example, `1 < a` is rewritten as `minValues.a > 1`.
///
/// For Unary `Not`, we push the Not down using De Morgan's Laws to invert everything below the Not.
///
/// Unary `IsNull` checks if the null counts indicate that the column could contain a null.
///
/// The junction operations are rewritten as follows:
/// - `AND` is rewritten as a conjunction of the rewritten operands where we just skip operands that
///   are not eligible for data skipping.
/// - `OR` is rewritten only if all operands are eligible for data skipping. Otherwise, the whole OR
///   predicate is dropped.
#[cfg(test)]
pub(crate) fn as_data_skipping_predicate(pred: &Pred) -> Option<Pred> {
    DataSkippingPredicateCreator::default().eval(pred)
}

/// Like `as_data_skipping_predicate`, but invokes [`KernelPredicateEvaluator::eval_sql_where`]
/// instead of [`KernelPredicateEvaluator::eval`].
fn as_sql_data_skipping_predicate(pred: &Pred) -> Option<Pred> {
    DataSkippingPredicateCreator::default().eval_sql_where(pred)
}

pub(crate) struct DataSkippingFilter {
    stats_schema: SchemaRef,
    select_stats_evaluator: Arc<dyn ExpressionEvaluator>,
    skipping_evaluator: Arc<dyn PredicateEvaluator>,
    filter_evaluator: Arc<dyn PredicateEvaluator>,
    json_handler: Arc<dyn JsonHandler>,
}

impl DataSkippingFilter {
    /// Creates a new data skipping filter. Returns None if there is no predicate, or the predicate
    /// is ineligible for data skipping.
    ///
    /// NOTE: None is equivalent to a trivial filter that always returns TRUE (= keeps all files),
    /// but using an Option lets the engine easily avoid the overhead of applying trivial filters.
    pub(crate) fn new(
        engine: &dyn Engine,
        physical_predicate: Option<(PredicateRef, SchemaRef)>,
    ) -> Option<Self> {
        static STATS_EXPR: LazyLock<ExpressionRef> =
            LazyLock::new(|| Arc::new(column_expr!("add.stats")));
        static FILTER_PRED: LazyLock<PredicateRef> =
            LazyLock::new(|| Arc::new(column_expr!("output").distinct(Expr::literal(false))));

        let (predicate, referenced_schema) = physical_predicate?;
        debug!("Creating a data skipping filter for {:#?}", predicate);

        // Convert all fields into nullable, as stats may not be available for all columns
        // (and usually aren't for partition columns).
        struct NullableStatsTransform;
        impl<'a> SchemaTransform<'a> for NullableStatsTransform {
            fn transform_struct_field(
                &mut self,
                field: &'a StructField,
            ) -> Option<Cow<'a, StructField>> {
                use Cow::*;
                let field = match self.transform(&field.data_type)? {
                    Borrowed(_) if field.is_nullable() => Borrowed(field),
                    data_type => Owned(StructField {
                        name: field.name.clone(),
                        data_type: data_type.into_owned(),
                        nullable: true,
                        metadata: field.metadata.clone(),
                    }),
                };
                Some(field)
            }
        }

        // Convert a min/max stats schema into a nullcount schema (all leaf fields are LONG)
        struct NullCountStatsTransform;
        impl<'a> SchemaTransform<'a> for NullCountStatsTransform {
            fn transform_primitive(
                &mut self,
                _ptype: &'a PrimitiveType,
            ) -> Option<Cow<'a, PrimitiveType>> {
                Some(Cow::Owned(PrimitiveType::Long))
            }
        }

        let stats_schema = NullableStatsTransform
            .transform_struct(&referenced_schema)?
            .into_owned();

        let nullcount_schema = NullCountStatsTransform
            .transform_struct(&stats_schema)?
            .into_owned();

        let stats_with_collation = DataType::Map(Box::new(MapType::new(
            DataType::STRING, // key: "icu.UNICODE.75.1"
            DataType::Struct(Box::new(StructType::new_unchecked([
                StructField::nullable("minValues", stats_schema.clone()),
                StructField::nullable("maxValues", stats_schema.clone()),
            ]))),
            true, // value contains nulls
        )));


        let stats_schema = Arc::new(StructType::new_unchecked([
            StructField::nullable("numRecords", DataType::LONG),
            StructField::nullable("nullCount", nullcount_schema),
            StructField::nullable("minValues", stats_schema.clone()),
            StructField::nullable("maxValues", stats_schema),
            StructField::nullable("statsWithCollation", stats_with_collation),
        ]));

        // Skipping happens in several steps:
        //
        // 1. The stats selector fetches add.stats from the metadata
        //
        // 2. The predicate (skipping evaluator) produces false for any file whose stats prove we
        //    can safely skip it. A value of true means the stats say we must keep the file, and
        //    null means we could not determine whether the file is safe to skip, because its stats
        //    were missing/null.
        //
        // 3. The selection evaluator does DISTINCT(col(predicate), 'false') to produce true (= keep) when
        //    the predicate is true/null and false (= skip) when the predicate is false.
        let select_stats_evaluator = engine
            .evaluation_handler()
            .new_expression_evaluator(
                get_log_add_schema().clone(),
                STATS_EXPR.clone(),
                DataType::STRING,
            )
            // A stats expression failure here doesn't affect correctness
            // as its a performance optimization so we log the error and continue.
            .inspect_err(|e| error!("Failed to create select stats evaluator: {e}"))
            .ok()?;

        let skipping_evaluator = engine
            .evaluation_handler()
            .new_predicate_evaluator(
                stats_schema.clone(),
                Arc::new(as_sql_data_skipping_predicate(&predicate)?),
            )
            // A skipping predicate expression failure here doesn't affect correctness
            // as its a performance optimization so we log the error and continue.
            .inspect_err(|e| error!("Failed to create skipping evaluator: {e}"))
            .ok()?;

        let filter_evaluator = engine
            .evaluation_handler()
            .new_predicate_evaluator(stats_schema.clone(), FILTER_PRED.clone())
            // A filter predicate expression failure here doesn't affect correctness
            // as its a performance optimization so we log the error and continue.
            .inspect_err(|e| error!("Failed to create filter evaluator: {e}"))
            .ok()?;

        Some(Self {
            stats_schema,
            select_stats_evaluator,
            skipping_evaluator,
            filter_evaluator,
            json_handler: engine.json_handler(),
        })
    }

    /// Apply the DataSkippingFilter to an EngineData batch of actions. Returns a selection vector
    /// which can be applied to the actions to find those that passed data skipping.
    pub(crate) fn apply(&self, actions: &dyn EngineData) -> DeltaResult<Vec<bool>> {
        // retrieve and parse stats from actions data
        let stats = self.select_stats_evaluator.evaluate(actions)?;
        assert_eq!(stats.len(), actions.len());
        let parsed_stats = self
            .json_handler
            .parse_json(stats, self.stats_schema.clone())?;
        assert_eq!(parsed_stats.len(), actions.len());

        // evaluate the predicate on the parsed stats, then convert to selection vector
        let skipping_predicate = self.skipping_evaluator.evaluate(&*parsed_stats)?;
        assert_eq!(skipping_predicate.len(), actions.len());
        let selection_vector = self
            .filter_evaluator
            .evaluate(skipping_predicate.as_ref())?;
        assert_eq!(selection_vector.len(), actions.len());

        // visit the engine's selection vector to produce a Vec<bool>
        let mut visitor = SelectionVectorVisitor::default();
        visitor.visit_rows_of(selection_vector.as_ref())?;
        Ok(visitor.selection_vector)

        // TODO(zach): add some debug info about data skipping that occurred
        // let before_count = actions.length();
        // debug!(
        //     "number of actions before/after data skipping: {before_count} / {}",
        //     filtered_actions.num_rows()
        // );
    }
}

struct DataSkippingPredicateCreator {
    /// Current expression context for the predicate being evaluated.
    /// Used to determine if collation-aware stats should be used.
    current_context: std::cell::RefCell<Option<crate::expressions::ExprContext>>,
}

impl Default for DataSkippingPredicateCreator {
    fn default() -> Self {
        Self {
            current_context: std::cell::RefCell::new(None),
        }
    }
}

/// Type of statistics to retrieve (min or max values).
enum StatType {
    Min,
    Max,
}

impl DataSkippingPredicateCreator {
    /// Helper to get stats for string columns, handling collation if present.
    ///
    /// Returns:
    /// - `Some(expr)`: Use this stat expression (collation-aware or default)
    /// - `None`: Collation version mismatch, drop the predicate
    fn get_string_stat(&self, col: &ColumnName, stat_type: StatType) -> Option<Expr> {
        let context = self.current_context.borrow();

        if let Some(ref ctx) = *context {
            if let Some(ref collation) = ctx.collation {
                // Check version compatibility first (data skipping only)
                if !collation.is_version_compatible() {
                    // Version mismatch - drop this predicate for data skipping
                    return None;
                }

                if collation.requires_stats_with_collation() {
                    let stats_key = collation.to_stats_key().ok()?;
                    // Use element_at to access the map: statsWithCollation[stats_key]
                    let map_element = Expr::element_at(
                        Expr::column(["statsWithCollation"]),
                        Expr::literal(stats_key),
                    );
                    // Then access minValues.col or maxValues.col within the struct
                    let stat_field = match stat_type {
                        StatType::Min => "minValues",
                        StatType::Max => "maxValues",
                    };
                    let mut fields = vec![stat_field.to_string()];
                    fields.extend(col.as_ref().iter().cloned());
                    return Some(Expr::struct_field(map_element, fields));
                }
            }
        }

        // Default: UTF8_BINARY or no collation context
        match stat_type {
            StatType::Min => Some(joined_column_expr!("minValues", col)),
            StatType::Max => Some(joined_column_expr!("maxValues", col)),
        }
    }
}

impl DataSkippingPredicateEvaluator for DataSkippingPredicateCreator {
    type Output = Pred;
    type ColumnStat = Expr;

    /// Retrieves the minimum value of a column, if it exists and has the requested type.
    fn get_min_stat(&self, col: &ColumnName, data_type: &DataType) -> Option<Expr> {
        match data_type {
            &DataType::STRING => self.get_string_stat(col, StatType::Min),
            _ => Some(joined_column_expr!("minValues", col)),
        }
    }

    /// Retrieves the maximum value of a column, if it exists and has the requested type.
    // TODO(#1002): we currently don't support file skipping on timestamp columns' max stat since
    // they are truncated to milliseconds in add.stats.
    fn get_max_stat(&self, col: &ColumnName, data_type: &DataType) -> Option<Expr> {
        match data_type {
            &DataType::TIMESTAMP | &DataType::TIMESTAMP_NTZ => None,
            &DataType::STRING => self.get_string_stat(col, StatType::Max),
            _ => Some(joined_column_expr!("maxValues", col)),
        }
    }

    /// Retrieves the null count of a column, if it exists.
    fn get_nullcount_stat(&self, col: &ColumnName) -> Option<Expr> {
        Some(joined_column_expr!("nullCount", col))
    }

    /// Retrieves the row count of a column (parquet footers always include this stat).
    fn get_rowcount_stat(&self) -> Option<Expr> {
        Some(column_expr!("numRecords"))
    }

    fn eval_partial_cmp(
        &self,
        ord: Ordering,
        col: Expr,
        val: &Scalar,
        inverted: bool,
    ) -> Option<Pred> {
        let pred_fn = match (ord, inverted) {
            (Ordering::Less, false) => Pred::lt,
            (Ordering::Less, true) => Pred::ge,
            (Ordering::Equal, false) => Pred::eq,
            (Ordering::Equal, true) => Pred::ne,
            (Ordering::Greater, false) => Pred::gt,
            (Ordering::Greater, true) => Pred::le,
        };

        let predicate = pred_fn(col, val.clone());

        // Attach collation if present
        let context = self.current_context.borrow();
        Some(match context.as_ref().and_then(|ctx| ctx.collation.as_ref()) {
            Some(collation) => predicate.with_collation(collation.clone()),
            None => predicate,
        })
    }

    fn eval_pred_scalar(&self, val: &Scalar, inverted: bool) -> Option<Pred> {
        KernelPredicateEvaluatorDefaults::eval_pred_scalar(val, inverted).map(Pred::literal)
    }

    fn eval_pred_scalar_is_null(&self, val: &Scalar, inverted: bool) -> Option<Pred> {
        KernelPredicateEvaluatorDefaults::eval_pred_scalar_is_null(val, inverted).map(Pred::literal)
    }

    // NOTE: This is nearly identical to the impl for ParquetStatsProvider in
    // parquet_stats_skipping.rs, except it uses `Expression` and `Predicate` instead of `Scalar`.
    fn eval_pred_is_null(&self, col: &ColumnName, inverted: bool) -> Option<Pred> {
        let safe_to_skip = match inverted {
            true => self.get_rowcount_stat()?, // all-null
            false => Expr::literal(0i64),      // no-null
        };
        Some(Pred::ne(self.get_nullcount_stat(col)?, safe_to_skip))
    }

    fn eval_pred_binary_scalars(
        &self,
        op: BinaryPredicateOp,
        left: &Scalar,
        right: &Scalar,
        context: Option<&crate::expressions::ExprContext>,
        inverted: bool,
    ) -> Option<Pred> {
        KernelPredicateEvaluatorDefaults::eval_pred_binary_scalars(op, left, right, context, inverted)
            .map(Pred::literal)
    }

    /// Override to capture context for collation-aware stats routing.
    /// This enables get_min_stat/get_max_stat to use statsWithCollation for non-binary collations.
    fn eval_pred_lt(
        &self,
        col: &ColumnName,
        val: &Scalar,
        context: Option<&crate::expressions::ExprContext>,
        inverted: bool,
    ) -> Option<Pred> {
        // Store context for get_min_stat/get_max_stat to use
        *self.current_context.borrow_mut() = context.cloned();

        // Call the default implementation (from DataSkippingPredicateEvaluator trait)
        // which will now use our updated get_min_stat/get_max_stat
        if inverted {
            self.partial_cmp_max_stat(col, val, Ordering::Less, true)
        } else {
            self.partial_cmp_min_stat(col, val, Ordering::Less, false)
        }
    }

    /// Override to capture context for collation-aware stats routing.
    fn eval_pred_gt(
        &self,
        col: &ColumnName,
        val: &Scalar,
        context: Option<&crate::expressions::ExprContext>,
        inverted: bool,
    ) -> Option<Pred> {
        // Store context for get_min_stat/get_max_stat to use
        *self.current_context.borrow_mut() = context.cloned();

        // Call the default implementation (from DataSkippingPredicateEvaluator trait)
        if inverted {
            self.partial_cmp_min_stat(col, val, Ordering::Greater, true)
        } else {
            self.partial_cmp_max_stat(col, val, Ordering::Greater, false)
        }
    }

    /// Override to capture context for collation-aware stats routing.
    fn eval_pred_eq(
        &self,
        col: &ColumnName,
        val: &Scalar,
        context: Option<&crate::expressions::ExprContext>,
        inverted: bool,
    ) -> Option<Pred> {
        // Store context for get_min_stat/get_max_stat to use
        *self.current_context.borrow_mut() = context.cloned();

        // Use the same logic as the default implementation
        let (op, preds) = if inverted {
            // Column could compare not-equal if min or max value differs from the literal.
            let preds = [
                self.partial_cmp_min_stat(col, val, Ordering::Equal, true),
                self.partial_cmp_max_stat(col, val, Ordering::Equal, true),
            ];
            (JunctionPredicateOp::Or, preds)
        } else {
            // Column could compare equal if its min/max values bracket the literal.
            let preds = [
                self.partial_cmp_min_stat(col, val, Ordering::Greater, true),
                self.partial_cmp_max_stat(col, val, Ordering::Less, true),
            ];
            (JunctionPredicateOp::And, preds)
        };
        DataSkippingPredicateEvaluator::finish_eval_pred_junction(self, op, &mut preds.into_iter(), false)
    }

    fn eval_pred_opaque(
        &self,
        op: &OpaquePredicateOpRef,
        exprs: &[Expr],
        inverted: bool,
    ) -> Option<Pred> {
        op.as_data_skipping_predicate(self, exprs, inverted)
    }

    fn finish_eval_pred_junction(
        &self,
        mut op: JunctionPredicateOp,
        preds: &mut dyn Iterator<Item = Option<Pred>>,
        inverted: bool,
    ) -> Option<Pred> {
        if inverted {
            op = op.invert();
        }
        // NOTE: We can potentially see a LOT of NULL inputs in a big WHERE clause with lots of
        // unsupported data skipping operations. We can't "just" flatten them all away for AND,
        // because that could produce TRUE where NULL would otherwise be expected. Similarly, we
        // don't want to "just" try_collect inputs for OR, because that can cause OR to produce NULL
        // where FALSE would otherwise be expected. So, we filter out all nulls except the first,
        // observing that one NULL is enough to produce the correct behavior during predicate eval.
        let mut keep_null = true;
        let preds: Vec<_> = preds
            .flat_map(|p| match p {
                Some(pred) => Some(pred),
                None => keep_null.then(|| {
                    keep_null = false;
                    Pred::null_literal()
                }),
            })
            .collect();
        Some(Pred::junction(op, preds))
    }
}
