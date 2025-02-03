use crate::page::{PageType, PAGE_SIZE, PAGE_HEADER_SIZE};

pub const META_MAGIC: [u8; 4] = *b"GMGR";

/// Size of the serialized meta fields before the checksum.
/// magic(4) + version(4) + root_page_id(4) + next_page_id(4) + txn_id(8) + wal_offset(8) = 32
const META_FIELDS_SIZE: usize = 32;
/// Total serialized size including checksum.
/// The meta page descriptor, stored in pages 0 and 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub magic: [u8; 4],
    pub version: u32,
    pub root_page_id: u32,
    pub next_page_id: u32,
    pub txn_id: u64,
    pub wal_offset: u64,
    pub checksum: u32,
}

impl Meta {
    /// Create a fresh meta with default initial values.
    pub fn new_initial() -> Self {
        let mut m = Meta {
            magic: META_MAGIC,
            version: 1,
            root_page_id: 0,
            next_page_id: 2, // pages 0 and 1 are reserved for meta
            txn_id: 0,
            wal_offset: 0,
            checksum: 0,
        };
        m.checksum = m.compute_checksum();
        m
    }

    /// Compute CRC32C over the serialized fields (excluding checksum itself).
    pub fn compute_checksum(&self) -> u32 {
        let mut buf = [0u8; META_FIELDS_SIZE];
        self.serialize_fields(&mut buf);
        crc32c::crc32c(&buf)
    }

    /// Update the checksum field to match current contents.
    pub fn update_checksum(&mut self) {
        self.checksum = self.compute_checksum();
    }

    /// Returns true if the stored checksum matches the computed one.
    pub fn validate_checksum(&self) -> bool {
        self.checksum == self.compute_checksum()
    }

    fn serialize_fields(&self, buf: &mut [u8; META_FIELDS_SIZE]) {
        buf[0..4].copy_from_slice(&self.magic);
        buf[4..8].copy_from_slice(&self.version.to_le_bytes());
        buf[8..12].copy_from_slice(&self.root_page_id.to_le_bytes());
        buf[12..16].copy_from_slice(&self.next_page_id.to_le_bytes());
        buf[16..24].copy_from_slice(&self.txn_id.to_le_bytes());
        buf[24..32].copy_from_slice(&self.wal_offset.to_le_bytes());
    }

    /// Serialize the full meta (including checksum) into a page-sized buffer.
    pub fn to_page(&self, slot: u8) -> [u8; PAGE_SIZE] {
        let mut buf = [0u8; PAGE_SIZE];
        // Write page header
        buf[0] = PageType::Meta as u8;
        buf[1..5].copy_from_slice(&(slot as u32).to_le_bytes());
        // Write meta fields after the page header
        let off = PAGE_HEADER_SIZE;
        self.serialize_fields(
            <&mut [u8; META_FIELDS_SIZE]>::try_from(&mut buf[off..off + META_FIELDS_SIZE]).unwrap(),
        );
        let coff = off + META_FIELDS_SIZE;
        buf[coff..coff + 4].copy_from_slice(&self.checksum.to_le_bytes());
        buf
    }

    /// Deserialize a Meta from a page buffer. Returns None if magic is wrong.
    pub fn from_page(buf: &[u8; PAGE_SIZE]) -> Option<Self> {
        let off = PAGE_HEADER_SIZE;
        let magic: [u8; 4] = buf[off..off + 4].try_into().unwrap();
        if magic != META_MAGIC {
            return None;
        }
        let version = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap());
        let root_page_id = u32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap());
        let next_page_id = u32::from_le_bytes(buf[off + 12..off + 16].try_into().unwrap());
        let txn_id = u64::from_le_bytes(buf[off + 16..off + 24].try_into().unwrap());
        let wal_offset = u64::from_le_bytes(buf[off + 24..off + 32].try_into().unwrap());
        let coff = off + META_FIELDS_SIZE;
        let checksum = u32::from_le_bytes(buf[coff..coff + 4].try_into().unwrap());
        Some(Meta {
            magic,
            version,
            root_page_id,
            next_page_id,
            txn_id,
            wal_offset,
            checksum,
        })
    }
}

/// Read the active meta from the two meta page slots.
/// Returns (meta, slot) where slot is 0 or 1.
pub fn pick_active_meta(
    page0: &[u8; PAGE_SIZE],
    page1: &[u8; PAGE_SIZE],
) -> Option<(Meta, u8)> {
    let m0 = Meta::from_page(page0).filter(|m| m.validate_checksum());
    let m1 = Meta::from_page(page1).filter(|m| m.validate_checksum());
    match (m0, m1) {
        (Some(a), Some(b)) => {
            if b.txn_id > a.txn_id {
                Some((b, 1))
            } else {
                Some((a, 0))
            }
        }
        (Some(a), None) => Some((a, 0)),
        (None, Some(b)) => Some((b, 1)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_serialize() {
        let meta = Meta::new_initial();
        let page = meta.to_page(0);
        let restored = Meta::from_page(&page).unwrap();
        assert_eq!(meta, restored);
        assert!(restored.validate_checksum());
    }

    #[test]
    fn checksum_detects_corruption() {
        let meta = Meta::new_initial();
        let mut page = meta.to_page(0);
        // Corrupt a byte in the meta fields area
        page[PAGE_HEADER_SIZE + 10] ^= 0xFF;
        let restored = Meta::from_page(&page).unwrap();
        assert!(!restored.validate_checksum());
    }

    #[test]
    fn pick_higher_txn_id() {
        let mut m0 = Meta::new_initial();
        m0.txn_id = 5;
        m0.update_checksum();

        let mut m1 = Meta::new_initial();
        m1.txn_id = 10;
        m1.update_checksum();

        let p0 = m0.to_page(0);
        let p1 = m1.to_page(1);

        let (active, slot) = pick_active_meta(&p0, &p1).unwrap();
        assert_eq!(slot, 1);
        assert_eq!(active.txn_id, 10);
    }

    #[test]
    fn pick_survives_one_corrupt() {
        let mut m0 = Meta::new_initial();
        m0.txn_id = 5;
        m0.update_checksum();
        let p0 = m0.to_page(0);

        // page 1 is all zeros — invalid magic
        let p1 = [0u8; PAGE_SIZE];

        let (active, slot) = pick_active_meta(&p0, &p1).unwrap();
        assert_eq!(slot, 0);
        assert_eq!(active.txn_id, 5);
    }
}
