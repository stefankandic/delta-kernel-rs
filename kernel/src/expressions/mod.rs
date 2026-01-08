//! Definitions and functions to create and manipulate kernel expressions

use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use itertools::Itertools;

pub use self::column_names::{
    column_expr, column_expr_ref, column_name, column_pred, joined_column_expr, joined_column_name,
    ColumnName,
};
pub use self::scalars::{ArrayData, DecimalData, MapData, Scalar, StructData};
use self::transforms::{ExpressionTransform as _, GetColumnReferences};
use crate::kernel_predicates::{
    DirectDataSkippingPredicateEvaluator, DirectPredicateEvaluator,
    IndirectDataSkippingPredicateEvaluator,
};
use crate::{DataType, DeltaResult, DynPartialEq};

mod column_names;
pub(crate) mod literal_expression_transform;
mod scalars;
pub mod transforms;

pub type ExpressionRef = std::sync::Arc<Expression>;
pub type PredicateRef = std::sync::Arc<Predicate>;

////////////////////////////////////////////////////////////////////////
// Operators
////////////////////////////////////////////////////////////////////////

/// A unary predicate operator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryPredicateOp {
    /// Unary Is Null
    IsNull,
}

/// A binary predicate operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryPredicateOp {
    /// Comparison Less Than
    LessThan,
    /// Comparison Greater Than
    GreaterThan,
    /// Comparison Equal
    Equal,
    /// Distinct
    Distinct,
    /// IN
    In,
}

/// A unary expression operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryExpressionOp {
    /// Convert struct data to JSON-encoded strings
    ToJson,
}

/// A binary expression operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryExpressionOp {
    /// Arithmetic Plus
    Plus,
    /// Arithmetic Minus
    Minus,
    /// Arithmetic Multiply
    Multiply,
    /// Arithmetic Divide
    Divide,
}

/// A variadic expression operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VariadicExpressionOp {
    /// Collapse multiple values into one by taking the first non-null value
    Coalesce,
    /// Access an element in a map by key: element_at(map, key)
    /// Takes exactly 2 arguments: the map expression and the key expression
    ElementAt,
    /// Access a field in a struct: struct_field(struct_expr, field_path)
    /// Takes 2+ arguments: the struct expression and one or more field names as string literals
    /// for nested access
    StructField,
}

/// A junction (AND/OR) predicate operator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JunctionPredicateOp {
    /// Conjunction
    And,
    /// Disjunction
    Or,
}

/// A kernel-supplied scalar expression evaluator which in particular can convert column references
/// (i.e. [`Expression::Column`]) to [`Scalar`] values. [`OpaqueExpressionOp::eval_expr_scalar`] and
/// [`OpaquePredicateOp::eval_pred_scalar`] rely on this evaluator.
///
/// If the evaluator produces `None`, it means kernel was unable to evaluate
/// the input expression. Otherwise, `Some(Scalar)` is the result of that evaluation (possibly
/// `Scalar::Null` if the output was NULL).
pub type ScalarExpressionEvaluator<'a> = dyn Fn(&Expression) -> Option<Scalar> + 'a;

/// An opaque expression operation (ie defined and implemented by the engine).
pub trait OpaqueExpressionOp: DynPartialEq + std::fmt::Debug {
    /// Succinctly identifies this op
    fn name(&self) -> &str;

    /// Attempts scalar evaluation of this opaque expression, e.g. for partition pruning.
    ///
    /// Implementations can evaluate the child expressions however they see fit, possibly by
    /// calling back to the provided [`ScalarExpressionEvaluator`],
    ///
    /// An output of `Err` indicates that this operation does not support scalar evaluation, or was
    /// invoked incorrectly (e.g. with the wrong number and/or types of arguments, None input,
    /// etc); the operation is disqualified from participating in partition pruning.
    ///
    /// `Ok(Scalar::Null)` means the operation actually produced a legitimately NULL result.
    fn eval_expr_scalar(
        &self,
        eval_expr: &ScalarExpressionEvaluator<'_>,
        exprs: &[Expression],
    ) -> DeltaResult<Scalar>;
}

/// An opaque predicate operation (ie defined and implemented by the engine).
pub trait OpaquePredicateOp: DynPartialEq + std::fmt::Debug {
    /// Succinctly identifies this op
    fn name(&self) -> &str;

    /// Attempts scalar evaluation of this (possibly inverted) opaque predicate on behalf of a
    /// [`DirectPredicateEvaluator`], e.g. for partition pruning or to evaluate an opaque data
    /// skipping predicate produced previously by an [`IndirectDataSkippingPredicateEvaluator`].
    ///
    /// Implementations can evaluate the child expressions however they see fit, possibly by calling
    /// back to the provided [`ScalarExpressionEvaluator`] and/or [`DirectPredicateEvaluator`].
    ///
    /// An output of `Err` indicates that this operation does not support scalar evaluation, or was
    /// invoked incorrectly (e.g. wrong number and/or types of arguments, None input, etc); the
    /// operation is disqualified from participating in partition pruning and/or data skipping.
    ///
    /// `Ok(None)` means the operation actually produced a legitimately NULL output.
    fn eval_pred_scalar(
        &self,
        eval_expr: &ScalarExpressionEvaluator<'_>,
        eval_pred: &DirectPredicateEvaluator<'_>,
        exprs: &[Expression],
        inverted: bool,
    ) -> DeltaResult<Option<bool>>;

    /// Evaluates this (possibly inverted) opaque predicate for data skipping on behalf of a
    /// [`DirectDataSkippingPredicateEvaluator`], e.g. for parquet row group skipping.
    ///
    /// Implementations can evaluate the child expressions however they see fit, possibly by
    /// calling back to the provided [`DirectDataSkippingPredicateEvaluator`].
    ///
    /// An output of `None` indicates that this operation does not support evaluation as a data
    /// skipping predicate, or was invoked incorrectly (e.g. wrong number and/or types of arguments,
    /// None input, etc.); the operation is disqualified from participating in row group skipping.
    fn eval_as_data_skipping_predicate(
        &self,
        evaluator: &DirectDataSkippingPredicateEvaluator<'_>,
        exprs: &[Expression],
        inverted: bool,
    ) -> Option<bool>;

