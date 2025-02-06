use std::path::Path;

use crate::reader::WalReader;
use crate::record::{WalRecord, PAGE_SIZE};
use crate::WalError;

/// An action to replay during recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    ReplayPage { page_id: u32, data: Box<[u8; PAGE_SIZE]> },
}

/// Recover committed transactions from the WAL starting at `from_offset`.
///
/// Only page writes from committed transactions (those followed by a `Commit`
/// record) are returned. Uncommitted tail writes are discarded — this is the
/// normal crash-recovery semantic.
pub fn recover(wal_path: &Path, from_offset: u64) -> Result<Vec<RecoveryAction>, WalError> {
    let reader = WalReader::open(wal_path)?;

    let mut pending_pages: Vec<(u32, Box<[u8; PAGE_SIZE]>)> = Vec::new();
    let mut committed_actions: Vec<RecoveryAction> = Vec::new();

    for result in reader.iter_from(from_offset) {
        let record = result?;
        match record {
            WalRecord::PageWrite { page_id, data } => {
                pending_pages.push((page_id, data));
            }
            WalRecord::Commit { .. } => {
                // Flush pending pages as committed
                for (page_id, data) in pending_pages.drain(..) {
                    committed_actions.push(RecoveryAction::ReplayPage { page_id, data });
                }
            }
            WalRecord::Checkpoint { .. } => {
                // Checkpoint doesn't affect recovery logic here —
                // the caller already chose from_offset accordingly.
            }
        }
    }

    // Any remaining pending_pages are uncommitted — discard them.
    Ok(committed_actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::WalWriter;
    use tempfile::tempdir;

    fn make_page(fill: u8) -> [u8; PAGE_SIZE] {
        [fill; PAGE_SIZE]
    }

    #[test]
    fn recover_committed_only() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        {
            let mut w = WalWriter::create(&path).unwrap();
            // Txn 1: committed
            w.append_page_write(10, &make_page(0xAA)).unwrap();
            w.append_commit(1).unwrap();
            // Txn 2: committed
            w.append_page_write(20, &make_page(0xBB)).unwrap();
            w.append_page_write(21, &make_page(0xCC)).unwrap();
            w.append_commit(2).unwrap();
            // Txn 3: uncommitted (no commit record)
            w.append_page_write(30, &make_page(0xDD)).unwrap();
            w.sync().unwrap();
        }

        let actions = recover(&path, 0).unwrap();
        assert_eq!(actions.len(), 3);

        match &actions[0] {
            RecoveryAction::ReplayPage { page_id, data } => {
                assert_eq!(*page_id, 10);
                assert_eq!(data[0], 0xAA);
            }
        }
        match &actions[1] {
            RecoveryAction::ReplayPage { page_id, data } => {
                assert_eq!(*page_id, 20);
                assert_eq!(data[0], 0xBB);
            }
        }
        match &actions[2] {
            RecoveryAction::ReplayPage { page_id, data } => {
                assert_eq!(*page_id, 21);
                assert_eq!(data[0], 0xCC);
            }
        }
    }

    #[test]
    fn recover_with_truncation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        let truncate_after;
        {
            let mut w = WalWriter::create(&path).unwrap();
            w.append_page_write(1, &make_page(0x11)).unwrap();
            w.append_commit(1).unwrap();
            truncate_after = w.offset();
            // Write a second txn that we'll truncate mid-record
            w.append_page_write(2, &make_page(0x22)).unwrap();
            w.sync().unwrap();
        }

        // Truncate the file just after the first commit
        {
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_len(truncate_after + 10).unwrap(); // keep 10 bytes of torn record
        }

        let actions = recover(&path, 0).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            RecoveryAction::ReplayPage { page_id, data } => {
                assert_eq!(*page_id, 1);
                assert_eq!(data[0], 0x11);
            }
        }
    }

    #[test]
    fn recover_empty_wal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");
        WalWriter::create(&path).unwrap();

        let actions = recover(&path, 0).unwrap();
        assert!(actions.is_empty());
    }

    #[test]
    fn recover_from_offset() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        let second_txn_start;
        {
            let mut w = WalWriter::create(&path).unwrap();
            w.append_page_write(1, &make_page(0x11)).unwrap();
            w.append_commit(1).unwrap();
            second_txn_start = w.offset();
            w.append_page_write(2, &make_page(0x22)).unwrap();
            w.append_commit(2).unwrap();
            w.sync().unwrap();
        }

        let actions = recover(&path, second_txn_start).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            RecoveryAction::ReplayPage { page_id, .. } => assert_eq!(*page_id, 2),
        }
    }
}
