use anyhow::{Result, anyhow, bail};
use roaring::RoaringBitmap;
use sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Query, Select, SetExpr,
    SetOperator, Statement, UnaryOperator, Value, ValueWithSpan,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::engine::Engine;
use crate::inversion;
use crate::memory::MemoryStore;
use crate::model::{DistanceMetric, QueryResult};

pub fn execute_sql(engine: &Engine, sql: &str) -> Result<QueryResult> {
    execute_sql_with_embed(engine, None, sql, None)
}

pub fn execute_sql_with_embed(
    engine: &Engine,
    store: Option<&MemoryStore>,
    sql: &str,
    shivvr_url: Option<&str>,
) -> Result<QueryResult> {
    let dialect = GenericDialect {};
    let statements = Parser::parse_sql(&dialect, sql)?;
    if statements.len() != 1 {
        bail!("expected exactly one SQL statement");
    }

    let statement = &statements[0];
    let ids = match statement {
        Statement::Query(query) => execute_query(engine, store, query, shivvr_url)?,
        _ => bail!("only SELECT/QUERY statements are supported"),
    };

    Ok(QueryResult {
        ids: ids.into_iter().collect(),
    })
}

fn execute_query(
    engine: &Engine,
    store: Option<&MemoryStore>,
    query: &Query,
    shivvr_url: Option<&str>,
) -> Result<RoaringBitmap> {
    execute_set_expr(engine, store, &query.body, shivvr_url)
}

fn execute_set_expr(
    engine: &Engine,
    store: Option<&MemoryStore>,
    body: &SetExpr,
    shivvr_url: Option<&str>,
) -> Result<RoaringBitmap> {
    match body {
        SetExpr::Select(select) => execute_select(engine, store, select, shivvr_url),
        SetExpr::SetOperation {
            left, op, right, ..
        } => {
            let left_bitmap = execute_set_expr(engine, store, left, shivvr_url)?;
            let right_bitmap = execute_set_expr(engine, store, right, shivvr_url)?;
            let out = match op {
                SetOperator::Union => &left_bitmap | &right_bitmap,
                SetOperator::Intersect => &left_bitmap & &right_bitmap,
                SetOperator::Except | SetOperator::Minus => &left_bitmap - &right_bitmap,
            };
            Ok(out)
        }
        SetExpr::Query(query) => execute_query(engine, store, query, shivvr_url),
        SetExpr::Values(_) => bail!("VALUES is not supported"),
        _ => bail!("unsupported SELECT body in scaffold"),
    }
}

fn execute_select(
    engine: &Engine,
    store: Option<&MemoryStore>,
    select: &Select,
    shivvr_url: Option<&str>,
) -> Result<RoaringBitmap> {
    let Some(from) = select.from.first() else {
        bail!("missing FROM clause");
    };
    let table_name = from.relation.to_string().to_lowercase();
    if table_name != "docs" {
        bail!("only table `docs` is supported in scaffold");
    }

    let all = engine.all_bitmap();
    match &select.selection {
        Some(expr) => eval_predicate(engine, store, expr, &all, shivvr_url),
        None => Ok(all),
    }
}

