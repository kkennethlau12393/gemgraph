//! In-memory representations of B+tree leaf and internal nodes.
//!
//! Each node occupies exactly one 4096-byte page. The layout after the 5-byte
//! page header is documented in the crate-level specification.

use gemgraph_pager::page::{
    PageId, PageType, PAGE_HEADER_SIZE, PAGE_SIZE, read_page_header, write_page_header,
};

use crate::{BTreeError, Result};

// ---------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------

/// Offset of `num_entries` / `num_keys` (right after page header).
const NUM_OFFSET: usize = PAGE_HEADER_SIZE; // 5
/// Offset of `data_end`.
const DATA_END_OFFSET: usize = NUM_OFFSET + 2; // 7

// -- Leaf-specific ---
/// Start of the leaf slot array.
const LEAF_SLOTS_OFFSET: usize = DATA_END_OFFSET + 2; // 9
/// Bytes per leaf slot: key_len(2) + val_len(2) + data_offset(2).
const LEAF_SLOT_SIZE: usize = 6;

// -- Internal-specific ---
/// Offset of `right_child` in an internal node.
const INTERNAL_RIGHT_CHILD_OFFSET: usize = DATA_END_OFFSET + 2; // 9
/// Start of the internal slot array.
const INTERNAL_SLOTS_OFFSET: usize = INTERNAL_RIGHT_CHILD_OFFSET + 4; // 13
/// Bytes per internal slot: key_len(2) + data_offset(2) + child(4).
const INTERNAL_SLOT_SIZE: usize = 8;

// ---------------------------------------------------------------------------
// LeafNode
// ---------------------------------------------------------------------------

/// A single key-value entry in a leaf node.
#[derive(Debug, Clone)]
pub struct LeafEntry {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

/// In-memory representation of a B+tree leaf node.
#[derive(Debug, Clone)]
pub struct LeafNode {
    pub page_id: PageId,
    pub entries: Vec<LeafEntry>,
}

impl LeafNode {
    /// Create an empty leaf node for the given page.
    pub fn new(page_id: PageId) -> Self {
        LeafNode {
            page_id,
            entries: Vec::new(),
        }
    }

    /// Deserialize a leaf node from a page buffer.
    pub fn from_page(buf: &[u8; PAGE_SIZE]) -> Result<Self> {
        let (pt, page_id) = read_page_header(buf);
        if pt != PageType::BtreeLeaf {
            return Err(BTreeError::CorruptNode);
        }
        let num = read_u16(buf, NUM_OFFSET) as usize;
        let _data_end = read_u16(buf, DATA_END_OFFSET);

        let mut entries = Vec::with_capacity(num);
        for i in 0..num {
            let slot_off = LEAF_SLOTS_OFFSET + i * LEAF_SLOT_SIZE;
            let key_len = read_u16(buf, slot_off) as usize;
            let val_len = read_u16(buf, slot_off + 2) as usize;
            let data_off = read_u16(buf, slot_off + 4) as usize;

            if data_off + key_len + val_len > PAGE_SIZE {
                return Err(BTreeError::CorruptNode);
            }
            let key = buf[data_off..data_off + key_len].to_vec();
            let value = buf[data_off + key_len..data_off + key_len + val_len].to_vec();
            entries.push(LeafEntry { key, value });
        }

        Ok(LeafNode { page_id, entries })
    }

    /// Serialize this leaf node into a page buffer.
    pub fn to_page(&self) -> [u8; PAGE_SIZE] {
        let mut buf = [0u8; PAGE_SIZE];
        write_page_header(&mut buf, PageType::BtreeLeaf, self.page_id);

        let num = self.entries.len() as u16;
        write_u16(&mut buf, NUM_OFFSET, num);

        // Data grows backward from end of page.
        let mut data_cursor = PAGE_SIZE;

        for (i, entry) in self.entries.iter().enumerate() {
            let total = entry.key.len() + entry.value.len();
            data_cursor -= total;

            buf[data_cursor..data_cursor + entry.key.len()].copy_from_slice(&entry.key);
            buf[data_cursor + entry.key.len()..data_cursor + total].copy_from_slice(&entry.value);

            let slot_off = LEAF_SLOTS_OFFSET + i * LEAF_SLOT_SIZE;
            write_u16(&mut buf, slot_off, entry.key.len() as u16);
            write_u16(&mut buf, slot_off + 2, entry.value.len() as u16);
            write_u16(&mut buf, slot_off + 4, data_cursor as u16);
        }

        write_u16(&mut buf, DATA_END_OFFSET, data_cursor as u16);
        buf
    }

