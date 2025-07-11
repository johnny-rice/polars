use std::fmt::Formatter;
use std::iter::FlatMap;

use polars_core::prelude::*;

use self::visitor::{AexprNode, RewritingVisitor, TreeWalker};
use crate::constants::get_len_name;
use crate::prelude::*;

/// Utility to write comma delimited strings
pub fn comma_delimited<S>(mut s: String, items: &[S]) -> String
where
    S: AsRef<str>,
{
    s.push('(');
    for c in items {
        s.push_str(c.as_ref());
        s.push_str(", ");
    }
    s.pop();
    s.pop();
    s.push(')');
    s
}

/// Utility to write comma delimited
pub(crate) fn fmt_column_delimited<S: AsRef<str>>(
    f: &mut Formatter<'_>,
    items: &[S],
    container_start: &str,
    container_end: &str,
) -> std::fmt::Result {
    write!(f, "{container_start}")?;
    for (i, c) in items.iter().enumerate() {
        write!(f, "{}", c.as_ref())?;
        if i != (items.len() - 1) {
            write!(f, ", ")?;
        }
    }
    write!(f, "{container_end}")
}

pub(crate) fn is_scan(plan: &IR) -> bool {
    matches!(plan, IR::Scan { .. } | IR::DataFrameScan { .. })
}

/// A projection that only takes a column or a column + alias.
#[cfg(feature = "meta")]
pub(crate) fn aexpr_is_simple_projection(current_node: Node, arena: &Arena<AExpr>) -> bool {
    arena
        .iter(current_node)
        .all(|(_node, e)| matches!(e, AExpr::Column(_)))
}

pub fn has_aexpr<F>(current_node: Node, arena: &Arena<AExpr>, matches: F) -> bool
where
    F: Fn(&AExpr) -> bool,
{
    arena.iter(current_node).any(|(_node, e)| matches(e))
}

pub fn has_aexpr_window(current_node: Node, arena: &Arena<AExpr>) -> bool {
    has_aexpr(current_node, arena, |e| matches!(e, AExpr::Window { .. }))
}

pub fn has_aexpr_literal(current_node: Node, arena: &Arena<AExpr>) -> bool {
    has_aexpr(current_node, arena, |e| matches!(e, AExpr::Literal(_)))
}

/// Can check if an expression tree has a matching_expr. This
/// requires a dummy expression to be created that will be used to pattern match against.
pub fn has_expr<F>(current_expr: &Expr, matches: F) -> bool
where
    F: Fn(&Expr) -> bool,
{
    current_expr.into_iter().any(matches)
}

/// Check if expression is independent from any column.
pub(crate) fn is_column_independent_aexpr(expr: Node, arena: &Arena<AExpr>) -> bool {
    !has_aexpr(expr, arena, |e| match e {
        AExpr::Column(_) | AExpr::Len => true,
        #[cfg(feature = "dtype-struct")]
        AExpr::Function {
            input: _,
            function: IRFunctionExpr::StructExpr(IRStructFunction::FieldByName(_)),
            options: _,
        } => true,
        _ => false,
    })
}

pub fn has_null(current_expr: &Expr) -> bool {
    has_expr(
        current_expr,
        |e| matches!(e, Expr::Literal(LiteralValue::Scalar(sc)) if sc.is_null()),
    )
}

pub fn aexpr_output_name(node: Node, arena: &Arena<AExpr>) -> PolarsResult<PlSmallStr> {
    for (_, ae) in arena.iter(node) {
        match ae {
            // don't follow the partition by branch
            AExpr::Window { function, .. } => return aexpr_output_name(*function, arena),
            AExpr::Column(name) => return Ok(name.clone()),
            AExpr::Len => return Ok(get_len_name()),
            AExpr::Literal(val) => return Ok(val.output_column_name().clone()),
            AExpr::Ternary { truthy, .. } => return aexpr_output_name(*truthy, arena),
            _ => {},
        }
    }
    let expr = node_to_expr(node, arena);
    polars_bail!(
        ComputeError:
        "unable to find root column name for expr '{expr:?}' when calling 'output_name'",
    );
}