fn eval_predicate(
    engine: &Engine,
    store: Option<&MemoryStore>,
    expr: &Expr,
    universe: &RoaringBitmap,
    shivvr_url: Option<&str>,
) -> Result<RoaringBitmap> {
    match expr {
        Expr::BinaryOp { left, op, right } => match op {
            BinaryOperator::And => {
                let l = eval_predicate(engine, store, left, universe, shivvr_url)?;
                let r = eval_predicate(engine, store, right, universe, shivvr_url)?;
                Ok(&l & &r)
            }
            BinaryOperator::Or => {
                let l = eval_predicate(engine, store, left, universe, shivvr_url)?;
                let r = eval_predicate(engine, store, right, universe, shivvr_url)?;
                Ok(&l | &r)
            }
            BinaryOperator::Eq => {
                // Check if this is a virtual column comparison (column on the left)
                if let Some(field) = extract_identifier(left) {
                    if is_virtual_column(&field) {
                        let rhs = extract_literal_as_string(right)?;
                        return eval_virtual_predicate(&field, op, &rhs, universe, store);
                    }
                }
                eval_eq_predicate(engine, left, right)
            }
            BinaryOperator::Lt
            | BinaryOperator::Gt
            | BinaryOperator::LtEq
            | BinaryOperator::GtEq => {
                let field = extract_identifier(left)
                    .ok_or_else(|| anyhow!("comparison operators require `column op value`"))?;
                let rhs = extract_literal_as_string(right)?;
                if is_virtual_column(&field) {
                    return eval_virtual_predicate(&field, op, &rhs, universe, store);
                }
                if NUMERIC_REF_FIELDS.contains(&field.as_str()) {
                    let n: u32 = rhs.parse().map_err(|_| {
                        anyhow!("expected integer for ref field `{field}`, got `{rhs}`")
                    })?;
                    let (low, high, inc_low, inc_high) = match op {
                        BinaryOperator::Lt => (None, Some(n), true, false),
                        BinaryOperator::LtEq => (None, Some(n), true, true),
                        BinaryOperator::Gt => (Some(n), None, false, true),
                        BinaryOperator::GtEq => (Some(n), None, true, true),
                        _ => unreachable!(),
                    };
                    let bm = engine.bitmap_for_tag_range(&field, low, high, inc_low, inc_high);
                    return Ok(&bm & universe);
                }
                bail!(
                    "comparison operators (< > <= >=) require a virtual column or numeric ref \
                     field (page/chapter/volume), not tag `{field}`"
                );
            }
            BinaryOperator::NotEq => {
                let field = extract_identifier(left)
                    .ok_or_else(|| anyhow!("`!=` requires `column != value`"))?;
                let rhs = extract_literal_as_string(right)?;
                if is_virtual_column(&field) {
                    return eval_virtual_predicate(&field, op, &rhs, universe, store);
                }
                if NUMERIC_REF_FIELDS.contains(&field.as_str())
                    || STRING_REF_FIELDS.contains(&field.as_str())
                {
                    let bm = engine.bitmap_for_tag_eq(&field, &rhs);
                    return Ok(universe - &bm);
                }
                bail!(
                    "`!=` is only supported on virtual columns or ref fields, not tag `{field}`"
                );
            }
            _ => bail!("unsupported binary operator in WHERE: {op:?}"),
        },
        Expr::Nested(inner) => eval_predicate(engine, store, inner, universe, shivvr_url),
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr,
        } => {
            let nested = eval_predicate(engine, store, expr, universe, shivvr_url)?;
            Ok(universe - &nested)
        }
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => {
            let field = extract_identifier(expr)
                .ok_or_else(|| anyhow!("BETWEEN requires a column identifier"))?;
            if !NUMERIC_REF_FIELDS.contains(&field.as_str()) {
                bail!(
                    "BETWEEN is only supported on numeric ref fields (page/chapter/volume), \
                     not `{field}`"
                );
            }
            let lo: u32 = extract_literal_as_string(low)?.parse().map_err(|_| {
                anyhow!("BETWEEN lower bound must be an integer for `{field}`")
            })?;
            let hi: u32 = extract_literal_as_string(high)?.parse().map_err(|_| {
                anyhow!("BETWEEN upper bound must be an integer for `{field}`")
            })?;
            let bm = engine.bitmap_for_tag_range(&field, Some(lo), Some(hi), true, true);
            let result = &bm & universe;
            if *negated {
                Ok(universe - &result)
            } else {
                Ok(result)
            }
        }
        Expr::Function(function) => eval_vector_function(engine, function, None, shivvr_url),
        _ => bail!("unsupported WHERE expression in scaffold: {expr:?}"),
    }
}

// ---------------------------------------------------------------------------
// Reference fields — bibliographic metadata flattened into Row.tags so they
// participate in the engine's bitmap index. Numeric fields can be range-queried
// via `<`, `<=`, `>`, `>=`, and `BETWEEN`. Both numeric and string ref fields
// support `=` and `!=`.
// ---------------------------------------------------------------------------

const NUMERIC_REF_FIELDS: &[&str] = &["page", "chapter", "volume"];
const STRING_REF_FIELDS: &[&str] = &["book", "author", "section", "url", "filename", "chapter_id"];

