# ZelligeDB 🧩

**A relational database storage engine built from scratch in Rust — pages, B+Tree, write-ahead log, SQL, and a Postgres-compatible wire protocol, one verified commit at a time.**

[![CI](https://github.com/hajar-benhadj/zellige-db/actions/workflows/ci.yml/badge.svg)](https://github.com/hajar-benhadj/zellige-db/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

*Zellige* is the Moroccan art of assembling small, precise tiles into intricate
patterns. ZelligeDB applies the same idea to databases: 4 KiB pages, assembled
into trees, journals, and tables — every tile checksummed, every layer tested.

## Why?

Because using a database and understanding one are different skills. This
project builds the real thing from first principles — no database libraries,
no hand-waving:

- a **pager** that turns a file into reliable, corruption-detecting pages
- a **B+Tree** index with splits, merges, and range scans
- a **write-ahead log** that survives `kill -9` at any instant
- a **hand-written SQL** lexer, parser, planner, and Volcano executor
- **MVCC** transactions with snapshot isolation
- a server that **real `psql` clients can connect to**
- differential testing against SQLite and randomized crash testing

## Status & Roadmap

| Phase | Milestone | Status |
|-------|-----------|--------|
| 0 | Workspace, CI, docs | ✅ |
| 1 | Pager: 4 KiB pages, CRC32 checksums, free list | ✅ |
| 2 | B+Tree with property-based tests | ✅ |
| 3 | WAL + crash recovery + crash-test harness | ✅ |
| 4 | SQL: parser, executor, REPL + differential testing vs SQLite | ✅ |
| 5 | Secondary indexes + MVCC snapshot isolation | ✅ |
| 6 | Postgres wire protocol — `psql` compatibility | ✅ |
| 7 | Benchmarks (YCSB-lite) + documented optimizations | ⬜ |
| 8 | WASM playground — try it in the browser | ✅ |
| 9 | v0.1.0 release, blog series | ⬜ |

## Try it in your browser

**[▶ Open the SQL playground](https://hajar-benhadj.github.io/zellige-db/)** —
the full engine compiled to WebAssembly: create tables, insert rows, run
transactions. No server, no network; your SQL never leaves the tab.

## Numbers (measured, not promised)

| benchmark | result |
|---|---|
| point query, index scan vs full scan | **132x faster** |
| batched txn vs per-statement auto-commit | **7.8x faster** (same fsync guarantee) |
| page cache (ADR-0008) | **25-35% off** read-heavy paths |

Reproduce with `cargo bench`; full tables and the war story behind them in
[docs/benchmarks.md](docs/benchmarks.md).

## Quick start

```bash
cargo build --release
cargo run -p zdb-server   # CLI scaffold (REPL arrives in phase 4)
cargo test                # run the full test suite
```

## Architecture

```
┌──────────────────────────────────────────────┐
│  zdb-server   REPL · TCP server · wire proto │
├──────────────────────────────────────────────┤
│  zdb-sql      lexer → parser → planner →     │
│               Volcano executor               │
├──────────────────────────────────────────────┤
│  zdb-core     WAL · B+Tree · Pager           │
├──────────────────────────────────────────────┤
│  disk         one .zdb file + one .wal file  │
└──────────────────────────────────────────────┘
```

Design principles: correctness before performance · checksum everything ·
no `unsafe` · minimal dependencies · every decision gets an
[ADR](docs/adr/).

## Documentation

- [Architecture overview](docs/ARCHITECTURE.md)
- [Architecture Decision Records](docs/adr/)
- [Learning notes (per phase)](docs/learning-notes/)

## License

[MIT](LICENSE) © Hajar Benhadj
