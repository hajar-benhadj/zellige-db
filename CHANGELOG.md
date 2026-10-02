# Changelog

All notable changes to ZelligeDB are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/).

## [0.1.0] — 2026-10-02

The first complete release: a relational database engine built from
scratch in Rust — storage, indexes, transactions, SQL, a Postgres
wire-protocol server, and a browser playground.

### Storage core (zdb-core)

- 4 KiB pages with CRC32-verified headers; free-list page reuse with
  double-free rejection; self-describing meta page (ADR-0002)
- B+Tree with byte-weighted leaf splits, chained leaves for range scans,
  borrow/merge rebalancing, and a full structural integrity checker
  (ADR-0003); deterministic LCG property tests (~40k ops/run) against
  `BTreeMap`
- Write-ahead log with checksummed full-page redo frames; commit = fsync
  of the journal; checkpoints copy pages into the data file and truncate
  the log; torn-tail-safe recovery (ADR-0004)
- Randomized crash suite: 200-point journal-tearing fuzz, `mem::forget`
  kill simulation, torn data-file repair, rollback isolation
- MVCC row versioning (`xmin`/`xmax`) with snapshot-visibility rules that
  compose with the WAL (unknown transaction id = committed) (ADR-0006)
- Page cache (ADR-0008): 25–35% off read-heavy paths

### SQL (zdb-sql)

- Hand-written lexer, recursive-descent parser, planner, and
  Volcano-style executor (ADR-0005): `CREATE/DROP TABLE`, `CREATE/DROP
  INDEX`, `INSERT`, `SELECT` (WHERE, multi-column ORDER BY, LIMIT,
  COUNT(*)), `UPDATE`, `DELETE`, `BEGIN/COMMIT/ROLLBACK`, `SHOW TABLES`,
  expressions with LIKE and IS NULL
- Secondary indexes with backfill and full DML maintenance; planner picks
  prefix-bounded index scans and always re-applies the full filter
- Clonable sessions over a shared engine: single writer, snapshot readers,
  `Locked` for conflicting writers
- **Differential testing against SQLite** (Linux CI): 600 randomized
  queries across 4 seeds must match exactly
- Measured: index scan **132× faster** than full scan at 10k rows;
  batched transactions **7.8× faster** than per-statement auto-commit

### Server (zdb-server)

- Postgres wire protocol v3, simple query mode: stock `psql` connects and
  works (ADR-0007); real SQLSTATE error codes; multi-statement batches
- SQL REPL with psql-style ASCII tables; `zdb demo-tree` B+Tree dumper

### WebAssembly (zdb-wasm)

- The whole engine compiles to wasm32 via the PageFile abstraction
  (ADR-0009); zero-dependency browser playground deployed to GitHub
  Pages: <https://hajar-benhadj.github.io/zellige-db/>

### Infrastructure

- CI on Linux/Windows/macOS: rustfmt, clippy `-D warnings`, full test
  suite; differential tests on Linux; Pages deployment workflow
- 9 Architecture Decision Records; learning notes per phase (Darija)
- 65 tests across 15 suites, all passing at release
