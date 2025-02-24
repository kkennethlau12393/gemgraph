pub mod ast;
pub mod executor;
pub mod lexer;
pub mod parser;

pub use executor::{execute_read, execute_write, QueryResult, QueryStats};

use gemgraph_graph::GraphError;

#[derive(Debug, thiserror::Error)]
pub enum CypherError {
    #[error("lex error: {0}")]
    Lex(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("execution error: {0}")]
    Execution(String),

    #[error("graph error: {0}")]
    Graph(#[from] GraphError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemgraph_graph::{GraphDb, Value};
    use tempfile::tempdir;

    fn make_graph() -> (tempfile::TempDir, GraphDb) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("testgraph");
        let db = GraphDb::create(&path).unwrap();
        (dir, db)
    }

    // -----------------------------------------------------------------------
    // CREATE tests
    // -----------------------------------------------------------------------

    #[test]
    fn create_single_node() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            let result = execute_write("CREATE (n:Person {name: 'Alice', age: 30})", &mut wtx).unwrap();
            assert_eq!(result.stats.nodes_created, 1);
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN n.name", &rtx).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::String("Alice".into()));
    }

    #[test]
    fn create_node_and_edge() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            let result = execute_write(
                "CREATE (n:Person {name: 'Alice'})-[:KNOWS]->(m:Person {name: 'Bob'})",
                &mut wtx,
            )
            .unwrap();
            assert_eq!(result.stats.nodes_created, 2);
            assert_eq!(result.stats.edges_created, 1);
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read(
            "MATCH (n:Person)-[:KNOWS]->(m:Person) RETURN n.name, m.name",
            &rtx,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::String("Alice".into()));
        assert_eq!(result.rows[0][1], Value::String("Bob".into()));
    }

    // -----------------------------------------------------------------------
    // MATCH + RETURN tests
    // -----------------------------------------------------------------------

    #[test]
    fn match_return_all_nodes() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write("CREATE (n:Person {name: 'Alice'})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Bob'})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Charlie'})", &mut wtx).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN n.name", &rtx).unwrap();
        assert_eq!(result.rows.len(), 3);
    }

    #[test]
    fn match_with_property_filter() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write("CREATE (n:Person {name: 'Alice', age: 30})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Bob', age: 25})", &mut wtx).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read(
            "MATCH (n:Person {name: 'Alice'}) RETURN n.name, n.age",
            &rtx,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::String("Alice".into()));
        assert_eq!(result.rows[0][1], Value::Int(30));
    }

    #[test]
    fn match_with_where_filter() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write("CREATE (n:Person {name: 'Alice', age: 30})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Bob', age: 25})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Charlie', age: 35})", &mut wtx).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read(
            "MATCH (n:Person) WHERE n.age > 28 RETURN n.name",
            &rtx,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 2);

        let names: Vec<&Value> = result.rows.iter().map(|r| &r[0]).collect();
        assert!(names.contains(&&Value::String("Alice".into())));
        assert!(names.contains(&&Value::String("Charlie".into())));
    }

    #[test]
    fn match_with_limit() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            for i in 0..10 {
                execute_write(
                    &format!("CREATE (n:Person {{name: 'P{}'}})", i),
                    &mut wtx,
                )
                .unwrap();
            }
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN n.name LIMIT 3", &rtx).unwrap();
        assert_eq!(result.rows.len(), 3);
    }

    #[test]
    fn match_edge_traversal() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write(
                "CREATE (n:Person {name: 'Alice'})-[:KNOWS]->(m:Person {name: 'Bob'})",
                &mut wtx,
            )
            .unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read(
            "MATCH (n:Person {name: 'Alice'})-[:KNOWS]->(m:Person) RETURN m.name",
            &rtx,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::String("Bob".into()));
    }

    #[test]
    fn match_return_edge_variable() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write(
                "CREATE (n:Person {name: 'Alice'})-[:KNOWS]->(m:Person {name: 'Bob'})",
                &mut wtx,
            )
            .unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read(
            "MATCH (n:Person)-[r:KNOWS]->(m:Person) RETURN n.name, r, m.name",
            &rtx,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.columns, vec!["n.name", "r", "m.name"]);
        assert_eq!(result.rows[0][0], Value::String("Alice".into()));
        // r is serialized as a string representation of the edge
        match &result.rows[0][1] {
            Value::String(s) => assert!(s.contains("KNOWS")),
            other => panic!("expected string representation of edge, got {:?}", other),
        }
        assert_eq!(result.rows[0][2], Value::String("Bob".into()));
    }

    #[test]
    fn match_count_aggregation() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write("CREATE (n:Person {name: 'Alice'})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Bob'})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Charlie'})", &mut wtx).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN count(n)", &rtx).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::Int(3));
    }

    // -----------------------------------------------------------------------
    // DELETE tests
    // -----------------------------------------------------------------------

    #[test]
    fn delete_node() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write("CREATE (n:Person {name: 'Alice'})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Bob'})", &mut wtx).unwrap();
            wtx.commit().unwrap();
        }

        {
            let mut wtx = db.write_txn();
            let result = execute_write(
                "MATCH (n:Person {name: 'Alice'}) DELETE n",
                &mut wtx,
            )
            .unwrap();
            assert_eq!(result.stats.nodes_deleted, 1);
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN n.name", &rtx).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::String("Bob".into()));
    }

    #[test]
    fn delete_edge() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write(
                "CREATE (n:Person {name: 'Alice'})-[:KNOWS]->(m:Person {name: 'Bob'})",
                &mut wtx,
            )
            .unwrap();
            wtx.commit().unwrap();
        }

        {
            let mut wtx = db.write_txn();
            let result = execute_write(
                "MATCH (n:Person)-[r:KNOWS]->(m:Person) DELETE r",
                &mut wtx,
            )
            .unwrap();
            assert_eq!(result.stats.edges_deleted, 1);
            wtx.commit().unwrap();
        }

        // Edge gone, but nodes remain
        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN n.name", &rtx).unwrap();
        assert_eq!(result.rows.len(), 2);

        let result = execute_read(
            "MATCH (n:Person)-[:KNOWS]->(m:Person) RETURN m.name",
            &rtx,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 0);
    }

    // -----------------------------------------------------------------------
    // MERGE tests
    // -----------------------------------------------------------------------

    #[test]
    fn merge_creates_if_not_exists() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            let result = execute_write("MERGE (n:Person {name: 'Alice'})", &mut wtx).unwrap();
            assert_eq!(result.stats.nodes_created, 1);
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN n.name", &rtx).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::String("Alice".into()));
    }

    #[test]
    fn merge_returns_existing() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write("CREATE (n:Person {name: 'Alice'})", &mut wtx).unwrap();
            wtx.commit().unwrap();
        }

        {
            let mut wtx = db.write_txn();
            let result = execute_write("MERGE (n:Person {name: 'Alice'})", &mut wtx).unwrap();
            assert_eq!(result.stats.nodes_created, 0);
            assert_eq!(result.rows.len(), 1);
            wtx.commit().unwrap();
        }

        // Still only one Person named Alice
        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN n.name", &rtx).unwrap();
        assert_eq!(result.rows.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Multi-hop pattern
    // -----------------------------------------------------------------------

    #[test]
    fn multi_hop_pattern() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            // Create a chain: Alice -KNOWS-> Bob -KNOWS-> Charlie
            execute_write(
                "CREATE (n:Person {name: 'Alice'})-[:KNOWS]->(m:Person {name: 'Bob'})",
                &mut wtx,
            )
            .unwrap();
            // Now create Bob -KNOWS-> Charlie. First find Bob's ID.
            // We need to use the graph API directly for this since our CREATE
            // doesn't support referencing existing nodes.
            use std::collections::HashMap;
            let bob_ids = wtx.nodes_by_label("Person").unwrap();
            let mut bob_id = None;
            for nid in bob_ids {
                let node = wtx.get_node(nid).unwrap().unwrap();
                if node.properties.get("name") == Some(&Value::String("Bob".into())) {
                    bob_id = Some(nid);
                    break;
                }
            }
            let bob_id = bob_id.unwrap();
            let charlie_id = wtx
                .create_node("Person", {
                    let mut m = HashMap::new();
                    m.insert("name".into(), Value::String("Charlie".into()));
                    m
                })
                .unwrap();
            wtx.create_edge(bob_id, charlie_id, "KNOWS", HashMap::new())
                .unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read(
            "MATCH (a:Person)-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person) RETURN a.name, b.name, c.name",
            &rtx,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::String("Alice".into()));
        assert_eq!(result.rows[0][1], Value::String("Bob".into()));
        assert_eq!(result.rows[0][2], Value::String("Charlie".into()));
    }

    // -----------------------------------------------------------------------
    // WHERE with AND/OR
    // -----------------------------------------------------------------------

    #[test]
    fn where_and_filter() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write("CREATE (n:Person {name: 'Alice', age: 30})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Bob', age: 25})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Charlie', age: 35})", &mut wtx).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read(
            "MATCH (n:Person) WHERE n.age > 20 AND n.age < 32 RETURN n.name",
            &rtx,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 2);
        let names: Vec<&Value> = result.rows.iter().map(|r| &r[0]).collect();
        assert!(names.contains(&&Value::String("Alice".into())));
        assert!(names.contains(&&Value::String("Bob".into())));
    }

    #[test]
    fn where_equality_filter() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write("CREATE (n:Person {name: 'Alice', age: 30})", &mut wtx).unwrap();
            execute_write("CREATE (n:Person {name: 'Bob', age: 25})", &mut wtx).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read(
            "MATCH (n:Person) WHERE n.name = 'Bob' RETURN n.age",
            &rtx,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::Int(25));
    }

    // -----------------------------------------------------------------------
    // Property return
    // -----------------------------------------------------------------------

    #[test]
    fn return_multiple_properties() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write("CREATE (n:Person {name: 'Alice', age: 30})", &mut wtx).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN n.name, n.age", &rtx).unwrap();
        assert_eq!(result.columns, vec!["n.name", "n.age"]);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::String("Alice".into()));
        assert_eq!(result.rows[0][1], Value::Int(30));
    }

    // -----------------------------------------------------------------------
    // Error cases
    // -----------------------------------------------------------------------

    #[test]
    fn create_requires_write_txn() {
        let (_dir, db) = make_graph();
        let rtx = db.read_txn();
        let result = execute_read("CREATE (n:Person {name: 'Alice'})", &rtx);
        assert!(result.is_err());
    }

    #[test]
    fn parse_error_produces_cypher_error() {
        let (_dir, db) = make_graph();
        let rtx = db.read_txn();
        let result = execute_read("MATCH RETURN", &rtx);
        assert!(result.is_err());
        match result {
            Err(CypherError::Parse(_)) => {}
            other => panic!("expected parse error, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Return whole node
    // -----------------------------------------------------------------------

    #[test]
    fn return_whole_node() {
        let (_dir, mut db) = make_graph();
        {
            let mut wtx = db.write_txn();
            execute_write("CREATE (n:Person {name: 'Alice'})", &mut wtx).unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN n", &rtx).unwrap();
        assert_eq!(result.rows.len(), 1);
        match &result.rows[0][0] {
            Value::String(s) => {
                assert!(s.contains("Person"));
                assert!(s.contains("Alice"));
            }
            other => panic!("expected string, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // No match returns empty
    // -----------------------------------------------------------------------

    #[test]
    fn no_match_returns_empty() {
        let (_dir, db) = make_graph();
        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN n", &rtx).unwrap();
        assert_eq!(result.rows.len(), 0);
    }

    // -----------------------------------------------------------------------
    // Count on empty returns 0
    // -----------------------------------------------------------------------

    #[test]
    fn count_empty() {
        let (_dir, db) = make_graph();
        let rtx = db.read_txn();
        let result = execute_read("MATCH (n:Person) RETURN count(n)", &rtx).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], Value::Int(0));
    }
}
