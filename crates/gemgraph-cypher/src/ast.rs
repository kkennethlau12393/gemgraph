use std::collections::HashMap;

/// Top-level statement produced by the parser.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Match(MatchClause),
    Create(CreateClause),
    Merge(MergeClause),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchClause {
    pub pattern: Pattern,
    pub where_clause: Option<Expr>,
    pub return_clause: Option<ReturnClause>,
    pub delete_clause: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateClause {
    pub pattern: Pattern,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeClause {
    pub pattern: NodePattern,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnClause {
    pub items: Vec<ReturnItem>,
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReturnItem {
    Variable(String),
    Property(String, String),
    FunctionCall(String, Box<ReturnItem>),
    Alias(Box<ReturnItem>, String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub elements: Vec<PatternElement>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternElement {
    Node(NodePattern),
    Edge(EdgePattern),
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodePattern {
    pub variable: Option<String>,
    pub label: Option<String>,
    pub properties: HashMap<String, Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgePattern {
    pub variable: Option<String>,
    pub edge_type: Option<String>,
    pub direction: EdgeDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeDirection {
    Right,
    Left,
    Both,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Property(String, String),
    Literal(LiteralValue),
    Comparison(Box<Expr>, CmpOp, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Neq,
    Lt,
    Gt,
    Lte,
    Gte,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Null,
}
