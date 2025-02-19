//! Catalog tree: maps TreeId -> root PageId for each user-created B+tree.
//!
//! The catalog itself is a B+tree whose root is stored in `meta.root_page_id`.
//! Keys are `TreeId.0.to_le_bytes()` (4 bytes), values are `PageId.0.to_le_bytes()` (4 bytes).

use gemgraph_btree::BTree;
use gemgraph_pager::pager::Pager;
use gemgraph_pager::PageId;

use crate::txn::TreeId;
use crate::Result;

/// Look up the root PageId for a given TreeId in the catalog tree.
pub fn catalog_get(pager: &mut Pager, catalog_root: PageId, tree_id: TreeId) -> Result<Option<PageId>> {
    let mut tree = BTree::open(pager, catalog_root);
    let key = tree_id.0.to_le_bytes();
    match tree.get(&key)? {
        Some(val) => {
            let pid = u32::from_le_bytes(val[..4].try_into().unwrap());
            Ok(Some(PageId(pid)))
        }
        None => Ok(None),
    }
}

/// Set the root PageId for a given TreeId in the catalog tree.
/// Returns the (possibly new) catalog root.
pub fn catalog_set(
    pager: &mut Pager,
    catalog_root: PageId,
    tree_id: TreeId,
    root: PageId,
) -> Result<PageId> {
    let mut tree = BTree::open(pager, catalog_root);
    let key = tree_id.0.to_le_bytes();
    let val = root.0.to_le_bytes();
    tree.insert(&key, &val)?;
    Ok(tree.root())
}

/// List all tree entries in the catalog. Returns (TreeId, root PageId) pairs.
pub fn catalog_list(pager: &mut Pager, catalog_root: PageId) -> Result<Vec<(TreeId, PageId)>> {
    let mut tree = BTree::open(pager, catalog_root);
    let entries = tree.range(&0u32.to_le_bytes(), &u32::MAX.to_le_bytes())?;
    let mut result = Vec::with_capacity(entries.len());
    for (k, v) in entries {
        let tid = u32::from_le_bytes(k[..4].try_into().unwrap());
        let pid = u32::from_le_bytes(v[..4].try_into().unwrap());
        result.push((TreeId(tid), PageId(pid)));
    }
    Ok(result)
}

/// Find the next available TreeId by scanning the catalog for the max existing id.
pub fn catalog_next_tree_id(pager: &mut Pager, catalog_root: PageId) -> Result<TreeId> {
    let entries = catalog_list(pager, catalog_root)?;
    let max_id = entries.iter().map(|(tid, _)| tid.0).max().unwrap_or(0);
    if entries.is_empty() {
        Ok(TreeId(1))
    } else {
        Ok(TreeId(max_id + 1))
    }
}
