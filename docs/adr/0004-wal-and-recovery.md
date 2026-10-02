# ADR-0004: Full-page redo WAL and the recovery algorithm

- **Status:** Accepted
- **Date:** 2026-10-02

## Context

Until now, `sync()` was the only durability line: a crash between a page
write and the next fsync could leave the data file torn, half-updated, or
both. The engine needs transactions with an absolute guarantee — *a
transaction is durable the moment `commit()` returns, whatever happens
after* — and it must survive torn writes at every level (journal, meta
page, data pages).

## Decision

**Write-ahead logging with full-page redo.**

- Every page mutation is journaled as a checksummed frame — `(lsn, page_id,
  CRC32, 4 KiB image)` — appended to `foo.zdb.wal` *before* the page may
  reach the data file. Nothing but the journal is written during a
  transaction; the data file is only touched at checkpoints. This is why
  no rollback undo records are needed: the data file simply never contains
  uncommitted state.
- `commit()` appends a commit frame (page id `u32::MAX`) and fsyncs the
  journal. That fsync is the durability line.
- `checkpoint()` copies buffered images into the data file, fsyncs it, and
  truncates the journal. It runs on clean `Drop`, when the journal passes
  256 buffered pages, or manually. Timing affects only performance, never
  correctness.
- **Recovery** (inside `open`): parse frames from the front until the first
  torn/CRC-failed/out-of-sequence frame — the crash artifact — then replay
  every frame up to the last *committed* one, in LSN order, through an
  unchecked write; re-derive in-memory meta from the repaired page 0;
  truncate the journal. Replay is order-based, not LSN-compared: frames
  carry the whole page, so re-applying an image is always safe.
- **Meta page is journaled like any page.** Page allocation and free
  mutate in-memory bookkeeping only (`alloc_slot`/`free_slot`); the meta
  image is written through the journal itself. An early `Free` marking
  reaching the data file before its commit record would mean committed
  data loss — the WAL-first rule forbids exactly that.
- **The catalog lives in the meta page** (`catalog_root`, one pointer) and
  is itself a B+Tree mapping tree names to roots (ADR-0003). Trees get
  durable roots, and catalog mutations inherit all of the above for free.
- Rollback truncates the journal to the transaction's start offset,
  drops the transaction's buffered images, and restores the meta snapshot.

**Crash simulation without signals:** tests abandon the database with
`std::mem::forget` — no `Drop`, no checkpoint, exactly like `kill -9` —
and then tear the journal at adversarial byte offsets. The invariant under
test: after recovery, every committed transaction is fully present and
every uncommitted one fully absent, whatever the crash point.

## Consequences

- Positive: one 4 KiB frame format covers data, meta, and catalog; recovery
  is a linear scan plus sequential writes; torn tails cannot half-apply;
  the same `PageIo` trait lets tools bypass the journal entirely.
- Negative: full-page images make the journal ~4 KiB per mutated page (a
  delta format is a phase-7+ optimization); auto-checkpoint at 256 pages
  bounds memory, not durability; replay rewrites pages already on disk
  (acceptable: one-time cost after a crash).
- Alternatives rejected: physical delta records (smaller, far harder to
  make correct); undo logging (requires reading pre-images during recovery
  of a possibly-torn file); fsync-per-page (correct but orders of magnitude
  slower than fsync-per-commit).
