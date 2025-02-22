use crate::types::{Direction, NodeId, EdgeId};

/// FNV-1a hash truncated to u32. Deterministic across runs.
pub fn hash_str(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ---------------------------------------------------------------------------
// Adjacency key: 29 bytes
// [src_node_id: 8 BE] [direction: 1] [edge_type_hash: 4 BE] [dst_node_id: 8 BE] [edge_id: 8 BE]
// ---------------------------------------------------------------------------

pub const ADJ_KEY_LEN: usize = 29;

pub fn encode_adjacency_key(
    src: NodeId,
    dir: Direction,
    edge_type_hash: u32,
    dst: NodeId,
    edge_id: EdgeId,
) -> [u8; ADJ_KEY_LEN] {
    let mut buf = [0u8; ADJ_KEY_LEN];
    buf[0..8].copy_from_slice(&src.to_be_bytes());
    buf[8] = dir as u8;
    buf[9..13].copy_from_slice(&edge_type_hash.to_be_bytes());
    buf[13..21].copy_from_slice(&dst.to_be_bytes());
    buf[21..29].copy_from_slice(&edge_id.to_be_bytes());
    buf
}

pub fn decode_adjacency_key(key: &[u8]) -> (NodeId, Direction, u32, NodeId, EdgeId) {
    let src = u64::from_be_bytes(key[0..8].try_into().unwrap());
    let dir = if key[8] == 0 { Direction::Out } else { Direction::In };
    let type_hash = u32::from_be_bytes(key[9..13].try_into().unwrap());
    let dst = u64::from_be_bytes(key[13..21].try_into().unwrap());
    let edge_id = u64::from_be_bytes(key[21..29].try_into().unwrap());
    (src, dir, type_hash, dst, edge_id)
}

/// Build start key for range scan: all outgoing edges of `node` with a specific type.
pub fn adj_prefix_start(node: NodeId, dir: Direction, type_hash: u32) -> [u8; ADJ_KEY_LEN] {
    encode_adjacency_key(node, dir, type_hash, 0, 0)
}

/// Build end key for range scan: all outgoing edges of `node` with a specific type.
pub fn adj_prefix_end(node: NodeId, dir: Direction, type_hash: u32) -> [u8; ADJ_KEY_LEN] {
    encode_adjacency_key(node, dir, type_hash, u64::MAX, u64::MAX)
}

/// Build start key for all edges of `node` in a direction (any type).
pub fn adj_dir_start(node: NodeId, dir: Direction) -> [u8; ADJ_KEY_LEN] {
    encode_adjacency_key(node, dir, 0, 0, 0)
}

/// Build end key for all edges of `node` in a direction (any type).
pub fn adj_dir_end(node: NodeId, dir: Direction) -> [u8; ADJ_KEY_LEN] {
    encode_adjacency_key(node, dir, u32::MAX, u64::MAX, u64::MAX)
}

// ---------------------------------------------------------------------------
// Label index key: 12 bytes
// [label_hash: 4 BE] [node_id: 8 BE]
// ---------------------------------------------------------------------------

pub const LABEL_KEY_LEN: usize = 12;

pub fn encode_label_key(label_hash: u32, node_id: NodeId) -> [u8; LABEL_KEY_LEN] {
    let mut buf = [0u8; LABEL_KEY_LEN];
    buf[0..4].copy_from_slice(&label_hash.to_be_bytes());
    buf[4..12].copy_from_slice(&node_id.to_be_bytes());
    buf
}

pub fn decode_label_key(key: &[u8]) -> (u32, NodeId) {
    let hash = u32::from_be_bytes(key[0..4].try_into().unwrap());
    let node_id = u64::from_be_bytes(key[4..12].try_into().unwrap());
    (hash, node_id)
}

pub fn label_prefix_start(label_hash: u32) -> [u8; LABEL_KEY_LEN] {
    encode_label_key(label_hash, 0)
}

pub fn label_prefix_end(label_hash: u32) -> [u8; LABEL_KEY_LEN] {
    encode_label_key(label_hash, u64::MAX)
}