    /// Returns the number of free bytes available for new entries.
    pub fn free_space(&self) -> usize {
        let slot_end = LEAF_SLOTS_OFFSET + self.entries.len() * LEAF_SLOT_SIZE;
        let data_start = self.data_area_end();
        if data_start > slot_end {
            data_start - slot_end
        } else {
            0
        }
    }

    /// Space needed to insert a key-value pair.
    pub fn space_needed(key_len: usize, val_len: usize) -> usize {
        LEAF_SLOT_SIZE + key_len + val_len
    }

    /// Whether this node can fit another entry with the given key and value sizes.
    pub fn can_fit(&self, key_len: usize, val_len: usize) -> bool {
        self.free_space() >= Self::space_needed(key_len, val_len)
    }

    /// Binary search for a key. Returns Ok(index) if found, Err(index) for
    /// the insertion point.
    pub fn search(&self, key: &[u8]) -> std::result::Result<usize, usize> {
        self.entries
            .binary_search_by(|e| e.key.as_slice().cmp(key))
    }

    /// Insert or update a key-value pair. The entries vector is kept sorted.
    /// Does NOT check free_space — caller must verify before calling.
    pub fn upsert(&mut self, key: &[u8], value: &[u8]) {
        match self.search(key) {
            Ok(i) => {
                self.entries[i].value = value.to_vec();
            }
            Err(i) => {
                self.entries.insert(
                    i,
                    LeafEntry {
                        key: key.to_vec(),
                        value: value.to_vec(),
                    },
                );
            }
        }
    }

    /// Split this node roughly in half, returning the new (right) leaf node
    /// and the separator key (first key of the new node).
    pub fn split(&mut self, new_page_id: PageId) -> (LeafNode, Vec<u8>) {
        let mid = self.entries.len() / 2;
        let right_entries = self.entries.split_off(mid);
        let sep = right_entries[0].key.clone();
        let right = LeafNode {
            page_id: new_page_id,
            entries: right_entries,
        };
        (right, sep)
    }

    /// Delete an entry by key. Returns true if found and removed.
    pub fn delete(&mut self, key: &[u8]) -> bool {
        if let Ok(i) = self.search(key) {
            self.entries.remove(i);
            true
        } else {
            false
        }
    }

    // helpers ---------------------------------------------------------------

    fn data_area_end(&self) -> usize {
        if self.entries.is_empty() {
            return PAGE_SIZE;
        }
        let mut used: usize = 0;
        for e in &self.entries {
            used += e.key.len() + e.value.len();
        }
        PAGE_SIZE - used
    }
}

// ---------------------------------------------------------------------------
// InternalNode
// ---------------------------------------------------------------------------

/// A single key + left-child entry in an internal node.
#[derive(Debug, Clone)]
pub struct InternalEntry {
    pub key: Vec<u8>,
    pub child: PageId, // left child pointer for this key
}

/// In-memory representation of a B+tree internal node.
#[derive(Debug, Clone)]
pub struct InternalNode {
    pub page_id: PageId,
    pub entries: Vec<InternalEntry>,
    pub right_child: PageId,
}

impl InternalNode {
    /// Create an internal node with a single separator key and two children.
    pub fn new(page_id: PageId, left: PageId, key: Vec<u8>, right: PageId) -> Self {
        InternalNode {
            page_id,
            entries: vec![InternalEntry { key, child: left }],
            right_child: right,
        }
    }

