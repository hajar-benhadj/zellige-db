# ADR-0001: Rust as the implementation language

- **Status:** Accepted
- **Date:** 2026-10-02

## Context

ZelligeDB needs a systems language: manual control over byte layouts,
 predictable performance, and easy compilation to WebAssembly (for the phase-8
browser playground) and to native binaries (for the phase-6 wire-protocol
server).

Candidates: **Rust**, **Go**, **C++**.

## Decision

Use **Rust** (stable toolchain, edition 2024).

## Consequences

- Positive: memory safety without a garbage collector; modern databases
  (TiKV, Materialize, Neon, SurrealDB, redb, sled) validate the ecosystem;
  first-class WASM target; `#![deny(unsafe_code)]` is enforceable.
- Negative: steeper learning curve (ownership, lifetimes). Mitigated by
  phase-by-phase [learning notes](../learning-notes/) and by favoring simple,
  owned data structures early (`&mut self` APIs, no clever borrowing).
- Alternatives rejected: Go (faster to write, but weaker statement for
  systems work and less mature WASM story); C++ (safety burden too high for
  a solo project).
