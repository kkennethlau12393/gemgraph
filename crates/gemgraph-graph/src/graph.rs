use std::path::Path;

use gemgraph_mvcc::{Database, TreeId};

use crate::{GraphError, GraphReadTxn, GraphWriteTxn, Result};

const MAGIC: &[u8] = b"gemgraph_v1";

pub struct GraphDb {
    db: Database,
    nodes_tree: TreeId,
    edges_tree: TreeId,
    adjacency_tree: TreeId,
    label_index_tree: TreeId,
    meta_tree: TreeId,
}

impl GraphDb {
    /// Create a new graph database at the given path.
    pub fn create(path: &Path) -> Result<Self> {
        let mut db = Database::create(path)?;

        // Create 5 trees in deterministic order inside a single write txn.
        let (meta_tree, nodes_tree, edges_tree, adjacency_tree, label_index_tree);
        {
            let mut wtx = db.write_txn();
            meta_tree = wtx.create_tree()?;        // TreeId(1)
            nodes_tree = wtx.create_tree()?;        // TreeId(2)
            edges_tree = wtx.create_tree()?;        // TreeId(3)
            adjacency_tree = wtx.create_tree()?;    // TreeId(4)
            label_index_tree = wtx.create_tree()?;  // TreeId(5)

            // Write magic marker
            wtx.insert(meta_tree, b"magic", MAGIC)?;

            // Initialize ID counters
            wtx.insert(meta_tree, b"next_node_id", &1u64.to_le_bytes())?;
            wtx.insert(meta_tree, b"next_edge_id", &1u64.to_le_bytes())?;

            wtx.commit()?;
        }

        Ok(GraphDb {
            db,
            nodes_tree,
            edges_tree,
            adjacency_tree,
            label_index_tree,
            meta_tree,
        })
    }

    /// Open an existing graph database.
    pub fn open(path: &Path) -> Result<Self> {
        let db = Database::open(path)?;

        // Trees were created in deterministic order.
        let meta_tree = TreeId(1);
        let nodes_tree = TreeId(2);
        let edges_tree = TreeId(3);
        let adjacency_tree = TreeId(4);
        let label_index_tree = TreeId(5);

        // Verify magic marker.
        {
            let rtx = db.read_txn();
            let magic = rtx.get(meta_tree, b"magic")?;
            match magic {
                Some(v) if v == MAGIC => {}
                _ => {
                    return Err(GraphError::InvalidDatabase(
                        "missing or invalid magic marker".into(),
                    ));
                }
            }
        }

        Ok(GraphDb {
            db,
            nodes_tree,
            edges_tree,
            adjacency_tree,
            label_index_tree,
            meta_tree,
        })
    }

    pub fn read_txn(&self) -> GraphReadTxn<'_> {
        GraphReadTxn {
            txn: self.db.read_txn(),
            nodes_tree: self.nodes_tree,
            edges_tree: self.edges_tree,
            adjacency_tree: self.adjacency_tree,
            label_index_tree: self.label_index_tree,
        }
    }

    pub fn write_txn(&mut self) -> GraphWriteTxn<'_> {
        GraphWriteTxn {
            txn: self.db.write_txn(),
            nodes_tree: self.nodes_tree,
            edges_tree: self.edges_tree,
            adjacency_tree: self.adjacency_tree,
            label_index_tree: self.label_index_tree,
            meta_tree: self.meta_tree,
        }
    }
}
