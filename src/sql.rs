use anyhow::{Result, anyhow, bail};
use roaring::RoaringBitmap;
use sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Query, Select, SetExpr,
    SetOperator, Statement, UnaryOperator, Value, ValueWithSpan,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::engine::Engine;
use crate::model::{DistanceMetric, QueryResult};

pub fn execute_sql(engine: &Engine, sql: &str) -> Result<QueryResult> {
    let dialect = GenericDialect {};
    let statements = Parser::parse_sql(&dialect, sql)?;
    if statements.len() != 1 {
        bail!("expected exactly one SQL statement");
    }

    let statement = &statements[0];
    let ids = match statement {
        Statement::Query(query) => execute_query(engine, query)?,
        _ => bail!("only SELECT/QUERY statements are supported"),
    };

    Ok(QueryResult {
        ids: ids.into_iter().collect(),
    })
}

fn execute_query(engine: &Engine, query: &Query) -> Result<RoaringBitmap> {
    execute_set_expr(engine, &query.body)
}

fn execute_set_expr(engine: &Engine, body: &SetExpr) -> Result<RoaringBitmap> {
    match body {
        SetExpr::Select(select) => execute_select(engine, select),
        SetExpr::SetOperation {
            left, op, right, ..
        } => {
            let left_bitmap = execute_set_expr(engine, left)?;
            let right_bitmap = execute_set_expr(engine, right)?;
            let out = match op {
                SetOperator::Union => &left_bitmap | &right_bitmap,
                SetOperator::Intersect => &left_bitmap & &right_bitmap,
                SetOperator::Except | SetOperator::Minus => &left_bitmap - &right_bitmap,
            };
            Ok(out)
        }
        SetExpr::Query(query) => execute_query(engine, query),
        SetExpr::Values(_) => bail!("VALUES is not supported"),
        _ => bail!("unsupported SELECT body in scaffold"),
    }
}

fn execute_select(engine: &Engine, select: &Select) -> Result<RoaringBitmap> {
    let Some(from) = select.from.first() else {
        bail!("missing FROM clause");
    };
    let table_name = from.relation.to_string().to_lowercase();
    if table_name != "docs" {
        bail!("only table `docs` is supported in scaffold");
    }

    let all = engine.all_bitmap();
    match &select.selection {
        Some(expr) => eval_predicate(engine, expr, &all),
        None => Ok(all),
    }
}

fn eval_predicate(engine: &Engine, expr: &Expr, universe: &RoaringBitmap) -> Result<RoaringBitmap> {
    match expr {
        Expr::BinaryOp { left, op, right } => match op {
            BinaryOperator::And => {
                let l = eval_predicate(engine, left, universe)?;
                let r = eval_predicate(engine, right, universe)?;
                Ok(&l & &r)
            }
            BinaryOperator::Or => {
                let l = eval_predicate(engine, left, universe)?;
                let r = eval_predicate(engine, right, universe)?;
                Ok(&l | &r)
            }
            BinaryOperator::Eq => eval_eq_predicate(engine, left, right),
            _ => bail!("unsupported binary operator in WHERE: {op:?}"),
        },
        Expr::Nested(inner) => eval_predicate(engine, inner, universe),
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr,
        } => {
            let nested = eval_predicate(engine, expr, universe)?;
            Ok(universe - &nested)
        }
        Expr::Function(function) => eval_vector_function(engine, function, None),
        _ => bail!("unsupported WHERE expression in scaffold: {expr:?}"),
    }
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
) -> Result<RoaringBitmap> {
    let fn_name = function.name.to_string().to_lowercase();
    let args = function_args_as_exprs(&function.args)?;
    if args.len() < 2 {
        bail!("vector function expects at least 2 args: vector_text, k");
    }
    let vector_text = extract_literal_as_string(args[0])?;
    let query = parse_vector_literal(&vector_text)?;
    let k_text = extract_literal_as_string(args[1])?;
    let k: usize = k_text.parse()?;
    let metric = match fn_name.as_str() {
        "vector_topk_cosine" => DistanceMetric::Cosine,
        "vector_topk_l2" => DistanceMetric::L2,
        "vector_topk_jaccard" => DistanceMetric::Jaccard,
        _ => bail!("unknown vector function `{fn_name}`"),
    };
    Ok(engine.vector_topk_bitmap(&query, k, metric, candidate_filter))
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
    use crate::model::Row;

    fn row(id: u32, region: &str, tier: &str, vector: Vec<f32>) -> Row {
        let mut tags = BTreeMap::new();
        tags.insert("region".to_string(), region.to_string());
        tags.insert("tier".to_string(), tier.to_string());
        Row { id, tags, vector }
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
}