// ---------------------------------------------------------------------------
// Virtual columns — thermodynamic fields from MemoryStore
// ---------------------------------------------------------------------------

const VIRTUAL_COLUMNS: &[&str] = &[
    "fidelity",
    "alpha",
    "decay_alpha",
    "effective_alpha",
    "state",
    "keystone",
    "recalls",
    "recall_count",
    "importance",
    "created_at",
    "last_recalled",
    "staleness",
    "age",
    "emotion",
    "consolidation_depth",
];

fn is_virtual_column(name: &str) -> bool {
    VIRTUAL_COLUMNS.contains(&name)
}

fn eval_virtual_predicate(
    column: &str,
    op: &BinaryOperator,
    rhs_str: &str,
    universe: &RoaringBitmap,
    store: Option<&MemoryStore>,
) -> Result<RoaringBitmap> {
    let store = store.ok_or_else(|| {
        anyhow!("virtual column `{column}` requires memory store")
    })?;

    let mut result = RoaringBitmap::new();
    for id in universe.iter() {
        let Some(record) = store.get(id) else {
            continue; // row exists in engine but has no memory record — skip
        };
        let matches = match column {
            "fidelity" => {
                let rhs: f32 = rhs_str.parse().map_err(|_| anyhow!("invalid f32 for fidelity: {rhs_str}"))?;
                cmp_f32(record.fidelity, op, rhs)?
            }
            "alpha" | "decay_alpha" => {
                let rhs: f32 = rhs_str.parse().map_err(|_| anyhow!("invalid f32 for {column}: {rhs_str}"))?;
                cmp_f32(record.decay_alpha, op, rhs)?
            }
            "effective_alpha" => {
                let rhs: f32 = rhs_str.parse().map_err(|_| anyhow!("invalid f32 for effective_alpha: {rhs_str}"))?;
                cmp_f32(record.effective_alpha(), op, rhs)?
            }
            "state" => {
                let lhs = format!("{:?}", record.state);
                cmp_str(&lhs, op, rhs_str)?
            }
            "keystone" => {
                let rhs: bool = rhs_str.parse().map_err(|_| anyhow!("invalid bool for keystone: {rhs_str}"))?;
                cmp_bool(record.keystone, op, rhs)?
            }
            "recalls" | "recall_count" => {
                let rhs: u64 = rhs_str.parse().map_err(|_| anyhow!("invalid integer for {column}: {rhs_str}"))?;
                cmp_u64(record.recall_count as u64, op, rhs)?
            }
            "importance" => {
                let rhs: f32 = rhs_str.parse().map_err(|_| anyhow!("invalid f32 for importance: {rhs_str}"))?;
                cmp_f32(record.importance, op, rhs)?
            }
            "created_at" => {
                let rhs: u64 = rhs_str.parse().map_err(|_| anyhow!("invalid u64 for created_at: {rhs_str}"))?;
                cmp_u64(record.created_at, op, rhs)?
            }
            "last_recalled" => {
                let rhs: u64 = rhs_str.parse().map_err(|_| anyhow!("invalid u64 for last_recalled: {rhs_str}"))?;
                cmp_u64(record.last_recalled, op, rhs)?
            }
            "staleness" => {
                let rhs: u64 = rhs_str.parse().map_err(|_| anyhow!("invalid u64 for staleness: {rhs_str}"))?;
                cmp_u64(record.staleness(), op, rhs)?
            }
            "age" => {
                let rhs: u64 = rhs_str.parse().map_err(|_| anyhow!("invalid u64 for age: {rhs_str}"))?;
                cmp_u64(record.age(), op, rhs)?
            }
            "emotion" => {
                let lhs = record
                    .emotion
                    .as_ref()
                    .map(|e| e.primary.as_str())
                    .unwrap_or("null");
                cmp_str(lhs, op, rhs_str)?
            }
            "consolidation_depth" => {
                let rhs: u64 = rhs_str.parse().map_err(|_| anyhow!("invalid integer for consolidation_depth: {rhs_str}"))?;
                cmp_u64(record.consolidation_depth as u64, op, rhs)?
            }
            _ => bail!("unknown virtual column `{column}`"),
        };
        if matches {
            result.insert(id);
        }
    }
    Ok(result)
}

