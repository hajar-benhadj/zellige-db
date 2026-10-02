# ADR-0005: Hand-written SQL front-end and the differential test strategy

- **Status:** Accepted
- **Date:** 2026-10-02

## Context

Phase 4 needs SQL over the storage engine: a lexer, parser, planner, and
executor, plus a way to *trust* the whole stack. Two design questions:

1. hand-write the SQL front-end or pull `sqlparser`?
2. how do we prove the engine answers correctly, not just consistently?

The project's purpose is demonstrating deep capability — and correctness
claims need an independent referee.

## Decision

**Hand-write everything** (lexer → recursive-descent parser → AST →
naive planner → Volcano-style executor). The SQL surface for v0.1:

- `CREATE TABLE` (INTEGER, TEXT, BOOLEAN), `DROP TABLE`, `SHOW TABLES`
- `INSERT … VALUES (…), (…)`, `SELECT` with `WHERE` / multi-column
  `ORDER BY` / `LIMIT` / `COUNT(*)`
- `UPDATE … SET`, `DELETE`, `BEGIN` / `COMMIT` / `ROLLBACK`
- Expressions: comparisons, `AND`/`OR`/`NOT`, arithmetic, `LIKE` (`%`, `_`),
  `IS [NOT] NULL`

**NULL semantics** match SQLite where cheap: comparisons against NULL are
false (no three-valued logic; `IS NULL` is the tool), NULLs order first
ascending, and aggregate-free projection. Arithmetic is strict i64.

**Tables are clustered B+trees**: each table is a tree mapping an 8-byte
big-endian row id (insertion order) to an encoded row (NULL bitmap +
typed payload). Schemas live in a `zdb:tables` tree — itself just another
B+Tree in the catalog, so DDL inherits crash safety for free (ADR-0004).

**Differential testing against SQLite** is the referee: identical random
workloads (300 rows, 150 randomized `SELECT`s with predicates/LIKE/ORDER
BY/LIMIT per seed) run through both engines, and rows must match exactly,
order included (`id` is the deterministic tiebreaker). The suite links
`rusqlite` (bundled C) behind the `diff-sqlite` feature and runs in CI on
Linux only — the Windows GNU toolchain cannot compile the C dependency
without extra tooling, and CI coverage is what matters for a correctness
claim.

## Consequences

- Positive: the parser/executor are portfolio-visible code, not a wrapper
  around a library; every layer is unit-testable in isolation; the
  differential suite converts "it seems to work" into "SQLite and I agree
  on 600 randomized queries"; auto-commit-per-statement rides on ADR-0004's
  commit semantics.
- Negative: no cost-based planning yet (every query scans — phase 5/7
  territory); no JOIN/GROUP BY yet (v0.2 roadmap); the `diff-sqlite`
  feature adds a C toolchain requirement to the dev environment that runs
  it (CI Linux only).
- Alternatives rejected: `sqlparser` crate (hides the interesting 80%);
  differential testing against a home-grown reference interpreter (the
  referee must not share the suspect's bugs); keeping SQLite comparison
  out entirely (correctness by vibes — rejected on principle).