    /// Converts this (possibly inverted) opaque predicate to a data skipping predicate on behalf of
    /// an [`IndirectDataSkippingPredicateEvaluator`], e.g. for stats-based file pruning.
    ///
    /// Implementations can transform the predicate and its child expressions however they see fit,
    /// possibly by calling back to the owning [`IndirectDataSkippingPredicateEvaluator`].
    ///
    /// An output of `None` indicates that this operation does not support conversion to a data
    /// skipping predicate, or was invoked incorrectly (e.g. wrong number and/or types of arguments,
    /// None input, etc.); the operation is disqualified from participating in file pruning.
    //
    // NOTE: It would be nicer if this method could accept an `Arc<Self>`, in case the data skipping
    // predicate rewrite can reuse the same operation. But sadly, that would not be dyn-compatible.
    fn as_data_skipping_predicate(
        &self,
        evaluator: &IndirectDataSkippingPredicateEvaluator<'_>,
        exprs: &[Expression],
        inverted: bool,
    ) -> Option<Predicate>;
}

/// A shared reference to an [`OpaqueExpressionOp`] instance.
pub type OpaqueExpressionOpRef = Arc<dyn OpaqueExpressionOp>;

/// A shared reference to an [`OpaquePredicateOp`] instance.
pub type OpaquePredicateOpRef = Arc<dyn OpaquePredicateOp>;

////////////////////////////////////////////////////////////////////////
// Expressions and predicates
////////////////////////////////////////////////////////////////////////

/// Context information for expression evaluation.
///
/// Contains settings that affect how expressions are evaluated, such as collation
/// for string comparisons. This provides an extensible way to add evaluation context
/// without modifying expression structures.
///
/// # Examples
///
/// ```ignore
/// use delta_kernel::expressions::ExprContext;
/// use delta_kernel::collation::CollationIdentifier;
///
/// // Default context (no special settings)
/// let ctx = ExprContext::new();
///
/// // Context with collation
/// let ctx = ExprContext::with_collation(CollationIdentifier::spark("UTF8_LCASE"));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct ExprContext {
    /// Collation for string comparisons.
    ///
    /// If `None`, uses default binary (byte-wise) comparison.
    pub collation: Option<crate::collation::CollationIdentifier>,

    // Future context fields can be added here without breaking changes:
    // pub timezone: Option<String>,
    // pub locale: Option<String>,
    // pub decimal_precision: Option<u8>,
}

impl ExprContext {
    /// Creates a new expression context with default settings.
    pub fn new() -> Self {
        Self { collation: None }
    }

    /// Creates an expression context with the specified collation.
    pub fn with_collation(collation: crate::collation::CollationIdentifier) -> Self {
        Self {
            collation: Some(collation),
        }
    }

    /// Returns the collation from this context, if present.
    pub fn collation(&self) -> Option<&crate::collation::CollationIdentifier> {
        self.collation.as_ref()
    }
}