fn cmp_f32(lhs: f32, op: &BinaryOperator, rhs: f32) -> Result<bool> {
    Ok(match op {
        BinaryOperator::Eq => (lhs - rhs).abs() < f32::EPSILON,
        BinaryOperator::NotEq => (lhs - rhs).abs() >= f32::EPSILON,
        BinaryOperator::Lt => lhs < rhs,
        BinaryOperator::Gt => lhs > rhs,
        BinaryOperator::LtEq => lhs <= rhs,
        BinaryOperator::GtEq => lhs >= rhs,
        _ => bail!("unsupported operator for f32 comparison: {op:?}"),
    })
}

fn cmp_u64(lhs: u64, op: &BinaryOperator, rhs: u64) -> Result<bool> {
    Ok(match op {
        BinaryOperator::Eq => lhs == rhs,
        BinaryOperator::NotEq => lhs != rhs,
        BinaryOperator::Lt => lhs < rhs,
        BinaryOperator::Gt => lhs > rhs,
        BinaryOperator::LtEq => lhs <= rhs,
        BinaryOperator::GtEq => lhs >= rhs,
        _ => bail!("unsupported operator for u64 comparison: {op:?}"),
    })
}

fn cmp_str(lhs: &str, op: &BinaryOperator, rhs: &str) -> Result<bool> {
    Ok(match op {
        BinaryOperator::Eq => lhs == rhs,
        BinaryOperator::NotEq => lhs != rhs,
        BinaryOperator::Lt => lhs < rhs,
        BinaryOperator::Gt => lhs > rhs,
        BinaryOperator::LtEq => lhs <= rhs,
        BinaryOperator::GtEq => lhs >= rhs,
        _ => bail!("unsupported operator for string comparison: {op:?}"),
    })
}

fn cmp_bool(lhs: bool, op: &BinaryOperator, rhs: bool) -> Result<bool> {
    Ok(match op {
        BinaryOperator::Eq => lhs == rhs,
        BinaryOperator::NotEq => lhs != rhs,
        _ => bail!("unsupported operator for bool comparison: {op:?}"),
    })
}

fn eval_eq_predicate(engine: &Engine, left: &Expr, right: &Expr) -> Result<RoaringBitmap> {
    if let Some(field) = extract_identifier(left) {
        let value = extract_literal_as_string(right)?;
        return Ok(engine.bitmap_for_tag_eq(&field, &value));
    }
    if let Some(field) = extract_identifier(right) {
        let value = extract_literal_as_string(left)?;
        return Ok(engine.bitmap_for_tag_eq(&field, &value));
    }
    bail!("expected `field = value` predicate")
}

fn eval_vector_function(
    engine: &Engine,
    function: &sqlparser::ast::Function,
    candidate_filter: Option<&RoaringBitmap>,
    shivvr_url: Option<&str>,
) -> Result<RoaringBitmap> {
    let fn_name = function.name.to_string().to_lowercase();
    let args = function_args_as_exprs(&function.args)?;

    // tag_jaccard('field1', 'val1', 'field2', 'val2')
    // Returns the intersection of the two tag-value bitmaps — rows present in both sets.
    if fn_name == "tag_jaccard" {
        if args.len() != 4 {
            bail!("tag_jaccard expects 4 args: field1, val1, field2, val2");
        }
        let field1 = extract_literal_as_string(args[0])?;
        let val1   = extract_literal_as_string(args[1])?;
        let field2 = extract_literal_as_string(args[2])?;
        let val2   = extract_literal_as_string(args[3])?;
        let a = engine.bitmap_for_tag_eq(&field1, &val1);
        let b = engine.bitmap_for_tag_eq(&field2, &val2);
        let result = &a & &b;
        return Ok(match candidate_filter {
            Some(f) => &result & f,
            None => result,
        });
    }

    if args.len() < 2 {
        bail!("vector function expects at least 2 args: vector_or_embed, k");
    }

    // Resolve first arg: either embed('text') function call or raw vector literal
    let query = resolve_vector_arg(args[0], shivvr_url)?;

    let k_text = extract_literal_as_string(args[1])?;
    let k: usize = k_text.parse()?;
    let metric = match fn_name.as_str() {
        "vector_topk_cosine" => DistanceMetric::Cosine,
        "vector_topk_l2" => DistanceMetric::L2,
        _ => bail!("unknown vector function `{fn_name}`"),
    };
    Ok(engine.vector_topk_bitmap(&query, k, metric, candidate_filter))
}

