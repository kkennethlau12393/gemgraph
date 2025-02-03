use crate::page::{PageId, PageType, PAGE_SIZE, PAGE_HEADER_SIZE, PAGE_PAYLOAD_SIZE};

/// Number of PageId entries that fit in a single freelist page.
/// Each entry is 4 bytes. The first 4 bytes of payload store the next-page pointer.
const ENTRIES_PER_PAGE: usize = (PAGE_PAYLOAD_SIZE - 4) / 4;

/// An in-memory freelist of available page ids.
#[derive(Debug)]
pub struct FreeList {
    pub pages: Vec<PageId>,
}

impl FreeList {
    pub fn new() -> Self {
        FreeList { pages: Vec::new() }
    }

    /// Allocate a page: pop from the freelist, or return None to signal the
    /// caller should bump next_page_id.
    pub fn alloc(&mut self) -> Option<PageId> {
        self.pages.pop()
    }

    /// Return a page to the freelist.
    pub fn free(&mut self, id: PageId) {
        self.pages.push(id);
    }

    /// Serialize the freelist into a chain of pages.
    /// Returns a vec of (PageId, page_data) pairs. The caller must supply page ids
    /// for the chain pages themselves (allocated from the pager).
    /// `chain_page_ids` must have enough entries to hold the freelist.
    pub fn persist(&self, chain_page_ids: &[PageId]) -> Vec<(PageId, [u8; PAGE_SIZE])> {
        let mut result = Vec::new();
        let chunks: Vec<&[PageId]> = if self.pages.is_empty() {
            vec![]
        } else {
            self.pages.chunks(ENTRIES_PER_PAGE).collect()
        };

        for (i, chunk) in chunks.iter().enumerate() {
            let pid = chain_page_ids[i];
            let mut buf = [0u8; PAGE_SIZE];
            // Page header
            buf[0] = PageType::FreeList as u8;
            buf[1..5].copy_from_slice(&pid.0.to_le_bytes());

            let payload = &mut buf[PAGE_HEADER_SIZE..];
            // Next pointer: 0 means end of chain
            let next = if i + 1 < chunks.len() {
                chain_page_ids[i + 1].0
            } else {
                0
            };
            payload[0..4].copy_from_slice(&next.to_le_bytes());

            // Entries
            for (j, entry) in chunk.iter().enumerate() {
                let off = 4 + j * 4;
                payload[off..off + 4].copy_from_slice(&entry.0.to_le_bytes());
            }
            // Store entry count in the last 4 bytes of payload for reconstruction
            let count_off = PAGE_PAYLOAD_SIZE - 4;
            payload[count_off..count_off + 4].copy_from_slice(&(chunk.len() as u32).to_le_bytes());

            result.push((pid, buf));
        }
        result
    }

    /// Restore the freelist by reading a chain of pages starting at `head`.
    /// `read_fn` reads a page by its PageId.
    pub fn restore<F>(head: PageId, mut read_fn: F) -> Self
    where
        F: FnMut(PageId) -> [u8; PAGE_SIZE],
    {
        let mut pages = Vec::new();
        let mut current = head;

        loop {
            if current.0 == 0 {
                break;
            }
            let buf = read_fn(current);
            let payload = &buf[PAGE_HEADER_SIZE..];

            let next = u32::from_le_bytes(payload[0..4].try_into().unwrap());

            // Read entry count from end of payload
            let count_off = PAGE_PAYLOAD_SIZE - 4;
            let count =
                u32::from_le_bytes(payload[count_off..count_off + 4].try_into().unwrap()) as usize;

            for j in 0..count {
                let off = 4 + j * 4;
                let id = u32::from_le_bytes(payload[off..off + 4].try_into().unwrap());
                pages.push(PageId(id));
            }

            current = PageId(next);
        }

        FreeList { pages }
    }

    /// Number of chain pages needed to persist this freelist.
    pub fn chain_pages_needed(&self) -> usize {
        if self.pages.is_empty() {
            0
        } else {
            (self.pages.len() + ENTRIES_PER_PAGE - 1) / ENTRIES_PER_PAGE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_returns_lifo() {
        let mut fl = FreeList::new();
        fl.free(PageId(10));
        fl.free(PageId(20));
        fl.free(PageId(30));
        assert_eq!(fl.alloc(), Some(PageId(30)));
        assert_eq!(fl.alloc(), Some(PageId(20)));
        assert_eq!(fl.alloc(), Some(PageId(10)));
        assert_eq!(fl.alloc(), None);
    }

    #[test]
    fn persist_and_restore_empty() {
        let fl = FreeList::new();
        assert_eq!(fl.chain_pages_needed(), 0);
        let persisted = fl.persist(&[]);
        assert!(persisted.is_empty());
    }

    #[test]
    fn persist_and_restore_round_trip() {
        let mut fl = FreeList::new();
        for i in 10..25 {
            fl.free(PageId(i));
        }

        let needed = fl.chain_pages_needed();
        let chain_ids: Vec<PageId> = (100..100 + needed as u32).map(PageId).collect();
        let persisted = fl.persist(&chain_ids);

        // Build a simple lookup from the persisted pages
        let page_map: std::collections::HashMap<u32, [u8; PAGE_SIZE]> =
            persisted.into_iter().map(|(pid, buf)| (pid.0, buf)).collect();

        let restored = FreeList::restore(chain_ids[0], |pid| {
            page_map.get(&pid.0).copied().unwrap_or([0u8; PAGE_SIZE])
        });

        assert_eq!(restored.pages.len(), fl.pages.len());
        assert_eq!(restored.pages, fl.pages);
    }
}