impl Default for ExprContext {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnaryPredicate {
    /// The operator.
    pub op: UnaryPredicateOp,
    /// The input expression.
    pub expr: Box<Expression>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BinaryPredicate {
    /// The operator.
    pub op: BinaryPredicateOp,
    /// The left-hand side of the operation.
    pub left: Box<Expression>,
    /// The right-hand side of the operation.
    pub right: Box<Expression>,
    /// Optional expression evaluation context.
    ///
    /// Contains settings like collation that affect how the comparison is evaluated.
    /// If `None`, uses default evaluation settings (binary comparison for strings).
    pub context: Option<ExprContext>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnaryExpression {
    /// The operator.
    pub op: UnaryExpressionOp,
    /// The input expression.
    pub expr: Box<Expression>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BinaryExpression {
    /// The operator.
    pub op: BinaryExpressionOp,
    /// The left-hand side of the operation.
    pub left: Box<Expression>,
    /// The right-hand side of the operation.
    pub right: Box<Expression>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariadicExpression {
    /// The operator.
    pub op: VariadicExpressionOp,
    /// The input expressions.
    pub exprs: Vec<Expression>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JunctionPredicate {
    /// The operator.
    pub op: JunctionPredicateOp,
    /// The input predicates.
    pub preds: Vec<Predicate>,
}

// NOTE: We have to use `Arc<dyn OpaquePredicateOp>` instead of `Box<dyn OpaquePredicateOp>` because
// we cannot require `OpaquePredicateOp: Clone` (not a dyn-compatible trait). Instead, we must rely
// on cheap `Arc` clone, which does not duplicate the inner object.
#[derive(Clone, Debug)]
pub struct OpaquePredicate {
    pub op: OpaquePredicateOpRef,
    pub exprs: Vec<Expression>,
}

impl OpaquePredicate {
    fn new(op: OpaquePredicateOpRef, exprs: impl IntoIterator<Item = Expression>) -> Self {
        let exprs = exprs.into_iter().collect();
        Self { op, exprs }
    }
}

// NOTE: We have to use `Arc<dyn OpaqueExpressionOp>` instead of `Box<dyn OpaqueExpressionOp>`
// because we cannot require `OpaqueExpressionOp: Clone` (not a dyn-compatible trait). Instead, we
// must rely on cheap `Arc` clone, which does not duplicate the inner object.
#[derive(Clone, Debug)]
pub struct OpaqueExpression {
    pub op: OpaqueExpressionOpRef,
    pub exprs: Vec<Expression>,
}

impl OpaqueExpression {
    fn new(op: OpaqueExpressionOpRef, exprs: impl IntoIterator<Item = Expression>) -> Self {
        let exprs = exprs.into_iter().collect();
        Self { op, exprs }
    }
}

/// A transformation affecting a single field (one pieces of a [`Transform`]). The transformation
/// could insert 0+ new fields after the target, or could replace the target with 0+ a new fields).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FieldTransform {
    /// The list of expressions this field transform emits at the target location.
    pub exprs: Vec<ExpressionRef>,
    /// If true, the output expressions replace the input field instead of following after it.
    pub is_replace: bool,
}

/// A transformation that efficiently represents sparse modifications to struct schemas.
///
/// `Transform` achieves `O(changes)` space complexity instead of `O(schema_width)` by only
/// specifying those fields that actually change (inserted, replaced, or deleted). Any input field
/// not specifically mentioned by the transform is passed through, unmodified and with the same
/// relative field ordering. This is particularly useful for wide schemas where only a few columns
/// need to be modified and/or dropped, or where a small number of columns need to be injected.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transform {
    /// The path to the nested input struct this transform operates on (if any). If no path is
    /// given, the transform operates directly on top-level columns.
    pub input_path: Option<ColumnName>,
    /// A mapping from named input fields to the transform to be performed on each field.
    pub field_transforms: HashMap<String, FieldTransform>,
    /// A list of new fields to emit before processing the first input field.
    pub prepended_fields: Vec<ExpressionRef>,
}

impl Transform {
    /// Creates a new top-level identity transform. The various `with_xxx` helper methods can be
    /// used to add specific field transforms.
    pub fn new_top_level() -> Self {
        Self::default()
    }

    /// Creates a new identity transform that operates on fields of a nested struct identified by
    /// `path`. The various `with_xxx` helper methods can be used to add specific field transforms.
    pub fn new_nested<A>(path: impl IntoIterator<Item = A>) -> Self
    where
        ColumnName: FromIterator<A>,
    {
        Self {
            input_path: Some(ColumnName::new(path)),
            ..Default::default()
        }
    }

    /// Specifies a field to drop.
    pub fn with_dropped_field(mut self, name: impl Into<String>) -> Self {
        let field_transform = self.field_transform(name);
        field_transform.is_replace = true;
        self
    }

    /// Specifies an expression to replace a field with.
    pub fn with_replaced_field(mut self, name: impl Into<String>, expr: ExpressionRef) -> Self {
        let field_transform = self.field_transform(name);
        field_transform.exprs.push(expr);
        field_transform.is_replace = true;
        self
    }

    /// Specifies an expression to insert after an optional predecessor (None = prepend, emit the
    /// expression before the first input field). Multiple fields can be inserted after the same
    /// predecessor, and they will be emitted in the same order they were registered.
    pub fn with_inserted_field(
        mut self,
        after: Option<impl Into<String>>,
        expr: ExpressionRef,
    ) -> Self {
        match after {
            Some(field_name) => self.field_transform(field_name).exprs.push(expr),
            None => self.prepended_fields.push(expr),
        }
        self
    }

    /// True if this is the identity transform (all input fields pass through unchanged, with no new
    /// fields inserted).
    pub fn is_identity(&self) -> bool {
        self.prepended_fields.is_empty() && self.field_transforms.is_empty()
    }

    /// None, if this is a top-level transform. Otherwise, the path of this nested transform.
    pub fn input_path(&self) -> Option<&ColumnName> {
        self.input_path.as_ref()
    }

    // Gets or creates the field transform for a named input field
    fn field_transform(&mut self, field_name: impl Into<String>) -> &mut FieldTransform {
        self.field_transforms.entry(field_name.into()).or_default()
    }
}

/// A SQL expression.
///
/// These expressions do not track or validate data types, other than the type
/// of literals. It is up to the expression evaluator to validate the
/// expression against a schema and add appropriate casts as required.
#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    /// A literal value.
    Literal(Scalar),
    /// A column reference by name.
    Column(ColumnName),
    /// A predicate treated as a boolean expression
    Predicate(Box<Predicate>), // should this be Arc?
    /// A struct computed from a Vec of expressions
    Struct(Vec<ExpressionRef>),
    /// A sparse transformation of a struct schema. More efficient than `Struct` for wide schemas
    /// where only a few fields change, achieving O(changes) instead of O(schema_width) complexity.
    Transform(Transform),
    /// An expression that takes one expression as input.
    Unary(UnaryExpression),
    /// An expression that takes two expressions as input.
    Binary(BinaryExpression),
    /// An expression that takes a variable number of expressions as input.
    Variadic(VariadicExpression),
    /// An expression that the engine defines and implements. Kernel interacts with the expression
    /// only through methods provided by the [`OpaqueExpressionOp`] trait.
    Opaque(OpaqueExpression),
    /// An unknown expression (i.e. one that neither kernel nor engine attempts to evaluate). For
    /// data skipping purposes, kernel treats unknown expressions as if they were literal NULL
    /// values (which may disable skipping if it "poisons" the predicate), but engines MUST NOT
    /// attempt to interpret them as NULL when evaluating query filters because it could produce
    /// incorrect results. For example, converting `WHERE <fancy-udf-invocation> IS NULL` to `WHERE
    /// <unknown> IS NULL` to `WHERE NULL IS NULL` is equivalent to `WHERE TRUE` and would include
    /// all rows -- almost certainly NOT what the query author intended. Use `Expression::Opaque`
    /// for expressions kernel doesn't understand but which engine can still evaluate.
    Unknown(String),
}

/// A SQL predicate.
///
/// These predicates do not track or validate data types, other than the type
/// of literals. It is up to the predicate evaluator to validate the
/// predicate against a schema and add appropriate casts as required.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    /// A boolean-valued expression, useful for e.g. `AND(<boolean_col1>, <boolean_col2>)`.
    BooleanExpression(Expression),
    /// Boolean inversion (true <-> false)
    ///
    /// NOTE: NOT is not a normal unary predicate, because it requires a predicate as input (not an
    /// expression), and is never directly evaluated. Instead, observing that all predicates are
    /// invertible, NOT is always pushed down into its child predicate, inverting it. For example,
    /// `NOT (a < b)` pushes down and inverts `<` to `>=`, producing `a >= b`.
    Not(Box<Predicate>),
    /// A unary operation.
    Unary(UnaryPredicate),
    /// A binary operation.
    Binary(BinaryPredicate),
    /// A junction operation (AND/OR).
    Junction(JunctionPredicate),
    /// A predicate that the engine defines and implements. Kernel interacts with the predicate
    /// only through methods provided by the [`OpaquePredicateOp`] trait.
    Opaque(OpaquePredicate),
    /// An unknown predicate (i.e. one that neither kernel nor engine attempts to evaluate). For
    /// data skipping purposes, kernel treats unknown predicates as if they were literal NULL values
    /// (which may disable skipping if it "poisons" the predicate), but engines MUST NOT attempt to
    /// interpret them as NULL when evaluating query filters because it could produce incorrect
    /// results. For example, converting `WHERE <fancy-udf-invocation>` to `WHERE NULL` is
    /// equivalent to `WHERE FALSE` and would filter out all rows -- almost certainly NOT what the
    /// query author intended. Use `Predicate::Opaque` for predicates kernel doesn't understand
    /// but which engine can still evaluate.
    Unknown(String),
}

////////////////////////////////////////////////////////////////////////
// Struct/Enum impls
////////////////////////////////////////////////////////////////////////

impl BinaryPredicateOp {
    /// True if this is a comparison for which NULL input always produces NULL output
    pub(crate) fn is_null_intolerant(&self) -> bool {
        use BinaryPredicateOp::*;
        match self {
            LessThan | GreaterThan | Equal => true,
            Distinct | In => false, // tolerates NULL input
        }
    }
}

impl JunctionPredicateOp {
    pub(crate) fn invert(&self) -> JunctionPredicateOp {
        use JunctionPredicateOp::*;
        match self {
            And => Or,
            Or => And,
        }
    }
}

impl UnaryExpression {
    fn new(op: UnaryExpressionOp, expr: impl Into<Expression>) -> Self {
        let expr = Box::new(expr.into());
        Self { op, expr }
    }
}

impl UnaryPredicate {
    fn new(op: UnaryPredicateOp, expr: impl Into<Expression>) -> Self {
        let expr = Box::new(expr.into());
        Self { op, expr }
    }
}

impl BinaryExpression {
    fn new(
        op: BinaryExpressionOp,
        left: impl Into<Expression>,
        right: impl Into<Expression>,
    ) -> Self {
        let left = Box::new(left.into());
        let right = Box::new(right.into());
        Self { op, left, right }
    }
}

impl BinaryPredicate {
    #[allow(dead_code)]
    fn new(
        op: BinaryPredicateOp,
        left: impl Into<Expression>,
        right: impl Into<Expression>,
    ) -> Self {
        let left = Box::new(left.into());
        let right = Box::new(right.into());
        Self { op, left, right, context: None }
    }

