use std::collections::HashMap;

use gemgraph_graph::{
    Direction, Edge, GraphReadTxn, GraphWriteTxn, Node, NodeId, EdgeId, Value,
};

use crate::ast::*;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::CypherError;

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub stats: QueryStats,
}

#[derive(Debug, Clone, Default)]
pub struct QueryStats {
    pub nodes_created: u64,
    pub edges_created: u64,
    pub nodes_deleted: u64,
    pub edges_deleted: u64,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn execute_read(query: &str, txn: &GraphReadTxn) -> Result<QueryResult, CypherError> {
    let stmt = parse_query(query)?;
    match stmt {
        Statement::Match(m) => execute_match_read(m, txn),
        Statement::Create(_) => Err(CypherError::Execution(
            "CREATE requires a write transaction".into(),
        )),
        Statement::Merge(_) => Err(CypherError::Execution(
            "MERGE requires a write transaction".into(),
        )),
    }
}

pub fn execute_write(query: &str, txn: &mut GraphWriteTxn) -> Result<QueryResult, CypherError> {
    let stmt = parse_query(query)?;
    match stmt {
        Statement::Match(m) => {
            if m.delete_clause.is_some() {
                execute_match_delete(m, txn)
            } else {
                execute_match_write(m, txn)
            }
        }
        Statement::Create(c) => execute_create(c, txn),
        Statement::Merge(m) => execute_merge(m, txn),
    }
}

fn parse_query(query: &str) -> Result<Statement, CypherError> {
    let mut lexer = Lexer::new(query);
    let tokens = lexer.tokenize()?;
    let mut parser = Parser::new(tokens);
    parser.parse()
}

// ---------------------------------------------------------------------------
// Binding environment — maps variable names to node/edge IDs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Binding {
    Node(NodeId),
    Edge(EdgeId),
}

type Env = HashMap<String, Binding>;

// ---------------------------------------------------------------------------
// MATCH + RETURN (read path)
// ---------------------------------------------------------------------------

fn execute_match_read(m: MatchClause, txn: &GraphReadTxn) -> Result<QueryResult, CypherError> {
    let limit = m.return_clause.as_ref().and_then(|r| r.limit);
    let envs = expand_pattern_read(&m.pattern, txn)?;

    // Fuse WHERE filter + LIMIT to avoid materializing all results
    let envs: Vec<Env> = if let Some(ref where_expr) = m.where_clause {
        let iter = envs.into_iter()
            .filter(|env| eval_where_read(where_expr, env, txn).unwrap_or(false));
        if let Some(lim) = limit {
            iter.take(lim as usize).collect()
        } else {
            iter.collect()
        }
    } else if let Some(lim) = limit {
        envs.into_iter().take(lim as usize).collect()
    } else {
        envs
    };

    let ret = match m.return_clause {
        Some(r) => r,
        None => return Ok(QueryResult::default()),
    };

    project_results_read(&ret, &envs, txn)
}

/// Same as execute_match_read but using a GraphWriteTxn for reads within a write txn.
fn execute_match_write(m: MatchClause, txn: &GraphWriteTxn) -> Result<QueryResult, CypherError> {
    let limit = m.return_clause.as_ref().and_then(|r| r.limit);
    let envs = expand_pattern_write(&m.pattern, txn)?;

    let envs: Vec<Env> = if let Some(ref where_expr) = m.where_clause {
        let iter = envs.into_iter()
            .filter(|env| eval_where_write(where_expr, env, txn).unwrap_or(false));
        if let Some(lim) = limit {
            iter.take(lim as usize).collect()
        } else {
            iter.collect()
        }
    } else if let Some(lim) = limit {
        envs.into_iter().take(lim as usize).collect()
    } else {
        envs
    };

    let ret = match m.return_clause {
        Some(r) => r,
        None => return Ok(QueryResult::default()),
    };

    project_results_write(&ret, &envs, txn)
}

// ---------------------------------------------------------------------------
// Pattern expansion — read txn
// ---------------------------------------------------------------------------

/// Key used to track which node is the "current" one for edge expansion.
const LAST_NODE_KEY: &str = "__last_node__";

