//! Planner + Volcano-style execution over the storage engine.
//!
//! The plan is deliberately simple for v1: every scan is a full B+Tree
//! scan of the table's row tree, wrapped in `Filter → Project → Sort →
//! Limit` iterators (materialized where the operator needs randomness
//! of access). Phase 5 adds index-scan planning on top.

use zdb_core::BTree;

use crate::parser::{BinOp, Expr, Select, SelectItem, Statement};
use crate::types::{DataType, Schema, SqlError, Value, decode_row, encode_row};

pub const SCHEMA_TREE: &str = "zdb:tables";

fn data_tree_name(table: &str) -> String {
    format!("zdb:t:{table}")
}

fn row_key(row_id: u64) -> Vec<u8> {
    row_id.to_be_bytes().to_vec()
}

impl crate::SqlEngine {
    pub(crate) fn run(&mut self, stmt: Statement) -> Result<crate::Output, SqlError> {
        match stmt {
            Statement::CreateTable { name, columns } => self.create_table(&name, columns),
            Statement::DropTable { name } => self.drop_table(&name),
            Statement::Insert { table, rows } => self.insert(&table, rows),
            Statement::Select(select) => self.select(&select),
            Statement::Update {
                table,
                assignments,
                filter,
            } => self.update(&table, assignments, filter),
            Statement::Delete { table, filter } => self.delete(&table, filter),
            Statement::Begin | Statement::Commit | Statement::Rollback => {
                unreachable!("transaction statements are handled by execute()")
            }
            Statement::ShowTables => self.show_tables(),
        }
    }

    fn schema_tree(&mut self) -> Result<BTree, SqlError> {
        Ok(self.db.open_tree(SCHEMA_TREE)?)
    }

    fn load_schema(&mut self, table: &str) -> Result<Schema, SqlError> {
        let tree = self.schema_tree()?;
        match tree.get(&mut self.db, table.as_bytes())? {
            Some(bytes) => Ok(Schema::decode(&bytes)?),
            None => Err(SqlError::UnknownTable(table.to_string())),
        }
    }

    fn save_schema(&mut self, table: &str, schema: &Schema) -> Result<(), SqlError> {
        let mut tree = self.schema_tree()?;
        tree.insert(&mut self.db, table.as_bytes(), &schema.encode())?;
        self.db.save_tree(SCHEMA_TREE, &tree)?;
        Ok(())
    }

    fn create_table(
        &mut self,
        name: &str,
        columns: Vec<(String, DataType)>,
    ) -> Result<crate::Output, SqlError> {
        if self.load_schema(name).is_ok() {
            return Err(SqlError::DuplicateTable(name.to_string()));
        }
        let schema = Schema {
            columns,
            next_row_id: 1,
        };
        self.save_schema(name, &schema)?;
        Ok(crate::Output::Command {
            tag: "CREATE TABLE".into(),
        })
    }

    fn drop_table(&mut self, name: &str) -> Result<crate::Output, SqlError> {
        self.load_schema(name)?; // errors when unknown
        let mut tree = self.schema_tree()?;
        tree.delete(&mut self.db, name.as_bytes())?;
        self.db.save_tree(SCHEMA_TREE, &tree)?;
        // The row tree's pages stay in the file (space reclaim is a later
        // phase); it is unlinked from the catalog so it becomes invisible.
        let empty = BTree::empty();
        self.db.save_tree(&data_tree_name(name), &empty)?;
        Ok(crate::Output::Command {
            tag: "DROP TABLE".into(),
        })
    }

    fn insert(&mut self, table: &str, rows: Vec<Vec<Expr>>) -> Result<crate::Output, SqlError> {
        let schema = self.load_schema(table)?;
        let mut data = self.db.open_tree(&data_tree_name(table))?;
        let mut schema = schema;
        let mut count = 0u64;
        for row_exprs in rows {
            let values = row_exprs
                .iter()
                .map(eval_const)
                .collect::<Result<Vec<_>, SqlError>>()?;
            let encoded = encode_row(&schema, &values)?;
            let row_id = schema.next_row_id;
            schema.next_row_id += 1;
            data.insert(&mut self.db, &row_key(row_id), &encoded)?;
            count += 1;
        }
        self.save_schema(table, &schema)?;
        self.db.save_tree(&data_tree_name(table), &data)?;
        Ok(crate::Output::Command {
            tag: format!("INSERT 0 {count}"),
        })
    }

