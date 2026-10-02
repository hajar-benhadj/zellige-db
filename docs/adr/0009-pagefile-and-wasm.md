# ADR-0009: The PageFile abstraction and the in-memory database

- **Status:** Accepted
- **Date:** 2026-10-02

## Context

Phase 8 compiles the engine to WebAssembly so the playground can run in a
browser tab. WebAssembly has no filesystem: the pager and the WAL — both
built directly on `std::fs::File` — cannot exist there as written. The
playground also wants *session-local* storage anyway (each page load is a
fresh database), so durable files are not even the requirement.

## Decision

**One tiny trait under both the pager and the WAL:**

```rust
pub(crate) trait PageFile: Send {
    fn read_exact_at(&mut self, buf: &mut [u8], offset: u64) -> io::Result<()>;
    fn write_all_at(&mut self, buf: &[u8], offset: u64) -> io::Result<()>;
    fn set_len(&mut self, len: u64) -> io::Result<()>;
    fn sync(&mut self) -> io::Result<()>;
}
```

Two implementations: `OsFile` (seek + read/write over `std::fs::File`,
`sync_all` for durability) and `MemoryFile` (a `Vec<u8>` whose holes read
as zeros, exactly like a sparse file; `sync` is a no-op — memory *is* the
durable medium). `Pager::create_memory()` and `Database::create_memory()`
compose them, and `SqlEngine::create_memory()` exposes the whole SQL
engine to `zdb-wasm`'s wasm-bindgen bindings. The bindings module is
`#[cfg(target_arch = "wasm32")]`, so host builds compile the crate as an
empty lib and `cargo test --workspace` stays green on all three CI
operating systems.

The WAL's buffered writer became direct positioned writes: frames are
appended at `bytes_written` and fsync remains the durability line.

## Consequences

- Positive: one codebase runs on SSDs and inside a browser tab — the
  playground executes the same parser, planner, B+Tree, and WAL code paths
  the server does; the diff was mechanical (no algorithm changed).
- Negative: `Box<dyn PageFile>` adds one virtual call per page access
  (noise next to the syscalls it replaces); the memory file has no size
  ceiling (a playground session can exhaust tab memory — acceptable for a
  demo; OPFS persistence is the natural v0.2 extension).
- The verified end-to-end path: wasm build → wasm-bindgen → static
  `index.html` → real browser executes `CREATE TABLE` / `INSERT` /
  `SELECT` and renders the ASCII tables — all without a server.