/// Resolve the first argument of a vector function.
/// Handles both `embed('text')` (calls shivvr) and raw vector literals `'[1,0,0]'`.
fn resolve_vector_arg(expr: &Expr, shivvr_url: Option<&str>) -> Result<Vec<f32>> {
    match expr {
        // embed('text') — call shivvr to get the vector
        Expr::Function(func) => {
            let name = func.name.to_string().to_lowercase();
            if name != "embed" {
                bail!("unsupported function `{name}` in vector arg; use embed('text')");
            }
            let inner_args = function_args_as_exprs(&func.args)?;
            if inner_args.is_empty() {
                bail!("embed() requires a text argument");
            }
            let text = extract_literal_as_string(inner_args[0])?;
            let url = shivvr_url.ok_or_else(|| {
                anyhow!("embed() requires SHIVVR_URL but no embedding service is configured")
            })?;
            inversion::embed_text(url, &text)
                .ok_or_else(|| anyhow!("embed() failed: shivvr did not return a vector for {:?}", text))
        }
        // Raw vector literal: '[1,0,0]' or '1,0,0'
        _ => {
            let vector_text = extract_literal_as_string(expr)?;
            parse_vector_literal(&vector_text)
        }
    }
}

fn function_args_as_exprs(args: &FunctionArguments) -> Result<Vec<&Expr>> {
    let FunctionArguments::List(list) = args else {
        bail!("unsupported function argument list");
    };
    let mut out = Vec::with_capacity(list.args.len());
    for arg in &list.args {
        let expr = match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => expr,
            FunctionArg::Named { arg, .. } | FunctionArg::ExprNamed { arg, .. } => match arg {
                FunctionArgExpr::Expr(expr) => expr,
                _ => bail!("unsupported named function arg"),
            },
            _ => bail!("unsupported function arg type"),
        };
        out.push(expr);
    }
    Ok(out)
}

fn extract_identifier(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.to_lowercase()),
        Expr::CompoundIdentifier(idents) => idents.last().map(|id| id.value.to_lowercase()),
        _ => None,
    }
}

fn extract_literal_as_string(expr: &Expr) -> Result<String> {
    match expr {
        Expr::Value(vws) => value_to_string(&vws.value),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => {
            if let Expr::Value(ValueWithSpan {
                value: Value::Number(n, _),
                ..
            }) = expr.as_ref()
            {
                return Ok(format!("-{n}"));
            }
            bail!("unsupported unary literal")
        }
        _ => bail!("expected literal expression"),
    }
}

fn value_to_string(value: &Value) -> Result<String> {
    match value {
        Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => Ok(s.clone()),
        Value::Number(n, _) => Ok(n.clone()),
        Value::Boolean(v) => Ok(v.to_string()),
        _ => Err(anyhow!("unsupported literal value")),
    }
}

