pub mod catalog;
pub mod database;
pub mod txn;

pub use database::Database;
pub use txn::{ReadTxn, WriteTxn, TreeId};

use gemgraph_btree::BTreeError;
use gemgraph_pager::PagerError;
use gemgraph_wal::WalError;

#[derive(Debug, thiserror::Error)]
pub enum MvccError {
    #[error("pager error: {0}")]
    Pager(#[from] PagerError),

    #[error("btree error: {0}")]
    BTree(#[from] BTreeError),

    #[error("wal error: {0}")]
    Wal(#[from] WalError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tree not found: {0:?}")]
    TreeNotFound(TreeId),

    #[error("database already exists at path")]
    AlreadyExists,

    #[error("database not found at path")]
    NotFound,
}

pub type Result<T> = std::result::Result<T, MvccError>;
