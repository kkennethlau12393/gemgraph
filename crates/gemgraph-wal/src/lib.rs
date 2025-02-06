//! Write-Ahead Log for gemgraph crash recovery.
//!
//! Before the pager commits changes to the data file, all mutations are first
//! appended to the WAL. On crash recovery the WAL is replayed to restore
//! uncommitted-but-durable changes.

pub mod record;
pub mod writer;
pub mod reader;
pub mod recovery;

pub use record::WalRecord;
pub use writer::WalWriter;
pub use reader::{WalReader, WalIter};
pub use recovery::{recover, RecoveryAction};

/// Errors produced by WAL operations.
#[derive(Debug, thiserror::Error)]
pub enum WalError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("corrupt WAL record")]
    CorruptRecord,

    #[error("unexpected end of file")]
    UnexpectedEof,
}