fn parse_vector_literal(text: &str) -> Result<Vec<f32>> {
    // Accept either "[1,2,3]" or "1,2,3".
    let trimmed = text.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        bail!("empty vector literal");
    }
    let mut vector = Vec::new();
    for item in trimmed.split(',') {
        vector.push(item.trim().parse::<f32>()?);
    }
    Ok(vector)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::memory::{MemoryRecord, MemoryStore};
    use crate::model::Row;

    fn row(id: u32, region: &str, tier: &str, vector: Vec<f32>) -> Row {
        let mut tags = BTreeMap::new();
        tags.insert("region".to_string(), region.to_string());
        tags.insert("tier".to_string(), tier.to_string());
        Row {
            id,
            tags,
            vector,
            refs: None,
        }
    }

    fn tagged_row(id: u32, tags: &[(&str, &str)], vector: Vec<f32>) -> Row {
        let mut map = BTreeMap::new();
        for (k, v) in tags {
            map.insert(k.to_string(), v.to_string());
        }
        Row {
            id,
            tags: map,
            vector,
            refs: None,
        }
    }

    /// Build an engine + memory store with records at epoch 0 timestamps.
    /// Returns (engine, store) with records having the given fidelities.
    fn setup_with_fidelities(entries: &[(u32, f32)]) -> (Engine, MemoryStore) {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        for &(id, fidelity) in entries {
            engine
                .upsert(row(id, "us", "gold", vec![1.0, 0.0, 0.0]))
                .unwrap();
            let mut rec = MemoryRecord::new_at(id, 0);
            rec.fidelity = fidelity;
            store.insert(rec);
        }
        (engine, store)
    }

    #[test]
    fn set_ops_query_executes() {
        let mut engine = Engine::new();
        engine
            .upsert(row(1, "us", "gold", vec![1.0, 0.0, 0.0]))
            .unwrap();
        engine
            .upsert(row(2, "us", "silver", vec![0.5, 0.5, 0.0]))
            .unwrap();
        engine
            .upsert(row(3, "eu", "gold", vec![0.0, 1.0, 0.0]))
            .unwrap();

        let sql =
            "SELECT id FROM docs WHERE region='us' INTERSECT SELECT id FROM docs WHERE tier='gold'";
        let result = execute_sql(&engine, sql).unwrap();
        assert_eq!(result.ids, vec![1]);
    }

    #[test]
    fn vector_function_query_executes() {
        let mut engine = Engine::new();
        engine
            .upsert(row(1, "us", "gold", vec![1.0, 0.0, 0.0]))
            .unwrap();
        engine
            .upsert(row(2, "us", "silver", vec![0.5, 0.5, 0.0]))
            .unwrap();
        engine
            .upsert(row(3, "eu", "gold", vec![0.0, 1.0, 0.0]))
            .unwrap();

        let sql = "SELECT id FROM docs WHERE vector_topk_cosine('[1,0,0]', 2)";
        let result = execute_sql(&engine, sql).unwrap();
        assert_eq!(result.ids.len(), 2);
        assert_eq!(result.ids[0], 1);
    }

    // --- Virtual column tests ---

    #[test]
    fn virtual_fidelity_filter() {
        let (engine, store) = setup_with_fidelities(&[(1, 0.9), (2, 0.5), (3, 0.7)]);
        let sql = "SELECT id FROM docs WHERE fidelity < 0.8";
        let result = execute_sql_with_embed(&engine, Some(&store), sql, None).unwrap();
        let mut ids = result.ids.clone();
        ids.sort();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn virtual_state_filter() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();

        for id in 1..=3 {
            engine
                .upsert(row(id, "us", "gold", vec![1.0, 0.0, 0.0]))
                .unwrap();
        }

        let r1 = MemoryRecord::new_at(1, 0); // Active
        let mut r2 = MemoryRecord::new_at(2, 0);
        r2.forgive(); // Forgiven
        let mut r3 = MemoryRecord::new_at(3, 0);
        r3.forgive();
        r3.archive(); // Archived

        store.insert(r1);
        store.insert(r2);
        store.insert(r3);

        let sql = "SELECT id FROM docs WHERE state = 'Active'";
        let result = execute_sql_with_embed(&engine, Some(&store), sql, None).unwrap();
        assert_eq!(result.ids, vec![1]);

        let sql = "SELECT id FROM docs WHERE state = 'Forgiven'";
        let result = execute_sql_with_embed(&engine, Some(&store), sql, None).unwrap();
        assert_eq!(result.ids, vec![2]);
    }

    #[test]
    fn virtual_keystone_filter() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();

        for id in 1..=3 {
            engine
                .upsert(row(id, "us", "gold", vec![1.0, 0.0, 0.0]))
                .unwrap();
        }

        let mut r1 = MemoryRecord::new_at(1, 0);
        r1.keystone = true;
        let r2 = MemoryRecord::new_at(2, 0);
        let mut r3 = MemoryRecord::new_at(3, 0);
        r3.keystone = true;

        store.insert(r1);
        store.insert(r2);
        store.insert(r3);

        let sql = "SELECT id FROM docs WHERE keystone = true";
        let result = execute_sql_with_embed(&engine, Some(&store), sql, None).unwrap();
        let mut ids = result.ids.clone();
        ids.sort();
        assert_eq!(ids, vec![1, 3]);
    }

    #[test]
    fn virtual_staleness_filter() {
        // Records created at epoch 0 will have huge staleness (current time - 0)
        let (engine, store) = setup_with_fidelities(&[(1, 1.0), (2, 1.0)]);
        let sql = "SELECT id FROM docs WHERE staleness > 0";
        let result = execute_sql_with_embed(&engine, Some(&store), sql, None).unwrap();
        let mut ids = result.ids.clone();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn virtual_compound_filter() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();

        for id in 1..=4 {
            engine
                .upsert(row(id, "us", "gold", vec![1.0, 0.0, 0.0]))
                .unwrap();
        }

        // id=1: fidelity=0.9, keystone=true
        let mut r1 = MemoryRecord::new_at(1, 0);
        r1.fidelity = 0.9;
        r1.keystone = true;
        // id=2: fidelity=0.9, keystone=false
        let mut r2 = MemoryRecord::new_at(2, 0);
        r2.fidelity = 0.9;
        // id=3: fidelity=0.3, keystone=false
        let mut r3 = MemoryRecord::new_at(3, 0);
        r3.fidelity = 0.3;
        // id=4: fidelity=0.6, keystone=false
        let mut r4 = MemoryRecord::new_at(4, 0);
        r4.fidelity = 0.6;

        store.insert(r1);
        store.insert(r2);
        store.insert(r3);
        store.insert(r4);

        let sql = "SELECT id FROM docs WHERE fidelity > 0.5 AND keystone = false";
        let result = execute_sql_with_embed(&engine, Some(&store), sql, None).unwrap();
        let mut ids = result.ids.clone();
        ids.sort();
        assert_eq!(ids, vec![2, 4]);
    }

    #[test]
    fn virtual_mixed_with_tags() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();

        // id=1: channel=hearing, fidelity=0.9
        engine
            .upsert(tagged_row(
                1,
                &[("channel", "hearing")],
                vec![1.0, 0.0, 0.0],
            ))
            .unwrap();
        let mut r1 = MemoryRecord::new_at(1, 0);
        r1.fidelity = 0.9;
        store.insert(r1);

        // id=2: channel=hearing, fidelity=0.3
        engine
            .upsert(tagged_row(
                2,
                &[("channel", "hearing")],
                vec![0.0, 1.0, 0.0],
            ))
            .unwrap();
        let mut r2 = MemoryRecord::new_at(2, 0);
        r2.fidelity = 0.3;
        store.insert(r2);

        // id=3: channel=vision, fidelity=0.9
        engine
            .upsert(tagged_row(
                3,
                &[("channel", "vision")],
                vec![0.0, 0.0, 1.0],
            ))
            .unwrap();
        let mut r3 = MemoryRecord::new_at(3, 0);
        r3.fidelity = 0.9;
        store.insert(r3);

        let sql = "SELECT id FROM docs WHERE channel = 'hearing' AND fidelity > 0.5";
        let result = execute_sql_with_embed(&engine, Some(&store), sql, None).unwrap();
        assert_eq!(result.ids, vec![1]);
    }

    #[test]
    fn existing_tag_queries_still_work() {
        // Regression: tag queries must work identically with store=None
        let mut engine = Engine::new();
        engine
            .upsert(row(1, "us", "gold", vec![1.0, 0.0, 0.0]))
            .unwrap();
        engine
            .upsert(row(2, "eu", "silver", vec![0.0, 1.0, 0.0]))
            .unwrap();

        let sql = "SELECT id FROM docs WHERE region = 'us'";
        let result = execute_sql_with_embed(&engine, None, sql, None).unwrap();
        assert_eq!(result.ids, vec![1]);
    }

    #[test]
    fn virtual_column_requires_store() {
        let mut engine = Engine::new();
        engine
            .upsert(row(1, "us", "gold", vec![1.0, 0.0, 0.0]))
            .unwrap();

        let sql = "SELECT id FROM docs WHERE fidelity > 0.5";
        let result = execute_sql_with_embed(&engine, None, sql, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("requires memory store"),
            "unexpected error: {err}"
        );
    }
}
