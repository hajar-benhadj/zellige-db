# ZelligeDB Architecture

This document is the living map of the system. Decisions *why* each piece
looks the way it does live in [adr/](adr/) — one record per decision.

## Layer cake

```
            ┌─────────────────────────────────────────────┐
   client ──►  zdb-server                                 │
            │  REPL (phase 4) · TCP + pg wire (phase 6)   │
            ├─────────────────────────────────────────────┤
            │  zdb-sql                                    │
            │  lexer → parser → AST → planner → executor  │
            ├─────────────────────────────────────────────┤
            │  zdb-core                                   │
            │  WAL (phase 3) ─ B+Tree (phase 2) ──┐       │
            │  MVCC version store (phase 5)       │       │
            ├─────────────────────────────────────┼───────┤
            │  Pager (phase 1)                    │       │
            └─────────────────────────────────────┼───────┘
                                                  ▼
                         data.zdb (4 KiB pages) + data.wal
```

## The layers

### Pager (phase 1)

The only layer allowed to touch the file. It divides one file into fixed
4 KiB **pages**. Every page carries a small header (type, page id, reserved
LSN slot) and a trailing CRC32 checksum, so any corruption — torn write,
bad sector, editor mishap — is *detected* at read time, never silently
propagated. Freed pages go on a free list inside the file itself, so the
file format is self-describing.

### B+Tree (phase 2)

Ordered key → page-id mapping built directly on the pager. Interior nodes
route, leaf nodes hold entries, splits/merges keep the tree balanced.
Correctness is pinned by property-based tests against `std::collections::BTreeMap`
as the reference model, plus a `zdb debug tree` dumper that later feeds the
browser visualizer.

### WAL (phase 3)

Before any data page is mutated, a redo record describing the change is
appended to the write-ahead log. Recovery = replay committed records.
The durability rule: a transaction is "committed" only once its WAL record
is fsynced. Data pages may be stale or torn — recovery doesn't care.

### SQL front-end (phase 4)

Hand-written lexer and recursive-descent parser (no parser libraries — the
parser *is* the exercise), a naive planner that will grow cost-based choices,
and a Volcano-style executor tree (`SeqScan → Filter → Project → Limit`).

### MVCC (phase 5)

Every tuple version carries `xmin`/`xmax`; readers take a snapshot and never
block writers, writers never block readers. Snapshot isolation is proven by
tests that reproduce dirty-read / non-repeatable-read / phantom anomalies
and demonstrate their absence.

### Server (phase 6)

Speaks a useful subset of the Postgres wire protocol (protocol v3, simple
query mode) so stock `psql` — and anything built on it — connects without
knowing it isn't PostgreSQL.

## Design principles

1. **Correctness before performance.** Crash tests and differential tests
   come before benchmarks. Slow-and-always-right beats fast-and-sometimes-wrong.
2. **Checksum everything that touches disk.** Corruption must be detected,
   loudly, at the earliest possible layer.
3. **No `unsafe`.** The whole engine compiles under `#![deny(unsafe_code)]`.
4. **Minimal dependencies.** Hand-write the parts that teach (parser, B+Tree,
   WAL); depend on crates only for leaf utilities (CRC32, error derive).
5. **Every decision gets an ADR.** Future-you should be able to reconstruct
   *why*, not just *what*.
6. **Every phase ships something visible** — a CLI, a number, a demo — not
   silent code.

## Durability contract (v1, pre-WAL)

Until phase 3 the contract is intentionally modest: `Pager::sync()` fsyncs
the file; anything before the last `sync()` may or may not survive a crash —
and that gap is exactly what the WAL exists to close.
