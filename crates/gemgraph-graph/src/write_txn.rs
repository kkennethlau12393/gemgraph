use std::collections::HashMap;

use gemgraph_mvcc::{WriteTxn, TreeId};

use crate::keys;
use crate::types::*;
use crate::{GraphError, Result};

pub struct GraphWriteTxn<'db> {
    pub(crate) txn: WriteTxn<'db>,
    pub(crate) nodes_tree: TreeId,
    pub(crate) edges_tree: TreeId,
    pub(crate) adjacency_tree: TreeId,
    pub(crate) label_index_tree: TreeId,
    pub(crate) meta_tree: TreeId,
}

impl<'db> GraphWriteTxn<'db> {
    // ------------------------------------------------------------------
    // ID allocation
    // ------------------------------------------------------------------

    fn alloc_node_id(&mut self) -> Result<NodeId> {
        let raw = self.txn.get(self.meta_tree, b"next_node_id")?
            .expect("next_node_id missing");
        let id = u64::from_le_bytes(raw.try_into().unwrap());
        let next = id + 1;
        self.txn.insert(self.meta_tree, b"next_node_id", &next.to_le_bytes())?;
        Ok(id)
    }

    fn alloc_edge_id(&mut self) -> Result<EdgeId> {
        let raw = self.txn.get(self.meta_tree, b"next_edge_id")?
            .expect("next_edge_id missing");
        let id = u64::from_le_bytes(raw.try_into().unwrap());
        let next = id + 1;
        self.txn.insert(self.meta_tree, b"next_edge_id", &next.to_le_bytes())?;
        Ok(id)
    }

    // ------------------------------------------------------------------
    // Create
    // ------------------------------------------------------------------

    pub fn create_node(&mut self, label: &str, props: HashMap<String, Value>) -> Result<NodeId> {
        let id = self.alloc_node_id()?;
        let rec = NodeRecord {
            label: label.to_string(),
            props,
        };
        let data = bincode::serialize(&rec)?;
        self.txn.insert(self.nodes_tree, &id.to_be_bytes(), &data)?;

        // Label index
        let label_hash = keys::hash_str(label);
        let label_key = keys::encode_label_key(label_hash, id);
        self.txn.insert(self.label_index_tree, &label_key, &[])?;

        Ok(id)
    }

    pub fn create_edge(
        &mut self,
        src: NodeId,
        dst: NodeId,
        edge_type: &str,
        props: HashMap<String, Value>,
    ) -> Result<EdgeId> {
        // Verify both nodes exist.
        if self.txn.get(self.nodes_tree, &src.to_be_bytes())?.is_none() {
            return Err(GraphError::NodeNotFound(src));
        }
        if self.txn.get(self.nodes_tree, &dst.to_be_bytes())?.is_none() {
            return Err(GraphError::NodeNotFound(dst));
        }

        let id = self.alloc_edge_id()?;
        let rec = EdgeRecord {
            src,
            dst,
            edge_type: edge_type.to_string(),
            props,
        };
        let data = bincode::serialize(&rec)?;
        self.txn.insert(self.edges_tree, &id.to_be_bytes(), &data)?;

        // Adjacency: outgoing from src
        let type_hash = keys::hash_str(edge_type);
        let out_key = keys::encode_adjacency_key(src, Direction::Out, type_hash, dst, id);
        self.txn.insert(self.adjacency_tree, &out_key, &[])?;

        // Adjacency: incoming to dst
        let in_key = keys::encode_adjacency_key(dst, Direction::In, type_hash, src, id);
        self.txn.insert(self.adjacency_tree, &in_key, &[])?;

        Ok(id)
    }

    // ------------------------------------------------------------------
    // Delete
    // ------------------------------------------------------------------

    pub fn delete_node(&mut self, id: NodeId) -> Result<bool> {
        let key = id.to_be_bytes();
        let data = match self.txn.get(self.nodes_tree, &key)? {
            Some(d) => d,
            None => return Ok(false),
        };

        let rec: NodeRecord = bincode::deserialize(&data)?;

        // Remove label index entry
        let label_hash = keys::hash_str(&rec.label);
        let label_key = keys::encode_label_key(label_hash, id);
        self.txn.delete(self.label_index_tree, &label_key)?;

        // Remove all outgoing adjacency entries and their edge records
        let out_start = keys::adj_dir_start(id, Direction::Out);
        let out_end = keys::adj_dir_end(id, Direction::Out);
        let out_entries = self.txn.range(self.adjacency_tree, &out_start, &out_end)?;
        for (adj_key, _) in &out_entries {
            let (_src, _dir, type_hash, dst, edge_id) = keys::decode_adjacency_key(adj_key);
            // Remove the reverse (incoming) adjacency entry on the other node
            let in_key = keys::encode_adjacency_key(dst, Direction::In, type_hash, id, edge_id);
            self.txn.delete(self.adjacency_tree, &in_key)?;
            // Remove the edge record
            self.txn.delete(self.edges_tree, &edge_id.to_be_bytes())?;
            // Remove the outgoing adjacency entry
            self.txn.delete(self.adjacency_tree, adj_key)?;
        }

        // Remove all incoming adjacency entries and their edge records
        let in_start = keys::adj_dir_start(id, Direction::In);
        let in_end = keys::adj_dir_end(id, Direction::In);
        let in_entries = self.txn.range(self.adjacency_tree, &in_start, &in_end)?;
        for (adj_key, _) in &in_entries {
            let (_src, _dir, type_hash, dst, edge_id) = keys::decode_adjacency_key(adj_key);
            // Remove the reverse (outgoing) adjacency entry on the other node
            let out_key = keys::encode_adjacency_key(dst, Direction::Out, type_hash, id, edge_id);
            self.txn.delete(self.adjacency_tree, &out_key)?;
            // Remove the edge record
            self.txn.delete(self.edges_tree, &edge_id.to_be_bytes())?;
            // Remove the incoming adjacency entry
            self.txn.delete(self.adjacency_tree, adj_key)?;
        }

        // Remove the node record itself
        self.txn.delete(self.nodes_tree, &key)?;

        Ok(true)
    }

