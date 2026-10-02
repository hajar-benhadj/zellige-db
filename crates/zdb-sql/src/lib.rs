//! ZelligeDB SQL front-end (phase 4).
//!
//! Hand-written on purpose — the parser is the project, not a dependency:
//!
//! - lexer   — turns SQL text into tokens
//! - parser  — recursive descent, tokens into an AST
//! - planner — picks scans/index scans, arranges operators
//! - executor — Volcano-style iterator tree
//!
//! Nothing in this crate uses `unsafe`.

#![deny(unsafe_code)]
