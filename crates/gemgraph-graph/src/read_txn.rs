use gemgraph_mvcc::{ReadTxn, TreeId};

use crate::keys;
use crate::types::*;
use crate::Result;

pub struct GraphReadTxn<'db> {
    pub(crate) txn: ReadTxn<'db>,
    pub(crate) nodes_tree: TreeId,
    pub(crate) edges_tree: TreeId,
    pub(crate) adjacency_tree: TreeId,
    pub(crate) label_index_tree: TreeId,
}

impl<'db> GraphReadTxn<'db> {
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

    pub fn get_node_property(&self, id: NodeId, key: &str) -> Result<Option<Value>> {
        match self.get_node(id)? {
            Some(node) => Ok(node.properties.get(key).cloned()),
            None => Ok(None),
        }
    }
}
