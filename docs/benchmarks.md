# Benchmarks

Measured on the developer's Windows machine (windows-gnu, release build,
warm OS page cache, SSD). Absolute numbers are machine-specific; the
*ratios* between configurations are the point. Every number here is
reproducible with `cargo bench` — the harness is checked in
(`crates/zdb-core/benches/storage.rs`, `crates/zdb-sql/benches/sql.rs`).

## Storage layer (zdb-core)

| benchmark | ops | median time | ops/s |
|---|---|---|---|
| B+Tree insert, sequential 100k | 100,000 | ~42 µs/op | ~24,000 |
| B+Tree point get, 100k keys | 100,000 | ~26 µs/op | ~38,000 |
| B+Tree range scan, 100 × 1,000 keys | 100,000 | ~10 µs/op | ~100,000 |

With the page cache disabled (pre-ADR-0008, same machine): inserts ~55 µs,
point gets ~36 µs, scans ~15 µs. The cache removes one seek+read syscall
pair per hot-page access — roughly **25–35% off read-heavy paths** for a
40-line change.

## SQL layer (zdb-sql)

| benchmark | ops | time/op | vs. baseline |
|---|---|---|---|
| INSERT, 10k **auto-commit** statements | 10,000 | ~2,574 µs | 1.0× |
| INSERT, 10k rows in **one batched txn** | 10,000 | ~330 µs | **7.8× faster** |
| point query, **full scan** (10k rows) | 200 | ~87,000 µs | 1.0× |
| point query, **index scan** (planner) | 200 | ~662 µs | **132× faster** |

Reading these honestly:

- The auto-commit cost *is* the durability guarantee: one statement = one
  fsync'd WAL commit. Batch your statements inside a transaction and the
  same guarantee amortizes over the batch — this is precisely why
  `BEGIN`/`COMMIT` exists.
- The 132× index-scan win grows with table size (full scan is O(n) per
  query, index scan is O(log n + matches)).
- Known bottleneck, measured and documented: UPDATE/DELETE filters still
  full-scan (the index planner covers SELECT only in v0.1) and every
  update appends a new row version (no GC yet). Both are roadmap items,
  not accidents.

## A war story from this exact harness

Adding the page cache introduced a stale-read bug: recovery's raw page
writes bypassed the cache, so a *repaired* meta page read back *stale*
from the cache and recovery produced `PageOutOfBounds` on perfectly good
data. The randomized crash suite caught it on the first run after the
change — which is the entire argument for testing correctness the way
this project does (see ADR-0004). The fix was two lines; the test that
guards it stays forever.