/// output name of expr
pub fn expr_output_name(expr: &Expr) -> PolarsResult<PlSmallStr> {
    for e in expr {
        match e {
            // don't follow the partition by branch
            Expr::Window { function, .. } => return expr_output_name(function),
            Expr::Column(name) => return Ok(name.clone()),
            Expr::Alias(_, name) => return Ok(name.clone()),
            Expr::KeepName(_) => polars_bail!(nyi = "`name.keep` is not allowed here"),
            Expr::RenameAlias { expr, function } => return function.call(&expr_output_name(expr)?),
            Expr::Len => return Ok(get_len_name()),
            Expr::Literal(val) => return Ok(val.output_column_name().clone()),
            _ => {},
        }
    }
    polars_bail!(
        ComputeError:
        "unable to find root column name for expr '{expr:?}' when calling 'output_name'",
    );
}

#[allow(clippy::type_complexity)]
pub fn expr_to_leaf_column_names_iter(expr: &Expr) -> impl Iterator<Item = PlSmallStr> + '_ {
    expr_to_leaf_column_exprs_iter(expr).flat_map(|e| expr_to_leaf_column_name(e).ok())
}

/// This should gradually replace expr_to_root_column as this will get all names in the tree.
pub fn expr_to_leaf_column_names(expr: &Expr) -> Vec<PlSmallStr> {
    expr_to_leaf_column_names_iter(expr).collect()
}

/// unpack alias(col) to name of the root column name
pub fn expr_to_leaf_column_name(expr: &Expr) -> PolarsResult<PlSmallStr> {
    let mut leaves = expr_to_leaf_column_exprs_iter(expr).collect::<Vec<_>>();
    polars_ensure!(leaves.len() <= 1, ComputeError: "found more than one root column name");
    match leaves.pop() {
        Some(Expr::Column(name)) => Ok(name.clone()),
        Some(Expr::Selector(_)) => polars_bail!(
            ComputeError: "selector has no root column name",
        ),
        Some(_) => unreachable!(),
        None => polars_bail!(
            ComputeError: "no root column name found",
        ),
    }
}

#[allow(clippy::type_complexity)]
pub(crate) fn aexpr_to_column_nodes_iter<'a>(
    root: Node,
    arena: &'a Arena<AExpr>,
) -> FlatMap<AExprIter<'a>, Option<ColumnNode>, fn((Node, &'a AExpr)) -> Option<ColumnNode>> {
    arena.iter(root).flat_map(|(node, ae)| {
        if matches!(ae, AExpr::Column(_)) {
            Some(ColumnNode(node))
        } else {
            None
        }
    })
}

pub fn column_node_to_name(node: ColumnNode, arena: &Arena<AExpr>) -> &PlSmallStr {
    if let AExpr::Column(name) = arena.get(node.0) {
        name
    } else {
        unreachable!()
    }
}

/// Get all leaf column expressions in the expression tree.
pub(crate) fn expr_to_leaf_column_exprs_iter(expr: &Expr) -> impl Iterator<Item = &Expr> {
    expr.into_iter().flat_map(|e| match e {
        Expr::Column(_) => Some(e),
        _ => None,
    })
}

/// Take a list of expressions and a schema and determine the output schema.
pub fn expressions_to_schema(
    expr: &[Expr],
    schema: &Schema,
    ctxt: Context,
) -> PolarsResult<Schema> {
    let mut expr_arena = Arena::with_capacity(4 * expr.len());
    expr.iter()
        .map(|expr| {
            let mut field = expr.to_field_amortized(schema, ctxt, &mut expr_arena)?;

            field.dtype = field.dtype.materialize_unknown(true)?;
            Ok(field)
        })
        .collect()
}

pub fn aexpr_to_leaf_names_iter(
    node: Node,
    arena: &Arena<AExpr>,
) -> impl Iterator<Item = PlSmallStr> + '_ {
    aexpr_to_column_nodes_iter(node, arena).map(|node| match arena.get(node.0) {
        AExpr::Column(name) => name.clone(),
        _ => unreachable!(),
    })
}

