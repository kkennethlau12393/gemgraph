#![cfg(test)]

use std::collections::BTreeMap;

use proptest::prelude::*;
use tempfile::tempdir;

use crate::tree::BTree;
use gemgraph_pager::pager::Pager;

// ---------------------------------------------------------------------------
// Strategy helpers
// ---------------------------------------------------------------------------

fn arb_key() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 1..=100)
}

fn arb_value() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 1..=200)
}

fn arb_kv_pairs(max: usize) -> impl Strategy<Value = Vec<(Vec<u8>, Vec<u8>)>> {
    prop::collection::vec((arb_key(), arb_value()), 1..=max)
}

// ---------------------------------------------------------------------------
// Helper to create a fresh pager + btree
// ---------------------------------------------------------------------------

fn make_pager() -> (tempfile::TempDir, Pager) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.db");
    let pager = Pager::create(&path).unwrap();
    (dir, pager)
}

// ---------------------------------------------------------------------------
// 1. Insert-get round-trip oracle
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 50, .. ProptestConfig::default() })]

    #[test]
    fn insert_get_roundtrip_oracle(pairs in arb_kv_pairs(500), missing_keys in prop::collection::vec(arb_key(), 1..=20)) {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        let mut oracle = BTreeMap::<Vec<u8>, Vec<u8>>::new();

        for (k, v) in &pairs {
            tree.insert(k, v).unwrap();
            oracle.insert(k.clone(), v.clone());
        }

        // Every key in oracle must match.
        for (k, v) in &oracle {
            let got = tree.get(k).unwrap();
            prop_assert_eq!(got.as_deref(), Some(v.as_slice()));
        }

        // Missing keys should return None.
        for k in &missing_keys {
            if !oracle.contains_key(k) {
                let got = tree.get(k).unwrap();
                prop_assert!(got.is_none(), "expected None for missing key, got {:?}", got);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Insert-delete oracle
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Op {
    Insert(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (arb_key(), arb_value()).prop_map(|(k, v)| Op::Insert(k, v)),
        arb_key().prop_map(Op::Delete),
    ]
}

fn arb_ops(max: usize) -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(arb_op(), 1..=max)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 50, .. ProptestConfig::default() })]

    #[test]
    fn insert_delete_oracle(ops in arb_ops(500)) {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        let mut oracle = BTreeMap::<Vec<u8>, Vec<u8>>::new();

        for op in &ops {
            match op {
                Op::Insert(k, v) => {
                    tree.insert(k, v).unwrap();
                    oracle.insert(k.clone(), v.clone());
                }
                Op::Delete(k) => {
                    let btree_existed = tree.delete(k).unwrap();
                    let oracle_existed = oracle.remove(k).is_some();
                    prop_assert_eq!(btree_existed, oracle_existed,
                        "delete existence mismatch for key {:?}", k);
                }
            }
        }

        // Every key still in oracle must be present with matching value.
        for (k, v) in &oracle {
            let got = tree.get(k).unwrap();
            prop_assert_eq!(got.as_deref(), Some(v.as_slice()),
                "mismatch for key {:?}", k);
        }

        // Deleted keys (those that appeared in Delete ops but not in oracle) return None.
        for op in &ops {
            if let Op::Delete(k) = op {
                if !oracle.contains_key(k) {
                    let got = tree.get(k).unwrap();
                    prop_assert!(got.is_none(),
                        "expected None for deleted key {:?}, got {:?}", k, got);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Range scan oracle
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 50, .. ProptestConfig::default() })]

    #[test]
    fn range_scan_oracle(
        pairs in arb_kv_pairs(200),
        start_key in arb_key(),
        end_key in arb_key(),
    ) {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);
        let mut oracle = BTreeMap::<Vec<u8>, Vec<u8>>::new();

        for (k, v) in &pairs {
            tree.insert(k, v).unwrap();
            oracle.insert(k.clone(), v.clone());
        }

        // Ensure start <= end for a valid range.
        let (lo, hi) = if start_key <= end_key {
            (start_key.as_slice(), end_key.as_slice())
        } else {
            (end_key.as_slice(), start_key.as_slice())
        };

        let btree_results = tree.range(lo, hi).unwrap();
        let oracle_results: Vec<(Vec<u8>, Vec<u8>)> = oracle
            .range::<Vec<u8>, _>(&lo.to_vec()..=&hi.to_vec())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        prop_assert_eq!(btree_results.len(), oracle_results.len(),
            "range result length mismatch: btree={}, oracle={}",
            btree_results.len(), oracle_results.len());

        for (bt, or) in btree_results.iter().zip(oracle_results.iter()) {
            prop_assert_eq!(bt, or, "range entry mismatch");
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Idempotent insert (update semantics)
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn idempotent_insert(key in arb_key(), val1 in arb_value(), val2 in arb_value()) {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);

        tree.insert(&key, &val1).unwrap();
        let got1 = tree.get(&key).unwrap();
        prop_assert_eq!(got1.as_deref(), Some(val1.as_slice()));

        tree.insert(&key, &val2).unwrap();
        let got2 = tree.get(&key).unwrap();
        prop_assert_eq!(got2.as_deref(), Some(val2.as_slice()),
            "second insert should overwrite the first value");
    }
}

// ---------------------------------------------------------------------------
// 5. Sorted iteration
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 50, .. ProptestConfig::default() })]

    #[test]
    fn sorted_iteration(pairs in arb_kv_pairs(300)) {
        let (_dir, mut pager) = make_pager();
        let mut tree = BTree::new(&mut pager);

        for (k, v) in &pairs {
            tree.insert(k, v).unwrap();
        }

        // Full range scan: smallest possible key to largest possible key.
        let results = tree.range(&[0u8], &vec![0xFFu8; 100]).unwrap();

        // Results must be sorted by key.
        for window in results.windows(2) {
            prop_assert!(window[0].0 <= window[1].0,
                "keys not sorted: {:?} > {:?}", window[0].0, window[1].0);
        }
    }
}
