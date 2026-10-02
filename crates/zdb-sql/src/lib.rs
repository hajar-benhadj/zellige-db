//! ZelligeDB SQL front-end.
//!
//! [`SqlEngine`] is a *session* over a shared [`Database`]: clone it for
//! concurrent sessions (the server does), execute SQL through the MVCC
//! layer. Explicit `BEGIN`/`COMMIT`/`ROLLBACK` hold the engine's single
//! storage transaction; every other write statement is auto-committed.

#![deny(unsafe_code)]

pub mod exec;
pub mod lexer;
pub mod mvcc;
pub mod output;
pub mod parser;
pub mod types;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

/// One session's explicit-transaction context (see ADR-0006).
#[derive(Debug, Clone)]
pub(crate) struct TxnContext {
    pub id: u64,
}

/// A SQL session over the shared storage engine. Cheap to clone: each
/// clone is an independent session with its own transaction state.
#[derive(Clone)]
pub struct SqlEngine {
    db: Arc<Mutex<Database>>,
    path: PathBuf,
    txn: Option<TxnContext>,
}

impl SqlEngine {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        Ok(SqlEngine {
            db: Arc::new(Mutex::new(Database::create(&path)?)),
            path,
            txn: None,
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        Ok(SqlEngine {
            db: Arc::new(Mutex::new(Database::open(&path)?)),
            path,
            txn: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Execute one SQL statement. Outside an explicit transaction every
    /// statement is its own transaction (auto-commit on success, rollback
    /// on error) — exactly the guarantee the WAL makes cheap.
    pub fn execute(&mut self, sql: &str) -> Result<Output, SqlError> {
        let stmt = parser::parse(sql)?;

        match stmt {
            Statement::Begin => {
                if self.txn.is_some() {
                    return Err(SqlError::Parse("transaction already open".into()));
                }
                let id = match self.db.lock().unwrap().begin() {
                    Ok(id) => id,
                    // Another session holds the single storage transaction.
                    Err(DbError::NestedTransaction) => {
                        return Err(SqlError::Locked);
                    }
                    Err(e) => return Err(e.into()),
                };
                self.txn = Some(TxnContext { id });
                Ok(Output::Command {
                    tag: "BEGIN".into(),
                })
            }
            Statement::Commit => {
                self.txn
                    .take()
                    .ok_or_else(|| SqlError::Parse("no transaction is open".into()))?;
                match self.db.lock().unwrap().commit() {
                    Ok(()) => Ok(Output::Command {
                        tag: "COMMIT".into(),
                    }),
                    Err(e) => Err(e.into()),
                }
            }
            Statement::Rollback => {
                self.txn
                    .take()
                    .ok_or_else(|| SqlError::Parse("no transaction is open".into()))?;
                match self.db.lock().unwrap().rollback() {
                    Ok(()) => Ok(Output::Command {
                        tag: "ROLLBACK".into(),
                    }),
                    Err(e) => Err(e.into()),
                }
            }
            stmt => {
                let mut db = self.db.lock().unwrap();
                let txn_id = self.txn.as_ref().map(|t| t.id);
                let holding = self.txn.is_some();
                let is_write = matches!(
                    stmt,
                    Statement::CreateTable { .. }
                        | Statement::DropTable { .. }
                        | Statement::CreateIndex { .. }
                        | Statement::DropIndex { .. }
                        | Statement::Insert { .. }
                        | Statement::Update { .. }
                        | Statement::Delete { .. }
                );

                if !is_write || holding {
                    // Reads need no storage transaction; writes inside an
                    // explicit transaction run under the one already open.
                    return exec::run(&mut db, txn_id, stmt);
                }
                // Auto-commit write: one statement, one storage transaction.
                let auto_id = match db.begin() {
                    Ok(id) => id,
                    Err(DbError::NestedTransaction) => {
                        return Err(SqlError::Locked);
                    }
                    Err(e) => return Err(e.into()),
                };
                match exec::run(&mut db, Some(auto_id), stmt) {
                    Ok(output) => {
                        db.commit()?;
                        Ok(output)
                    }
                    Err(e) => {
                        db.rollback()?;
                        Err(e)
                    }
                }
            }
        }
    }

    /// Direct access to the underlying database (tools; single-threaded).
    pub fn db(&self) -> &Mutex<Database> {
        &self.db
    }
}
