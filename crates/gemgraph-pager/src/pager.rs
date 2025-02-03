use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::freelist::FreeList;
use crate::meta::{self, Meta};
use crate::page::{PageId, PAGE_SIZE};

#[derive(Debug, thiserror::Error)]
pub enum PagerError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("corrupt meta pages: neither slot has a valid checksum")]
    CorruptMeta,

    #[error("invalid page id: {0}")]
    InvalidPageId(u32),

    #[error("checksum mismatch")]
    ChecksumMismatch,
}

pub type Result<T> = std::result::Result<T, PagerError>;

/// Default cache capacity: 4096 pages = 16 MB of cached data.
const DEFAULT_CACHE_CAPACITY: usize = 4096;

pub struct Pager {
    file: File,
    meta: Meta,
    meta_slot: u8,
    freelist: Vec<PageId>,
    /// Page id of the first freelist chain page (0 means no persisted freelist).
    freelist_head: u32,
    /// LRU page cache: maps PageId -> cached page data.
    cache: HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    /// LRU order: most recently used at the back, evict from front.
    lru_order: Vec<u32>,
    cache_capacity: usize,
}

impl Pager {
    /// Create a brand-new database file at `path`. Overwrites if it exists.
    pub fn create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;

        let meta = Meta::new_initial();

        // Write initial meta to both slots
        let p0 = meta.to_page(0);
        let p1 = meta.to_page(1);
        file.write_all_at(&p0, PageId(0).offset())?;
        file.write_all_at(&p1, PageId(1).offset())?;
        file.sync_data()?;