    /// Sets the expression context for this predicate (builder pattern).
    pub fn with_context(mut self, context: ExprContext) -> Self {
        self.context = Some(context);
        self
    }

    /// Convenience method to set the collation for this predicate (builder pattern).
    ///
    /// This is equivalent to `with_context(ExprContext::with_collation(collation))`.
    pub fn with_collation(self, collation: crate::collation::CollationIdentifier) -> Self {
        self.with_context(ExprContext::with_collation(collation))
    }

    /// Creates a copy of this predicate with new left and right expressions, preserving
    /// the operator and context.
    /// ```
    pub fn with_expressions(
        &self,
        left: impl Into<Expression>,
        right: impl Into<Expression>,
    ) -> Self {
        Self {
            op: self.op,
            left: Box::new(left.into()),
            right: Box::new(right.into()),
            context: self.context.clone(),
        }
    }
}

impl VariadicExpression {
    fn new(
        op: VariadicExpressionOp,
        exprs: impl IntoIterator<Item = impl Into<Expression>>,
    ) -> Self {
        let exprs = exprs.into_iter().map(Into::into).collect();
        Self { op, exprs }
    }
}

impl JunctionPredicate {
    fn new(op: JunctionPredicateOp, preds: Vec<Predicate>) -> Self {
        Self { op, preds }
    }
}

impl Expression {
    /// Returns a set of columns referenced by this expression.
    pub fn references(&self) -> HashSet<&ColumnName> {
        let mut references = GetColumnReferences::default();
        let _ = references.transform_expr(self);
        references.into_inner()
    }

    /// Create a new column name expression from input satisfying `FromIterator for ColumnName`.
    pub fn column<A>(field_names: impl IntoIterator<Item = A>) -> Expression
    where
        ColumnName: FromIterator<A>,
    {
        ColumnName::new(field_names).into()
    }

    /// Create a new expression for a literal value
    pub fn literal(value: impl Into<Scalar>) -> Self {
        Self::Literal(value.into())
    }

    /// Creates a NULL literal expression
    pub const fn null_literal(data_type: DataType) -> Self {
        Self::Literal(Scalar::Null(data_type))
    }

    /// Wraps a predicate as a boolean-valued expression
    pub fn from_pred(value: Predicate) -> Self {
        match value {
            Predicate::BooleanExpression(expr) => expr,
            _ => Self::Predicate(Box::new(value)),
        }
    }

    /// Create a new struct expression
    pub fn struct_from(exprs: impl IntoIterator<Item = impl Into<Arc<Self>>>) -> Self {
        Self::Struct(exprs.into_iter().map(Into::into).collect())
    }

    /// Create a new transform expression
    pub fn transform(transform: Transform) -> Self {
        Self::Transform(transform)
    }

    /// Create a new element_at expression to access a map element by key
    /// Example: element_at(map_col, "key") returns map_col["key"]
    pub fn element_at(map: impl Into<Self>, key: impl Into<Self>) -> Self {
        Self::Variadic(VariadicExpression::new(
            VariadicExpressionOp::ElementAt,
            [map.into(), key.into()],
        ))
    }

    /// Create a new struct_field expression to access a field in a struct
    /// Example: struct_field(struct_expr, "field1", "field2") returns struct_expr.field1.field2
    pub fn struct_field(struct_expr: impl Into<Self>, fields: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut exprs = vec![struct_expr.into()];
        exprs.extend(fields.into_iter().map(|f| Self::literal(f.into())));
        Self::Variadic(VariadicExpression::new(
            VariadicExpressionOp::StructField,
            exprs,
        ))
    }

    /// Create a new predicate `self IS NULL`
    pub fn is_null(self) -> Predicate {
        Predicate::is_null(self)
    }

    /// Create a new predicate `self IS NOT NULL`
    pub fn is_not_null(self) -> Predicate {
        Predicate::is_not_null(self)
    }

    /// Create a new predicate `self == other`
    pub fn eq(self, other: impl Into<Self>) -> Predicate {
        Predicate::eq(self, other)
    }

    /// Create a new predicate `self != other`
    pub fn ne(self, other: impl Into<Self>) -> Predicate {
        Predicate::ne(self, other)
    }

    /// Create a new predicate `self <= other`
    pub fn le(self, other: impl Into<Self>) -> Predicate {
        Predicate::le(self, other)
    }

    /// Create a new predicate `self < other`
    pub fn lt(self, other: impl Into<Self>) -> Predicate {
        Predicate::lt(self, other)
    }

    /// Create a new predicate `self >= other`
    pub fn ge(self, other: impl Into<Self>) -> Predicate {
        Predicate::ge(self, other)
    }

    /// Create a new predicate `self > other`
    pub fn gt(self, other: impl Into<Self>) -> Predicate {
        Predicate::gt(self, other)
    }

    /// Create a new predicate `DISTINCT(self, other)`
    pub fn distinct(self, other: impl Into<Self>) -> Predicate {
        Predicate::distinct(self, other)
    }

    /// Create a new predicate `self == other` with the specified collation.
    ///
    /// # Example
    /// ```ignore
    /// use delta_kernel::collation::CollationIdentifier;
    /// let pred = col("name").eq_collated("Alice", CollationIdentifier::spark("UTF8_LCASE"));
    /// ```
    pub fn eq_collated(
        self,
        other: impl Into<Self>,
        collation: crate::collation::CollationIdentifier,
    ) -> Predicate {
        Predicate::eq_collated(self, other, collation)
    }

