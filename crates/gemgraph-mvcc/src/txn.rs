//! Read and write transactions.

use std::collections::HashMap;

use gemgraph_btree::BTree;
use gemgraph_pager::PageId;

use crate::catalog;
use crate::database::Database;
use crate::Result;

/// Identifies a user-created B+tree within the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TreeId(pub u32);

// ---------------------------------------------------------------------------
// ReadTxn
// ---------------------------------------------------------------------------

/// A read-only transaction. Sees a consistent snapshot of the database at the
/// time it was created. Borrows the database immutably.
pub struct ReadTxn<'db> {
    db: &'db Database,
    root: PageId,
}

impl Database {
    /// Begin a read-only transaction. The returned `ReadTxn` sees a snapshot as
    /// of the most recent commit.
    pub fn read_txn(&self) -> ReadTxn<'_> {
        ReadTxn {
            db: self,
            root: self.current_root,
        }
    }

    /// Begin a read-write transaction. Only one can be active at a time,
    /// enforced by the `&mut self` borrow.
    pub fn write_txn(&mut self) -> WriteTxn<'_> {
        let catalog_root = self.current_root;
        WriteTxn {
            db: self,
            catalog_root,
            tree_roots: HashMap::new(),
            committed: false,
        }
    }
}

impl<'db> ReadTxn<'db> {
    /// Point lookup in a user tree.
    pub fn get(&self, tree_id: TreeId, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let catalog_root = self.root;
        if catalog_root == PageId(0) {
            return Err(crate::MvccError::TreeNotFound(tree_id));
        }

        let pager = self.db.pager_mut();

        let tree_root = catalog::catalog_get(pager, catalog_root, tree_id)?
            .ok_or(crate::MvccError::TreeNotFound(tree_id))?;

        let mut tree = BTree::open(pager, tree_root);
        Ok(tree.get(key)?)
    }

    /// Inclusive range scan over a user tree.
    pub fn range(
        &self,
        tree_id: TreeId,
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let catalog_root = self.root;
        if catalog_root == PageId(0) {
            return Err(crate::MvccError::TreeNotFound(tree_id));
        }

        let pager = self.db.pager_mut();

        let tree_root = catalog::catalog_get(pager, catalog_root, tree_id)?
            .ok_or(crate::MvccError::TreeNotFound(tree_id))?;

        let mut tree = BTree::open(pager, tree_root);
        Ok(tree.range(start, end)?)
    }
}

// ---------------------------------------------------------------------------
// WriteTxn
// ---------------------------------------------------------------------------

/// A read-write transaction. Holds exclusive access to the database via `&mut`.
/// Changes are only made durable when `commit()` is called.
pub struct WriteTxn<'db> {
    db: &'db mut Database,
    /// Current catalog root (may change as we insert into the catalog).
    catalog_root: PageId,
    /// Cache of tree roots modified during this transaction.
    tree_roots: HashMap<TreeId, PageId>,
    /// Whether commit() was called.
    committed: bool,
}

impl<'db> WriteTxn<'db> {
    /// Create a new user tree. Returns its `TreeId`.
    pub fn create_tree(&mut self) -> Result<TreeId> {
        // Ensure catalog exists.
        self.ensure_catalog()?;

        let pager = self.db.pager_mut();
        let new_id = catalog::catalog_next_tree_id(pager, self.catalog_root)?;

        // Create an empty B+tree for this new tree.
        let btree = BTree::new(pager);
        let root = btree.root();

        // Register in catalog.
        self.catalog_root = catalog::catalog_set(pager, self.catalog_root, new_id, root)?;
        self.tree_roots.insert(new_id, root);

        Ok(new_id)
    }

    /// Point lookup in a user tree (sees uncommitted writes from this txn).
    pub fn get(&self, tree_id: TreeId, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let tree_root = self.resolve_tree_root(tree_id)?;
        let pager = self.db.pager_mut();
        let mut tree = BTree::open(pager, tree_root);
        Ok(tree.get(key)?)
    }