    fn select(&mut self, select: &Select) -> Result<crate::Output, SqlError> {
        let schema = self.load_schema(&select.from)?;
        let data = self.db.open_tree(&data_tree_name(&select.from))?;

        // Scan → filter.
        let mut matching: Vec<Vec<Value>> = Vec::new();
        let scan = data.scan(&mut self.db)?;
        for (_, row_bytes) in scan {
            let row = decode_row(&schema, &row_bytes)?;
            if let Some(filter) = &select.filter
                && eval(filter, &schema, &row)? != Value::Bool(true)
            {
                continue;
            }
            matching.push(row);
        }

        if matches!(select.items.first(), Some(SelectItem::CountStar)) {
            return Ok(crate::Output::Query {
                columns: vec!["count".into()],
                rows: vec![vec![Value::Int(matching.len() as i64)]],
            });
        }

        // Sort on the *full* rows (ORDER BY may reference columns that are
        // not projected), then project. SQLite orders NULLs first ascending;
        // we match that.
        let order_indices: Vec<(usize, bool)> = select
            .order_by
            .iter()
            .map(|o| {
                let idx = schema
                    .column_index(&o.column)
                    .ok_or_else(|| SqlError::UnknownColumn(o.column.clone()))?;
                Ok((idx, o.descending))
            })
            .collect::<Result<_, SqlError>>()?;
        matching.sort_by(|a, b| {
            for &(idx, descending) in &order_indices {
                let ord = cmp_for_order(&a[idx], &b[idx], descending);
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        });

        // Project.
        let indices: Vec<usize> = match &select.items[..] {
            [SelectItem::Star] => (0..schema.columns.len()).collect(),
            items => items
                .iter()
                .map(|item| match item {
                    SelectItem::Column(name) => schema
                        .column_index(name)
                        .ok_or_else(|| SqlError::UnknownColumn(name.clone())),
                    _ => unreachable!("COUNT(*) handled above"),
                })
                .collect::<Result<_, SqlError>>()?,
        };
        let columns = indices
            .iter()
            .map(|&i| schema.columns[i].0.clone())
            .collect();
        let mut rows: Vec<Vec<Value>> = matching
            .into_iter()
            .map(|row| indices.iter().map(|&i| row[i].clone()).collect())
            .collect();

        if let Some(limit) = select.limit {
            rows.truncate(limit as usize);
        }
        Ok(crate::Output::Query { columns, rows })
    }

    fn update(
        &mut self,
        table: &str,
        assignments: Vec<(String, Expr)>,
        filter: Option<Expr>,
    ) -> Result<crate::Output, SqlError> {
        let schema = self.load_schema(table)?;
        let mut data = self.db.open_tree(&data_tree_name(table))?;
        let mut updates: Vec<(u64, Vec<u8>)> = Vec::new();
        let scan = data.scan(&mut self.db)?;
        for (key, row_bytes) in scan {
            let row = decode_row(&schema, &row_bytes)?;
            let keep = match &filter {
                Some(f) => eval(f, &schema, &row)? == Value::Bool(true),
                None => true,
            };
            if !keep {
                continue;
            }
            let mut new_row = row;
            for (column, expr) in &assignments {
                let idx = schema
                    .column_index(column)
                    .ok_or_else(|| SqlError::UnknownColumn(column.clone()))?;
                new_row[idx] = eval(expr, &schema, &new_row)?;
            }
            let encoded = encode_row(&schema, &new_row)?;
            let row_id = u64::from_be_bytes(key[..8].try_into().unwrap());
            updates.push((row_id, encoded));
        }
        let count = updates.len() as u64;
        for (row_id, encoded) in updates {
            data.insert(&mut self.db, &row_key(row_id), &encoded)?;
        }
        Ok(crate::Output::Command {
            tag: format!("UPDATE {count}"),
        })
    }

    fn delete(&mut self, table: &str, filter: Option<Expr>) -> Result<crate::Output, SqlError> {
        let schema = self.load_schema(table)?;
        let mut data = self.db.open_tree(&data_tree_name(table))?;
        let mut doomed: Vec<u64> = Vec::new();
        let scan = data.scan(&mut self.db)?;
        for (key, row_bytes) in scan {
            let row = decode_row(&schema, &row_bytes)?;
            let keep = match &filter {
                Some(f) => eval(f, &schema, &row)? == Value::Bool(true),
                None => true,
            };
            if keep {
                doomed.push(u64::from_be_bytes(key[..8].try_into().unwrap()));
            }
        }
        let count = doomed.len() as u64;
        for row_id in doomed {
            data.delete(&mut self.db, &row_key(row_id))?;
        }
        Ok(crate::Output::Command {
            tag: format!("DELETE {count}"),
        })
    }

    fn show_tables(&mut self) -> Result<crate::Output, SqlError> {
        let tree = self.schema_tree()?;
        let mut names: Vec<String> = tree
            .scan(&mut self.db)?
            .map(|(k, _)| String::from_utf8_lossy(&k).into_owned())
            .collect();
        names.sort();
        Ok(crate::Output::Query {
            columns: vec!["tables".into()],
            rows: names.into_iter().map(|n| vec![Value::Text(n)]).collect(),
        })
    }
}

/// Constant-fold an expression that must not reference columns (INSERT values).
fn eval_const(expr: &Expr) -> Result<Value, SqlError> {
    match expr {
        Expr::Column(name) => Err(SqlError::Parse(format!(
            "column reference '{name}' is not allowed here; INSERT takes literal values"
        ))),
        other => {
            // Literals and constant arithmetic: evaluate against an empty row.
            eval(
                other,
                &Schema {
                    columns: vec![],
                    next_row_id: 0,
                },
                &[],
            )
        }
    }
}

/// Evaluate an expression against a row. Deviation from full SQL, documented:
/// comparisons with NULL yield false (use IS NULL), and WHERE keeps only
/// boolean true.
pub fn eval(expr: &Expr, schema: &Schema, row: &[Value]) -> Result<Value, SqlError> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Column(name) => {
            let idx = schema
                .column_index(name)
                .ok_or_else(|| SqlError::UnknownColumn(name.clone()))?;
            Ok(row[idx].clone())
        }
        Expr::Not(inner) => Ok(Value::Bool(!as_bool(&eval(inner, schema, row)?)?)),
        Expr::Neg(inner) => match eval(inner, schema, row)? {
            Value::Int(n) => Ok(Value::Int(-n)),
            Value::Null => Ok(Value::Null),
            v => Err(SqlError::TypeMismatch {
                column_type: "INTEGER".into(),
                value_type: v.type_name().into(),
            }),
        },
        Expr::Like(inner, pattern) => match eval(inner, schema, row)? {
            Value::Text(s) => Ok(Value::Bool(like_match(&s, pattern))),
            Value::Null => Ok(Value::Bool(false)),
            v => Err(SqlError::TypeMismatch {
                column_type: "TEXT".into(),
                value_type: v.type_name().into(),
            }),
        },
        Expr::IsNull(inner, negated) => {
            let is_null = eval(inner, schema, row)? == Value::Null;
            Ok(Value::Bool(is_null != *negated))
        }
        Expr::Binary(op, l, r) => {
            let lv = eval(l, schema, row)?;
            let rv = eval(r, schema, row)?;
            eval_binary(*op, lv, rv)
        }
    }
}