        Ok(Pager {
            file,
            meta,
            meta_slot: 0,
            freelist: Vec::new(),
            freelist_head: 0,
            cache: HashMap::with_capacity(DEFAULT_CACHE_CAPACITY),
            lru_order: Vec::with_capacity(DEFAULT_CACHE_CAPACITY),
            cache_capacity: DEFAULT_CACHE_CAPACITY,
        })
    }

    /// Open an existing database file, validate meta pages, rebuild freelist.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;

        let p0 = read_page_raw(&file, PageId(0))?;
        let p1 = read_page_raw(&file, PageId(1))?;

        let (meta, slot) = meta::pick_active_meta(&p0, &p1).ok_or(PagerError::CorruptMeta)?;

        let active_page = if slot == 0 { &p0 } else { &p1 };
        let fl_head = read_freelist_head_from_meta_page(active_page);

        let freelist = if fl_head != 0 {
            FreeList::restore(PageId(fl_head), |pid| {
                read_page_raw(&file, pid).unwrap_or([0u8; PAGE_SIZE])
            })
        } else {
            FreeList::new()
        };

        Ok(Pager {
            file,
            meta,
            meta_slot: slot,
            freelist: freelist.pages,
            freelist_head: fl_head,
            cache: HashMap::with_capacity(DEFAULT_CACHE_CAPACITY),
            lru_order: Vec::with_capacity(DEFAULT_CACHE_CAPACITY),
            cache_capacity: DEFAULT_CACHE_CAPACITY,
        })
    }

    /// Read a page — serves from cache if available, otherwise reads from disk
    /// and caches the result.
    pub fn read_page(&mut self, id: PageId) -> Result<[u8; PAGE_SIZE]> {
        let key = id.0;

        // Cache hit: return cached data and promote in LRU
        if let Some(data) = self.cache.get(&key) {
            let page = **data;
            self.touch_lru(key);
            return Ok(page);
        }

        // Cache miss: read from disk
        let page = read_page_raw(&self.file, id)?;

        // Insert into cache, evicting if needed
        self.cache_insert(key, page);

        Ok(page)
    }

    /// Write a page — writes through to disk AND updates the cache.
    pub fn write_page(&mut self, id: PageId, data: &[u8; PAGE_SIZE]) -> Result<()> {
        self.file.write_all_at(data, id.offset())?;

        // Update cache with the written data
        let key = id.0;
        if self.cache.contains_key(&key) {
            self.cache.insert(key, Box::new(*data));
            self.touch_lru(key);
        } else {
            self.cache_insert(key, *data);
        }

        Ok(())
    }

    /// Allocate a new page. Pops from the freelist or bumps next_page_id.
    pub fn alloc_page(&mut self) -> PageId {
        if let Some(id) = self.freelist.pop() {
            id
        } else {
            let id = PageId(self.meta.next_page_id);
            self.meta.next_page_id += 1;
            id
        }
    }

    /// Return a page to the freelist and evict from cache.
    pub fn free_page(&mut self, id: PageId) {
        self.freelist.push(id);
        self.cache.remove(&id.0);
        self.lru_order.retain(|&k| k != id.0);
    }

    // ------------------------------------------------------------------
    // LRU cache internals
    // ------------------------------------------------------------------

    /// Move `key` to the back of the LRU order (most recently used).
    fn touch_lru(&mut self, key: u32) {
        // For small-to-medium caches this linear scan is fine.
        // For very large caches we'd use a doubly-linked list.
        if let Some(pos) = self.lru_order.iter().position(|&k| k == key) {
            self.lru_order.remove(pos);
        }
        self.lru_order.push(key);
    }

    /// Insert a page into the cache, evicting the LRU entry if at capacity.
    fn cache_insert(&mut self, key: u32, data: [u8; PAGE_SIZE]) {
        if self.cache.len() >= self.cache_capacity {
            // Evict the least recently used entry (front of lru_order)
            if let Some(evict_key) = self.lru_order.first().copied() {
                self.cache.remove(&evict_key);
                self.lru_order.remove(0);
            }
        }
        self.cache.insert(key, Box::new(data));
        self.lru_order.push(key);
    }

    /// Get a reference to the current active meta.
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// Commit a new meta: persist the freelist, then write meta to the NEXT
    /// slot, sync, and update internal state.
    pub fn commit_meta(&mut self, mut new_meta: Meta) -> Result<()> {
        // First, persist freelist if non-empty.
        // Free old freelist chain pages (they can be reused).
        // For simplicity, allocate new chain pages from next_page_id.
        let fl = FreeList { pages: self.freelist.clone() };
        let needed = fl.chain_pages_needed();

        let chain_ids: Vec<PageId> = (0..needed)
            .map(|_| {
                let id = PageId(new_meta.next_page_id);
                new_meta.next_page_id += 1;
                id
            })
            .collect();

        let chain_pages = fl.persist(&chain_ids);
        for (pid, data) in &chain_pages {
            self.write_page(*pid, data)?;
        }

        let fl_head = if chain_ids.is_empty() {
            0u32
        } else {
            chain_ids[0].0
        };

        // Write meta to the OTHER slot
        let next_slot = 1 - self.meta_slot;
        new_meta.update_checksum();

        let mut page = new_meta.to_page(next_slot);
        // Store freelist head after the meta serialized data
        write_freelist_head_to_meta_page(&mut page, fl_head);

        self.file.write_all_at(&page, PageId(next_slot as u32).offset())?;
        self.file.sync_data()?;

        self.meta = new_meta;
        self.meta_slot = next_slot;
        self.freelist_head = fl_head;

        Ok(())
    }

    /// Flush file data to disk.
    pub fn sync(&self) -> Result<()> {
        self.file.sync_data()?;
        Ok(())
    }
}

fn read_page_raw(file: &File, id: PageId) -> Result<[u8; PAGE_SIZE]> {
    let mut buf = [0u8; PAGE_SIZE];
    file.read_exact_at(&mut buf, id.offset())?;
    Ok(buf)
}

/// Offset within the meta page where we store the freelist head pointer.
/// This comes right after the meta serialized data:
///   PAGE_HEADER(5) + META_FIELDS(32) + CHECKSUM(4) = 41
const FREELIST_HEAD_OFFSET: usize = 5 + 32 + 4;

