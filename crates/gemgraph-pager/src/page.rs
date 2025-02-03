/// Fixed page size in bytes.
pub const PAGE_SIZE: usize = 4096;

/// Size of the per-page header (page_type: u8 + page_id: u32).
pub const PAGE_HEADER_SIZE: usize = 5;

/// Usable payload bytes per page after the header.
pub const PAGE_PAYLOAD_SIZE: usize = PAGE_SIZE - PAGE_HEADER_SIZE;

/// Newtype wrapper around a page number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageId(pub u32);

impl PageId {
    /// Byte offset of this page within the data file.
    pub fn offset(self) -> u64 {
        self.0 as u64 * PAGE_SIZE as u64
    }
}

/// Discriminant stored in the first byte of every page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageType {
    Meta = 0,
    FreeList = 1,
    BtreeInternal = 2,
    BtreeLeaf = 3,
    Overflow = 4,
}

impl PageType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Meta),
            1 => Some(Self::FreeList),
            2 => Some(Self::BtreeInternal),
            3 => Some(Self::BtreeLeaf),
            4 => Some(Self::Overflow),
            _ => None,
        }
    }
}

/// Write the standard 5-byte header into a page buffer.
pub fn write_page_header(buf: &mut [u8; PAGE_SIZE], page_type: PageType, page_id: PageId) {
    buf[0] = page_type as u8;
    buf[1..5].copy_from_slice(&page_id.0.to_le_bytes());
}

/// Read the standard 5-byte header from a page buffer.
pub fn read_page_header(buf: &[u8; PAGE_SIZE]) -> (PageType, PageId) {
    let pt = PageType::from_u8(buf[0]).unwrap_or(PageType::Meta);
    let id = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
    (pt, PageId(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let mut buf = [0u8; PAGE_SIZE];
        write_page_header(&mut buf, PageType::BtreeLeaf, PageId(42));
        let (pt, id) = read_page_header(&buf);
        assert_eq!(pt, PageType::BtreeLeaf);
        assert_eq!(id, PageId(42));
    }

    #[test]
    fn page_id_offset() {
        assert_eq!(PageId(0).offset(), 0);
        assert_eq!(PageId(1).offset(), 4096);
        assert_eq!(PageId(10).offset(), 40960);
    }
}