    pub fn delete_edge(&mut self, id: EdgeId) -> Result<bool> {
        let key = id.to_be_bytes();
        let data = match self.txn.get(self.edges_tree, &key)? {
            Some(d) => d,
            None => return Ok(false),
        };

        let rec: EdgeRecord = bincode::deserialize(&data)?;
        let type_hash = keys::hash_str(&rec.edge_type);

        // Remove outgoing adjacency
        let out_key = keys::encode_adjacency_key(rec.src, Direction::Out, type_hash, rec.dst, id);
        self.txn.delete(self.adjacency_tree, &out_key)?;

        // Remove incoming adjacency
        let in_key = keys::encode_adjacency_key(rec.dst, Direction::In, type_hash, rec.src, id);
        self.txn.delete(self.adjacency_tree, &in_key)?;

        // Remove the edge record
        self.txn.delete(self.edges_tree, &key)?;

        Ok(true)
    }

    // ------------------------------------------------------------------
    // Properties
    // ------------------------------------------------------------------

    pub fn set_node_property(&mut self, id: NodeId, key: &str, value: Value) -> Result<()> {
        let node_key = id.to_be_bytes();
        let data = self.txn.get(self.nodes_tree, &node_key)?
            .ok_or(GraphError::NodeNotFound(id))?;
        let mut rec: NodeRecord = bincode::deserialize(&data)?;
        rec.props.insert(key.to_string(), value);
        let new_data = bincode::serialize(&rec)?;
        self.txn.insert(self.nodes_tree, &node_key, &new_data)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Read operations (delegated through write txn)
    // ------------------------------------------------------------------

    pub fn get_node(&self, id: NodeId) -> Result<Option<Node>> {
        let key = id.to_be_bytes();
        match self.txn.get(self.nodes_tree, &key)? {
            Some(data) => {
                let rec: NodeRecord = bincode::deserialize(&data)?;
                Ok(Some(Node {
                    id,
                    label: rec.label,
                    properties: rec.props,
                }))
            }
            None => Ok(None),
        }
    }

    pub fn get_edge(&self, id: EdgeId) -> Result<Option<Edge>> {
        let key = id.to_be_bytes();
        match self.txn.get(self.edges_tree, &key)? {
            Some(data) => {
                let rec: EdgeRecord = bincode::deserialize(&data)?;
                Ok(Some(Edge {
                    id,
                    src: rec.src,
                    dst: rec.dst,
                    edge_type: rec.edge_type,
                    properties: rec.props,
                }))
            }
            None => Ok(None),
        }
    }

    pub fn neighbors(
        &self,
        node_id: NodeId,
        direction: Direction,
        edge_type: Option<&str>,
    ) -> Result<Vec<(EdgeId, NodeId)>> {
        let (start, end) = match edge_type {
            Some(t) => {
                let h = keys::hash_str(t);
                (keys::adj_prefix_start(node_id, direction, h),
                 keys::adj_prefix_end(node_id, direction, h))
            }
            None => {
                (keys::adj_dir_start(node_id, direction),
                 keys::adj_dir_end(node_id, direction))
            }
        };

        let entries = self.txn.range(self.adjacency_tree, &start, &end)?;
        let mut results = Vec::with_capacity(entries.len());
        for (key, _) in &entries {
            let (_src, _dir, _type_hash, dst, edge_id) = keys::decode_adjacency_key(key);
            results.push((edge_id, dst));
        }
        Ok(results)
    }

    pub fn nodes_by_label(&self, label: &str) -> Result<Vec<NodeId>> {
        let h = keys::hash_str(label);
        let start = keys::label_prefix_start(h);
        let end = keys::label_prefix_end(h);

        let entries = self.txn.range(self.label_index_tree, &start, &end)?;
        let mut results = Vec::with_capacity(entries.len());
        for (key, _) in &entries {
            let (_hash, node_id) = keys::decode_label_key(key);
            results.push(node_id);
        }
        Ok(results)
    }

    // ------------------------------------------------------------------
    // Transaction control
    // ------------------------------------------------------------------

    pub fn commit(self) -> Result<()> {
        self.txn.commit()?;
        Ok(())
    }

    pub fn abort(self) {
        self.txn.abort();
    }
}
