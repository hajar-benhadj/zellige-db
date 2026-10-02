//! `zdb` — the ZelligeDB command line entry point.
//!
//! - `zdb [file]`            — the SQL REPL (creates the file when missing)
//! - `zdb demo-tree [f] [n]` — build a B+Tree of n entries and dump it
//! - `zdb serve ...`         — Postgres wire protocol (phase 6)

use std::io::{BufRead, Write};
use std::process::ExitCode;

use zdb_core::{BTree, Database, DbError};
use zdb_sql::SqlEngine;
use zdb_sql::output::render;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("repl") => repl(args.get(1).map(String::as_str)),
        Some("demo-tree") => demo_tree(&args[1..]),
        Some("serve") => serve_cmd(&args[1..]),
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("ZelligeDB v{}", env!("CARGO_PKG_VERSION"));
    eprintln!();
    eprintln!("usage:");
    eprintln!("  zdb [file]                      SQL REPL (default file: zellige.zdb)");
    eprintln!("  zdb demo-tree [file] [entries]  B+Tree structure dump");
    eprintln!("  zdb serve [file] [--port N]     Postgres wire server (psql-compatible)");
    ExitCode::from(2)
}

fn repl(path_arg: Option<&str>) -> ExitCode {
    let path = path_arg.unwrap_or("zellige.zdb");
    let engine = if std::path::Path::new(path).exists() {
        SqlEngine::open(path)
    } else {
        SqlEngine::create(path)
    };
    let mut engine = match engine {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error opening {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "ZelligeDB v{} — {} (SQL REPL)",
        env!("CARGO_PKG_VERSION"),
        path
    );
    println!(
        "Type SQL ending with ';'. .quit to exit. Try: CREATE TABLE t (id INTEGER, name TEXT);"
    );

    let stdin = std::io::stdin();
    let mut buffer = String::new();
    loop {
        if buffer.is_empty() {
            print!("zdb> ");
        } else {
            print!("  .. ");
        }
        let _ = std::io::stdout().flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if buffer.is_empty() {
            match trimmed {
                ".quit" | ".exit" => break,
                "" => continue,
                _ => {}
            }
        }
        buffer.push(' ');
        buffer.push_str(trimmed);

        if !trimmed.ends_with(';') {
            continue; // keep reading a multi-line statement
        }
        let sql = std::mem::take(&mut buffer);
        match engine.execute(&sql) {
            Ok(output) => print!("{}", render(&output)),
            Err(e) => println!("ERROR: {e}"),
        }
    }
    ExitCode::SUCCESS
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

fn serve_cmd(args: &[String]) -> ExitCode {
    let mut file = "zellige.zdb".to_string();
    let mut port: u16 = 5432;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" if i + 1 < args.len() => {
                match args[i + 1].parse() {
                    Ok(p) => port = p,
                    Err(_) => {
                        eprintln!("error: --port expects a number");
                        return ExitCode::FAILURE;
                    }
                }
                i += 2;
            }
            other => {
                file = other.to_string();
                i += 1;
            }
        }
    }
    let engine = if std::path::Path::new(&file).exists() {
        SqlEngine::open(&file)
    } else {
        SqlEngine::create(&file)
    };
    match engine {
        Ok(engine) => match zdb_server::serve::serve(engine, port) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("error opening {file}: {e}");
            ExitCode::FAILURE
        }
    }
}
