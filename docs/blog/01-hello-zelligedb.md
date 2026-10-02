# I built a database engine from scratch in Rust. Here's what survived.

*Part 1 of the ZelligeDB build log — 2026-10-02*

Most portfolios have a CRUD app. Very few have a database. I wanted to
know whether I could build the real thing — pages on disk, a B+Tree, a
write-ahead log, SQL, and eventually a server that `psql` could connect
to — without borrowing a single library that does the interesting work.
This is the story of [ZelligeDB](https://github.com/hajar-benhadj/zellige-db)
at v0.1.0, told through the four moments that taught me the most.

*Zellige* is the Moroccan art of assembling small, precise tiles into
intricate patterns. That turned out to be the right metaphor: the engine
is 4 KiB tiles (pages), assembled into trees, journals, and tables — every
tile checksummed, every layer tested.

## 1. The crash test that justifies the whole design

The scariest question in storage engineering: what happens if the power
dies *mid-write*? ZelligeDB's answer is a write-ahead log with full-page
redo: every page mutation is journaled with a CRC32 checksum *before* the
data file is ever touched. `commit()` is one fsync of the journal — and
that is the durability line.

The test suite simulates crashes without process signals: it abandons the
database with `std::mem::forget` (no destructor, no checkpoint — exactly
like `kill -9`) and then tears the journal file at 200 adversarial byte
offsets. The invariant under test is absolute:

> after recovery, every committed transaction is fully present and every
> uncommitted one is fully absent — whatever the crash point.

Designing for that invariant forced the best decision of the project: the
meta page — page counters, free list, catalog root, transaction ids — is
journaled like any other page. An early `Free` marking reaching the data
file before its commit record would silently lose committed data. WAL-first
forbids exactly that, and rollback became "forget the buffered pages"
instead of a whole undo subsystem.

## 2. The property test that hunts B+Tree bugs for free

My B+Tree had unit tests. They were not enough — splits, borrows, and
merges have *far* too many interleavings to enumerate by hand. So the
suite drives random insert/get/delete sequences with a five-line LCG
against `std::collections::BTreeMap` as the reference model, plus a
`check_integrity` walk (ordering, occupancy, leaf-chain agreement) every
500 operations.

The bug hunter turned out to be a **60-key universe over 4,000
operations**: a tree so small that every operation triggers structural
rebalancing. Large trees test growth; tiny trees test the merge paths
where the real bugs live. Every assertion prints its seed — any failure
reproduces exactly, on any machine.

## 3. The war story: a cache that un-repaired a repair

Phase 7 added a page cache — forty lines, ~30% off read-heavy paths. The
first crash-suite run after that change failed with `PageOutOfBounds(4)`
on perfectly good data.

The cache invalidated nothing on recovery's raw writes: a *repaired* meta
page read back *stale* from the cache, and recovery quietly restored the
broken state it had just fixed. Two-line fix, permanent regression test.
The lesson is not "caches are dangerous" — it is that the crash suite
converts subtle invalidation bugs into reproducible one-line findings,
which is the entire argument for testing correctness before performance.

## 4. Measuring instead of adjectives

Every number in the README is reproducible via `cargo bench`:

- point queries through a secondary index: **132× faster** than a full
  scan at 10k rows (and the gap grows with the table);
- inserts batched in one transaction: **7.8× faster** than auto-commit —
  same fsync guarantee, amortized;
- the page cache: 25–35% off read-heavy paths.

The benchmarks also documented an honest weakness: UPDATE filters still
full-scan (the planner covers SELECT in v0.1) and every update appends a
new row version with no GC yet. Roadmap items, not accidents.

## What shipped

- **Storage**: pages + B+Tree + WAL + MVCC, `deny(unsafe_code)`,
  differential-tested against SQLite (600 randomized queries must match
  exactly)
- **Server**: `psql -h 127.0.0.1` connects to it — real wire protocol,
  real SQLSTATE codes
- **Browser**: the same engine compiled to WebAssembly —
  [try it live](https://hajar-benhadj.github.io/zellige-db/), no server,
  your SQL never leaves the tab
- **65 tests across 15 suites**, CI on three operating systems, nine
  Architecture Decision Records

## What's next

Extended query mode (GUI clients), UPDATE/DELETE through the index
planner, version GC, OPFS persistence for the playground, and — the one
I'm most looking forward to — group commit.

The repo: [github.com/hajar-benhadj/zellige-db](https://github.com/hajar-benhadj/zellige-db).
If you read one file, make it
[docs/adr/0004-wal-and-recovery.md](https://github.com/hajar-benhadj/zellige-db/blob/main/docs/adr/0004-wal-and-recovery.md) —
it's the decision everything else stands on.