    /// Create a new predicate `self < other` with the specified collation.
    pub fn lt_collated(
        self,
        other: impl Into<Self>,
        collation: crate::collation::CollationIdentifier,
    ) -> Predicate {
        Predicate::lt_collated(self, other, collation)
    }

    /// Create a new predicate `self > other` with the specified collation.
    pub fn gt_collated(
        self,
        other: impl Into<Self>,
        collation: crate::collation::CollationIdentifier,
    ) -> Predicate {
        Predicate::gt_collated(self, other, collation)
    }

    /// Create a new predicate `self <= other` with the specified collation.
    pub fn le_collated(
        self,
        other: impl Into<Self>,
        collation: crate::collation::CollationIdentifier,
    ) -> Predicate {
        Predicate::le_collated(self, other, collation)
    }

    /// Create a new predicate `self >= other` with the specified collation.
    pub fn ge_collated(
        self,
        other: impl Into<Self>,
        collation: crate::collation::CollationIdentifier,
    ) -> Predicate {
        Predicate::ge_collated(self, other, collation)
    }

    /// Create a new predicate `self != other` with the specified collation.
    pub fn ne_collated(
        self,
        other: impl Into<Self>,
        collation: crate::collation::CollationIdentifier,
    ) -> Predicate {
        Predicate::ne_collated(self, other, collation)
    }

    /// Create a new predicate `DISTINCT(self, other)` with the specified collation.
    pub fn distinct_collated(
        self,
        other: impl Into<Self>,
        collation: crate::collation::CollationIdentifier,
    ) -> Predicate {
        Predicate::distinct_collated(self, other, collation)
    }

    /// Creates a new unary expression
    pub fn unary(op: UnaryExpressionOp, expr: impl Into<Expression>) -> Self {
        Self::Unary(UnaryExpression::new(op, expr))
    }

    /// Creates a new binary expression lhs OP rhs
    pub fn binary(
        op: BinaryExpressionOp,
        lhs: impl Into<Expression>,
        rhs: impl Into<Expression>,
    ) -> Self {
        Self::Binary(BinaryExpression::new(op, lhs, rhs))
    }

    /// Creates a new variadic expression
    pub fn variadic(
        op: VariadicExpressionOp,
        exprs: impl IntoIterator<Item = impl Into<Expression>>,
    ) -> Self {
        Self::Variadic(VariadicExpression::new(op, exprs))
    }

    /// Creates a new opaque expression
    pub fn opaque(
        op: impl OpaqueExpressionOp,
        exprs: impl IntoIterator<Item = Expression>,
    ) -> Self {
        Self::Opaque(OpaqueExpression::new(Arc::new(op), exprs))
    }

    /// Creates a new unknown expression
    pub fn unknown(name: impl Into<String>) -> Self {
        Self::Unknown(name.into())
    }
}

impl Predicate {
    /// Returns a set of columns referenced by this predicate.
    pub fn references(&self) -> HashSet<&ColumnName> {
        let mut references = GetColumnReferences::default();
        let _ = references.transform_pred(self);
        references.into_inner()
    }

    /// Creates a new boolean column reference. See also [`Expression::column`].
    pub fn column<A>(field_names: impl IntoIterator<Item = A>) -> Predicate
    where
        ColumnName: FromIterator<A>,
    {
        Self::from_expr(ColumnName::new(field_names))
    }

    /// Create a new literal boolean value
    pub const fn literal(value: bool) -> Self {
        Self::BooleanExpression(Expression::Literal(Scalar::Boolean(value)))
    }

    /// Creates a NULL literal boolean value
    pub const fn null_literal() -> Self {
        Self::BooleanExpression(Expression::Literal(Scalar::Null(DataType::BOOLEAN)))
    }

    /// Converts a boolean-valued expression into a predicate
    pub fn from_expr(expr: impl Into<Expression>) -> Self {
        match expr.into() {
            Expression::Predicate(p) => *p,
            expr => Predicate::BooleanExpression(expr),
        }
    }

    /// Logical NOT (boolean inversion)
    pub fn not(pred: impl Into<Self>) -> Self {
        Self::Not(Box::new(pred.into()))
    }

    /// Create a new predicate `self IS NULL`
    pub fn is_null(expr: impl Into<Expression>) -> Predicate {
        Self::unary(UnaryPredicateOp::IsNull, expr)
    }

    /// Create a new predicate `self IS NOT NULL`
    pub fn is_not_null(expr: impl Into<Expression>) -> Predicate {
        Self::not(Self::is_null(expr))
    }

    /// Create a new predicate `self == other`
    pub fn eq(a: impl Into<Expression>, b: impl Into<Expression>) -> Self {
        Self::binary(BinaryPredicateOp::Equal, a, b)
    }

    /// Create a new predicate `self != other`
    pub fn ne(a: impl Into<Expression>, b: impl Into<Expression>) -> Self {
        Self::not(Self::binary(BinaryPredicateOp::Equal, a, b))
    }

    /// Create a new predicate `self <= other`
    pub fn le(a: impl Into<Expression>, b: impl Into<Expression>) -> Self {
        Self::not(Self::binary(BinaryPredicateOp::GreaterThan, a, b))
    }

    /// Create a new predicate `self < other`
    pub fn lt(a: impl Into<Expression>, b: impl Into<Expression>) -> Self {
        Self::binary(BinaryPredicateOp::LessThan, a, b)
    }

    /// Create a new predicate `self >= other`
    pub fn ge(a: impl Into<Expression>, b: impl Into<Expression>) -> Self {
        Self::not(Self::binary(BinaryPredicateOp::LessThan, a, b))
    }

    /// Create a new predicate `self > other`
    pub fn gt(a: impl Into<Expression>, b: impl Into<Expression>) -> Self {
        Self::binary(BinaryPredicateOp::GreaterThan, a, b)
    }

    /// Create a new predicate `DISTINCT(self, other)`
    pub fn distinct(a: impl Into<Expression>, b: impl Into<Expression>) -> Self {
        Self::binary(BinaryPredicateOp::Distinct, a, b)
    }

    /// Attaches an expression context to this predicate (builder pattern).
    ///
    /// For binary predicates, this sets the evaluation context. For other predicates,
    /// this is a no-op (context only applies to binary predicates).
    ///
    /// # Example
    /// ```ignore
    /// use delta_kernel::expressions::ExprContext;
    /// use delta_kernel::collation::CollationIdentifier;
    /// let ctx = ExprContext::with_collation(CollationIdentifier::spark("UTF8_LCASE"));
    /// let pred = Predicate::eq(col("name"), "Alice").with_context(ctx);
    /// ```
    pub fn with_context(self, context: ExprContext) -> Self {
        match self {
            Self::Binary(bp) => Self::Binary(bp.with_context(context)),
            other => other, // Context only applies to binary predicates
        }
    }

