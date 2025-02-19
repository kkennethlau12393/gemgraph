//! Database: the top-level handle that owns the pager, WAL, and committed state.

use std::cell::UnsafeCell;
use std::path::{Path, PathBuf};

use gemgraph_pager::pager::Pager;
use gemgraph_pager::PageId;
use gemgraph_wal::WalWriter;

use crate::Result;

/// The main database handle. Owns the pager and WAL writer.
///
/// Use `read_txn()` for read-only access and `write_txn()` for mutations.
/// Only one write transaction can be active at a time (enforced by `&mut self`).
pub struct Database {
    /// Wrapped in UnsafeCell because BTree requires `&mut Pager` even for
    /// read-only operations (get, range). This is safe in our single-threaded
    /// transaction model: ReadTxn borrows &Database (no mutations to meta/wal),
    /// and WriteTxn borrows &mut Database (exclusive access).
    pub(crate) pager: UnsafeCell<Pager>,
    pub(crate) wal: WalWriter,
    #[allow(dead_code)]
    pub(crate) db_path: PathBuf,
    #[allow(dead_code)]
    pub(crate) wal_path: PathBuf,
    /// Root of the catalog B+tree (maps TreeId -> root PageId).
    pub(crate) current_root: PageId,
    /// Monotonically increasing transaction counter.
    pub(crate) current_txn_id: u64,
}

// Safety: Database is not Sync — we rely on Rust's borrow checker to ensure
// only one &mut exists at a time. The UnsafeCell is only accessed through
// controlled paths (read_txn / write_txn).
unsafe impl Send for Database {}

impl Database {
    /// Get a mutable reference to the pager.
    ///
    /// # Safety
    /// Caller must ensure no aliasing &mut references exist. In practice this
    /// is guaranteed by our transaction model: ReadTxn holds &Database and
    /// WriteTxn holds &mut Database, so they can never coexist.
    pub(crate) fn pager_mut(&self) -> &mut Pager {
        unsafe { &mut *self.pager.get() }
    }

    /// Create a brand-new database. The data file will be at `{path}.db` and the
    /// WAL at `{path}.wal`.
    pub fn create(path: &Path) -> Result<Self> {
        let db_path = path.with_extension("db");
        let wal_path = path.with_extension("wal");

        let pager = Pager::create(&db_path)?;
        let wal = WalWriter::create(&wal_path)?;

        let current_root = PageId(pager.meta().root_page_id);
        let current_txn_id = pager.meta().txn_id;

        Ok(Database {
            pager: UnsafeCell::new(pager),
            wal,
            db_path,
            wal_path,
            current_root,
            current_txn_id,
        })
    }

    /// Open an existing database, running WAL recovery if needed.
    pub fn open(path: &Path) -> Result<Self> {
        let db_path = path.with_extension("db");
        let wal_path = path.with_extension("wal");

        let mut pager = Pager::open(&db_path)?;
        let wal_offset = pager.meta().wal_offset;

        // Run WAL recovery: replay any committed pages that haven't been
        // reflected in the data file yet.
        if wal_path.exists() {
            let actions = gemgraph_wal::recover(&wal_path, wal_offset)?;
            for action in &actions {
                match action {
                    gemgraph_wal::RecoveryAction::ReplayPage { page_id, data } => {
                        pager.write_page(PageId(*page_id), data)?;
                    }
                }
            }

            if !actions.is_empty() {
                let wal_writer_tmp = WalWriter::open(&wal_path)?;
                let new_wal_offset = wal_writer_tmp.offset();

                let mut new_meta = pager.meta().clone();
                new_meta.wal_offset = new_wal_offset;
                new_meta.txn_id += 1;
                pager.commit_meta(new_meta)?;
                pager.sync()?;
            }
        }

        let current_root = PageId(pager.meta().root_page_id);
        let current_txn_id = pager.meta().txn_id;

        let wal = if wal_path.exists() {
            WalWriter::open(&wal_path)?
        } else {
            WalWriter::create(&wal_path)?
        };

        Ok(Database {
            pager: UnsafeCell::new(pager),
            wal,
            db_path,
            wal_path,
            current_root,
            current_txn_id,
        })
    }
}
