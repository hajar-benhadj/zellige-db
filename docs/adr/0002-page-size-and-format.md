# ADR-0002: Page size, header layout, and integrity model

- **Status:** Accepted
- **Date:** 2026-10-02

## Context

The pager turns one file into an array of pages. We must fix, up front:

1. the page size,
2. what lives in the page header,
3. how the file detects corruption (torn writes, flipped bits, bad sectors),
4. how freed space is reclaimed.

The engine is a single-file, single-writer system at this phase; the WAL
(phase 3) will later sit on top and must find a slot for per-page LSNs.

## Decision

**Page size: 4096 bytes.** It matches the common filesystem block size and
the default of battle-tested engines (SQLite ships 4096 by default). 8–16 KiB
pages would reduce per-page header overhead and seek count, at the cost of
more write amplification for small records — a trade to revisit with real
benchmark data in phase 7, not before.

**Header: 16 bytes, little-endian.**

| offset | size | field |
|--------|------|-------|
| 0 | 4 | page type (u32 enum) |
| 4 | 4 | page id — redundant self-description; survives out-of-band inspection of a raw file |
| 8 | 4 | next-free pointer (meaningful only for `Free` pages) |
| 12 | 4 | reserved — will hold the WAL LSN from phase 3 |

**Integrity: trailing CRC32 (crc32fast) covering bytes 0..4092.** Placement
at the tail means the checksum covers the *entire* header and payload;
decoding verifies before any struct is built, and unknown page types are a
hard error, never a guess. Cost: 4 bytes per page (0.1%) and one hashing pass
per read/write — cheap insurance.

**Free space: an in-file singly linked list.** `Free` pages carry
`next_free`; the meta page (page 0) stores the head and count. The file is
fully self-describing with zero out-of-file state. Reuse is LIFO, which
keeps recycled pages cache-warm.

**Endianness: little-endian everywhere**, matching x86/ARM and every wire
format we are likely to speak later.

## Consequences

- Positive: corruption is always *detected at read time*; the format needs
  no migration path for the LSN slot; the file can be inspected with a hex
  editor during development.
- Negative: 0.1% storage overhead and a checksum pass on every page I/O
  (accepted, see above); 16-byte header on every 4 KiB (0.4%, accepted).
- Double free is rejected by re-reading the page type before relinking —
  one extra read per free, which buys a hard error instead of a silent
  free-list cycle.
- Known limitation, deferred to phase 3: between `write_page` and `sync`,
  a crash may leave the free list or page count stale. The WAL and its
  recovery will own atomicity from phase 3; until then the durability
  contract is "stable after `sync()`".
- Alternatives rejected: header checksums only (miss payload corruption);
  block-level checksumming via filesystems (not portable, not educational);
  zlib/adler32 (weaker error detection than CRC32 for bit flips).