fn read_freelist_head_from_meta_page(page: &[u8; PAGE_SIZE]) -> u32 {
    let off = FREELIST_HEAD_OFFSET;
    u32::from_le_bytes(page[off..off + 4].try_into().unwrap())
}

fn write_freelist_head_to_meta_page(page: &mut [u8; PAGE_SIZE], head: u32) {
    let off = FREELIST_HEAD_OFFSET;
    page[off..off + 4].copy_from_slice(&head.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_and_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");

        // Create
        {
            let pager = Pager::create(&path).unwrap();
            assert_eq!(pager.meta().txn_id, 0);
            assert_eq!(pager.meta().next_page_id, 2);
        }

        // Reopen
        {
            let pager = Pager::open(&path).unwrap();
            assert_eq!(pager.meta().txn_id, 0);
            assert_eq!(pager.meta().next_page_id, 2);
        }
    }

    #[test]
    fn read_write_page() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");
        let mut pager = Pager::create(&path).unwrap();

        let pid = pager.alloc_page();
        assert_eq!(pid, PageId(2));

        let mut data = [0u8; PAGE_SIZE];
        data[0] = 0xAB;
        data[4095] = 0xCD;
        pager.write_page(pid, &data).unwrap();

        let read_back = pager.read_page(pid).unwrap();
        assert_eq!(read_back[0], 0xAB);
        assert_eq!(read_back[4095], 0xCD);
    }

    #[test]
    fn meta_commit_flips_slot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");
        let mut pager = Pager::create(&path).unwrap();
        assert_eq!(pager.meta_slot, 0);

        let mut m = pager.meta().clone();
        m.txn_id = 1;
        pager.commit_meta(m).unwrap();
        assert_eq!(pager.meta_slot, 1);
        assert_eq!(pager.meta().txn_id, 1);

        let mut m = pager.meta().clone();
        m.txn_id = 2;
        pager.commit_meta(m).unwrap();
        assert_eq!(pager.meta_slot, 0);
        assert_eq!(pager.meta().txn_id, 2);
    }

    #[test]
    fn meta_survives_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");

        {
            let mut pager = Pager::create(&path).unwrap();
            let mut m = pager.meta().clone();
            m.txn_id = 42;
            m.root_page_id = 7;
            pager.commit_meta(m).unwrap();
        }

        {
            let pager = Pager::open(&path).unwrap();
            assert_eq!(pager.meta().txn_id, 42);
            assert_eq!(pager.meta().root_page_id, 7);
        }
    }

    #[test]
    fn freelist_alloc_reuses_freed_pages() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");
        let mut pager = Pager::create(&path).unwrap();

        let p1 = pager.alloc_page();
        let p2 = pager.alloc_page();
        assert_eq!(p1, PageId(2));
        assert_eq!(p2, PageId(3));

        pager.free_page(p1);
        let p3 = pager.alloc_page();
        assert_eq!(p3, PageId(2)); // reused
    }

    #[test]
    fn freelist_survives_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");

        {
            let mut pager = Pager::create(&path).unwrap();
            let p1 = pager.alloc_page(); // PageId(2)
            let _p2 = pager.alloc_page(); // PageId(3)
            // Write some data so the file is large enough
            pager.write_page(p1, &[0u8; PAGE_SIZE]).unwrap();
            pager.write_page(_p2, &[0u8; PAGE_SIZE]).unwrap();
            pager.free_page(p1);

            let mut m = pager.meta().clone();
            m.txn_id = 1;
            pager.commit_meta(m).unwrap();
        }

        {
            let mut pager = Pager::open(&path).unwrap();
            // The freelist should contain PageId(2)
            let reused = pager.alloc_page();
            assert_eq!(reused, PageId(2));
        }
    }
}
