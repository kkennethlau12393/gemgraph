//! GemGraph — an embeddable graph database in Rust.
//!
//! Re-exports the public API from the component crates.

pub use gemgraph_graph::{
    GraphDb, GraphReadTxn, GraphWriteTxn,
    Node, Edge, NodeId, EdgeId, Value, Direction,
    GraphError,
};

pub use gemgraph_cypher::{
    execute_read, execute_write,
    QueryResult, QueryStats,
    CypherError,
};

pub mod storage {
    pub use gemgraph_pager::{Pager, PageId, PageType, Meta, PagerError, PAGE_SIZE};
    pub use gemgraph_wal::{WalWriter, WalReader, WalRecord, WalError, recover, RecoveryAction};
    pub use gemgraph_btree::{BTree, BTreeError};
    pub use gemgraph_mvcc::{Database, ReadTxn, WriteTxn, TreeId, MvccError};
}
