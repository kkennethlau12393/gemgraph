//! GemGraph benchmarks vs SQLite.
//!
//! Workloads:
//! 1. KV insert throughput (100K entries)
//! 2. KV point lookup throughput
//! 3. KV range scan throughput
//! 4. Graph ingest (nodes + edges)
//! 5. Graph traversal (1-hop, 2-hop)
//! 6. Cypher MATCH query

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gemgraph::{GraphDb, Value, Direction, execute_read};
use rusqlite::{Connection, params};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct BenchResult {
    name: String,
    gemgraph_duration: Duration,
    sqlite_duration: Duration,
    count: u64,
}

impl BenchResult {
    fn print(&self) {
        let g_ms = self.gemgraph_duration.as_secs_f64() * 1000.0;
        let s_ms = self.sqlite_duration.as_secs_f64() * 1000.0;
        let g_ops = self.count as f64 / self.gemgraph_duration.as_secs_f64();
        let s_ops = self.count as f64 / self.sqlite_duration.as_secs_f64();
        let ratio = g_ms / s_ms;
        println!(
            "  {:<35} GemGraph: {:>8.1}ms ({:>10.0} ops/s)  SQLite: {:>8.1}ms ({:>10.0} ops/s)  ratio: {:.2}x",
            self.name, g_ms, g_ops, s_ms, s_ops, ratio
        );
    }
}