    /// Convenience method to attach a collation to this predicate (builder pattern).
    ///
    /// This is equivalent to `with_context(ExprContext::with_collation(collation))`.
    /// For binary predicates, this sets the collation. For other predicates, this is a no-op.
    ///
    /// # Example
    /// ```ignore
    /// use delta_kernel::collation::CollationIdentifier;
    /// let pred = Predicate::eq(col("name"), "Alice")
    ///     .with_collation(CollationIdentifier::spark("UTF8_LCASE"));
    /// ```
    pub fn with_collation(self, collation: crate::collation::CollationIdentifier) -> Self {
        self.with_context(ExprContext::with_collation(collation))
    }

    /// Checks if this predicate should be removed for parquet row group filtering.
    ///
    /// Currently removes predicates with non-binary collations since parquet stats
    /// use binary comparison.
    ///
    /// This is a private helper to make the removal logic extensible for future cases.
    fn should_remove_for_parquet_filter(&self) -> bool {
        match self {
            Self::Binary(bp) => {
                // Remove if this binary predicate has a non-binary collation
                if let Some(ref collation) = bp.context.as_ref().and_then(|ctx| ctx.collation.as_ref()) {
                    !collation.is_spark_utf8_binary()
                } else {
                    false
                }
            }
            _ => false, // Only binary predicates can have collations that need removal
        }
    }

    /// Transforms this predicate for parquet row group filtering by removing parts that
    /// cannot be safely evaluated using parquet stats.
    ///
    /// Returns `None` if the entire predicate cannot be used for parquet filtering.
    ///
    /// # Strategy
    /// - Predicates that should be removed (see `should_remove_for_parquet_filter`) return `None`
    /// - AND: Remove problematic parts, keep remaining predicates (conservative filtering)
    /// - OR: If any part should be removed, return `None` (can't safely filter)
    /// - NOT: If inner should be removed, return `None`
    ///
    /// # Examples
    /// - `x > 5 AND y = 'b'` (collation on y) → `Some(x > 5)`
    /// - `x > 5 OR y = 'b'` (collation on y) → `None` (must keep all row groups)
    /// - `y = 'b'` (collation on y) → `None`
    pub fn for_parquet_row_group_filter(&self) -> Option<Self> {
        // Check if this predicate should be removed
        if self.should_remove_for_parquet_filter() {
            return None;
        }

        match self {
            Self::Junction(jp) => {
                match jp.op {
                    JunctionPredicateOp::And => {
                        // For AND: Keep predicates that pass the filter
                        let filtered: Vec<_> = jp.preds.iter()
                            .filter_map(|p| p.for_parquet_row_group_filter())
                            .collect();

                        match filtered.len() {
                            0 => None, // All predicates were removed
                            1 => Some(filtered.into_iter().next().unwrap()),
                            _ => Some(Self::Junction(JunctionPredicate {
                                op: JunctionPredicateOp::And,
                                preds: filtered,
                            })),
                        }
                    }
                    JunctionPredicateOp::Or => {
                        // For OR: If any part should be removed, we can't safely filter
                        let transformed: Vec<_> = jp.preds.iter()
                            .map(|p| p.for_parquet_row_group_filter())
                            .collect();

                        if transformed.iter().any(|p| p.is_none()) {
                            // At least one predicate should be removed - can't filter
                            None
                        } else {
                            // All predicates are safe
                            Some(Self::Junction(JunctionPredicate {
                                op: JunctionPredicateOp::Or,
                                preds: transformed.into_iter().map(|p| p.unwrap()).collect(),
                            }))
                        }
                    }
                }
            }
            Self::Not(pred) => {
                // For NOT: If inner should be removed, we can't filter
                pred.for_parquet_row_group_filter().map(|p| Self::Not(Box::new(p)))
            }
            // All other predicate types that passed should_remove_for_parquet_filter are safe
            _ => Some(self.clone()),
        }
    }

    /// Create a new predicate `a == b` with the specified collation.
    pub fn eq_collated(
        a: impl Into<Expression>,
        b: impl Into<Expression>,
        collation: crate::collation::CollationIdentifier,
    ) -> Self {
        Self::eq(a, b).with_collation(collation)
    }

    /// Create a new predicate `a < b` with the specified collation.
    pub fn lt_collated(
        a: impl Into<Expression>,
        b: impl Into<Expression>,
        collation: crate::collation::CollationIdentifier,
    ) -> Self {
        Self::lt(a, b).with_collation(collation)
    }

    /// Create a new predicate `a > b` with the specified collation.
    pub fn gt_collated(
        a: impl Into<Expression>,
        b: impl Into<Expression>,
        collation: crate::collation::CollationIdentifier,
    ) -> Self {
        Self::gt(a, b).with_collation(collation)
    }

    /// Create a new predicate `a <= b` with the specified collation.
    pub fn le_collated(
        a: impl Into<Expression>,
        b: impl Into<Expression>,
        collation: crate::collation::CollationIdentifier,
    ) -> Self {
        Self::le(a, b).with_collation(collation)
    }

    /// Create a new predicate `a >= b` with the specified collation.
    pub fn ge_collated(
        a: impl Into<Expression>,
        b: impl Into<Expression>,
        collation: crate::collation::CollationIdentifier,
    ) -> Self {
        Self::ge(a, b).with_collation(collation)
    }

    /// Create a new predicate `a != b` with the specified collation.
    pub fn ne_collated(
        a: impl Into<Expression>,
        b: impl Into<Expression>,
        collation: crate::collation::CollationIdentifier,
    ) -> Self {
        Self::ne(a, b).with_collation(collation)
    }

    /// Create a new predicate `DISTINCT(a, b)` with the specified collation.
    pub fn distinct_collated(
        a: impl Into<Expression>,
        b: impl Into<Expression>,
        collation: crate::collation::CollationIdentifier,
    ) -> Self {
        Self::distinct(a, b).with_collation(collation)
    }

    /// Create a new predicate `self AND other`
    pub fn and(a: impl Into<Self>, b: impl Into<Self>) -> Self {
        Self::and_from([a.into(), b.into()])
    }

