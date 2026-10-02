# ADR-0003: B+Tree node layout and rebalancing policy

- **Status:** Accepted
- **Date:** 2026-10-02

## Context

Everything above the pager — tables, secondary indexes, the catalog — needs
an ordered mapping from byte keys to byte values with point lookups, range
scans, and structural survival under arbitrary insert/delete interleavings.
The structure must live inside 4 KiB pages, never in memory alone.

## Decision

**A classic B+Tree** with the following specifics:

- Keys and values are byte strings (max 800 B each, enforced at the API).
- Leaf nodes are chained (`prev`/`next`), so range scans walk leaves only.
- Separators are *copied up* on leaf splits (the key stays in the right
  leaf) and *moved up* on interior splits (the promoted separator leaves
  both halves). Routing: separator `s` is the smallest key of the subtree
  to its right; descend through the child of the last separator `<= key`.
- Leaf splits are **byte-weighted**, not entry-count-weighted: variable
  key/value sizes would otherwise produce lopsided 1%-/99%-pages.
- Occupancy floor: any non-root node keeps `>= 2` entries; deletes
  underflowing below that borrow from a sibling first, then merge.
  The floor is deliberately tiny — it forces merges to happen constantly
  in tests on small trees, where count-based half-full policies would
  leave merge paths untested until millions of keys.
- A root that shrinks to one child collapses; the child takes its place.

**Property testing over unit tests:** a deterministic 5-line LCG drives
random insert/get/delete sequences against `std::collections::BTreeMap` as
the reference model, with `check_integrity` (full structural walk: ordering,
occupancy, leaf-chain-vs-tree-order agreement) after every 500 ops. Seeds
are printed in every assertion message — a failure is exactly reproducible
on any platform. No RNG dependency needed for that guarantee.

## Consequences

- Positive: one structure serves the catalog, primary storage
  (clustered-index style, ADR-0004) and secondary indexes; the leaf chain
  gives free ordered scans; the property suite makes structural bugs
  reproducible by seed.
- Negative: ~40k randomized operations per test run cost ~4 s; node I/O is
  read-modify-write through the pager with no buffer pool yet (phase 7);
  MIN_ENTRIES = 2 keeps trees slightly less dense than a half-full policy
  would (a benchmark-phase trade).
- Alternatives rejected: B-Tree without leaf chain (no cheap range scans);
  LSM-tree (write-optimized, wrong fit for the read-heavy SQL demo and
  much harder to make crash-safe in this scope); skipping merges entirely
  (unbounded space growth, and the interesting 50% of the algorithm
  disappears from the project).