    /// Insert or update a key-value pair in a user tree.
    pub fn insert(&mut self, tree_id: TreeId, key: &[u8], value: &[u8]) -> Result<()> {
        let tree_root = self.resolve_tree_root(tree_id)?;
        let pager = self.db.pager_mut();
        let mut btree = BTree::open(pager, tree_root);
        btree.insert(key, value)?;
        let new_root = btree.root();

        // Update catalog if root changed (due to split).
        if new_root != tree_root {
            self.catalog_root =
                catalog::catalog_set(self.db.pager_mut(), self.catalog_root, tree_id, new_root)?;
        }
        self.tree_roots.insert(tree_id, new_root);
        Ok(())
    }

    /// Delete a key from a user tree. Returns true if the key existed.
    pub fn delete(&mut self, tree_id: TreeId, key: &[u8]) -> Result<bool> {
        let tree_root = self.resolve_tree_root(tree_id)?;
        let pager = self.db.pager_mut();
        let mut btree = BTree::open(pager, tree_root);
        let existed = btree.delete(key)?;
        let new_root = btree.root();

        if new_root != tree_root {
            self.catalog_root =
                catalog::catalog_set(self.db.pager_mut(), self.catalog_root, tree_id, new_root)?;
        }
        self.tree_roots.insert(tree_id, new_root);
        Ok(existed)
    }

    /// Inclusive range scan over a user tree.
    pub fn range(
        &self,
        tree_id: TreeId,
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let tree_root = self.resolve_tree_root(tree_id)?;
        let pager = self.db.pager_mut();
        let mut tree = BTree::open(pager, tree_root);
        Ok(tree.range(start, end)?)
    }

    /// Commit all changes made in this transaction. This is the atomic
    /// durability boundary:
    ///
    /// 1. Append a Commit record to the WAL and sync it.
    /// 2. Sync the data file (all page writes from BTree are already there).
    /// 3. Update the pager meta (new catalog root, txn_id, wal_offset).
    pub fn commit(mut self) -> Result<()> {
        let new_txn_id = self.db.current_txn_id + 1;

        // Write WAL commit record.
        self.db.wal.append_commit(new_txn_id)?;
        self.db.wal.sync()?;
        let new_wal_offset = self.db.wal.offset();

        // Sync data file (all page writes from BTree are already in the file).
        let pager = self.db.pager_mut();
        pager.sync()?;

        // Commit meta with new catalog root and txn_id.
        let mut new_meta = pager.meta().clone();
        new_meta.root_page_id = self.catalog_root.0;
        new_meta.txn_id = new_txn_id;
        new_meta.wal_offset = new_wal_offset;
        pager.commit_meta(new_meta)?;

        // Update in-memory state.
        self.db.current_root = self.catalog_root;
        self.db.current_txn_id = new_txn_id;
        self.committed = true;

        Ok(())
    }

