//! ZelligeDB SQL front-end.
//!
//! [`SqlEngine`] ties everything together: parse → plan → execute over a
//! [`Database`](zdb_core::Database), with statement-scoped auto-transactions
//! and explicit `BEGIN`/`COMMIT`/`ROLLBACK`.

#![deny(unsafe_code)]

pub mod exec;
pub mod lexer;
pub mod output;
pub mod parser;
pub mod types;

use std::path::{Path, PathBuf};

use zdb_core::{Database, DbError};

use crate::parser::Statement;
use crate::types::{SqlError, Value};

/// Result of executing one statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// Rows for `SELECT` / `SHOW TABLES`.
    Query {
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
    /// A psql-style command tag: `CREATE TABLE`, `INSERT 0 3`, `UPDATE 12`…
    Command { tag: String },
}

pub struct SqlEngine {
    db: Database,
    in_txn: bool,
    path: PathBuf,
}

impl SqlEngine {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        Ok(SqlEngine {
            db: Database::create(&path)?,
            in_txn: false,
            path,
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        Ok(SqlEngine {
            db: Database::open(&path)?,
            in_txn: false,
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Execute one SQL statement. Outside an explicit transaction every
    /// statement is its own transaction (auto-commit on success, rollback
    /// on error) — exactly the guarantee the WAL makes cheap.
    pub fn execute(&mut self, sql: &str) -> Result<Output, SqlError> {
        match parser::parse(sql)? {
            Statement::Begin => {
                if self.in_txn {
                    return Err(SqlError::Parse("transaction already open".into()));
                }
                self.db.begin()?;
                self.in_txn = true;
                Ok(Output::Command {
                    tag: "BEGIN".into(),
                })
            }
            Statement::Commit => {
                if !self.in_txn {
                    return Err(SqlError::Parse("no transaction is open".into()));
                }
                self.db.commit()?;
                self.in_txn = false;
                Ok(Output::Command {
                    tag: "COMMIT".into(),
                })
            }
            Statement::Rollback => {
                if !self.in_txn {
                    return Err(SqlError::Parse("no transaction is open".into()));
                }
                self.db.rollback()?;
                self.in_txn = false;
                Ok(Output::Command {
                    tag: "ROLLBACK".into(),
                })
            }
            stmt => {
                let auto_commit = !self.in_txn;
                if auto_commit {
                    self.db.begin()?;
                }
                let result = self.run(stmt);
                if auto_commit {
                    match &result {
                        Ok(_) => self.db.commit()?,
                        Err(_) => self.db.rollback()?,
                    }
                }
                result
            }
        }
    }

    /// Direct access to the underlying database (tools, tests).
    pub fn db(&mut self) -> &mut Database {
        &mut self.db
    }
}