fn expand_pattern_read(
    pattern: &Pattern,
    txn: &GraphReadTxn,
) -> Result<Vec<Env>, CypherError> {
    let mut envs: Vec<Env> = vec![HashMap::new()];

    let mut i = 0;
    while i < pattern.elements.len() {
        match &pattern.elements[i] {
            PatternElement::Node(np) => {
                envs = expand_node_read(np, envs, txn)?;
                i += 1;
            }
            PatternElement::Edge(_) => {
                if i + 1 >= pattern.elements.len() {
                    return Err(CypherError::Execution("edge pattern not followed by node".into()));
                }
                let ep = match &pattern.elements[i] {
                    PatternElement::Edge(e) => e,
                    _ => unreachable!(),
                };
                let next_np = match &pattern.elements[i + 1] {
                    PatternElement::Node(n) => n,
                    _ => return Err(CypherError::Execution("expected node after edge".into())),
                };
                envs = expand_edge_read(ep, next_np, envs, txn)?;
                i += 2;
            }
        }
    }

    // Clean up internal tracking key
    for env in &mut envs {
        env.remove(LAST_NODE_KEY);
    }

    Ok(envs)
}

fn expand_node_read(
    np: &NodePattern,
    envs: Vec<Env>,
    txn: &GraphReadTxn,
) -> Result<Vec<Env>, CypherError> {
    // If we already have a binding for this variable, just filter
    if let Some(ref var) = np.variable {
        let has_existing = envs.first().map_or(false, |e| e.contains_key(var));
        if has_existing {
            let mut result = Vec::new();
            for mut env in envs {
                if let Some(Binding::Node(nid)) = env.get(var).cloned() {
                    let node = txn.get_node(nid).map_err(CypherError::Graph)?;
                    if let Some(node) = node {
                        if node_matches(np, &node) {
                            env.insert(LAST_NODE_KEY.to_string(), Binding::Node(nid));
                            result.push(env);
                        }
                    }
                }
            }
            return Ok(result);
        }
    }

    let candidates = if let Some(ref label) = np.label {
        txn.nodes_by_label(label).map_err(CypherError::Graph)?
    } else {
        return Err(CypherError::Execution(
            "first node pattern must have a label or be bound by a previous edge expansion".into(),
        ));
    };

    let has_prop_filter = !np.properties.is_empty();

    let mut result = Vec::new();
    for nid in candidates {
        // Skip loading full node when there are no property filters
        if has_prop_filter {
            let node = txn.get_node(nid).map_err(CypherError::Graph)?;
            if let Some(node) = node {
                if !node_matches(np, &node) {
                    continue;
                }
            } else {
                continue;
            }
        }
        for env in &envs {
            let mut new_env = env.clone();
            if let Some(ref var) = np.variable {
                new_env.insert(var.clone(), Binding::Node(nid));
            }
            new_env.insert(LAST_NODE_KEY.to_string(), Binding::Node(nid));
            result.push(new_env);
        }
    }
    Ok(result)
}