    /// Deserialize an internal node from a page buffer.
    pub fn from_page(buf: &[u8; PAGE_SIZE]) -> Result<Self> {
        let (pt, page_id) = read_page_header(buf);
        if pt != PageType::BtreeInternal {
            return Err(BTreeError::CorruptNode);
        }
        let num = read_u16(buf, NUM_OFFSET) as usize;
        let _data_end = read_u16(buf, DATA_END_OFFSET);
        let right_child = PageId(read_u32(buf, INTERNAL_RIGHT_CHILD_OFFSET));

        let mut entries = Vec::with_capacity(num);
        for i in 0..num {
            let slot_off = INTERNAL_SLOTS_OFFSET + i * INTERNAL_SLOT_SIZE;
            let key_len = read_u16(buf, slot_off) as usize;
            let data_off = read_u16(buf, slot_off + 2) as usize;
            let child = PageId(read_u32(buf, slot_off + 4));

            if data_off + key_len > PAGE_SIZE {
                return Err(BTreeError::CorruptNode);
            }
            let key = buf[data_off..data_off + key_len].to_vec();
            entries.push(InternalEntry { key, child });
        }

        Ok(InternalNode {
            page_id,
            entries,
            right_child,
        })
    }

    /// Serialize this internal node into a page buffer.
    pub fn to_page(&self) -> [u8; PAGE_SIZE] {
        let mut buf = [0u8; PAGE_SIZE];
        write_page_header(&mut buf, PageType::BtreeInternal, self.page_id);

        let num = self.entries.len() as u16;
        write_u16(&mut buf, NUM_OFFSET, num);
        write_u32(&mut buf, INTERNAL_RIGHT_CHILD_OFFSET, self.right_child.0);

        let mut data_cursor = PAGE_SIZE;

        for (i, entry) in self.entries.iter().enumerate() {
            data_cursor -= entry.key.len();
            buf[data_cursor..data_cursor + entry.key.len()].copy_from_slice(&entry.key);

            let slot_off = INTERNAL_SLOTS_OFFSET + i * INTERNAL_SLOT_SIZE;
            write_u16(&mut buf, slot_off, entry.key.len() as u16);
            write_u16(&mut buf, slot_off + 2, data_cursor as u16);
            write_u32(&mut buf, slot_off + 4, entry.child.0);
        }

        write_u16(&mut buf, DATA_END_OFFSET, data_cursor as u16);
        buf
    }

    /// Free space available for new keys/slots.
    pub fn free_space(&self) -> usize {
        let slot_end = INTERNAL_SLOTS_OFFSET + self.entries.len() * INTERNAL_SLOT_SIZE;
        let data_start = self.data_area_end();
        if data_start > slot_end {
            data_start - slot_end
        } else {
            0
        }
    }

    /// Space needed to insert a key.
    pub fn space_needed(key_len: usize) -> usize {
        INTERNAL_SLOT_SIZE + key_len
    }

    /// Whether this node can fit another key.
    pub fn can_fit(&self, key_len: usize) -> bool {
        self.free_space() >= Self::space_needed(key_len)
    }

    /// Find the child page to follow for the given search key.
    /// Returns (child_page_id, index) where index is the entry index
    /// whose child was selected, or entries.len() for right_child.
    pub fn find_child(&self, key: &[u8]) -> (PageId, usize) {
        for (i, entry) in self.entries.iter().enumerate() {
            if key < entry.key.as_slice() {
                return (entry.child, i);
            }
        }
        // Key is >= all keys, follow right_child.
        (self.right_child, self.entries.len())
    }

    /// Insert a separator key with its left child at the correct position,
    /// and update the right child if necessary.
    /// `left_child` goes under the new key, and `right_child` was the old
    /// child that was split — we need to insert the separator between them.
    pub fn insert_key(&mut self, key: Vec<u8>, left_child: PageId, right_child: PageId) {
        // Find position to insert: the new key separates left_child and right_child.
        // We need to find where right_child currently lives and insert before it.
        let pos = self
            .entries
            .binary_search_by(|e| e.key.as_slice().cmp(key.as_slice()))
            .unwrap_or_else(|i| i);

        // Insert the new entry: the left child of this key is `left_child`.
        self.entries.insert(pos, InternalEntry { key, child: left_child });
        // The child at pos+1 (or right_child if pos == entries.len()-1) should be right_child.
        // Actually, the way splits work: we're replacing one child pointer with two
        // (left, separator, right). The old child pointer that was in position `pos`
        // before the insert is now at position `pos+1`, or it was `right_child` of the
        // internal node. We need to set the pointer that follows our new key to `right_child`.
        if pos + 1 < self.entries.len() {
            self.entries[pos + 1].child = right_child;
        } else {
            self.right_child = right_child;
        }
    }

