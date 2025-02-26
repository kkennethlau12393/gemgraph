//! Crash-recovery stress tests for GemGraph.
//!
//! Verifies the core durability guarantee: after `commit()` returns, data
//! survives process restarts and crashes (via dual-meta page flip + WAL).

use std::collections::HashMap;
use std::io::BufRead;
use tempfile::tempdir;

use gemgraph::{GraphDb, Value};

fn props(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// Committed data from multiple txns survives unclean shutdown and reopen.
#[test]
fn committed_data_survives_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test1.db");

    let mut committed_ids = Vec::new();

    {
        let mut db = GraphDb::create(&path).unwrap();

        // Batch 1: committed
        let mut wtx = db.write_txn();
        for i in 0..5 {
            let id = wtx
                .create_node("Batch1", props(&[("idx", Value::Int(i))]))
                .unwrap();
            committed_ids.push(id);
        }
        wtx.commit().unwrap();

        // Batch 2: committed
        let mut wtx = db.write_txn();
        for i in 5..10 {
            let id = wtx
                .create_node("Batch2", props(&[("idx", Value::Int(i))]))
                .unwrap();
            committed_ids.push(id);
        }
        wtx.commit().unwrap();

        // Batch 3: uncommitted -- just drop the txn, then drop the db
        let mut wtx = db.write_txn();
        for i in 100..110 {
            wtx.create_node("Uncommitted", props(&[("idx", Value::Int(i))]))
                .unwrap();
        }
        // Drop txn without commit, then db drops (simulates unclean shutdown)
    }

    // Reopen -- WAL recovery runs here
    let db = GraphDb::open(&path).unwrap();
    let rtx = db.read_txn();

    // All 10 committed nodes must be present with correct data
    for &nid in &committed_ids {
        let node = rtx.get_node(nid).unwrap();
        assert!(node.is_some(), "committed node {} missing after recovery", nid);
    }

    // Verify Batch1 nodes
    let b1 = rtx.nodes_by_label("Batch1").unwrap();
    assert_eq!(b1.len(), 5, "expected 5 Batch1 nodes, found {}", b1.len());

    // Verify Batch2 nodes
    let b2 = rtx.nodes_by_label("Batch2").unwrap();
    assert_eq!(b2.len(), 5, "expected 5 Batch2 nodes, found {}", b2.len());

    // Verify properties are intact on a sample node
    let sample = rtx.get_node(committed_ids[0]).unwrap().unwrap();
    assert_eq!(sample.label, "Batch1");
    assert!(matches!(sample.properties.get("idx"), Some(Value::Int(0))));
}

/// 500 nodes across 50 commit cycles survive reopen with correct properties.
#[test]
fn data_integrity_many_commits() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test2.db");

    let mut all_ids = Vec::new();

    {
        let mut db = GraphDb::create(&path).unwrap();
        for batch in 0..50u64 {
            let mut wtx = db.write_txn();
            for item in 0..10u64 {
                let global = batch * 10 + item;
                let id = wtx
                    .create_node(
                        "Item",
                        props(&[
                            ("batch", Value::Int(batch as i64)),
                            ("item", Value::Int(item as i64)),
                            ("global", Value::Int(global as i64)),
                        ]),
                    )
                    .unwrap();
                all_ids.push((id, batch, item, global));
            }
            wtx.commit().unwrap();
        }
    }

    // Reopen and verify all 500 nodes
    let db = GraphDb::open(&path).unwrap();
    let rtx = db.read_txn();

    let items = rtx.nodes_by_label("Item").unwrap();
    assert_eq!(items.len(), 500, "expected 500 nodes after reopen");

    // Verify every node has correct properties
    for &(nid, batch, item, global) in &all_ids {
        let node = rtx.get_node(nid).unwrap().expect("node missing");
        assert_eq!(node.properties.get("batch"), Some(&Value::Int(batch as i64)));
        assert_eq!(node.properties.get("item"), Some(&Value::Int(item as i64)));
        assert_eq!(node.properties.get("global"), Some(&Value::Int(global as i64)));
    }
}