fn expand_edge_read(
    ep: &EdgePattern,
    next_np: &NodePattern,
    envs: Vec<Env>,
    txn: &GraphReadTxn,
) -> Result<Vec<Env>, CypherError> {
    let mut result = Vec::new();

    for env in &envs {
        let src_id = get_last_node_id(env)?;

        let directions = match ep.direction {
            EdgeDirection::Right => vec![Direction::Out],
            EdgeDirection::Left => vec![Direction::In],
            EdgeDirection::Both => vec![Direction::Out, Direction::In],
        };

        for dir in &directions {
            let neighbors = txn
                .neighbors(src_id, *dir, ep.edge_type.as_deref())
                .map_err(CypherError::Graph)?;

            for (edge_id, neighbor_id) in neighbors {
                let neighbor_node = txn.get_node(neighbor_id).map_err(CypherError::Graph)?;
                if let Some(neighbor_node) = neighbor_node {
                    if node_matches(next_np, &neighbor_node) {
                        let mut new_env = env.clone();
                        if let Some(ref var) = ep.variable {
                            new_env.insert(var.clone(), Binding::Edge(edge_id));
                        }
                        if let Some(ref var) = next_np.variable {
                            new_env.insert(var.clone(), Binding::Node(neighbor_id));
                        }
                        new_env.insert(LAST_NODE_KEY.to_string(), Binding::Node(neighbor_id));
                        result.push(new_env);
                    }
                }
            }
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Pattern expansion — write txn (duplicated because GraphWriteTxn has its own methods)
// ---------------------------------------------------------------------------

fn expand_pattern_write(
    pattern: &Pattern,
    txn: &GraphWriteTxn,
) -> Result<Vec<Env>, CypherError> {
    let mut envs: Vec<Env> = vec![HashMap::new()];
    let mut i = 0;
    while i < pattern.elements.len() {
        match &pattern.elements[i] {
            PatternElement::Node(np) => {
                envs = expand_node_write(np, envs, txn)?;
                i += 1;
            }
            PatternElement::Edge(_) => {
                if i + 1 >= pattern.elements.len() {
                    return Err(CypherError::Execution("edge pattern not followed by node".into()));
                }
                let ep = match &pattern.elements[i] {
                    PatternElement::Edge(e) => e,
                    _ => unreachable!(),
                };
                let next_np = match &pattern.elements[i + 1] {
                    PatternElement::Node(n) => n,
                    _ => return Err(CypherError::Execution("expected node after edge".into())),
                };
                envs = expand_edge_write(ep, next_np, envs, txn)?;
                i += 2;
            }
        }
    }
    for env in &mut envs {
        env.remove(LAST_NODE_KEY);
    }
    Ok(envs)
}

fn expand_node_write(
    np: &NodePattern,
    envs: Vec<Env>,
    txn: &GraphWriteTxn,
) -> Result<Vec<Env>, CypherError> {
    if let Some(ref var) = np.variable {
        let has_existing = envs.first().map_or(false, |e| e.contains_key(var));
        if has_existing {
            let mut result = Vec::new();
            for mut env in envs {
                if let Some(Binding::Node(nid)) = env.get(var).cloned() {
                    let node = txn.get_node(nid).map_err(CypherError::Graph)?;
                    if let Some(node) = node {
                        if node_matches(np, &node) {
                            env.insert(LAST_NODE_KEY.to_string(), Binding::Node(nid));
                            result.push(env);
                        }
                    }
                }
            }
            return Ok(result);
        }
    }

    let candidates = if let Some(ref label) = np.label {
        txn.nodes_by_label(label).map_err(CypherError::Graph)?
    } else {
        return Err(CypherError::Execution(
            "first node pattern must have a label or be bound by a previous edge expansion".into(),
        ));
    };

    let has_prop_filter = !np.properties.is_empty();

    let mut result = Vec::new();
    for nid in candidates {
        if has_prop_filter {
            let node = txn.get_node(nid).map_err(CypherError::Graph)?;
            if let Some(node) = node {
                if !node_matches(np, &node) {
                    continue;
                }
            } else {
                continue;
            }
        }
        for env in &envs {
            let mut new_env = env.clone();
            if let Some(ref var) = np.variable {
                new_env.insert(var.clone(), Binding::Node(nid));
            }
            new_env.insert(LAST_NODE_KEY.to_string(), Binding::Node(nid));
            result.push(new_env);
        }
    }
    Ok(result)
}

fn expand_edge_write(
    ep: &EdgePattern,
    next_np: &NodePattern,
    envs: Vec<Env>,
    txn: &GraphWriteTxn,
) -> Result<Vec<Env>, CypherError> {
    let mut result = Vec::new();

    for env in &envs {
        let src_id = get_last_node_id(env)?;

        let directions = match ep.direction {
            EdgeDirection::Right => vec![Direction::Out],
            EdgeDirection::Left => vec![Direction::In],
            EdgeDirection::Both => vec![Direction::Out, Direction::In],
        };

        for dir in &directions {
            let neighbors = txn
                .neighbors(src_id, *dir, ep.edge_type.as_deref())
                .map_err(CypherError::Graph)?;

            for (edge_id, neighbor_id) in neighbors {
                let neighbor_node = txn.get_node(neighbor_id).map_err(CypherError::Graph)?;
                if let Some(neighbor_node) = neighbor_node {
                    if node_matches(next_np, &neighbor_node) {
                        let mut new_env = env.clone();
                        if let Some(ref var) = ep.variable {
                            new_env.insert(var.clone(), Binding::Edge(edge_id));
                        }
                        if let Some(ref var) = next_np.variable {
                            new_env.insert(var.clone(), Binding::Node(neighbor_id));
                        }
                        new_env.insert(LAST_NODE_KEY.to_string(), Binding::Node(neighbor_id));
                        result.push(new_env);
                    }
                }
            }
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Node matching helpers
// ---------------------------------------------------------------------------

fn node_matches(np: &NodePattern, node: &Node) -> bool {
    if let Some(ref label) = np.label {
        if node.label != *label {
            return false;
        }
    }
    for (key, expr) in &np.properties {
        let expected = match expr_to_value(expr) {
            Some(v) => v,
            None => return false,
        };
        match node.properties.get(key) {
            Some(actual) => {
                if !values_equal(actual, &expected) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

fn expr_to_value(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Literal(lit) => Some(literal_to_value(lit)),
        _ => None,
    }
}

fn literal_to_value(lit: &LiteralValue) -> Value {
    match lit {
        LiteralValue::Int(n) => Value::Int(*n),
        LiteralValue::Float(f) => Value::Float(*f),
        LiteralValue::String(s) => Value::String(s.clone()),
        LiteralValue::Bool(b) => Value::Bool(*b),
        LiteralValue::Null => Value::Null,
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

fn value_compare(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// WHERE evaluation — read txn
// ---------------------------------------------------------------------------

fn eval_where_read(
    expr: &Expr,
    env: &Env,
    txn: &GraphReadTxn,
) -> Result<bool, CypherError> {
    match expr {
        Expr::Comparison(left, op, right) => {
            let lv = resolve_expr_read(left, env, txn)?;
            let rv = resolve_expr_read(right, env, txn)?;
            Ok(eval_cmp(&lv, op, &rv))
        }
        Expr::And(a, b) => Ok(eval_where_read(a, env, txn)? && eval_where_read(b, env, txn)?),
        Expr::Or(a, b) => Ok(eval_where_read(a, env, txn)? || eval_where_read(b, env, txn)?),
        Expr::Not(inner) => Ok(!eval_where_read(inner, env, txn)?),
        Expr::Literal(LiteralValue::Bool(b)) => Ok(*b),
        _ => Err(CypherError::Execution("invalid WHERE expression".into())),
    }
}

fn resolve_expr_read(
    expr: &Expr,
    env: &Env,
    txn: &GraphReadTxn,
) -> Result<Value, CypherError> {
    match expr {
        Expr::Property(var, prop) => {
            match env.get(var) {
                Some(Binding::Node(nid)) => {
                    let node = txn.get_node(*nid).map_err(CypherError::Graph)?
                        .ok_or_else(|| CypherError::Execution(format!("node {} not found", nid)))?;
                    Ok(node.properties.get(prop).cloned().unwrap_or(Value::Null))
                }
                Some(Binding::Edge(eid)) => {
                    let edge = txn.get_edge(*eid).map_err(CypherError::Graph)?
                        .ok_or_else(|| CypherError::Execution(format!("edge {} not found", eid)))?;
                    Ok(edge.properties.get(prop).cloned().unwrap_or(Value::Null))
                }
                None => Err(CypherError::Execution(format!("variable '{}' not bound", var))),
            }
        }
        Expr::Literal(lit) => Ok(literal_to_value(lit)),
        _ => Err(CypherError::Execution("unsupported expression in WHERE".into())),
    }
}

// ---------------------------------------------------------------------------
// WHERE evaluation — write txn
// ---------------------------------------------------------------------------

fn eval_where_write(
    expr: &Expr,
    env: &Env,
    txn: &GraphWriteTxn,
) -> Result<bool, CypherError> {
    match expr {
        Expr::Comparison(left, op, right) => {
            let lv = resolve_expr_write(left, env, txn)?;
            let rv = resolve_expr_write(right, env, txn)?;
            Ok(eval_cmp(&lv, op, &rv))
        }
        Expr::And(a, b) => Ok(eval_where_write(a, env, txn)? && eval_where_write(b, env, txn)?),
        Expr::Or(a, b) => Ok(eval_where_write(a, env, txn)? || eval_where_write(b, env, txn)?),
        Expr::Not(inner) => Ok(!eval_where_write(inner, env, txn)?),
        Expr::Literal(LiteralValue::Bool(b)) => Ok(*b),
        _ => Err(CypherError::Execution("invalid WHERE expression".into())),
    }
}

fn resolve_expr_write(
    expr: &Expr,
    env: &Env,
    txn: &GraphWriteTxn,
) -> Result<Value, CypherError> {
    match expr {
        Expr::Property(var, prop) => {
            match env.get(var) {
                Some(Binding::Node(nid)) => {
                    let node = txn.get_node(*nid).map_err(CypherError::Graph)?
                        .ok_or_else(|| CypherError::Execution(format!("node {} not found", nid)))?;
                    Ok(node.properties.get(prop).cloned().unwrap_or(Value::Null))
                }
                Some(Binding::Edge(eid)) => {
                    let edge = txn.get_edge(*eid).map_err(CypherError::Graph)?
                        .ok_or_else(|| CypherError::Execution(format!("edge {} not found", eid)))?;
                    Ok(edge.properties.get(prop).cloned().unwrap_or(Value::Null))
                }
                None => Err(CypherError::Execution(format!("variable '{}' not bound", var))),
            }
        }
        Expr::Literal(lit) => Ok(literal_to_value(lit)),
        _ => Err(CypherError::Execution("unsupported expression in WHERE".into())),
    }
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

fn eval_cmp(left: &Value, op: &CmpOp, right: &Value) -> bool {
    match op {
        CmpOp::Eq => values_equal(left, right),
        CmpOp::Neq => !values_equal(left, right),
        CmpOp::Lt => value_compare(left, right) == Some(std::cmp::Ordering::Less),
        CmpOp::Gt => value_compare(left, right) == Some(std::cmp::Ordering::Greater),
        CmpOp::Lte => matches!(value_compare(left, right), Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)),
        CmpOp::Gte => matches!(value_compare(left, right), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)),
    }
}

// ---------------------------------------------------------------------------
// Projection (RETURN) — read txn
// ---------------------------------------------------------------------------

fn project_results_read(
    ret: &ReturnClause,
    envs: &[Env],
    txn: &GraphReadTxn,
) -> Result<QueryResult, CypherError> {
    // Check for aggregation
    let has_agg = ret.items.iter().any(|item| is_aggregate(item));

    if has_agg {
        return project_aggregate_read(ret, envs, txn);
    }

    let columns = ret.items.iter().map(|item| return_item_name(item)).collect();
    let limit = ret.limit.unwrap_or(u64::MAX) as usize;

    let mut rows = Vec::new();
    for env in envs.iter().take(limit) {
        let mut row = Vec::new();
        for item in &ret.items {
            let val = resolve_return_item_read(item, env, txn)?;
            row.push(val);
        }
        rows.push(row);
    }

    Ok(QueryResult {
        columns,
        rows,
        stats: QueryStats::default(),
    })
}

fn project_aggregate_read(
    ret: &ReturnClause,
    envs: &[Env],
    txn: &GraphReadTxn,
) -> Result<QueryResult, CypherError> {
    let columns = ret.items.iter().map(|item| return_item_name(item)).collect();
    let mut row = Vec::new();

    for item in &ret.items {
        let val = match unwrap_alias(item) {
            ReturnItem::FunctionCall(name, _inner) if name == "count" => {
                Value::Int(envs.len() as i64)
            }
            other => {
                // Non-aggregate in aggregate query: take first env value
                if let Some(env) = envs.first() {
                    resolve_return_item_read(other, env, txn)?
                } else {
                    Value::Null
                }
            }
        };
        row.push(val);
    }

    Ok(QueryResult {
        columns,
        rows: vec![row],
        stats: QueryStats::default(),
    })
}

fn resolve_return_item_read(
    item: &ReturnItem,
    env: &Env,
    txn: &GraphReadTxn,
) -> Result<Value, CypherError> {
    match item {
        ReturnItem::Variable(var) => {
            match env.get(var) {
                Some(Binding::Node(nid)) => {
                    let node = txn.get_node(*nid).map_err(CypherError::Graph)?;
                    match node {
                        Some(n) => Ok(node_to_value(&n)),
                        None => Ok(Value::Null),
                    }
                }
                Some(Binding::Edge(eid)) => {
                    let edge = txn.get_edge(*eid).map_err(CypherError::Graph)?;
                    match edge {
                        Some(e) => Ok(edge_to_value(&e)),
                        None => Ok(Value::Null),
                    }
                }
                None => Err(CypherError::Execution(format!("variable '{}' not bound", var))),
            }
        }
        ReturnItem::Property(var, prop) => {
            match env.get(var) {
                Some(Binding::Node(nid)) => {
                    let node = txn.get_node(*nid).map_err(CypherError::Graph)?
                        .ok_or_else(|| CypherError::Execution(format!("node {} not found", nid)))?;
                    Ok(node.properties.get(prop).cloned().unwrap_or(Value::Null))
                }
                Some(Binding::Edge(eid)) => {
                    let edge = txn.get_edge(*eid).map_err(CypherError::Graph)?
                        .ok_or_else(|| CypherError::Execution(format!("edge {} not found", eid)))?;
                    Ok(edge.properties.get(prop).cloned().unwrap_or(Value::Null))
                }
                None => Err(CypherError::Execution(format!("variable '{}' not bound", var))),
            }
        }
        ReturnItem::Alias(inner, _) => resolve_return_item_read(inner, env, txn),
        ReturnItem::FunctionCall(name, _) => {
            Err(CypherError::Execution(format!("function '{}' not expected here", name)))
        }
    }
}

// ---------------------------------------------------------------------------
// Projection (RETURN) — write txn
// ---------------------------------------------------------------------------

fn project_results_write(
    ret: &ReturnClause,
    envs: &[Env],
    txn: &GraphWriteTxn,
) -> Result<QueryResult, CypherError> {
    let has_agg = ret.items.iter().any(|item| is_aggregate(item));

    if has_agg {
        return project_aggregate_write(ret, envs, txn);
    }

    let columns = ret.items.iter().map(|item| return_item_name(item)).collect();
    let limit = ret.limit.unwrap_or(u64::MAX) as usize;

    let mut rows = Vec::new();
    for env in envs.iter().take(limit) {
        let mut row = Vec::new();
        for item in &ret.items {
            let val = resolve_return_item_write(item, env, txn)?;
            row.push(val);
        }
        rows.push(row);
    }

    Ok(QueryResult {
        columns,
        rows,
        stats: QueryStats::default(),
    })
}

fn project_aggregate_write(
    ret: &ReturnClause,
    envs: &[Env],
    txn: &GraphWriteTxn,
) -> Result<QueryResult, CypherError> {
    let columns = ret.items.iter().map(|item| return_item_name(item)).collect();
    let mut row = Vec::new();

    for item in &ret.items {
        let val = match unwrap_alias(item) {
            ReturnItem::FunctionCall(name, _inner) if name == "count" => {
                Value::Int(envs.len() as i64)
            }
            other => {
                if let Some(env) = envs.first() {
                    resolve_return_item_write(other, env, txn)?
                } else {
                    Value::Null
                }
            }
        };
        row.push(val);
    }

    Ok(QueryResult {
        columns,
        rows: vec![row],
        stats: QueryStats::default(),
    })
}

fn resolve_return_item_write(
    item: &ReturnItem,
    env: &Env,
    txn: &GraphWriteTxn,
) -> Result<Value, CypherError> {
    match item {
        ReturnItem::Variable(var) => {
            match env.get(var) {
                Some(Binding::Node(nid)) => {
                    let node = txn.get_node(*nid).map_err(CypherError::Graph)?;
                    match node {
                        Some(n) => Ok(node_to_value(&n)),
                        None => Ok(Value::Null),
                    }
                }
                Some(Binding::Edge(eid)) => {
                    let edge = txn.get_edge(*eid).map_err(CypherError::Graph)?;
                    match edge {
                        Some(e) => Ok(edge_to_value(&e)),
                        None => Ok(Value::Null),
                    }
                }
                None => Err(CypherError::Execution(format!("variable '{}' not bound", var))),
            }
        }
        ReturnItem::Property(var, prop) => {
            match env.get(var) {
                Some(Binding::Node(nid)) => {
                    let node = txn.get_node(*nid).map_err(CypherError::Graph)?
                        .ok_or_else(|| CypherError::Execution(format!("node {} not found", nid)))?;
                    Ok(node.properties.get(prop).cloned().unwrap_or(Value::Null))
                }
                Some(Binding::Edge(eid)) => {
                    let edge = txn.get_edge(*eid).map_err(CypherError::Graph)?
                        .ok_or_else(|| CypherError::Execution(format!("edge {} not found", eid)))?;
                    Ok(edge.properties.get(prop).cloned().unwrap_or(Value::Null))
                }
                None => Err(CypherError::Execution(format!("variable '{}' not bound", var))),
            }
        }
        ReturnItem::Alias(inner, _) => resolve_return_item_write(inner, env, txn),
        ReturnItem::FunctionCall(name, _) => {
            Err(CypherError::Execution(format!("function '{}' not expected here", name)))
        }
    }
}

// ---------------------------------------------------------------------------
// CREATE
// ---------------------------------------------------------------------------

fn execute_create(
    c: CreateClause,
    txn: &mut GraphWriteTxn,
) -> Result<QueryResult, CypherError> {
    let mut stats = QueryStats::default();
    let mut env: Env = HashMap::new();

    let mut i = 0;
    while i < c.pattern.elements.len() {
        match &c.pattern.elements[i] {
            PatternElement::Node(np) => {
                let label = np.label.as_deref().unwrap_or("");
                let props = props_to_hashmap(&np.properties)?;
                let nid = txn.create_node(label, props).map_err(CypherError::Graph)?;
                stats.nodes_created += 1;
                if let Some(ref var) = np.variable {
                    env.insert(var.clone(), Binding::Node(nid));
                }
                i += 1;
            }
            PatternElement::Edge(ep) => {
                if i + 1 >= c.pattern.elements.len() {
                    return Err(CypherError::Execution("edge pattern not followed by node".into()));
                }

                // Capture source node BEFORE creating destination
                let src_id = find_last_node_id(&env)?;

                let next_np = match &c.pattern.elements[i + 1] {
                    PatternElement::Node(n) => n,
                    _ => return Err(CypherError::Execution("expected node after edge".into())),
                };

                let dst_label = next_np.label.as_deref().unwrap_or("");
                let dst_props = props_to_hashmap(&next_np.properties)?;
                let dst_id = txn.create_node(dst_label, dst_props).map_err(CypherError::Graph)?;
                stats.nodes_created += 1;
                if let Some(ref var) = next_np.variable {
                    env.insert(var.clone(), Binding::Node(dst_id));
                }

                let edge_type = ep.edge_type.as_deref().unwrap_or("RELATED");
                let eid = txn.create_edge(src_id, dst_id, edge_type, HashMap::new())
                    .map_err(CypherError::Graph)?;
                stats.edges_created += 1;
                if let Some(ref var) = ep.variable {
                    env.insert(var.clone(), Binding::Edge(eid));
                }

                i += 2;
            }
        }
    }

    Ok(QueryResult {
        columns: Vec::new(),
        rows: Vec::new(),
        stats,
    })
}

// ---------------------------------------------------------------------------
// DELETE
// ---------------------------------------------------------------------------

fn execute_match_delete(
    m: MatchClause,
    txn: &mut GraphWriteTxn,
) -> Result<QueryResult, CypherError> {
    // First, expand the pattern to find matches (using read capabilities of write txn)
    let envs = expand_pattern_write(&m.pattern, txn)?;

    let envs = if let Some(ref where_expr) = m.where_clause {
        envs.into_iter()
            .filter(|env| eval_where_write(where_expr, env, txn).unwrap_or(false))
            .collect()
    } else {
        envs
    };

    let delete_vars = m.delete_clause.unwrap_or_default();
    let mut stats = QueryStats::default();

    // Collect all IDs to delete first to avoid mutation during iteration
    let mut edges_to_delete = Vec::new();
    let mut nodes_to_delete = Vec::new();

    for env in &envs {
        for var in &delete_vars {
            match env.get(var) {
                Some(Binding::Edge(eid)) => edges_to_delete.push(*eid),
                Some(Binding::Node(nid)) => nodes_to_delete.push(*nid),
                None => {
                    return Err(CypherError::Execution(format!(
                        "variable '{}' not bound",
                        var
                    )));
                }
            }
        }
    }

    // Delete edges first
    for eid in edges_to_delete {
        if txn.delete_edge(eid).map_err(CypherError::Graph)? {
            stats.edges_deleted += 1;
        }
    }

    // Then delete nodes
    for nid in nodes_to_delete {
        if txn.delete_node(nid).map_err(CypherError::Graph)? {
            stats.nodes_deleted += 1;
        }
    }

    Ok(QueryResult {
        columns: Vec::new(),
        rows: Vec::new(),
        stats,
    })
}

// ---------------------------------------------------------------------------
// MERGE
// ---------------------------------------------------------------------------

fn execute_merge(
    m: MergeClause,
    txn: &mut GraphWriteTxn,
) -> Result<QueryResult, CypherError> {
    let np = &m.pattern;
    let label = np.label.as_deref().ok_or_else(|| {
        CypherError::Execution("MERGE requires a label".into())
    })?;

    // Search for existing node matching label + properties
    let candidates = txn.nodes_by_label(label).map_err(CypherError::Graph)?;
    for nid in candidates {
        let node = txn.get_node(nid).map_err(CypherError::Graph)?;
        if let Some(node) = node {
            if node_matches(np, &node) {
                // Found existing — return it
                return Ok(QueryResult {
                    columns: vec!["n".into()],
                    rows: vec![vec![node_to_value(&node)]],
                    stats: QueryStats::default(),
                });
            }
        }
    }

    // Not found — create
    let props = props_to_hashmap(&np.properties)?;
    let nid = txn.create_node(label, props).map_err(CypherError::Graph)?;
    let node = txn.get_node(nid).map_err(CypherError::Graph)?
        .ok_or_else(|| CypherError::Execution("failed to read created node".into()))?;

    Ok(QueryResult {
        columns: vec!["n".into()],
        rows: vec![vec![node_to_value(&node)]],
        stats: QueryStats {
            nodes_created: 1,
            ..Default::default()
        },
    })
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

fn get_last_node_id(env: &Env) -> Result<NodeId, CypherError> {
    match env.get(LAST_NODE_KEY) {
        Some(Binding::Node(nid)) => Ok(*nid),
        _ => Err(CypherError::Execution("no node binding found for edge expansion".into())),
    }
}

fn find_last_node_id(env: &Env) -> Result<NodeId, CypherError> {
    // Fallback for CREATE: find any node binding (highest ID heuristic)
    let mut last_id = None;
    for (key, binding) in env {
        if key == LAST_NODE_KEY {
            continue;
        }
        if let Binding::Node(nid) = binding {
            match last_id {
                None => last_id = Some(*nid),
                Some(prev) => {
                    if *nid > prev {
                        last_id = Some(*nid);
                    }
                }
            }
        }
    }
    last_id.ok_or_else(|| CypherError::Execution("no node binding found for edge expansion".into()))
}

fn props_to_hashmap(props: &HashMap<String, Expr>) -> Result<HashMap<String, Value>, CypherError> {
    let mut map = HashMap::new();
    for (k, expr) in props {
        let val = expr_to_value(expr).ok_or_else(|| {
            CypherError::Execution(format!("property '{}' must be a literal value", k))
        })?;
        map.insert(k.clone(), val);
    }
    Ok(map)
}

/// Convert a Node into a Value::String representation (for returning whole nodes).
fn node_to_value(node: &Node) -> Value {
    // Return a string representation: "(id:Label {props})"
    let mut s = format!("({}:{}", node.id, node.label);
    if !node.properties.is_empty() {
        s.push_str(" {");
        let mut first = true;
        // Sort keys for deterministic output
        let mut keys: Vec<&String> = node.properties.keys().collect();
        keys.sort();
        for k in keys {
            let v = &node.properties[k];
            if !first {
                s.push_str(", ");
            }
            s.push_str(&format!("{}: {}", k, value_display(v)));
            first = false;
        }
        s.push('}');
    }
    s.push(')');
    Value::String(s)
}

fn edge_to_value(edge: &Edge) -> Value {
    let mut s = format!("[{}:{}]", edge.id, edge.edge_type);
    if !edge.properties.is_empty() {
        // Simplified
        s = format!("[{}:{} {{...}}]", edge.id, edge.edge_type);
    }
    Value::String(s)
}

fn value_display(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) => format!("'{}'", s),
    }
}

fn return_item_name(item: &ReturnItem) -> String {
    match item {
        ReturnItem::Variable(v) => v.clone(),
        ReturnItem::Property(v, p) => format!("{}.{}", v, p),
        ReturnItem::FunctionCall(f, inner) => format!("{}({})", f, return_item_name(inner)),
        ReturnItem::Alias(_, alias) => alias.clone(),
    }
}

fn is_aggregate(item: &ReturnItem) -> bool {
    match item {
        ReturnItem::FunctionCall(_, _) => true,
        ReturnItem::Alias(inner, _) => is_aggregate(inner),
        _ => false,
    }
}

fn unwrap_alias(item: &ReturnItem) -> &ReturnItem {
    match item {
        ReturnItem::Alias(inner, _) => unwrap_alias(inner),
        other => other,
    }
}
