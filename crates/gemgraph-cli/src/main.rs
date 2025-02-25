use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use gemgraph::{GraphDb, execute_read, execute_write};

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("gemgraph.db"));

    let mut db = if path.with_extension("db").exists() {
        match GraphDb::open(&path) {
            Ok(db) => {
                eprintln!("Opened database at {}", path.display());
                db
            }
            Err(e) => {
                eprintln!("Error opening database: {e}");
                std::process::exit(1);
            }
        }
    } else {
        match GraphDb::create(&path) {
            Ok(db) => {
                eprintln!("Created new database at {}", path.display());
                db
            }
            Err(e) => {
                eprintln!("Error creating database: {e}");
                std::process::exit(1);
            }
        }
    };

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("gemgraph> ");
        stdout.flush().unwrap();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("Read error: {e}");
                break;
            }
        }

        let query = line.trim();
        if query.is_empty() {
            continue;
        }
        if query.eq_ignore_ascii_case("quit") || query.eq_ignore_ascii_case("exit") {
            break;
        }

        let upper = query.to_uppercase();
        let is_write = upper.starts_with("CREATE")
            || upper.starts_with("DELETE")
            || upper.starts_with("MERGE")
            || (upper.starts_with("MATCH") && upper.contains("DELETE"));

        if is_write {
            let mut txn = db.write_txn();
            match execute_write(query, &mut txn) {
                Ok(result) => {
                    print_result(&result);
                    if let Err(e) = txn.commit() {
                        eprintln!("Commit error: {e}");
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    txn.abort();
                }
            }
        } else {
            let txn = db.read_txn();
            match execute_read(query, &txn) {
                Ok(result) => print_result(&result),
                Err(e) => eprintln!("Error: {e}"),
            }
        }
    }

    eprintln!("Bye!");
}

fn print_result(result: &gemgraph::QueryResult) {
    if !result.columns.is_empty() {
        println!("{}", result.columns.join("\t"));
        println!("{}", "-".repeat(result.columns.len() * 16));
        for row in &result.rows {
            let cells: Vec<String> = row.iter().map(format_value).collect();
            println!("{}", cells.join("\t"));
        }
        println!("({} row{})", result.rows.len(), if result.rows.len() == 1 { "" } else { "s" });
    }

    let s = &result.stats;
    if s.nodes_created > 0 || s.edges_created > 0 || s.nodes_deleted > 0 || s.edges_deleted > 0 {
        let mut parts = Vec::new();
        if s.nodes_created > 0 { parts.push(format!("{} node(s) created", s.nodes_created)); }
        if s.edges_created > 0 { parts.push(format!("{} edge(s) created", s.edges_created)); }
        if s.nodes_deleted > 0 { parts.push(format!("{} node(s) deleted", s.nodes_deleted)); }
        if s.edges_deleted > 0 { parts.push(format!("{} edge(s) deleted", s.edges_deleted)); }
        println!("{}", parts.join(", "));
    }
}

fn format_value(v: &gemgraph::Value) -> String {
    match v {
        gemgraph::Value::Null => "null".to_string(),
        gemgraph::Value::Bool(b) => b.to_string(),
        gemgraph::Value::Int(i) => i.to_string(),
        gemgraph::Value::Float(f) => f.to_string(),
        gemgraph::Value::String(s) => format!("\"{}\"", s),
    }
}