    /// Split this internal node roughly in half. Returns (new_right_node, separator_key).
    /// The separator key is the middle key that gets pushed up to the parent.
    pub fn split(&mut self, new_page_id: PageId) -> (InternalNode, Vec<u8>) {
        let mid = self.entries.len() / 2;
        // The key at `mid` becomes the separator pushed to the parent.
        // entries[0..mid] stay in self, entries[mid+1..] go to right node.
        // The child of entries[mid] becomes the left-most child of the right node
        // ... actually entries[mid].child is the left child of the mid key.
        // In a B+tree internal split:
        //   Left node keeps entries[0..mid]
        //   Separator = entries[mid].key (pushed up)
        //   Right node gets entries[mid+1..], with right_child of left = entries[mid].child?
        //
        // Wait, let me think again. Internal node has:
        //   entries = [(key0, child0), (key1, child1), ..., (keyN, childN)]
        //   right_child
        // The children are: child0, key0, child1, key1, ..., childN, keyN, right_child
        // Wait no: child_i is the LEFT child of key_i. So the tree order is:
        //   child0 < key0 < child1 < key1 < ... < childN < keyN < right_child
        //
        // But that doesn't make sense for N+1 children with N keys. Let me re-read the spec:
        //   "child = left child pointer" — entries[i].child is the left child of entries[i].key
        //   right_child is the rightmost child
        //
        // So for entries [(k0,c0), (k1,c1), (k2,c2)] with right_child=rc:
        //   c0 < k0, then c1 < k1, then c2 < k2, then rc >= k2
        //   But c0 is the subtree for keys < k0
        //   c1 is the subtree for keys >= k0 and < k1
        //   etc.
        //
        // Wait, that's how find_child works:
        //   if key < entry[0].key => entry[0].child
        //   if key < entry[1].key => entry[1].child  ... but this is wrong!
        //   Because entry[1].child should be for keys between k0 and k1.
        //   And find_child returns entry[i].child for the first i where key < entry[i].key.
        //   So for key between k0 and k1, i=1, child=c1. That works if c1 is for [k0, k1).
        //
        // OK so: c0 is for (-inf, k0), c1 is for [k0, k1), c2 is for [k1, k2), rc is for [k2, +inf).
        //
        // Split at mid: push entries[mid].key up.
        // Left: entries[0..mid], right_child = entries[mid].child
        //   (because entries[mid].child covers [k_{mid-1}, k_mid), which is the rightmost subtree of the left half)
        //   Wait, no. Left node should have keys k0..k_{mid-1}. Its right_child covers [k_{mid-1}, k_mid).
        //   That right_child should be entries[mid].child? No, entries[mid].child covers [k_{mid-1}, k_mid).
        //   Actually the child at index i covers: [k_{i-1}, k_i) (where k_{-1} = -inf).
        //   So entries[mid].child covers [k_{mid-1}, k_mid).
        //   In the left node (keys 0..mid-1), the right_child should cover [k_{mid-1}, k_mid).
        //   But k_mid is being pushed up, so the left node's right_child covers [k_{mid-1}, separator).
        //   That is entries[mid].child. Correct.
        //
        // Right: entries[mid+1..], right_child = self.right_child
        //   entries[mid+1].child covers [k_mid, k_{mid+1}), which is correct for the right node.

        let sep_key = self.entries[mid].key.clone();
        let new_left_right_child = self.entries[mid].child;

        let right_entries: Vec<InternalEntry> = self.entries.split_off(mid + 1);
        // Remove the mid entry (it was pushed up as separator).
        self.entries.pop(); // removes the mid entry

        let old_right_child = self.right_child;
        self.right_child = new_left_right_child;

        let right = InternalNode {
            page_id: new_page_id,
            entries: right_entries,
            right_child: old_right_child,
        };

        (right, sep_key)
    }

