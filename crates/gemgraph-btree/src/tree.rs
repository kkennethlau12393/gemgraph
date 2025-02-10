//! B+tree implementation using the pager for storage.

use gemgraph_pager::page::{PageId, PageType, read_page_header};
use gemgraph_pager::pager::Pager;

use crate::node::{InternalNode, LeafNode};
use crate::{BTreeError, Result, MAX_KEY_SIZE, MAX_VALUE_SIZE};

/// A B+tree stored in pager pages.
pub struct BTree<'a> {
    pager: &'a mut Pager,
    root: PageId,
}

impl<'a> BTree<'a> {
    /// Create a new, empty B+tree. Allocates a root leaf page.
    pub fn new(pager: &'a mut Pager) -> Self {
        let root_id = pager.alloc_page();
        let root = LeafNode::new(root_id);
        let buf = root.to_page();
        pager.write_page(root_id, &buf).expect("write root page");
        BTree {
            pager,
            root: root_id,
        }
    }

    /// Open an existing B+tree with the given root page.
    pub fn open(pager: &'a mut Pager, root: PageId) -> Self {
        BTree { pager, root }
    }

    /// Return the current root page id.
    pub fn root(&self) -> PageId {
        self.root
    }

    /// Point lookup.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        validate_key(key)?;
        let leaf = self.find_leaf(key)?;
        match leaf.search(key) {
            Ok(i) => Ok(Some(leaf.entries[i].value.clone())),
            Err(_) => Ok(None),
        }
    }

    /// Insert or update a key-value pair.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        validate_key(key)?;
        validate_value(value)?;

        // Build a path from root to the target leaf.
        let path = self.find_path(key)?;
        let leaf_page_id = path.last().unwrap().0;

        let buf = self.pager.read_page(leaf_page_id)?;
        let mut leaf = LeafNode::from_page(&buf)?;

        // Check if this is an update (no extra space needed for the slot, just value diff).
        if let Ok(idx) = leaf.search(key) {
            // Update in place — always fits (we rewrite the whole page anyway).
            leaf.entries[idx].value = value.to_vec();
            let buf = leaf.to_page();
            self.pager.write_page(leaf.page_id, &buf)?;
            return Ok(());
        }

        // New insert — check if it fits.
        if leaf.can_fit(key.len(), value.len()) {
            leaf.upsert(key, value);
            let buf = leaf.to_page();
            self.pager.write_page(leaf.page_id, &buf)?;
            return Ok(());
        }

        // Need to split. Insert into the in-memory node first, then split.
        leaf.upsert(key, value);
        let new_page_id = self.pager.alloc_page();
        let (right, sep_key) = leaf.split(new_page_id);

        // Write both leaf pages.
        let buf_l = leaf.to_page();
        self.pager.write_page(leaf.page_id, &buf_l)?;
        let buf_r = right.to_page();
        self.pager.write_page(right.page_id, &buf_r)?;

        // Propagate the split upward.
        self.insert_into_parent(&path, sep_key, leaf.page_id, right.page_id)?;

        Ok(())
    }

    /// Delete a key. Returns true if the key existed.
    pub fn delete(&mut self, key: &[u8]) -> Result<bool> {
        validate_key(key)?;
        let leaf = self.find_leaf_mut(key)?;
        let leaf_page_id = leaf.page_id;
        let mut leaf = leaf;
        let existed = leaf.delete(key);
        if existed {
            let buf = leaf.to_page();
            self.pager.write_page(leaf_page_id, &buf)?;
        }
        Ok(existed)
    }

    /// Inclusive range scan: returns all (key, value) pairs where start <= key <= end.
    pub fn range(&mut self, start: &[u8], end: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut results = Vec::new();
        self.collect_range(self.root, start, end, &mut results)?;
        Ok(results)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Find the leaf node that should contain the given key.
    fn find_leaf(&mut self, key: &[u8]) -> Result<LeafNode> {
        let mut page_id = self.root;
        loop {
            let buf = self.pager.read_page(page_id)?;
            let (pt, _) = read_page_header(&buf);
            match pt {
                PageType::BtreeLeaf => return LeafNode::from_page(&buf),
                PageType::BtreeInternal => {
                    let node = InternalNode::from_page(&buf)?;
                    let (child, _) = node.find_child(key);
                    page_id = child;
                }
                _ => return Err(BTreeError::CorruptNode),
            }
        }
    }

    /// Same as find_leaf but returns a mutable-ready owned LeafNode.
    fn find_leaf_mut(&mut self, key: &[u8]) -> Result<LeafNode> {
        self.find_leaf(key)
    }

    /// Build a path from root to the leaf containing `key`.
    /// Returns a vector of (page_id, child_index) pairs.
    /// The last element is the leaf with child_index = 0 (unused).
    fn find_path(&mut self, key: &[u8]) -> Result<Vec<(PageId, usize)>> {
        let mut path = Vec::new();
        let mut page_id = self.root;
        loop {
            let buf = self.pager.read_page(page_id)?;
            let (pt, _) = read_page_header(&buf);
            match pt {
                PageType::BtreeLeaf => {
                    path.push((page_id, 0));
                    return Ok(path);
                }
                PageType::BtreeInternal => {
                    let node = InternalNode::from_page(&buf)?;
                    let (child, idx) = node.find_child(key);
                    path.push((page_id, idx));
                    page_id = child;
                }
                _ => return Err(BTreeError::CorruptNode),
            }
        }
    }

    /// After splitting a child, insert the separator key into the parent.
    /// `path` is the path from root to the leaf (inclusive).
    /// The leaf is the last element. We work upward from the second-to-last.
    fn insert_into_parent(
        &mut self,
        path: &[(PageId, usize)],
        mut sep_key: Vec<u8>,
        mut left_child: PageId,
        mut right_child: PageId,
    ) -> Result<()> {
        // Walk up the path (skip the leaf at the end).
        for i in (0..path.len() - 1).rev() {
            let parent_page_id = path[i].0;
            let buf = self.pager.read_page(parent_page_id)?;
            let mut parent = InternalNode::from_page(&buf)?;

            if parent.can_fit(sep_key.len()) {
                parent.insert_key(sep_key, left_child, right_child);
                let buf = parent.to_page();
                self.pager.write_page(parent.page_id, &buf)?;
                return Ok(());
            }

            // Parent is full — split it.
            parent.insert_key(sep_key, left_child, right_child);
            let new_page_id = self.pager.alloc_page();
            let (right_node, pushed_key) = parent.split(new_page_id);

            let buf_l = parent.to_page();
            self.pager.write_page(parent.page_id, &buf_l)?;
            let buf_r = right_node.to_page();
            self.pager.write_page(right_node.page_id, &buf_r)?;

            sep_key = pushed_key;
            left_child = parent.page_id;
            right_child = right_node.page_id;
        }

        // If we get here, the root itself was split. Create a new root.
        let new_root_id = self.pager.alloc_page();
        let new_root = InternalNode::new(new_root_id, left_child, sep_key, right_child);
        let buf = new_root.to_page();
        self.pager.write_page(new_root_id, &buf)?;
        self.root = new_root_id;

        Ok(())
    }

    /// Recursively collect entries in [start, end] from the subtree rooted at `page_id`.
    fn collect_range(
        &mut self,
        page_id: PageId,
        start: &[u8],
        end: &[u8],
        results: &mut Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Result<()> {
        let buf = self.pager.read_page(page_id)?;
        let (pt, _) = read_page_header(&buf);

        match pt {
            PageType::BtreeLeaf => {
                let leaf = LeafNode::from_page(&buf)?;
                for entry in &leaf.entries {
                    if entry.key.as_slice() >= start && entry.key.as_slice() <= end {
                        results.push((entry.key.clone(), entry.value.clone()));
                    }
                }
            }
            PageType::BtreeInternal => {
                let node = InternalNode::from_page(&buf)?;
                // We need to visit all children whose key ranges could overlap [start, end].
                for entry in &node.entries {
                    // child i covers keys < entry.key (or [prev_key, entry.key) ).
                    // We should visit it if start < entry.key (the child might have keys >= start).
                    if start < entry.key.as_slice() {
                        self.collect_range(entry.child, start, end, results)?;
                    }
                    // If end < entry.key, no need to check further children.
                    if end < entry.key.as_slice() {
                        return Ok(());
                    }
                }
                // Visit right_child (covers keys >= last key).
                self.collect_range(node.right_child, start, end, results)?;
            }
            _ => return Err(BTreeError::CorruptNode),
        }

        Ok(())
    }
}

fn validate_key(key: &[u8]) -> Result<()> {
    if key.len() > MAX_KEY_SIZE {
        return Err(BTreeError::KeyTooLarge(key.len(), MAX_KEY_SIZE));
    }
    Ok(())
}

fn validate_value(value: &[u8]) -> Result<()> {
    if value.len() > MAX_VALUE_SIZE {
        return Err(BTreeError::ValueTooLarge(value.len(), MAX_VALUE_SIZE));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_pager() -> (tempfile::TempDir, Pager) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");
        let pager = Pager::create(&path).unwrap();
        (dir, pager)
    }

    #[test]
    fn empty_tree_get_returns_none() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        assert!(tree.get(b"anything").unwrap().is_none());
    }

    #[test]
    fn insert_and_get_single() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        tree.insert(b"hello", b"world").unwrap();
        assert_eq!(tree.get(b"hello").unwrap(), Some(b"world".to_vec()));
        assert!(tree.get(b"nope").unwrap().is_none());
    }

    #[test]
    fn insert_update() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        tree.insert(b"key", b"old").unwrap();
        tree.insert(b"key", b"new").unwrap();
        assert_eq!(tree.get(b"key").unwrap(), Some(b"new".to_vec()));
    }

    #[test]
    fn delete_existing() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        tree.insert(b"a", b"1").unwrap();
        tree.insert(b"b", b"2").unwrap();
        assert!(tree.delete(b"a").unwrap());
        assert!(tree.get(b"a").unwrap().is_none());
        assert_eq!(tree.get(b"b").unwrap(), Some(b"2".to_vec()));
    }

    #[test]
    fn delete_nonexistent() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        assert!(!tree.delete(b"nope").unwrap());
    }

    #[test]
    fn range_scan() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        for i in 0u32..20 {
            let key = format!("key{:04}", i);
            let val = format!("val{:04}", i);
            tree.insert(key.as_bytes(), val.as_bytes()).unwrap();
        }
        let results = tree.range(b"key0005", b"key0010").unwrap();
        assert_eq!(results.len(), 6); // keys 5..=10
        assert_eq!(results[0].0, b"key0005");
        assert_eq!(results[5].0, b"key0010");
    }

    #[test]
    fn range_scan_empty_result() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        tree.insert(b"a", b"1").unwrap();
        tree.insert(b"z", b"2").unwrap();
        let results = tree.range(b"m", b"n").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn insert_1000_sequential_keys() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        for i in 0u32..1000 {
            let key = format!("key{:06}", i);
            let val = format!("val{:06}", i);
            tree.insert(key.as_bytes(), val.as_bytes()).unwrap();
        }
        // Verify all keys.
        for i in 0u32..1000 {
            let key = format!("key{:06}", i);
            let val = format!("val{:06}", i);
            let got = tree.get(key.as_bytes()).unwrap();
            assert_eq!(got, Some(val.into_bytes()), "mismatch at key {}", i);
        }
    }

    #[test]
    fn insert_1000_random_keys() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);

        // Simple LCG for deterministic "random" order.
        let mut keys: Vec<u32> = (0..1000).collect();
        let mut seed: u64 = 12345;
        for i in (1..keys.len()).rev() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let j = (seed >> 33) as usize % (i + 1);
            keys.swap(i, j);
        }

        for &i in &keys {
            let key = format!("key{:06}", i);
            let val = format!("val{:06}", i);
            tree.insert(key.as_bytes(), val.as_bytes()).unwrap();
        }
        for i in 0u32..1000 {
            let key = format!("key{:06}", i);
            let val = format!("val{:06}", i);
            let got = tree.get(key.as_bytes()).unwrap();
            assert_eq!(got, Some(val.into_bytes()), "mismatch at key {}", i);
        }
    }

    #[test]
    fn splits_happen() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        let initial_root = tree.root();

        // Insert enough to force splits. With 20-byte key+value per entry and
        // ~4087 bytes of usable space, a leaf holds ~200 entries.
        for i in 0u32..500 {
            let key = format!("key{:06}", i);
            let val = format!("val{:06}", i);
            tree.insert(key.as_bytes(), val.as_bytes()).unwrap();
        }

        // Root should have changed (new internal root after split).
        assert_ne!(tree.root(), initial_root);

        // Verify data integrity.
        for i in 0u32..500 {
            let key = format!("key{:06}", i);
            let val = format!("val{:06}", i);
            assert_eq!(tree.get(key.as_bytes()).unwrap(), Some(val.into_bytes()));
        }
    }

    #[test]
    fn insert_delete_reinsert() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);

        tree.insert(b"key1", b"val1").unwrap();
        tree.insert(b"key2", b"val2").unwrap();
        assert!(tree.delete(b"key1").unwrap());
        assert!(tree.get(b"key1").unwrap().is_none());

        tree.insert(b"key1", b"val1_new").unwrap();
        assert_eq!(tree.get(b"key1").unwrap(), Some(b"val1_new".to_vec()));
    }

    #[test]
    fn key_too_large() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        let big_key = vec![0u8; MAX_KEY_SIZE + 1];
        let err = tree.insert(&big_key, b"val").unwrap_err();
        assert!(matches!(err, BTreeError::KeyTooLarge(_, _)));
    }

    #[test]
    fn value_too_large() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        let big_val = vec![0u8; MAX_VALUE_SIZE + 1];
        let err = tree.insert(b"key", &big_val).unwrap_err();
        assert!(matches!(err, BTreeError::ValueTooLarge(_, _)));
    }

    #[test]
    fn range_scan_with_splits() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        for i in 0u32..500 {
            let key = format!("key{:06}", i);
            let val = format!("val{:06}", i);
            tree.insert(key.as_bytes(), val.as_bytes()).unwrap();
        }
        let results = tree.range(b"key000100", b"key000199").unwrap();
        assert_eq!(results.len(), 100);
        for (i, (k, _v)) in results.iter().enumerate() {
            let expected = format!("key{:06}", 100 + i as u32);
            assert_eq!(k, expected.as_bytes());
        }
    }

    #[test]
    fn empty_range_scan() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        let results = tree.range(b"a", b"z").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn open_existing_tree() {
        let (_dir, mut pager) = make_pager();
        let root_id;
        {
            let mut tree = BTree::new(&mut pager);
            tree.insert(b"persist", b"me").unwrap();
            root_id = tree.root();
        }
        {
            let mut tree = BTree::open(&mut pager, root_id);
            assert_eq!(tree.get(b"persist").unwrap(), Some(b"me".to_vec()));
        }
    }

    #[test]
    fn delete_after_splits() {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        for i in 0u32..300 {
            let key = format!("key{:06}", i);
            let val = format!("val{:06}", i);
            tree.insert(key.as_bytes(), val.as_bytes()).unwrap();
        }
        // Delete every other key.
        for i in (0u32..300).step_by(2) {
            let key = format!("key{:06}", i);
            assert!(tree.delete(key.as_bytes()).unwrap());
        }
        // Verify deleted keys are gone and remaining are present.
        for i in 0u32..300 {
            let key = format!("key{:06}", i);
            let val = format!("val{:06}", i);
            if i % 2 == 0 {
                assert!(tree.get(key.as_bytes()).unwrap().is_none());
            } else {
                assert_eq!(tree.get(key.as_bytes()).unwrap(), Some(val.into_bytes()));
            }
        }
    }
}