fn eval_binary(op: BinOp, lv: Value, rv: Value) -> Result<Value, SqlError> {
    use Value::{Bool, Int, Null, Text};
    match op {
        BinOp::And => Ok(Bool(as_bool(&lv)? && as_bool(&rv)?)),
        BinOp::Or => Ok(Bool(as_bool(&lv)? || as_bool(&rv)?)),
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => match (lv, rv) {
            (Int(a), Int(b)) => {
                let v = match op {
                    BinOp::Add => a.checked_add(b),
                    BinOp::Sub => a.checked_sub(b),
                    BinOp::Mul => a.checked_mul(b),
                    BinOp::Div if b != 0 => a.checked_div(b),
                    BinOp::Mod if b != 0 => a.checked_rem(b),
                    _ => return Err(SqlError::Parse("division by zero".into())),
                }
                .ok_or_else(|| SqlError::Parse("integer overflow".into()))?;
                Ok(Int(v))
            }
            (Null, _) | (_, Null) => Ok(Null),
            (a, b) => Err(SqlError::TypeMismatch {
                column_type: "INTEGER".into(),
                value_type: format!("{}, {}", a.type_name(), b.type_name()),
            }),
        },
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            match (&lv, &rv) {
                (Null, _) | (_, Null) => Ok(Bool(false)), // see module docs
                (Int(a), Int(b)) => Ok(Bool(compare(op, a.cmp(b)))),
                (Text(a), Text(b)) => Ok(Bool(compare(op, a.cmp(b)))),
                (Bool(a), Bool(b)) => Ok(Bool(compare(op, a.cmp(b)))),
                (a, b) => Err(SqlError::TypeMismatch {
                    column_type: a.type_name().into(),
                    value_type: b.type_name().into(),
                }),
            }
        }
    }
}

fn compare(op: BinOp, ord: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match op {
        BinOp::Eq => ord == Equal,
        BinOp::Ne => ord != Equal,
        BinOp::Lt => ord == Less,
        BinOp::Le => ord != Greater,
        BinOp::Gt => ord == Greater,
        BinOp::Ge => ord != Less,
        _ => unreachable!("non-comparison op"),
    }
}

fn as_bool(v: &Value) -> Result<bool, SqlError> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Null => Ok(false),
        other => Err(SqlError::TypeMismatch {
            column_type: "BOOLEAN".into(),
            value_type: other.type_name().into(),
        }),
    }
}

/// NULLs order first ascending (SQLite behavior); DESC reverses.
fn cmp_for_order(a: &Value, b: &Value, descending: bool) -> std::cmp::Ordering {
    let ord = match (a, b) {
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Text(x), Value::Text(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        // heterogeneous values never occur in a typed column
        _ => std::cmp::Ordering::Equal,
    };
    if descending { ord.reverse() } else { ord }
}

/// SQL LIKE with `%` (any run) and `_` (any one char), no escaping.
pub fn like_match(s: &str, pattern: &str) -> bool {
    let sc: Vec<char> = s.chars().collect();
    let pc: Vec<char> = pattern.chars().collect();
    like_rec(&sc, &pc)
}

fn like_rec(s: &[char], p: &[char]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some('%'), _) => {
            // try consuming zero or more input chars
            (0..=s.len()).any(|skip| like_rec(&s[skip..], &p[1..]))
        }
        (Some('_'), Some(_)) => like_rec(&s[1..], &p[1..]),
        (Some(&c), Some(&sc)) if c == sc => like_rec(&s[1..], &p[1..]),
        _ => false,
    }
}
