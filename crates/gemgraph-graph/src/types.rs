use std::collections::HashMap;
use serde::{Serialize, Deserialize};

pub type NodeId = u64;
pub type EdgeId = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out = 0,
    In = 1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRecord {
    pub label: String,
    pub props: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeRecord {
    pub src: NodeId,
    pub dst: NodeId,
    pub edge_type: String,
    pub props: HashMap<String, Value>,
}

/// User-facing node struct.
#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub label: String,
    pub properties: HashMap<String, Value>,
}

/// User-facing edge struct.
#[derive(Debug, Clone)]
pub struct Edge {
    pub id: EdgeId,
    pub src: NodeId,
    pub dst: NodeId,
    pub edge_type: String,
    pub properties: HashMap<String, Value>,
}