/// Child process commits 200 nodes, then is killed with SIGKILL. Parent
/// reopens the database and verifies all committed data survived the crash.
#[test]
fn fork_kill_crash_recovery() {
    // If we are the child process, run child logic and never return.
    if std::env::var("GEMGRAPH_CRASH_TEST_CHILD").is_ok() {
        let path_str = std::env::var("GEMGRAPH_CRASH_TEST_PATH").unwrap();
        let path = std::path::Path::new(&path_str);

        let mut db = GraphDb::create(path).unwrap();

        // Txn 1: commit 100 nodes
        {
            let mut wtx = db.write_txn();
            for i in 0..100i64 {
                wtx.create_node("Committed", props(&[("val", Value::Int(i))]))
                    .unwrap();
            }
            wtx.commit().unwrap();
        }

        // Txn 2: commit 100 more nodes
        {
            let mut wtx = db.write_txn();
            for i in 100..200i64 {
                wtx.create_node("Committed", props(&[("val", Value::Int(i))]))
                    .unwrap();
            }
            wtx.commit().unwrap();
        }

        // Signal readiness to parent, then block forever (parent will SIGKILL us)
        eprintln!("CHILD_READY");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }

    // --- Parent logic ---
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test3.db");

    let exe = std::env::current_exe().unwrap();
    let mut child = std::process::Command::new(&exe)
        .arg("--exact")
        .arg("fork_kill_crash_recovery")
        .arg("--nocapture")
        .env("GEMGRAPH_CRASH_TEST_CHILD", "1")
        .env("GEMGRAPH_CRASH_TEST_PATH", db_path.to_str().unwrap())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn child");

    // Wait for child to signal it has committed both batches
    let stderr = child.stderr.take().unwrap();
    let reader = std::io::BufReader::new(stderr);
    let mut ready = false;
    for line in reader.lines() {
        let line = line.unwrap();
        if line.contains("CHILD_READY") {
            ready = true;
            break;
        }
    }
    assert!(ready, "child never signalled readiness");

    // Kill the child with SIGKILL -- no chance to clean up
    child.kill().expect("failed to kill child");
    child.wait().expect("failed to wait for child");

    // Reopen the database -- recovery runs here
    let db = GraphDb::open(&db_path).unwrap();
    let rtx = db.read_txn();

    // All 200 committed nodes must survive the crash
    let committed = rtx.nodes_by_label("Committed").unwrap();
    assert_eq!(
        committed.len(),
        200,
        "expected 200 committed nodes after SIGKILL recovery, found {}",
        committed.len()
    );

    // Verify each committed node has correct properties
    let mut seen_vals: Vec<i64> = Vec::new();
    for &nid in &committed {
        let node = rtx.get_node(nid).unwrap().unwrap();
        assert_eq!(node.label, "Committed");
        match node.properties.get("val") {
            Some(Value::Int(v)) => {
                assert!(
                    (0..200).contains(v),
                    "committed node has unexpected val: {}",
                    v
                );
                seen_vals.push(*v);
            }
            other => panic!("expected Int val on committed node, got {:?}", other),
        }
    }

    // Every value from 0..200 must be present
    seen_vals.sort();
    let expected: Vec<i64> = (0..200).collect();
    assert_eq!(
        seen_vals, expected,
        "not all committed values survived the crash"
    );
}

/// 20 iterations of open-commit-close, verifying cumulative data each cycle.
#[test]
fn repeated_open_commit_close_cycles() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test4.db");

    let mut expected_total = 0u64;

    for iteration in 0..20u64 {
        let mut db = if iteration == 0 {
            GraphDb::create(&path).unwrap()
        } else {
            GraphDb::open(&path).unwrap()
        };

        // Verify committed count so far
        {
            let rtx = db.read_txn();
            let nodes = rtx.nodes_by_label("Durable").unwrap();
            assert_eq!(
                nodes.len() as u64, expected_total,
                "iteration {}: expected {} durable nodes, found {}",
                iteration, expected_total, nodes.len()
            );
        }

        // Commit 5 nodes
        {
            let mut wtx = db.write_txn();
            for j in 0..5u64 {
                let global = iteration * 5 + j;
                wtx.create_node(
                    "Durable",
                    props(&[
                        ("iter", Value::Int(iteration as i64)),
                        ("global", Value::Int(global as i64)),
                    ]),
                )
                .unwrap();
            }
            wtx.commit().unwrap();
        }
        expected_total += 5;
        // db drops here -- close and reopen on next iteration
    }

    // Final verification after all cycles
    let db = GraphDb::open(&path).unwrap();
    let rtx = db.read_txn();

    let durable = rtx.nodes_by_label("Durable").unwrap();
    assert_eq!(
        durable.len(),
        100,
        "expected 100 durable nodes (20 iterations * 5 nodes)"
    );

    // Verify complete coverage: all global values 0..100 exist
    let mut globals: Vec<i64> = durable
        .iter()
        .map(|&nid| {
            let node = rtx.get_node(nid).unwrap().unwrap();
            match node.properties.get("global") {
                Some(Value::Int(v)) => *v,
                _ => panic!("missing global property"),
            }
        })
        .collect();
    globals.sort();
    let expected: Vec<i64> = (0..100).collect();
    assert_eq!(globals, expected, "not all global values present");
}
