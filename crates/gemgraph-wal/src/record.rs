use crate::WalError;

pub const PAGE_WRITE: u8 = 1;
pub const COMMIT: u8 = 2;
pub const CHECKPOINT: u8 = 3;

pub const PAGE_SIZE: usize = 4096;

/// A single WAL record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalRecord {
    PageWrite { page_id: u32, data: Box<[u8; PAGE_SIZE]> },
    Commit { txn_id: u64 },
    Checkpoint { txn_id: u64 },
}

impl WalRecord {
    /// Return the record type byte.
    pub fn record_type(&self) -> u8 {
        match self {
            WalRecord::PageWrite { .. } => PAGE_WRITE,
            WalRecord::Commit { .. } => COMMIT,
            WalRecord::Checkpoint { .. } => CHECKPOINT,
        }
    }

    /// Serialize the payload (everything after the record_type byte).
    pub fn payload(&self) -> Vec<u8> {
        match self {
            WalRecord::PageWrite { page_id, data } => {
                let mut buf = Vec::with_capacity(4 + PAGE_SIZE);
                buf.extend_from_slice(&page_id.to_le_bytes());
                buf.extend_from_slice(data.as_ref());
                buf
            }
            WalRecord::Commit { txn_id } => txn_id.to_le_bytes().to_vec(),
            WalRecord::Checkpoint { txn_id } => txn_id.to_le_bytes().to_vec(),
        }
    }

    /// Deserialize from record_type + payload bytes.
    pub fn from_raw(record_type: u8, payload: &[u8]) -> Result<Self, WalError> {
        match record_type {
            PAGE_WRITE => {
                if payload.len() != 4 + PAGE_SIZE {
                    return Err(WalError::CorruptRecord);
                }
                let page_id = u32::from_le_bytes(payload[..4].try_into().unwrap());
                let mut data = Box::new([0u8; PAGE_SIZE]);
                data.copy_from_slice(&payload[4..]);
                Ok(WalRecord::PageWrite { page_id, data })
            }
            COMMIT => {
                if payload.len() != 8 {
                    return Err(WalError::CorruptRecord);
                }
                let txn_id = u64::from_le_bytes(payload[..8].try_into().unwrap());
                Ok(WalRecord::Commit { txn_id })
            }
            CHECKPOINT => {
                if payload.len() != 8 {
                    return Err(WalError::CorruptRecord);
                }
                let txn_id = u64::from_le_bytes(payload[..8].try_into().unwrap());
                Ok(WalRecord::Checkpoint { txn_id })
            }
            _ => Err(WalError::CorruptRecord),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_page_write() {
        let mut page = Box::new([0u8; PAGE_SIZE]);
        page[0] = 0xAB;
        page[4095] = 0xCD;
        let rec = WalRecord::PageWrite { page_id: 42, data: page };
        let payload = rec.payload();
        let decoded = WalRecord::from_raw(rec.record_type(), &payload).unwrap();
        assert_eq!(rec, decoded);
    }

    #[test]
    fn round_trip_commit() {
        let rec = WalRecord::Commit { txn_id: 1234567890 };
        let payload = rec.payload();
        let decoded = WalRecord::from_raw(rec.record_type(), &payload).unwrap();
        assert_eq!(rec, decoded);
    }

    #[test]
    fn round_trip_checkpoint() {
        let rec = WalRecord::Checkpoint { txn_id: 99 };
        let payload = rec.payload();
        let decoded = WalRecord::from_raw(rec.record_type(), &payload).unwrap();
        assert_eq!(rec, decoded);
    }

    #[test]
    fn bad_record_type() {
        assert!(WalRecord::from_raw(255, &[]).is_err());
    }

    #[test]
    fn wrong_payload_length() {
        assert!(WalRecord::from_raw(COMMIT, &[0; 4]).is_err());
    }
}
