# ADR-0006: MVCC, sessions, and the single-writer rule

- **Status:** Accepted
- **Date:** 2026-10-02

## Context

Phase 5 must add two capabilities without breaking the crash guarantees of
ADR-0004: secondary indexes (so the planner can avoid full scans) and MVCC
(so concurrent sessions never observe each other's uncommitted work).

## Decision

**Row versioning.** Every stored row is a version `[xmin u64][xmax u64]
[row bytes]` — creator and deleter transaction ids. Updates are
append-only: the old version is tombstoned in place and the new version is
written at a fresh row id (garbage collection is future work; documented
space growth). Visibility rule:

- inserted-visible: creator == this transaction, or creator committed
  *before* this transaction began;
- delete-hidden: deleter == this transaction, or deleter committed *before*
  this transaction began. Deletes committed *after* our snapshot began do
  not affect us — that is the snapshot guarantee.
- Unknown transaction ids are `Committed` by definition: a crash discards
  every uncommitted journal record, so anything on disk survived its commit.
  This one rule is what makes MVCC and the WAL compose for free.

**Transaction ids live in the meta page** (journaled like everything else),
so id allocation survives crashes; the in-memory registry holds only
session-lifetime statuses and may forget `Committed` entries at any time
(unknown ⇒ committed).

**Sessions over one shared `Database`.** `SqlEngine` holds
`Arc<Mutex<Database>>` and is `Clone`: each clone is an independent session.
The server (phase 6) gives every connection a clone.

**Single writer, snapshot readers** (SQLite's classic model): one storage
transaction may be open at a time; the session holding it is the writer.
A second writer's `BEGIN`/auto-commit write fails with `Locked` rather than
blocking. Readers never open storage transactions — they scan the current
buffered state and let the visibility rule filter uncommitted versions.
Consequence: dirty reads are impossible, updates are versioned (not
overwritten), and writer-writer conflicts are prevented structurally rather
than resolved after the fact.

**Secondary indexes.** `CREATE INDEX name ON table (column)` builds a
B+Tree over existing rows (backfill), keyed
`[encoded column value][row id BE u64]` — integers sign-flipped before
big-endian encoding so byte order equals numeric order; NULLs are not
indexed (use `IS NULL`, which full-scans). The planner picks an index scan
for a top-level conjunct `column op literal` (`=, <, <=, >, >=`) via
prefix bounds, then re-applies the *full* filter — correctness never
depends on the planner's choice. Every INSERT/UPDATE/DELETE maintains all
declared indexes; `CREATE INDEX`/`DROP INDEX` are DDL like any other and
inherit journal durability.

## Consequences

- Positive: readers and writers coexist without blocking; the differential
  suite, REPL, and wire protocol all see consistent semantics; the planner
  has a principled escape hatch (index scans) with a verified fallback.
- Negative: single writer per database (by design — multi-writer needs
  per-txn WAL contexts, a phase-2 project on its own); updates grow the
  row tree until a GC pass exists; index scans materialize their key range
  in memory before fetching rows (bounded by predicate selectivity).
- Alternatives rejected: in-place updates with undo records (breaks the
  WAL-first story); first-committer-wins conflict detection (meaningless
  under a single-writer rule); optimistic multi-writer (out of scope).