    /// Abort this transaction. Any pages written to the pager are orphaned
    /// (they will be reclaimed eventually). The meta is not updated, so the
    /// old state remains visible.
    pub fn abort(mut self) {
        self.committed = true;
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Ensure the catalog B+tree exists. If `catalog_root` is PageId(0), create it.
    fn ensure_catalog(&mut self) -> Result<()> {
        if self.catalog_root == PageId(0) {
            let btree = BTree::new(self.db.pager_mut());
            self.catalog_root = btree.root();
        }
        Ok(())
    }

    /// Resolve the root PageId for a given TreeId. Checks the local cache
    /// first, then falls back to the catalog.
    fn resolve_tree_root(&self, tree_id: TreeId) -> Result<PageId> {
        if let Some(&root) = self.tree_roots.get(&tree_id) {
            return Ok(root);
        }

        if self.catalog_root == PageId(0) {
            return Err(crate::MvccError::TreeNotFound(tree_id));
        }

        let pager = self.db.pager_mut();

        catalog::catalog_get(pager, self.catalog_root, tree_id)?
            .ok_or(crate::MvccError::TreeNotFound(tree_id))
    }
}

impl<'db> Drop for WriteTxn<'db> {
    fn drop(&mut self) {
        let _ = self.committed;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use tempfile::tempdir;

    fn make_db() -> (tempfile::TempDir, Database) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("testdb");
        let db = Database::create(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn create_tree_insert_and_read() {
        let (_dir, mut db) = make_db();

        let tree_id;
        {
            let mut wtx = db.write_txn();
            tree_id = wtx.create_tree().unwrap();
            wtx.insert(tree_id, b"hello", b"world").unwrap();
            assert_eq!(wtx.get(tree_id, b"hello").unwrap(), Some(b"world".to_vec()));
            wtx.commit().unwrap();
        }

        {
            let rtx = db.read_txn();
            assert_eq!(rtx.get(tree_id, b"hello").unwrap(), Some(b"world".to_vec()));
        }
    }

    #[test]
    fn write_txn_commit_then_read_txn_sees_data() {
        let (_dir, mut db) = make_db();

        let tid;
        {
            let mut wtx = db.write_txn();
            tid = wtx.create_tree().unwrap();
            wtx.insert(tid, b"key1", b"val1").unwrap();
            wtx.insert(tid, b"key2", b"val2").unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        assert_eq!(rtx.get(tid, b"key1").unwrap(), Some(b"val1".to_vec()));
        assert_eq!(rtx.get(tid, b"key2").unwrap(), Some(b"val2".to_vec()));
        assert_eq!(rtx.get(tid, b"nonexistent").unwrap(), None);
    }

    #[test]
    fn sequential_write_txns() {
        let (_dir, mut db) = make_db();

        let tid;
        {
            let mut wtx = db.write_txn();
            tid = wtx.create_tree().unwrap();
            wtx.insert(tid, b"a", b"1").unwrap();
            wtx.commit().unwrap();
        }
        {
            let mut wtx = db.write_txn();
            assert_eq!(wtx.get(tid, b"a").unwrap(), Some(b"1".to_vec()));
            wtx.insert(tid, b"b", b"2").unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        assert_eq!(rtx.get(tid, b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(rtx.get(tid, b"b").unwrap(), Some(b"2".to_vec()));
    }

    #[test]
    fn abort_write_txn_catalog_unchanged() {
        let (_dir, mut db) = make_db();

        let tid;
        {
            let mut wtx = db.write_txn();
            tid = wtx.create_tree().unwrap();
            wtx.insert(tid, b"visible", b"yes").unwrap();
            wtx.commit().unwrap();
        }

        // This txn inserts but aborts — catalog root is not updated.
        {
            let mut wtx = db.write_txn();
            wtx.insert(tid, b"invisible", b"data").unwrap();
            wtx.abort();
        }

        // The committed catalog root still points to the original tree state.
        let rtx = db.read_txn();
        assert_eq!(rtx.get(tid, b"visible").unwrap(), Some(b"yes".to_vec()));
    }

    #[test]
    fn multiple_trees() {
        let (_dir, mut db) = make_db();

        let tid1;
        let tid2;
        {
            let mut wtx = db.write_txn();
            tid1 = wtx.create_tree().unwrap();
            tid2 = wtx.create_tree().unwrap();
            assert_ne!(tid1, tid2);

            wtx.insert(tid1, b"key", b"tree1").unwrap();
            wtx.insert(tid2, b"key", b"tree2").unwrap();
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        assert_eq!(rtx.get(tid1, b"key").unwrap(), Some(b"tree1".to_vec()));
        assert_eq!(rtx.get(tid2, b"key").unwrap(), Some(b"tree2".to_vec()));
    }

    #[test]
    fn close_and_reopen_persists_data() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("testdb");

        let tid;
        {
            let mut db = Database::create(&path).unwrap();
            let mut wtx = db.write_txn();
            tid = wtx.create_tree().unwrap();
            wtx.insert(tid, b"persist", b"me").unwrap();
            wtx.commit().unwrap();
        }

        {
            let db = Database::open(&path).unwrap();
            let rtx = db.read_txn();
            assert_eq!(rtx.get(tid, b"persist").unwrap(), Some(b"me".to_vec()));
        }
    }

    #[test]
    fn delete_key() {
        let (_dir, mut db) = make_db();

        let tid;
        {
            let mut wtx = db.write_txn();
            tid = wtx.create_tree().unwrap();
            wtx.insert(tid, b"a", b"1").unwrap();
            wtx.insert(tid, b"b", b"2").unwrap();
            wtx.commit().unwrap();
        }
        {
            let mut wtx = db.write_txn();
            assert!(wtx.delete(tid, b"a").unwrap());
            assert!(!wtx.delete(tid, b"nonexistent").unwrap());
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        assert_eq!(rtx.get(tid, b"a").unwrap(), None);
        assert_eq!(rtx.get(tid, b"b").unwrap(), Some(b"2".to_vec()));
    }

    #[test]
    fn range_scan() {
        let (_dir, mut db) = make_db();

        let tid;
        {
            let mut wtx = db.write_txn();
            tid = wtx.create_tree().unwrap();
            for i in 0u32..20 {
                let key = format!("key{:04}", i);
                let val = format!("val{:04}", i);
                wtx.insert(tid, key.as_bytes(), val.as_bytes()).unwrap();
            }
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        let results = rtx.range(tid, b"key0005", b"key0010").unwrap();
        assert_eq!(results.len(), 6);
        assert_eq!(results[0].0, b"key0005");
        assert_eq!(results[5].0, b"key0010");
    }

    #[test]
    fn tree_not_found() {
        let (_dir, db) = make_db();
        let rtx = db.read_txn();
        let err = rtx.get(TreeId(999), b"key");
        assert!(err.is_err());
    }

    #[test]
    fn many_keys_across_txns() {
        let (_dir, mut db) = make_db();

        let tid;
        {
            let mut wtx = db.write_txn();
            tid = wtx.create_tree().unwrap();
            for i in 0u32..100 {
                let key = format!("key{:06}", i);
                let val = format!("val{:06}", i);
                wtx.insert(tid, key.as_bytes(), val.as_bytes()).unwrap();
            }
            wtx.commit().unwrap();
        }

        {
            let mut wtx = db.write_txn();
            for i in 100u32..200 {
                let key = format!("key{:06}", i);
                let val = format!("val{:06}", i);
                wtx.insert(tid, key.as_bytes(), val.as_bytes()).unwrap();
            }
            wtx.commit().unwrap();
        }

        let rtx = db.read_txn();
        for i in 0u32..200 {
            let key = format!("key{:06}", i);
            let val = format!("val{:06}", i);
            assert_eq!(
                rtx.get(tid, key.as_bytes()).unwrap(),
                Some(val.into_bytes()),
                "mismatch at key {}",
                i
            );
        }
    }

    #[test]
    fn reopen_and_continue_writing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("testdb");

        let tid;
        {
            let mut db = Database::create(&path).unwrap();
            let mut wtx = db.write_txn();
            tid = wtx.create_tree().unwrap();
            wtx.insert(tid, b"before", b"close").unwrap();
            wtx.commit().unwrap();
        }

        {
            let mut db = Database::open(&path).unwrap();
            let mut wtx = db.write_txn();
            wtx.insert(tid, b"after", b"reopen").unwrap();
            wtx.commit().unwrap();

            let rtx = db.read_txn();
            assert_eq!(rtx.get(tid, b"before").unwrap(), Some(b"close".to_vec()));
            assert_eq!(rtx.get(tid, b"after").unwrap(), Some(b"reopen".to_vec()));
        }
    }

    #[test]
    fn write_range_scan() {
        let (_dir, mut db) = make_db();

        let tid;
        {
            let mut wtx = db.write_txn();
            tid = wtx.create_tree().unwrap();
            wtx.insert(tid, b"a", b"1").unwrap();
            wtx.insert(tid, b"b", b"2").unwrap();
            wtx.insert(tid, b"c", b"3").unwrap();

            let results = wtx.range(tid, b"a", b"b").unwrap();
            assert_eq!(results.len(), 2);
            wtx.commit().unwrap();
        }
    }

    #[test]
    fn implicit_abort_on_drop() {
        let (_dir, mut db) = make_db();

        let tid;
        {
            let mut wtx = db.write_txn();
            tid = wtx.create_tree().unwrap();
            wtx.insert(tid, b"committed", b"yes").unwrap();
            wtx.commit().unwrap();
        }

        {
            let mut wtx = db.write_txn();
            wtx.insert(tid, b"dropped", b"data").unwrap();
            // dropped here
        }

        let rtx = db.read_txn();
        assert_eq!(rtx.get(tid, b"committed").unwrap(), Some(b"yes".to_vec()));
    }
}
