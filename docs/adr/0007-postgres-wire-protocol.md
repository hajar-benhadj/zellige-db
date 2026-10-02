# ADR-0007: Postgres wire protocol (simple query mode)

- **Status:** Accepted
- **Date:** 2026-10-02

## Context

The engine speaks SQL internally; phase 6 makes it speak SQL *to the
world*. Requiring clients to use a bespoke protocol would bury the
project — compatibility with existing tooling is what turns "a database"
into "a database you can try in thirty seconds."

## Decision

**Speak the Postgres front/back protocol v3, simple query mode.**

- Handshake: `SSLRequest` declined with `N` (plain TCP for v0.1), then the
  startup packet; the server answers `AuthenticationOk` (trust — it's a
  single-user demo), `ParameterStatus` (server_version, client_encoding,
  DateStyle), `BackendKeyData`, `ReadyForQuery`.
- Query loop: `Q` carries a possibly multi-statement batch; each statement
  produces `T` (RowDescription with real type OIDs: int8/text/bool),
  `D` (DataRow, text format, `-1` for NULL), `C` (CommandComplete with
  psql tags: `SELECT n`, `INSERT 0 n`, `UPDATE n`, `DELETE n`,
  `CREATE TABLE`, `BEGIN`, `COMMIT`…), and errors produce `E` with real
  Postgres error codes (`42P01` undefined_table, `42601` syntax,
  `55P03` lock_not_available…). `Z` closes every `Q`, status byte
  reflecting the session's transaction state (`I`/`T`).
- `X` (Terminate) closes cleanly; `S` (Sync) is answered with `Z`;
  unknown message types get a documented `E` explaining that extended
  query mode (`P`/`B`/`D`/`E`) is not implemented in v0.1.

**SQL surface needed one change:** `parse_all` — the wire delivers
statement *batches* (`stmt1; stmt2;`), so the parser gained multi-statement
parsing and `SqlEngine::execute_batch` reports outputs as they occur
(streaming semantics, not buffered).

**Error taxonomy:** `SqlError` variants map onto Postgres SQLSTATE codes,
so client tooling sees familiar codes.

**Testing without new dependencies:** the integration suite contains a
~150-line hand-written wire client that connects over a real TCP socket,
walks the full handshake, exchanges queries, and asserts message-level
details (SSL decline byte, ReadyForQuery transaction status, error fields,
psql tags, multi-session locking). No client crate needed; the protocol is
tested at the byte level, which is the level it actually lives at.

## Consequences

- Positive: `psql -h 127.0.0.1` works today; the demo is instantly
  reproducible by anyone with Postgres's client installed; error codes are
  real, not cosmetic.
- Negative: no TLS, no authentication beyond trust, no extended query
  protocol — GUI clients that insist on `Parse`/`Bind` (some do) will fail
  with a clear error; text-format rows only.
- Alternatives rejected: a custom protocol (kills the point); supporting
  extended mode now (three more message types with parameter binding —
  valuable, deferred to v0.2 with a real use); gRPC/HTTP (not what the
  ecosystem already knows how to speak).
