use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::record::{CHECKPOINT, COMMIT, PAGE_WRITE, PAGE_SIZE};
use crate::WalError;

/// Appends WAL records to a file.
pub struct WalWriter {
    file: File,
    offset: u64,
}

impl WalWriter {
    /// Create (or truncate) a new WAL file.
    pub fn create(path: &Path) -> Result<Self, WalError> {
        let file = File::create(path)?;
        Ok(Self { file, offset: 0 })
    }

    /// Open an existing WAL file, seeking to the end.
    pub fn open(path: &Path) -> Result<Self, WalError> {
        let mut file = OpenOptions::new().write(true).read(true).open(path)?;
        let offset = file.seek(SeekFrom::End(0))?;
        Ok(Self { file, offset })
    }

    /// Current write offset in the file.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Append a PageWrite record. Returns the WAL offset where the record starts.
    pub fn append_page_write(&mut self, page_id: u32, data: &[u8; PAGE_SIZE]) -> Result<u64, WalError> {
        let mut payload = Vec::with_capacity(4 + PAGE_SIZE);
        payload.extend_from_slice(&page_id.to_le_bytes());
        payload.extend_from_slice(data);
        self.append_record(PAGE_WRITE, &payload)
    }

    /// Append a Commit record. Returns the WAL offset where the record starts.
    pub fn append_commit(&mut self, txn_id: u64) -> Result<u64, WalError> {
        self.append_record(COMMIT, &txn_id.to_le_bytes())
    }

    /// Append a Checkpoint record. Returns the WAL offset where the record starts.
    pub fn append_checkpoint(&mut self, txn_id: u64) -> Result<u64, WalError> {
        self.append_record(CHECKPOINT, &txn_id.to_le_bytes())
    }

    /// Flush to disk via fsync.
    pub fn sync(&mut self) -> Result<(), WalError> {
        self.file.sync_all()?;
        Ok(())
    }

    /// Write a framed record: [length: u32][record_type: u8][payload][crc: u32]
    fn append_record(&mut self, record_type: u8, payload: &[u8]) -> Result<u64, WalError> {
        let start = self.offset;
        // length = 1 (record_type) + payload.len()
        let length = 1u32 + payload.len() as u32;

        // Build the checksummed region: record_type + payload
        let mut body = Vec::with_capacity(1 + payload.len());
        body.push(record_type);
        body.extend_from_slice(payload);
        let crc = crc32c::crc32c(&body);

        // Write: length, body, crc
        self.file.write_all(&length.to_le_bytes())?;
        self.file.write_all(&body)?;
        self.file.write_all(&crc.to_le_bytes())?;

        self.offset += 4 + body.len() as u64 + 4;
        Ok(start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::WalReader;
    use crate::record::WalRecord;
    use tempfile::tempdir;

    #[test]
    fn write_and_read_back() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        let mut page = [0u8; PAGE_SIZE];
        page[0] = 0xFF;

        {
            let mut w = WalWriter::create(&path).unwrap();
            w.append_page_write(1, &page).unwrap();
            w.append_page_write(2, &page).unwrap();
            w.append_commit(100).unwrap();
            w.sync().unwrap();
            assert!(w.offset() > 0);
        }

        let reader = WalReader::open(&path).unwrap();
        let records: Vec<WalRecord> = reader.iter_from(0).filter_map(|r| r.ok()).collect();
        assert_eq!(records.len(), 3);
        match &records[0] {
            WalRecord::PageWrite { page_id, data } => {
                assert_eq!(*page_id, 1);
                assert_eq!(data[0], 0xFF);
            }
            _ => panic!("expected PageWrite"),
        }
        match &records[2] {
            WalRecord::Commit { txn_id } => assert_eq!(*txn_id, 100),
            _ => panic!("expected Commit"),
        }
    }

    #[test]
    fn open_appends_to_end() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");
        let page = [0u8; PAGE_SIZE];

        {
            let mut w = WalWriter::create(&path).unwrap();
            w.append_page_write(1, &page).unwrap();
            w.append_commit(1).unwrap();
            w.sync().unwrap();
        }
        {
            let mut w = WalWriter::open(&path).unwrap();
            w.append_page_write(2, &page).unwrap();
            w.append_commit(2).unwrap();
            w.sync().unwrap();
        }

        let reader = WalReader::open(&path).unwrap();
        let records: Vec<WalRecord> = reader.iter_from(0).filter_map(|r| r.ok()).collect();
        assert_eq!(records.len(), 4);
    }
}