    /// Create a new predicate `self OR other`
    pub fn or(a: impl Into<Self>, b: impl Into<Self>) -> Self {
        Self::or_from([a.into(), b.into()])
    }

    /// Creates a new predicate AND(preds...)
    pub fn and_from(preds: impl IntoIterator<Item = Self>) -> Self {
        Self::junction(JunctionPredicateOp::And, preds)
    }

    /// Creates a new predicate OR(preds...)
    pub fn or_from(preds: impl IntoIterator<Item = Self>) -> Self {
        Self::junction(JunctionPredicateOp::Or, preds)
    }

    /// Creates a new unary predicate OP expr
    pub fn unary(op: UnaryPredicateOp, expr: impl Into<Expression>) -> Self {
        let expr = Box::new(expr.into());
        Self::Unary(UnaryPredicate { op, expr })
    }

    /// Creates a new binary predicate lhs OP rhs
    pub fn binary(
        op: BinaryPredicateOp,
        lhs: impl Into<Expression>,
        rhs: impl Into<Expression>,
    ) -> Self {
        Self::Binary(BinaryPredicate {
            op,
            left: Box::new(lhs.into()),
            right: Box::new(rhs.into()),
            context: None,
        })
    }

    /// Creates a new junction predicate OP(preds...)
    pub fn junction(op: JunctionPredicateOp, preds: impl IntoIterator<Item = Self>) -> Self {
        let preds = preds.into_iter().collect();
        Self::Junction(JunctionPredicate { op, preds })
    }

    /// Creates a new opaque predicate
    pub fn opaque(op: impl OpaquePredicateOp, exprs: impl IntoIterator<Item = Expression>) -> Self {
        Self::Opaque(OpaquePredicate::new(Arc::new(op), exprs))
    }

    /// Creates a new unknown predicate
    pub fn unknown(name: impl Into<String>) -> Self {
        Self::Unknown(name.into())
    }
}

////////////////////////////////////////////////////////////////////////
// Trait impls
////////////////////////////////////////////////////////////////////////

impl PartialEq for OpaquePredicate {
    fn eq(&self, other: &Self) -> bool {
        self.op.dyn_eq(other.op.any_ref()) && self.exprs == other.exprs
    }
}

impl PartialEq for OpaqueExpression {
    fn eq(&self, other: &Self) -> bool {
        self.op.dyn_eq(other.op.any_ref()) && self.exprs == other.exprs
    }
}

impl Display for UnaryExpressionOp {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use UnaryExpressionOp::*;
        match self {
            ToJson => write!(f, "TO_JSON"),
        }
    }
}

impl Display for BinaryExpressionOp {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use BinaryExpressionOp::*;
        match self {
            Plus => write!(f, "+"),
            Minus => write!(f, "-"),
            Multiply => write!(f, "*"),
            Divide => write!(f, "/"),
        }
    }
}

impl Display for VariadicExpressionOp {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use VariadicExpressionOp::*;
        match self {
            Coalesce => write!(f, "COALESCE"),
            ElementAt => write!(f, "ELEMENT_AT"),
            StructField => write!(f, "STRUCT_FIELD"),
        }
    }
}

impl Display for BinaryPredicateOp {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use BinaryPredicateOp::*;
        match self {
            LessThan => write!(f, "<"),
            GreaterThan => write!(f, ">"),
            Equal => write!(f, "="),
            // TODO(roeap): AFAIK DISTINCT does not have a commonly used operator symbol
            // so ideally this would not be used as we use Display for rendering expressions
            // in our code we take care of this, but theirs might not ...
            Distinct => write!(f, "DISTINCT"),
            In => write!(f, "IN"),
        }
    }
}

// Helper for displaying the children of variadic expressions and predicates
fn format_child_list<T: Display>(children: &[T]) -> String {
    children.iter().map(|c| format!("{c}")).join(", ")
}

impl Display for Expression {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use Expression::*;
        match self {
            Literal(l) => write!(f, "{l}"),
            Column(name) => write!(f, "Column({name})"),
            Predicate(p) => write!(f, "{p}"),
            Struct(exprs) => write!(f, "Struct({})", format_child_list(exprs)),
            Transform(transform) => {
                write!(f, "Transform(")?;
                let mut sep = "";
                if !transform.prepended_fields.is_empty() {
                    let prepended_fields = format_child_list(&transform.prepended_fields);
                    write!(f, "prepend [{prepended_fields}]")?;
                    sep = ", ";
                }
                for (field_name, field_transform) in &transform.field_transforms {
                    let insertions = &field_transform.exprs;
                    if insertions.is_empty() {
                        if field_transform.is_replace {
                            write!(f, "{sep}drop {field_name}")?;
                        } else {
                            continue; // no-op; ignore it and don't change `sep` below
                        }
                    } else {
                        let insertions = format_child_list(insertions);
                        if field_transform.is_replace {
                            write!(f, "{sep}replace {field_name} with [{insertions}]")?;
                        } else {
                            write!(f, "{sep}after {field_name} insert [{insertions}]")?;
                        }
                    }
                    sep = ", ";
                }
                write!(f, ")")
            }
            Unary(UnaryExpression { op, expr }) => write!(f, "{op}({expr})"),
            Binary(BinaryExpression { op, left, right }) => write!(f, "{left} {op} {right}"),
            Variadic(VariadicExpression { op, exprs }) => {
                write!(f, "{op}({})", format_child_list(exprs))
            }
            Opaque(OpaqueExpression { op, exprs }) => {
                write!(f, "{op:?}({})", format_child_list(exprs))
            }
            Unknown(name) => write!(f, "<unknown: {name}>"),
        }
    }
}

impl Display for Predicate {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use Predicate::*;
        match self {
            BooleanExpression(expr) => write!(f, "{expr}"),
            Not(pred) => write!(f, "NOT({pred})"),
            Binary(BinaryPredicate {
                op: BinaryPredicateOp::Distinct,
                left,
                right,
                context,
            }) => {
                write!(f, "DISTINCT({left}, {right})")?;
                if let Some(ctx) = context {
                    if let Some(coll) = &ctx.collation {
                        write!(f, " COLLATE {coll}")?;
                    }
                }
                Ok(())
            }
            Binary(BinaryPredicate {
                op,
                left,
                right,
                context,
            }) => {
                write!(f, "{left} {op} {right}")?;
                if let Some(ctx) = context {
                    if let Some(coll) = &ctx.collation {
                        write!(f, " COLLATE {coll}")?;
                    }
                }
                Ok(())
            }
            Unary(UnaryPredicate { op, expr }) => match op {
                UnaryPredicateOp::IsNull => write!(f, "{expr} IS NULL"),
            },
            Junction(JunctionPredicate { op, preds }) => {
                let op = match op {
                    JunctionPredicateOp::And => "AND",
                    JunctionPredicateOp::Or => "OR",
                };
                write!(f, "{op}({})", format_child_list(preds))
            }
            Opaque(OpaquePredicate { op, exprs }) => {
                write!(f, "{op:?}({})", format_child_list(exprs))
            }
            Unknown(name) => write!(f, "<unknown: {name}>"),
        }
    }
}