    fn data_area_end(&self) -> usize {
        if self.entries.is_empty() {
            return PAGE_SIZE;
        }
        let used: usize = self.entries.iter().map(|e| e.key.len()).sum();
        PAGE_SIZE - used
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

fn write_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn write_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_empty_round_trip() {
        let node = LeafNode::new(PageId(5));
        let buf = node.to_page();
        let restored = LeafNode::from_page(&buf).unwrap();
        assert_eq!(restored.page_id, PageId(5));
        assert!(restored.entries.is_empty());
    }

    #[test]
    fn leaf_insert_and_round_trip() {
        let mut node = LeafNode::new(PageId(10));
        node.upsert(b"charlie", b"value3");
        node.upsert(b"alice", b"value1");
        node.upsert(b"bob", b"value2");

        // Entries should be sorted.
        assert_eq!(node.entries[0].key, b"alice");
        assert_eq!(node.entries[1].key, b"bob");
        assert_eq!(node.entries[2].key, b"charlie");

        let buf = node.to_page();
        let restored = LeafNode::from_page(&buf).unwrap();
        assert_eq!(restored.entries.len(), 3);
        assert_eq!(restored.entries[0].key, b"alice");
        assert_eq!(restored.entries[0].value, b"value1");
        assert_eq!(restored.entries[1].key, b"bob");
        assert_eq!(restored.entries[1].value, b"value2");
        assert_eq!(restored.entries[2].key, b"charlie");
        assert_eq!(restored.entries[2].value, b"value3");
    }

    #[test]
    fn leaf_upsert_updates_value() {
        let mut node = LeafNode::new(PageId(1));
        node.upsert(b"key", b"old");
        node.upsert(b"key", b"new");
        assert_eq!(node.entries.len(), 1);
        assert_eq!(node.entries[0].value, b"new");
    }

    #[test]
    fn leaf_delete() {
        let mut node = LeafNode::new(PageId(1));
        node.upsert(b"a", b"1");
        node.upsert(b"b", b"2");
        node.upsert(b"c", b"3");
        assert!(node.delete(b"b"));
        assert!(!node.delete(b"b"));
        assert_eq!(node.entries.len(), 2);
        assert_eq!(node.entries[0].key, b"a");
        assert_eq!(node.entries[1].key, b"c");
    }

    #[test]
    fn leaf_split() {
        let mut node = LeafNode::new(PageId(1));
        for i in 0u32..10 {
            let key = format!("key{:04}", i);
            let val = format!("val{:04}", i);
            node.upsert(key.as_bytes(), val.as_bytes());
        }
        assert_eq!(node.entries.len(), 10);

        let (right, sep) = node.split(PageId(2));
        assert_eq!(node.entries.len(), 5);
        assert_eq!(right.entries.len(), 5);
        assert_eq!(sep, right.entries[0].key);
        assert_eq!(right.page_id, PageId(2));

        // Verify ordering: all left keys < sep <= all right keys.
        for e in &node.entries {
            assert!(e.key.as_slice() < sep.as_slice());
        }
        for e in &right.entries {
            assert!(e.key.as_slice() >= sep.as_slice());
        }
    }

    #[test]
    fn leaf_free_space() {
        let node = LeafNode::new(PageId(1));
        // Empty leaf: all payload after header + num(2) + data_end(2) is free.
        // Free = PAGE_SIZE - LEAF_SLOTS_OFFSET = 4096 - 9 = 4087
        assert_eq!(node.free_space(), PAGE_SIZE - LEAF_SLOTS_OFFSET);

        let mut node2 = LeafNode::new(PageId(2));
        node2.upsert(b"hello", b"world");
        let expected = PAGE_SIZE - LEAF_SLOTS_OFFSET - LEAF_SLOT_SIZE - 10;
        assert_eq!(node2.free_space(), expected);
    }

    #[test]
    fn internal_round_trip() {
        let node = InternalNode::new(PageId(20), PageId(5), b"separator".to_vec(), PageId(6));
        let buf = node.to_page();
        let restored = InternalNode::from_page(&buf).unwrap();
        assert_eq!(restored.page_id, PageId(20));
        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].key, b"separator");
        assert_eq!(restored.entries[0].child, PageId(5));
        assert_eq!(restored.right_child, PageId(6));
    }

