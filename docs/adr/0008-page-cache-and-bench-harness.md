# ADR-0008: Page cache, and a benchmark harness without dependencies

- **Status:** Accepted
- **Date:** 2026-10-02

## Context

Every page access costs two syscalls (`seek` + `read`/`write`). The
benchmark phase needed (a) reproducible numbers and (b) at least one
documented, measured optimization.

The Windows GNU toolchain cannot link some common transitive crates
(raw-dylib imports need a complete MinGW binutils, which is not installed),
so `criterion` was a build risk for a benchmark suite.

## Decision

**A 40-line per-process page cache inside the pager** (`HashMap<PageId,
Page>`, ceiling 8192 pages / 32 MiB, arbitrary eviction — the cache is a
pure accelerator, so policy is deliberately boring). Reads hit the cache
first; writes update it as they reach the file.

**A hand-rolled harness** (`[[bench]] harness = false` — plain programs)
reporting ops, wall time, ops/s and µs/op. Deterministic, portable across
the three CI operating systems, zero dependencies. Criterion remains the
choice if a statistical-rigor upgrade is ever needed; it can slot in
behind the same bench targets.

## Consequences

- Positive: measured 25–35% improvement on read-heavy paths (see
  [benchmarks.md](../benchmarks.md)); the full-scan-vs-index and
  auto-commit-vs-batched comparisons give the planner and the WAL real
  numbers instead of adjectives.
- Negative / the war story: the first cache implementation invalidated
  nothing on recovery's raw writes — a repaired meta page read back stale
  and recovery failed with `PageOutOfBounds`. The randomized crash suite
  caught it immediately. `write_page_raw` now updates the cache, and the
  test that caught it stays in the suite guarding the invariant.
- Alternatives rejected: criterion now (build risk + stats we do not need
  yet); LRU eviction (arbitrary is cheaper and equally correct); caching at
  the `Database` layer (the pager owns page identity, so the cache belongs
  there).