impl From<Scalar> for Expression {
    fn from(value: Scalar) -> Self {
        Self::literal(value)
    }
}

impl From<ColumnName> for Expression {
    fn from(value: ColumnName) -> Self {
        Self::Column(value)
    }
}

impl From<Predicate> for Expression {
    fn from(value: Predicate) -> Self {
        Self::from_pred(value)
    }
}

impl From<ColumnName> for Predicate {
    fn from(value: ColumnName) -> Self {
        Self::from_expr(value)
    }
}

impl<R: Into<Expression>> std::ops::Add<R> for Expression {
    type Output = Self;

    fn add(self, rhs: R) -> Self::Output {
        Self::binary(BinaryExpressionOp::Plus, self, rhs)
    }
}

impl<R: Into<Expression>> std::ops::Sub<R> for Expression {
    type Output = Self;

    fn sub(self, rhs: R) -> Self {
        Self::binary(BinaryExpressionOp::Minus, self, rhs)
    }
}

impl<R: Into<Expression>> std::ops::Mul<R> for Expression {
    type Output = Self;

    fn mul(self, rhs: R) -> Self {
        Self::binary(BinaryExpressionOp::Multiply, self, rhs)
    }
}

impl<R: Into<Expression>> std::ops::Div<R> for Expression {
    type Output = Self;

    fn div(self, rhs: R) -> Self {
        Self::binary(BinaryExpressionOp::Divide, self, rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::{column_expr, column_pred, Expression as Expr, Predicate as Pred};

    #[test]
    fn test_expression_format() {
        let cases = [
            (column_expr!("x"), "Column(x)"),
            (
                (column_expr!("x") + Expr::literal(4)) / Expr::literal(10) * Expr::literal(42),
                "Column(x) + 4 / 10 * 42",
            ),
            (
                Expr::struct_from([column_expr!("x"), Expr::literal(2), Expr::literal(10)]),
                "Struct(Column(x), 2, 10)",
            ),
        ];

        for (expr, expected) in cases {
            let result = format!("{expr}");
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn test_predicate_format() {
        let cases = [
            (column_pred!("x"), "Column(x)"),
            (column_expr!("x").eq(Expr::literal(2)), "Column(x) = 2"),
            (
                (column_expr!("x") - Expr::literal(4)).lt(Expr::literal(10)),
                "Column(x) - 4 < 10",
            ),
            (
                Pred::and(
                    column_expr!("x").ge(Expr::literal(2)),
                    column_expr!("x").le(Expr::literal(10)),
                ),
                "AND(NOT(Column(x) < 2), NOT(Column(x) > 10))",
            ),
            (
                Pred::and_from([
                    column_expr!("x").ge(Expr::literal(2)),
                    column_expr!("x").le(Expr::literal(10)),
                    column_expr!("x").le(Expr::literal(100)),
                ]),
                "AND(NOT(Column(x) < 2), NOT(Column(x) > 10), NOT(Column(x) > 100))",
            ),
            (
                Pred::or(
                    column_expr!("x").gt(Expr::literal(2)),
                    column_expr!("x").lt(Expr::literal(10)),
                ),
                "OR(Column(x) > 2, Column(x) < 10)",
            ),
            (
                column_expr!("x").eq(Expr::literal("foo")),
                "Column(x) = 'foo'",
            ),
        ];

        for (pred, expected) in cases {
            let result = format!("{pred}");
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn test_parquet_filter_transform_and() {
        use crate::collation::CollationIdentifier;
        use crate::expressions::Scalar;

        // Test: x > 5 AND y = 'b' (collation on y) → Should keep x > 5
        let x_gt_5 = Pred::gt(column_expr!("x"), Scalar::from(5));
        let y_eq_b = Pred::eq(column_expr!("y"), Scalar::from("b"))
            .with_collation(CollationIdentifier::spark("UTF8_LCASE"));

        let pred = Pred::and(x_gt_5.clone(), y_eq_b);

        let filtered = pred.for_parquet_row_group_filter();
        assert!(filtered.is_some(), "AND with collation should filter non-collated parts");

        // Should only contain x > 5
        let filtered = filtered.unwrap();

        // Verify it's the x > 5 predicate (or similar structure)
        match filtered {
            Pred::Binary(bp) => {
                assert_eq!(bp.op, super::BinaryPredicateOp::GreaterThan);
            }
            _ => panic!("Expected binary predicate, got: {filtered:?}"),
        }
    }

    #[test]
    fn test_parquet_filter_transform_or() {
        use crate::collation::CollationIdentifier;
        use crate::expressions::Scalar;

        // Test: x > 5 OR y = 'b' (collation on y) → Should return None (can't filter)
        let x_gt_5 = Pred::gt(column_expr!("x"), Scalar::from(5));
        let y_eq_b = Pred::eq(column_expr!("y"), Scalar::from("b"))
            .with_collation(CollationIdentifier::spark("UTF8_LCASE"));

        let pred = Pred::or(x_gt_5, y_eq_b);

        let filtered = pred.for_parquet_row_group_filter();
        assert!(filtered.is_none(), "OR with collation should return None");
    }

    #[test]
    fn test_parquet_filter_transform_binary_only() {
        use crate::expressions::Scalar;

        // Test: Pure UTF8_BINARY predicate should pass through unchanged
        let pred = Pred::eq(column_expr!("x"), Scalar::from("test"));

        let filtered = pred.for_parquet_row_group_filter();
        assert!(filtered.is_some(), "Binary predicate should pass through");
    }

    #[test]
    fn test_parquet_filter_transform_collated_only() {
        use crate::collation::CollationIdentifier;
        use crate::expressions::Scalar;

        // Test: Pure collated predicate should return None
        let pred = Pred::eq(column_expr!("x"), Scalar::from("test"))
            .with_collation(CollationIdentifier::spark("UTF8_LCASE"));

        let filtered = pred.for_parquet_row_group_filter();
        assert!(filtered.is_none(), "Collated predicate should return None");
    }
}