    #[test]
    fn internal_find_child() {
        let mut node = InternalNode::new(PageId(1), PageId(10), b"m".to_vec(), PageId(20));
        node.insert_key(b"t".to_vec(), PageId(20), PageId(30));
        // Children: 10 < "m" < 20 < "t" < 30
        assert_eq!(node.find_child(b"a").0, PageId(10));
        assert_eq!(node.find_child(b"m").0, PageId(20));
        assert_eq!(node.find_child(b"p").0, PageId(20));
        assert_eq!(node.find_child(b"t").0, PageId(30));
        assert_eq!(node.find_child(b"z").0, PageId(30));
    }

    #[test]
    fn internal_split() {
        // Create an internal node with several keys.
        let mut node = InternalNode::new(PageId(1), PageId(100), b"b".to_vec(), PageId(101));
        node.insert_key(b"d".to_vec(), PageId(101), PageId(102));
        node.insert_key(b"f".to_vec(), PageId(102), PageId(103));
        node.insert_key(b"h".to_vec(), PageId(103), PageId(104));
        node.insert_key(b"j".to_vec(), PageId(104), PageId(105));

        // entries: [(b,100), (d,101), (f,102), (h,103), (j,104)], right_child=105
        assert_eq!(node.entries.len(), 5);

        let (right, sep) = node.split(PageId(2));
        // mid = 5/2 = 2, so separator = "f"
        assert_eq!(sep, b"f");
        // Left: entries[0..2] = [(b,100), (d,101)], right_child = 102 (entries[2].child)
        assert_eq!(node.entries.len(), 2);
        assert_eq!(node.right_child, PageId(102));
        // Right: entries[3..5] = [(h,103), (j,104)], right_child = 105
        assert_eq!(right.entries.len(), 2);
        assert_eq!(right.entries[0].key, b"h");
        assert_eq!(right.right_child, PageId(105));

        // Verify round-trip of both halves.
        let buf_l = node.to_page();
        let buf_r = right.to_page();
        let _ = InternalNode::from_page(&buf_l).unwrap();
        let _ = InternalNode::from_page(&buf_r).unwrap();
    }

    #[test]
    fn internal_serialize_multiple_keys() {
        let mut node = InternalNode::new(PageId(50), PageId(1), b"key_a".to_vec(), PageId(2));
        node.insert_key(b"key_b".to_vec(), PageId(2), PageId(3));
        node.insert_key(b"key_c".to_vec(), PageId(3), PageId(4));

        let buf = node.to_page();
        let restored = InternalNode::from_page(&buf).unwrap();

        assert_eq!(restored.entries.len(), 3);
        assert_eq!(restored.entries[0].key, b"key_a");
        assert_eq!(restored.entries[0].child, PageId(1));
        assert_eq!(restored.entries[1].key, b"key_b");
        assert_eq!(restored.entries[1].child, PageId(2));
        assert_eq!(restored.entries[2].key, b"key_c");
        assert_eq!(restored.entries[2].child, PageId(3));
        assert_eq!(restored.right_child, PageId(4));
    }

    #[test]
    fn leaf_fill_and_split_round_trip() {
        // Fill a leaf until it cannot fit more, then split and verify both
        // halves serialize/deserialize correctly.
        let mut node = LeafNode::new(PageId(1));
        let mut i = 0u32;
        loop {
            let key = format!("k{:06}", i);
            let val = format!("v{:06}", i);
            if !node.can_fit(key.len(), val.len()) {
                break;
            }
            node.upsert(key.as_bytes(), val.as_bytes());
            i += 1;
        }
        assert!(node.entries.len() > 10); // sanity check that we inserted plenty

        let (right, _sep) = node.split(PageId(2));
        let buf_l = node.to_page();
        let buf_r = right.to_page();
        let left2 = LeafNode::from_page(&buf_l).unwrap();
        let right2 = LeafNode::from_page(&buf_r).unwrap();
        assert_eq!(left2.entries.len(), node.entries.len());
        assert_eq!(right2.entries.len(), right.entries.len());
    }
}
