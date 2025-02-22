pub mod types;
pub mod keys;
pub mod graph;
pub mod read_txn;
pub mod write_txn;

pub use types::{NodeId, EdgeId, Value, Node, Edge, Direction, NodeRecord, EdgeRecord};
pub use graph::GraphDb;
pub use read_txn::GraphReadTxn;
pub use write_txn::GraphWriteTxn;

use gemgraph_mvcc::MvccError;

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("mvcc error: {0}")]
    Mvcc(#[from] MvccError),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("node not found: {0}")]
    NodeNotFound(NodeId),

    #[error("edge not found: {0}")]
    EdgeNotFound(EdgeId),

    #[error("invalid database: {0}")]
    InvalidDatabase(String),
}

impl From<bincode::Error> for GraphError {
    fn from(e: bincode::Error) -> Self {
        GraphError::Serialization(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, GraphError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn make_graph() -> (tempfile::TempDir, GraphDb) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("testgraph");
        let db = GraphDb::create(&path).unwrap();
        (dir, db)
    }

    fn props(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn create_node_and_read_back() {
        let (_dir, mut db) = make_graph();

        let node_id;
        {
            let mut wtx = db.write_txn();
            node_id = wtx.create_node("Person", props(&[
                ("name", Value::String("Alice".into())),
                ("age", Value::Int(30)),
            ])).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let node = rtx.get_node(node_id).unwrap().unwrap();
        assert_eq!(node.id, node_id);
        assert_eq!(node.label, "Person");
        assert_eq!(node.properties.get("name"), Some(&Value::String("Alice".into())));
        assert_eq!(node.properties.get("age"), Some(&Value::Int(30)));
    }

    #[test]
    fn create_edge_and_traverse() {
        let (_dir, mut db) = make_graph();

        let (n1, n2, e1);
        {
            let mut wtx = db.write_txn();
            n1 = wtx.create_node("Person", HashMap::new()).unwrap();
            n2 = wtx.create_node("Person", HashMap::new()).unwrap();
            e1 = wtx.create_edge(n1, n2, "KNOWS", props(&[
                ("since", Value::Int(2020)),
            ])).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();

        // Check edge record
        let edge = rtx.get_edge(e1).unwrap().unwrap();
        assert_eq!(edge.src, n1);
        assert_eq!(edge.dst, n2);
        assert_eq!(edge.edge_type, "KNOWS");
        assert_eq!(edge.properties.get("since"), Some(&Value::Int(2020)));

        // Outgoing from n1
        let out = rtx.neighbors(n1, Direction::Out, Some("KNOWS")).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], (e1, n2));

        // Incoming to n2
        let inc = rtx.neighbors(n2, Direction::In, Some("KNOWS")).unwrap();
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0], (e1, n1));

        // n1 has no incoming KNOWS
        let empty = rtx.neighbors(n1, Direction::In, Some("KNOWS")).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn nodes_by_label() {
        let (_dir, mut db) = make_graph();

        let (n1, n2, n3);
        {
            let mut wtx = db.write_txn();
            n1 = wtx.create_node("Person", HashMap::new()).unwrap();
            n2 = wtx.create_node("Person", HashMap::new()).unwrap();
            n3 = wtx.create_node("City", HashMap::new()).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let people = rtx.nodes_by_label("Person").unwrap();
        assert_eq!(people.len(), 2);
        assert!(people.contains(&n1));
        assert!(people.contains(&n2));

        let cities = rtx.nodes_by_label("City").unwrap();
        assert_eq!(cities.len(), 1);
        assert!(cities.contains(&n3));

        let empty = rtx.nodes_by_label("Animal").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn delete_node_removes_from_all_indexes() {
        let (_dir, mut db) = make_graph();

        let (n1, n2, e1);
        {
            let mut wtx = db.write_txn();
            n1 = wtx.create_node("Person", HashMap::new()).unwrap();
            n2 = wtx.create_node("Person", HashMap::new()).unwrap();
            e1 = wtx.create_edge(n1, n2, "KNOWS", HashMap::new()).unwrap();
            wtx.commit().unwrap();
        }

        {
            let mut wtx = db.write_txn();
            assert!(wtx.delete_node(n1).unwrap());
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();

        // Node gone
        assert!(rtx.get_node(n1).unwrap().is_none());

        // Edge gone
        assert!(rtx.get_edge(e1).unwrap().is_none());

        // Label index updated
        let people = rtx.nodes_by_label("Person").unwrap();
        assert_eq!(people.len(), 1);
        assert!(people.contains(&n2));

        // Adjacency cleaned up on both sides
        let out = rtx.neighbors(n1, Direction::Out, None).unwrap();
        assert!(out.is_empty());
        let inc = rtx.neighbors(n2, Direction::In, None).unwrap();
        assert!(inc.is_empty());
    }

    #[test]
    fn delete_edge_removes_from_adjacency() {
        let (_dir, mut db) = make_graph();

        let (n1, n2, e1);
        {
            let mut wtx = db.write_txn();
            n1 = wtx.create_node("A", HashMap::new()).unwrap();
            n2 = wtx.create_node("B", HashMap::new()).unwrap();
            e1 = wtx.create_edge(n1, n2, "LINK", HashMap::new()).unwrap();
            wtx.commit().unwrap();
        }

        {
            let mut wtx = db.write_txn();
            assert!(wtx.delete_edge(e1).unwrap());
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        assert!(rtx.get_edge(e1).unwrap().is_none());
        assert!(rtx.neighbors(n1, Direction::Out, None).unwrap().is_empty());
        assert!(rtx.neighbors(n2, Direction::In, None).unwrap().is_empty());

        // Nodes still exist
        assert!(rtx.get_node(n1).unwrap().is_some());
        assert!(rtx.get_node(n2).unwrap().is_some());
    }

    #[test]
    fn properties_set_get_update() {
        let (_dir, mut db) = make_graph();

        let n1;
        {
            let mut wtx = db.write_txn();
            n1 = wtx.create_node("Item", props(&[
                ("name", Value::String("Widget".into())),
            ])).unwrap();
            wtx.commit().unwrap();
        }

        // Read initial property
        {
            let rtx = db.read_txn();
            let val = rtx.get_node_property(n1, "name").unwrap();
            assert_eq!(val, Some(Value::String("Widget".into())));
            let missing = rtx.get_node_property(n1, "color").unwrap();
            assert_eq!(missing, None);
        }

        // Update property
        {
            let mut wtx = db.write_txn();
            wtx.set_node_property(n1, "name", Value::String("Gadget".into())).unwrap();
            wtx.set_node_property(n1, "color", Value::String("blue".into())).unwrap();
            wtx.commit().unwrap();
        }

        {
            let rtx = db.read_txn();
            let node = rtx.get_node(n1).unwrap().unwrap();
            assert_eq!(node.properties.get("name"), Some(&Value::String("Gadget".into())));
            assert_eq!(node.properties.get("color"), Some(&Value::String("blue".into())));
        }
    }

    #[test]
    fn multiple_edge_types_between_same_nodes() {
        let (_dir, mut db) = make_graph();

        let (n1, n2, e_knows, e_works);
        {
            let mut wtx = db.write_txn();
            n1 = wtx.create_node("Person", HashMap::new()).unwrap();
            n2 = wtx.create_node("Person", HashMap::new()).unwrap();
            e_knows = wtx.create_edge(n1, n2, "KNOWS", HashMap::new()).unwrap();
            e_works = wtx.create_edge(n1, n2, "WORKS_WITH", HashMap::new()).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();

        // Filter by type
        let knows = rtx.neighbors(n1, Direction::Out, Some("KNOWS")).unwrap();
        assert_eq!(knows.len(), 1);
        assert_eq!(knows[0].0, e_knows);

        let works = rtx.neighbors(n1, Direction::Out, Some("WORKS_WITH")).unwrap();
        assert_eq!(works.len(), 1);
        assert_eq!(works[0].0, e_works);

        // All outgoing
        let all = rtx.neighbors(n1, Direction::Out, None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn bidirectional_traversal() {
        let (_dir, mut db) = make_graph();

        let (n1, n2, n3);
        {
            let mut wtx = db.write_txn();
            n1 = wtx.create_node("A", HashMap::new()).unwrap();
            n2 = wtx.create_node("B", HashMap::new()).unwrap();
            n3 = wtx.create_node("C", HashMap::new()).unwrap();
            // n1 -> n2 -> n3
            wtx.create_edge(n1, n2, "NEXT", HashMap::new()).unwrap();
            wtx.create_edge(n2, n3, "NEXT", HashMap::new()).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();

        // n2 outgoing
        let out = rtx.neighbors(n2, Direction::Out, Some("NEXT")).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, n3);

        // n2 incoming
        let inc = rtx.neighbors(n2, Direction::In, Some("NEXT")).unwrap();
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].1, n1);
    }

    #[test]
    fn persist_and_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("testgraph");

        let (n1, n2, e1);
        {
            let mut db = GraphDb::create(&path).unwrap();
            let mut wtx = db.write_txn();
            n1 = wtx.create_node("Person", props(&[
                ("name", Value::String("Alice".into())),
            ])).unwrap();
            n2 = wtx.create_node("Person", HashMap::new()).unwrap();
            e1 = wtx.create_edge(n1, n2, "KNOWS", HashMap::new()).unwrap();
            wtx.commit().unwrap();
        }

        {
            let db = GraphDb::open(&path).unwrap();
            let rtx = db.read_txn();

            let node = rtx.get_node(n1).unwrap().unwrap();
            assert_eq!(node.label, "Person");
            assert_eq!(node.properties.get("name"), Some(&Value::String("Alice".into())));

            let edge = rtx.get_edge(e1).unwrap().unwrap();
            assert_eq!(edge.src, n1);
            assert_eq!(edge.dst, n2);

            let neighbors = rtx.neighbors(n1, Direction::Out, None).unwrap();
            assert_eq!(neighbors.len(), 1);

            let people = rtx.nodes_by_label("Person").unwrap();
            assert_eq!(people.len(), 2);
        }
    }

    #[test]
    fn scale_100_nodes_and_edges() {
        let (_dir, mut db) = make_graph();

        let mut node_ids = Vec::new();
        let mut edge_ids = Vec::new();

        {
            let mut wtx = db.write_txn();

            // Create 100 nodes
            for i in 0..100u64 {
                let label = if i % 2 == 0 { "Even" } else { "Odd" };
                let id = wtx.create_node(label, props(&[
                    ("index", Value::Int(i as i64)),
                ])).unwrap();
                node_ids.push(id);
            }

            // Create edges: each node connects to the next (chain)
            for i in 0..99 {
                let eid = wtx.create_edge(
                    node_ids[i], node_ids[i + 1], "NEXT", HashMap::new(),
                ).unwrap();
                edge_ids.push(eid);
            }

            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();

        // Verify all nodes exist
        for &nid in &node_ids {
            assert!(rtx.get_node(nid).unwrap().is_some());
        }

        // Verify chain traversal
        let mut current = node_ids[0];
        for i in 0..99 {
            let out = rtx.neighbors(current, Direction::Out, Some("NEXT")).unwrap();
            assert_eq!(out.len(), 1, "node at index {} should have 1 outgoing", i);
            assert_eq!(out[0].1, node_ids[i + 1]);
            current = out[0].1;
        }

        // Last node has no outgoing
        let last_out = rtx.neighbors(*node_ids.last().unwrap(), Direction::Out, Some("NEXT")).unwrap();
        assert!(last_out.is_empty());

        // Label counts
        let evens = rtx.nodes_by_label("Even").unwrap();
        assert_eq!(evens.len(), 50);
        let odds = rtx.nodes_by_label("Odd").unwrap();
        assert_eq!(odds.len(), 50);
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let (_dir, mut db) = make_graph();
        let mut wtx = db.write_txn();
        assert!(!wtx.delete_node(999).unwrap());
        assert!(!wtx.delete_edge(999).unwrap());
        wtx.abort();
    }

    #[test]
    fn value_types() {
        let (_dir, mut db) = make_graph();

        let n1;
        {
            let mut wtx = db.write_txn();
            n1 = wtx.create_node("Test", props(&[
                ("null_val", Value::Null),
                ("bool_val", Value::Bool(true)),
                ("int_val", Value::Int(-42)),
                ("float_val", Value::Float(3.14)),
                ("str_val", Value::String("hello".into())),
            ])).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let node = rtx.get_node(n1).unwrap().unwrap();
        assert_eq!(node.properties.get("null_val"), Some(&Value::Null));
        assert_eq!(node.properties.get("bool_val"), Some(&Value::Bool(true)));
        assert_eq!(node.properties.get("int_val"), Some(&Value::Int(-42)));
        assert_eq!(node.properties.get("float_val"), Some(&Value::Float(3.14)));
        assert_eq!(node.properties.get("str_val"), Some(&Value::String("hello".into())));
    }

    #[test]
    fn write_txn_reads_own_writes() {
        let (_dir, mut db) = make_graph();

        let mut wtx = db.write_txn();
        let n1 = wtx.create_node("X", HashMap::new()).unwrap();
        let n2 = wtx.create_node("X", HashMap::new()).unwrap();

        // Read within same txn
        let node = wtx.get_node(n1).unwrap().unwrap();
        assert_eq!(node.label, "X");

        let e1 = wtx.create_edge(n1, n2, "REL", HashMap::new()).unwrap();
        let edge = wtx.get_edge(e1).unwrap().unwrap();
        assert_eq!(edge.src, n1);

        let neighbors = wtx.neighbors(n1, Direction::Out, None).unwrap();
        assert_eq!(neighbors.len(), 1);

        let by_label = wtx.nodes_by_label("X").unwrap();
        assert_eq!(by_label.len(), 2);

        wtx.commit().unwrap();
    }

    #[test]
    fn create_edge_to_nonexistent_node_fails() {
        let (_dir, mut db) = make_graph();

        let mut wtx = db.write_txn();
        let n1 = wtx.create_node("A", HashMap::new()).unwrap();
        let result = wtx.create_edge(n1, 999, "REL", HashMap::new());
        assert!(result.is_err());
        wtx.abort();
    }
}
