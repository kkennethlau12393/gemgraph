use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::record::WalRecord;
use crate::WalError;

/// Reads WAL records from a file.
pub struct WalReader {
    file: File,
}

impl WalReader {
    /// Open an existing WAL file for reading.
    pub fn open(path: &Path) -> Result<Self, WalError> {
        let file = File::open(path)?;
        Ok(Self { file })
    }

    /// Return an iterator over records starting at the given byte offset.
    pub fn iter_from(&self, offset: u64) -> WalIter {
        let file = self.file.try_clone().expect("failed to clone WAL file handle");
        WalIter { file, offset }
    }
}

/// Iterator that yields `WalRecord`s from a WAL file.
///
/// On encountering a truncated or corrupt record the iterator stops
/// (returns `None`) rather than returning an error. This matches crash
/// recovery semantics — a torn tail record is expected.
pub struct WalIter {
    file: File,
    offset: u64,
}

impl Iterator for WalIter {
    type Item = Result<WalRecord, WalError>;

    fn next(&mut self) -> Option<Self::Item> {
        // Try to seek to current offset
        if self.file.seek(SeekFrom::Start(self.offset)).is_err() {
            return None;
        }

        // Read the length field (4 bytes)
        let mut len_buf = [0u8; 4];
        if read_exact_or_none(&mut self.file, &mut len_buf).is_none() {
            return None;
        }
        let length = u32::from_le_bytes(len_buf) as usize;

        if length == 0 {
            return None;
        }

        // Read record_type + payload
        let mut body = vec![0u8; length];
        if read_exact_or_none(&mut self.file, &mut body).is_none() {
            return None; // truncated record
        }

        // Read CRC (4 bytes)
        let mut crc_buf = [0u8; 4];
        if read_exact_or_none(&mut self.file, &mut crc_buf).is_none() {
            return None; // truncated CRC
        }
        let stored_crc = u32::from_le_bytes(crc_buf);

        // Verify CRC
        let computed_crc = crc32c::crc32c(&body);
        if stored_crc != computed_crc {
            return None; // corrupt record
        }

        // Parse the record
        let record_type = body[0];
        let payload = &body[1..];
        match WalRecord::from_raw(record_type, payload) {
            Ok(record) => {
                self.offset += 4 + length as u64 + 4;
                Some(Ok(record))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

/// Read exactly `buf.len()` bytes, returning `None` if EOF is hit.
fn read_exact_or_none(file: &mut File, buf: &mut [u8]) -> Option<()> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => return None, // EOF
            Ok(n) => filled += n,
            Err(_) => return None,
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::PAGE_SIZE;
    use crate::writer::WalWriter;
    use tempfile::tempdir;

    #[test]
    fn iter_from_middle() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");
        let page = [0u8; PAGE_SIZE];

        let second_offset;
        {
            let mut w = WalWriter::create(&path).unwrap();
            w.append_page_write(1, &page).unwrap();
            second_offset = w.append_commit(1).unwrap();
            w.append_page_write(2, &page).unwrap();
            w.append_commit(2).unwrap();
            w.sync().unwrap();
        }

        let reader = WalReader::open(&path).unwrap();
        let records: Vec<WalRecord> = reader.iter_from(second_offset).filter_map(|r| r.ok()).collect();
        assert_eq!(records.len(), 3); // Commit(1), PageWrite(2), Commit(2)
        match &records[0] {
            WalRecord::Commit { txn_id } => assert_eq!(*txn_id, 1),
            _ => panic!("expected Commit"),
        }
    }

    #[test]
    fn truncated_record_stops_iteration() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");
        let page = [0u8; PAGE_SIZE];

        {
            let mut w = WalWriter::create(&path).unwrap();
            w.append_page_write(1, &page).unwrap();
            w.append_commit(1).unwrap();
            w.sync().unwrap();
        }

        // Truncate the file mid-way through what would be another record
        {
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            let len = file.metadata().unwrap().len();
            // Append a partial length field
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(&[0x10, 0x00]).unwrap(); // partial 2 of 4 bytes for length
            drop(file);
            // Verify file got longer
            let new_len = std::fs::metadata(&path).unwrap().len();
            assert_eq!(new_len, len + 2);
        }

        let reader = WalReader::open(&path).unwrap();
        let records: Vec<WalRecord> = reader.iter_from(0).filter_map(|r| r.ok()).collect();
        // Should still read the 2 good records; the partial one is silently skipped
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");
        WalWriter::create(&path).unwrap();

        let reader = WalReader::open(&path).unwrap();
        let records: Vec<WalRecord> = reader.iter_from(0).filter_map(|r| r.ok()).collect();
        assert!(records.is_empty());
    }
}
