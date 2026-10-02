//! `zdb` — the ZelligeDB command line entry point.
//!
//! Subcommands grow with the phases:
//! - `demo-tree [file] [n]` — build a B+Tree of n entries and dump it
//! - the SQL REPL arrives in phase 4; `zdb serve` (pg wire) in phase 6

use std::process::ExitCode;

use zdb_core::{BTree, Database, DbError};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("demo-tree") => demo_tree(&args[1..]),
        _ => {
            eprintln!("ZelligeDB v{}", env!("CARGO_PKG_VERSION"));
            eprintln!();
            eprintln!("usage:");
            eprintln!("  zdb demo-tree [file] [entries]   build a B+Tree and dump its structure");
            ExitCode::from(2)
        }
    }
}

fn demo_tree(args: &[String]) -> ExitCode {
    let path = args
        .first()
        .cloned()
        .unwrap_or_else(|| "demo.zdb".to_string());
    let entries: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(40);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{path}.wal"));

    let run = || -> Result<(), DbError> {
        let mut db = Database::create(&path)?;
        db.begin()?;
        let mut tree = BTree::empty();
        for i in 1..=entries {
            let key = format!("user-{i:04}");
            let value = format!("row-{i}");
            tree.insert(&mut db, key.as_bytes(), value.as_bytes())?;
        }
        db.commit()?;
        db.checkpoint()?;
        println!("ZelligeDB: inserted {entries} entries into {path}\n");
        for line in tree.debug_dump(&mut db)? {
            println!("{line}");
        }
        Ok(())
    };

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
