pub mod node;
pub mod tree;

#[cfg(test)]
mod proptest_tests;

pub use tree::BTree;

use gemgraph_pager::pager::PagerError;

/// Maximum allowed key size in bytes.
pub const MAX_KEY_SIZE: usize = 1000;
/// Maximum allowed value size in bytes.
pub const MAX_VALUE_SIZE: usize = 1000;

#[derive(Debug, thiserror::Error)]
pub enum BTreeError {
    #[error("pager error: {0}")]
    Pager(#[from] PagerError),
    #[error("key too large: {0} bytes (max {1})")]
    KeyTooLarge(usize, usize),
    #[error("value too large: {0} bytes (max {1})")]
    ValueTooLarge(usize, usize),
    #[error("corrupt node")]
    CorruptNode,
}

pub type Result<T> = std::result::Result<T, BTreeError>;