pub fn aexpr_to_leaf_names(node: Node, arena: &Arena<AExpr>) -> Vec<PlSmallStr> {
    aexpr_to_leaf_names_iter(node, arena).collect()
}

pub fn aexpr_to_leaf_name(node: Node, arena: &Arena<AExpr>) -> PlSmallStr {
    aexpr_to_leaf_names_iter(node, arena).next().unwrap()
}

/// check if a selection/projection can be done on the downwards schema
pub(crate) fn check_input_node(
    node: Node,
    input_schema: &Schema,
    expr_arena: &Arena<AExpr>,
) -> bool {
    aexpr_to_leaf_names_iter(node, expr_arena).all(|name| input_schema.contains(name.as_ref()))
}

pub(crate) fn check_input_column_node(
    node: ColumnNode,
    input_schema: &Schema,
    expr_arena: &Arena<AExpr>,
) -> bool {
    match expr_arena.get(node.0) {
        AExpr::Column(name) => input_schema.contains(name.as_ref()),
        // Invariant of `ColumnNode`
        _ => unreachable!(),
    }
}

pub(crate) fn aexprs_to_schema<I: IntoIterator<Item = K>, K: Into<Node>>(
    expr: I,
    schema: &Schema,
    ctxt: Context,
    arena: &Arena<AExpr>,
) -> Schema {
    expr.into_iter()
        .map(|node| {
            arena
                .get(node.into())
                .to_field(schema, ctxt, arena)
                .unwrap()
        })
        .collect()
}

pub(crate) fn expr_irs_to_schema<I: IntoIterator<Item = K>, K: AsRef<ExprIR>>(
    expr: I,
    schema: &Schema,
    ctxt: Context,
    arena: &Arena<AExpr>,
) -> Schema {
    expr.into_iter()
        .map(|e| {
            let e = e.as_ref();
            let mut field = e.field(schema, ctxt, arena).expect("should be resolved");

            // TODO! (can this be removed?)
            if let Some(name) = e.get_alias() {
                field.name = name.clone()
            }
            field.dtype = field.dtype.materialize_unknown(true).unwrap();
            field
        })
        .collect()
}

/// Concatenate multiple schemas into one, disallowing duplicate field names
pub fn merge_schemas(schemas: &[SchemaRef]) -> PolarsResult<Schema> {
    let schema_size = schemas.iter().map(|schema| schema.len()).sum();
    let mut merged_schema = Schema::with_capacity(schema_size);

    for schema in schemas {
        schema.iter().try_for_each(|(name, dtype)| {
            if merged_schema.with_column(name.clone(), dtype.clone()).is_none() {
                Ok(())
            } else {
                Err(polars_err!(Duplicate: "Column with name '{}' has more than one occurrence", name))
            }
        })?;
    }

    Ok(merged_schema)
}

/// Rename all reference to the column in `map` with their corresponding new name.
pub fn rename_columns(
    node: Node,
    expr_arena: &mut Arena<AExpr>,
    map: &PlIndexMap<PlSmallStr, PlSmallStr>,
) -> Node {
    struct RenameColumns<'a>(&'a PlIndexMap<PlSmallStr, PlSmallStr>);
    impl RewritingVisitor for RenameColumns<'_> {
        type Node = AexprNode;
        type Arena = Arena<AExpr>;

        fn mutate(
            &mut self,
            node: Self::Node,
            arena: &mut Self::Arena,
        ) -> PolarsResult<Self::Node> {
            if let AExpr::Column(name) = arena.get(node.node()) {
                if let Some(new_name) = self.0.get(name) {
                    return Ok(AexprNode::new(arena.add(AExpr::Column(new_name.clone()))));
                }
            }

            Ok(node)
        }
    }

    AexprNode::new(node)
        .rewrite(&mut RenameColumns(map), expr_arena)
        .unwrap()
        .node()
}