fn props(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

// ---------------------------------------------------------------------------
// Benchmark 1: KV Insert
// ---------------------------------------------------------------------------

fn bench_kv_insert(n: u64) -> BenchResult {
    let dir = tempdir().unwrap();

    // GemGraph
    let g_dur = {
        let path = dir.path().join("gemgraph_kv");
        let mut db = GraphDb::create(&path).unwrap();
        let start = Instant::now();
        // Batch commits every 1000 entries
        let batch = 1000;
        for chunk_start in (0..n).step_by(batch as usize) {
            let mut wtx = db.write_txn();
            let chunk_end = (chunk_start + batch).min(n);
            for i in chunk_start..chunk_end {
                let mut p = HashMap::new();
                p.insert("key".into(), Value::String(format!("key_{:08}", i)));
                p.insert("val".into(), Value::String(format!("value_{:08}", i)));
                wtx.create_node("KV", p).unwrap();
            }
            wtx.commit().unwrap();
        }
        start.elapsed()
    };

    // SQLite
    let s_dur = {
        let path = dir.path().join("sqlite_kv.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;").unwrap();
        conn.execute(
            "CREATE TABLE kv (id INTEGER PRIMARY KEY, key TEXT NOT NULL, val TEXT NOT NULL)",
            [],
        ).unwrap();
        let start = Instant::now();
        let batch = 1000;
        for chunk_start in (0..n).step_by(batch as usize) {
            let tx = conn.unchecked_transaction().unwrap();
            let chunk_end = (chunk_start + batch).min(n);
            for i in chunk_start..chunk_end {
                conn.execute(
                    "INSERT INTO kv (key, val) VALUES (?1, ?2)",
                    params![format!("key_{:08}", i), format!("value_{:08}", i)],
                ).unwrap();
            }
            tx.commit().unwrap();
        }
        start.elapsed()
    };

    BenchResult { name: format!("KV insert ({n} entries)"), gemgraph_duration: g_dur, sqlite_duration: s_dur, count: n }
}

// ---------------------------------------------------------------------------
// Benchmark 2: KV Point Lookup
// ---------------------------------------------------------------------------

fn bench_kv_lookup(n: u64) -> BenchResult {
    let dir = tempdir().unwrap();
    let lookups = n;

    // Setup GemGraph
    let path_g = dir.path().join("gemgraph_lookup");
    let mut db = GraphDb::create(&path_g).unwrap();
    let mut node_ids = Vec::new();
    {
        let batch = 1000;
        for chunk_start in (0..n).step_by(batch as usize) {
            let mut wtx = db.write_txn();
            let chunk_end = (chunk_start + batch).min(n);
            for i in chunk_start..chunk_end {
                let id = wtx.create_node("KV", props(&[
                    ("key", Value::String(format!("key_{:08}", i))),
                    ("val", Value::String(format!("value_{:08}", i))),
                ])).unwrap();
                node_ids.push(id);
            }
            wtx.commit().unwrap();
        }
    }

    // GemGraph lookups
    let g_dur = {
        let rtx = db.read_txn();
        let start = Instant::now();
        for i in 0..lookups {
            let idx = (i * 7 + 13) % n; // pseudo-random access
            let _ = rtx.get_node(node_ids[idx as usize]).unwrap();
        }
        start.elapsed()
    };

    // Setup + lookup SQLite
    let path_s = dir.path().join("sqlite_lookup.db");
    let conn = Connection::open(&path_s).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;").unwrap();
    conn.execute(
        "CREATE TABLE kv (id INTEGER PRIMARY KEY, key TEXT NOT NULL, val TEXT NOT NULL)",
        [],
    ).unwrap();
    {
        let tx = conn.unchecked_transaction().unwrap();
        for i in 0..n {
            conn.execute(
                "INSERT INTO kv (id, key, val) VALUES (?1, ?2, ?3)",
                params![i as i64, format!("key_{:08}", i), format!("value_{:08}", i)],
            ).unwrap();
        }
        tx.commit().unwrap();
    }

    let s_dur = {
        let start = Instant::now();
        let mut stmt = conn.prepare("SELECT key, val FROM kv WHERE id = ?1").unwrap();
        for i in 0..lookups {
            let idx = ((i * 7 + 13) % n) as i64;
            let _: String = stmt.query_row(params![idx], |row| row.get(0)).unwrap();
        }
        start.elapsed()
    };

    BenchResult { name: format!("KV point lookup ({lookups} lookups)"), gemgraph_duration: g_dur, sqlite_duration: s_dur, count: lookups }
}

// ---------------------------------------------------------------------------
// Benchmark 3: Graph Ingest
// ---------------------------------------------------------------------------

fn bench_graph_ingest(num_nodes: u64, num_edges: u64) -> BenchResult {
    let dir = tempdir().unwrap();

    // GemGraph
    let g_dur = {
        let path = dir.path().join("gemgraph_ingest");
        let mut db = GraphDb::create(&path).unwrap();
        let start = Instant::now();
        let mut nids = Vec::with_capacity(num_nodes as usize);
        // Insert nodes in batches
        let batch = 1000u64;
        for chunk_start in (0..num_nodes).step_by(batch as usize) {
            let mut wtx = db.write_txn();
            let chunk_end = (chunk_start + batch).min(num_nodes);
            for i in chunk_start..chunk_end {
                let id = wtx.create_node("N", props(&[("i", Value::Int(i as i64))])).unwrap();
                nids.push(id);
            }
            wtx.commit().unwrap();
        }
        // Insert edges in batches
        for chunk_start in (0..num_edges).step_by(batch as usize) {
            let mut wtx = db.write_txn();
            let chunk_end = (chunk_start + batch).min(num_edges);
            for i in chunk_start..chunk_end {
                let src = (i * 7 + 3) % num_nodes;
                let mut dst = (i * 13 + 17) % num_nodes;
                if dst == src { dst = (dst + 1) % num_nodes; }
                wtx.create_edge(nids[src as usize], nids[dst as usize], "E", HashMap::new()).unwrap();
            }
            wtx.commit().unwrap();
        }
        start.elapsed()
    };

    // SQLite (graph stored as node + edge tables)
    let s_dur = {
        let path = dir.path().join("sqlite_ingest.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;").unwrap();
        conn.execute("CREATE TABLE nodes (id INTEGER PRIMARY KEY, label TEXT, i INTEGER)", []).unwrap();
        conn.execute("CREATE TABLE edges (id INTEGER PRIMARY KEY, src INTEGER, dst INTEGER, type TEXT)", []).unwrap();
        let start = Instant::now();
        // Nodes
        {
            let tx = conn.unchecked_transaction().unwrap();
            for i in 0..num_nodes {
                conn.execute("INSERT INTO nodes (id, label, i) VALUES (?1, 'N', ?2)", params![i as i64, i as i64]).unwrap();
            }
            tx.commit().unwrap();
        }
        // Edges
        {
            let tx = conn.unchecked_transaction().unwrap();
            for i in 0..num_edges {
                let src = (i * 7 + 3) % num_nodes;
                let mut dst = (i * 13 + 17) % num_nodes;
                if dst == src { dst = (dst + 1) % num_nodes; }
                conn.execute("INSERT INTO edges (src, dst, type) VALUES (?1, ?2, 'E')", params![src as i64, dst as i64]).unwrap();
            }
            tx.commit().unwrap();
        }
        start.elapsed()
    };

    BenchResult {
        name: format!("Graph ingest ({num_nodes} nodes + {num_edges} edges)"),
        gemgraph_duration: g_dur, sqlite_duration: s_dur,
        count: num_nodes + num_edges,
    }
}

// ---------------------------------------------------------------------------
// Benchmark 4: Graph Traversal (1-hop, 2-hop)
// ---------------------------------------------------------------------------

fn bench_graph_traversal(num_nodes: u64, num_edges: u64, hops: u32) -> BenchResult {
    let dir = tempdir().unwrap();
    let sample_nodes = 100u64;

    // Setup GemGraph
    let path_g = dir.path().join("gemgraph_trav");
    let mut db = GraphDb::create(&path_g).unwrap();
    let mut nids = Vec::new();
    {
        let batch = 1000u64;
        for chunk_start in (0..num_nodes).step_by(batch as usize) {
            let mut wtx = db.write_txn();
            let chunk_end = (chunk_start + batch).min(num_nodes);
            for i in chunk_start..chunk_end {
                let id = wtx.create_node("N", props(&[("i", Value::Int(i as i64))])).unwrap();
                nids.push(id);
            }
            wtx.commit().unwrap();
        }
        for chunk_start in (0..num_edges).step_by(batch as usize) {
            let mut wtx = db.write_txn();
            let chunk_end = (chunk_start + batch).min(num_edges);
            for i in chunk_start..chunk_end {
                let src = (i * 7 + 3) % num_nodes;
                let mut dst = (i * 13 + 17) % num_nodes;
                if dst == src { dst = (dst + 1) % num_nodes; }
                wtx.create_edge(nids[src as usize], nids[dst as usize], "E", HashMap::new()).unwrap();
            }
            wtx.commit().unwrap();
        }
    }

    // GemGraph traversal
    let g_dur = {
        let rtx = db.read_txn();
        let start = Instant::now();
        let mut total_visited = 0u64;
        for s in 0..sample_nodes {
            let start_node = nids[(s * 37 % num_nodes) as usize];
            let mut frontier = vec![start_node];
            for _ in 0..hops {
                let mut next = Vec::new();
                for &nid in &frontier {
                    let neighbors = rtx.neighbors(nid, Direction::Out, Some("E")).unwrap();
                    for (_, neighbor) in neighbors {
                        next.push(neighbor);
                    }
                }
                frontier = next;
            }
            total_visited += frontier.len() as u64;
        }
        let _ = total_visited;
        start.elapsed()
    };

    // Setup SQLite
    let path_s = dir.path().join("sqlite_trav.db");
    let conn = Connection::open(&path_s).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;").unwrap();
    conn.execute("CREATE TABLE nodes (id INTEGER PRIMARY KEY, i INTEGER)", []).unwrap();
    conn.execute("CREATE TABLE edges (src INTEGER, dst INTEGER)", []).unwrap();
    conn.execute("CREATE INDEX idx_edges_src ON edges(src)", []).unwrap();
    {
        let tx = conn.unchecked_transaction().unwrap();
        for i in 0..num_nodes {
            conn.execute("INSERT INTO nodes VALUES (?1, ?2)", params![nids[i as usize] as i64, i as i64]).unwrap();
        }
        for i in 0..num_edges {
            let src = (i * 7 + 3) % num_nodes;
            let mut dst = (i * 13 + 17) % num_nodes;
            if dst == src { dst = (dst + 1) % num_nodes; }
            conn.execute("INSERT INTO edges VALUES (?1, ?2)", params![nids[src as usize] as i64, nids[dst as usize] as i64]).unwrap();
        }
        tx.commit().unwrap();
    }

    // SQLite traversal
    let s_dur = {
        let start = Instant::now();
        let mut total_visited = 0u64;
        for s in 0..sample_nodes {
            let start_node = nids[(s * 37 % num_nodes) as usize] as i64;
            let mut frontier = vec![start_node];
            for _ in 0..hops {
                let mut next = Vec::new();
                for &nid in &frontier {
                    let mut stmt = conn.prepare_cached("SELECT dst FROM edges WHERE src = ?1").unwrap();
                    let rows = stmt.query_map(params![nid], |row| row.get::<_, i64>(0)).unwrap();
                    for row in rows {
                        next.push(row.unwrap());
                    }
                }
                frontier = next;
            }
            total_visited += frontier.len() as u64;
        }
        let _ = total_visited;
        start.elapsed()
    };

    BenchResult {
        name: format!("{hops}-hop traversal ({sample_nodes} starts, {num_nodes}N/{num_edges}E)"),
        gemgraph_duration: g_dur, sqlite_duration: s_dur,
        count: sample_nodes,
    }
}

// ---------------------------------------------------------------------------
// Benchmark 5: Cypher MATCH query
// ---------------------------------------------------------------------------

fn bench_cypher_match() -> BenchResult {
    let dir = tempdir().unwrap();
    let num_nodes = 5000u64;

    // Setup GemGraph
    let path_g = dir.path().join("gemgraph_cypher");
    let mut db = GraphDb::create(&path_g).unwrap();
    {
        let batch = 1000u64;
        for chunk_start in (0..num_nodes).step_by(batch as usize) {
            let mut wtx = db.write_txn();
            let chunk_end = (chunk_start + batch).min(num_nodes);
            for i in chunk_start..chunk_end {
                wtx.create_node("Person", props(&[
                    ("name", Value::String(format!("person_{}", i))),
                    ("age", Value::Int((i % 80) as i64)),
                ])).unwrap();
            }
            wtx.commit().unwrap();
        }
    }

    let queries = 50u64;

    // GemGraph Cypher
    let g_dur = {
        let start = Instant::now();
        for _ in 0..queries {
            let rtx = db.read_txn();
            let result = execute_read("MATCH (n:Person) WHERE n.age > 50 RETURN n.name LIMIT 100", &rtx).unwrap();
            assert!(!result.rows.is_empty());
        }
        start.elapsed()
    };

    // SQLite equivalent
    let path_s = dir.path().join("sqlite_cypher.db");
    let conn = Connection::open(&path_s).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;").unwrap();
    conn.execute("CREATE TABLE person (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)", []).unwrap();
    conn.execute("CREATE INDEX idx_person_age ON person(age)", []).unwrap();
    {
        let tx = conn.unchecked_transaction().unwrap();
        for i in 0..num_nodes {
            conn.execute(
                "INSERT INTO person VALUES (?1, ?2, ?3)",
                params![i as i64, format!("person_{}", i), (i % 80) as i64],
            ).unwrap();
        }
        tx.commit().unwrap();
    }

    let s_dur = {
        let start = Instant::now();
        for _ in 0..queries {
            let mut stmt = conn.prepare_cached("SELECT name FROM person WHERE age > 50 LIMIT 100").unwrap();
            let rows: Vec<String> = stmt.query_map([], |row| row.get(0)).unwrap()
                .map(|r| r.unwrap()).collect();
            assert!(!rows.is_empty());
        }
        start.elapsed()
    };

    BenchResult {
        name: format!("Cypher MATCH WHERE LIMIT ({queries} queries, {num_nodes} nodes)"),
        gemgraph_duration: g_dur, sqlite_duration: s_dur,
        count: queries,
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    println!("=== GemGraph Benchmarks ===\n");
    println!("  Comparing GemGraph (custom B+tree engine) vs SQLite (WAL mode)\n");

    let results = vec![
        bench_kv_insert(100_000),
        bench_kv_lookup(100_000),
        bench_graph_ingest(10_000, 50_000),
        bench_graph_traversal(10_000, 50_000, 1),
        bench_graph_traversal(10_000, 50_000, 2),
        bench_cypher_match(),
    ];

    println!("\n  {:<35} {:^35} {:^35} {:>8}", "Benchmark", "GemGraph", "SQLite", "Ratio");
    println!("  {}", "-".repeat(120));
    for r in &results {
        r.print();
    }

    println!("\n  Ratio < 1.0 means GemGraph is faster. Ratio > 1.0 means SQLite is faster.");
    println!("  Note: GemGraph uses no query optimizer; SQLite has decades of optimization.");
    println!();
}
