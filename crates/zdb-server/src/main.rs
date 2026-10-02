//! `zdb` — the ZelligeDB command line entry point.
//!
//! Phase 4 adds the SQL REPL here; phase 6 adds `zdb serve` speaking the
//! Postgres wire protocol so real `psql` clients can connect.

fn main() {
    let version = env!("CARGO_PKG_VERSION");
    println!("ZelligeDB v{version} — storage engine scaffold");
    println!("The SQL REPL lands in phase 4; see docs/ARCHITECTURE.md for the roadmap.");
}
