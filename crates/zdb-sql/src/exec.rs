//! Planner + execution over the storage engine, MVCC-aware.
//!
//! Reads filter row versions through the visibility rule (ADR-0006);
//! writes stamp versions and maintain secondary indexes. The planner uses
//! an index scan when the WHERE clause contains a top-level comparison on
//! an indexed column and falls back to a full scan otherwise.

use zdb_core::{BTree, Database, DbError};

use crate::mvcc::{
    encode_index_key, prefix_upper_bound, split_version, stamp_row, tombstone, visible,
};
use crate::parser::{BinOp, Expr, Select, SelectItem, Statement};
use crate::types::{DataType, IndexDef, Schema, SqlError, Value, decode_row, encode_row};

pub const SCHEMA_TREE: &str = "zdb:tables";

fn data_tree_name(table: &str) -> String {
    format!("zdb:t:{table}")
}

fn index_tree_name(index: &str) -> String {
    format!("zdb:i:{index}")
}

fn row_key(row_id: u64) -> Vec<u8> {
    row_id.to_be_bytes().to_vec()
}

pub(crate) fn run(
    db: &mut Database,
    txn_id: Option<u64>,
    stmt: Statement,
) -> Result<crate::Output, SqlError> {
    match stmt {
        Statement::CreateTable { name, columns } => create_table(db, &name, columns),
        Statement::DropTable { name } => drop_table(db, &name),
        Statement::CreateIndex {
            name,
            table,
            column,
        } => create_index(db, &name, &table, &column),
        Statement::DropIndex { name } => drop_index(db, &name),
        Statement::Insert { table, rows } => insert(db, txn_id, &table, rows),
        Statement::Select(select) => select_stmt(db, txn_id, &select),
        Statement::Update {
            table,
            assignments,
            filter,
        } => update(db, txn_id, &table, assignments, filter),
        Statement::Delete { table, filter } => delete(db, txn_id, &table, filter),
        Statement::Begin | Statement::Commit | Statement::Rollback => {
            unreachable!("transaction statements are handled by SqlEngine::execute")
        }
        Statement::ShowTables => show_tables(db),
    }
}

// ---------------------------------------------------------------------------
// Catalog helpers
// ---------------------------------------------------------------------------

fn schema_tree(db: &mut Database) -> Result<BTree, SqlError> {
    Ok(db.open_tree(SCHEMA_TREE)?)
}

fn save_schema_tree(db: &mut Database, tree: &BTree) -> Result<(), SqlError> {
    db.save_tree(SCHEMA_TREE, tree)?;
    Ok(())
}

fn load_schema(db: &mut Database, table: &str) -> Result<Schema, SqlError> {
    let tree = schema_tree(db)?;
    match tree.get(db, table.as_bytes())? {
        Some(bytes) => Ok(Schema::decode(&bytes)?),
        None => Err(SqlError::UnknownTable(table.to_string())),
    }
}

fn save_schema(db: &mut Database, table: &str, schema: &Schema) -> Result<(), SqlError> {
    let mut tree = schema_tree(db)?;
    tree.insert(db, table.as_bytes(), &schema.encode())?;
    save_schema_tree(db, &tree)
}

fn load_data_tree(db: &mut Database, table: &str) -> Result<BTree, SqlError> {
    Ok(db.open_tree(&data_tree_name(table))?)
}

// ---------------------------------------------------------------------------
// DDL
// ---------------------------------------------------------------------------

fn create_table(
    db: &mut Database,
    name: &str,
    columns: Vec<(String, DataType)>,
) -> Result<crate::Output, SqlError> {
    if load_schema(db, name).is_ok() {
        return Err(SqlError::DuplicateTable(name.to_string()));
    }
    let schema = Schema {
        columns,
        next_row_id: 1,
        indexes: Vec::new(),
    };
    save_schema(db, name, &schema)?;
    Ok(crate::Output::Command {
        tag: "CREATE TABLE".into(),
    })
}

fn drop_table(db: &mut Database, name: &str) -> Result<crate::Output, SqlError> {
    let schema = load_schema(db, name)?; // errors when unknown
    let mut tree = schema_tree(db)?;
    tree.delete(db, name.as_bytes())?;
    save_schema_tree(db, &tree)?;

    // Unlink row and index trees from the catalog; their pages stay in the
    // file until a space-reclamation pass exists (documented limitation).
    let empty = BTree::empty();
    db.save_tree(&data_tree_name(name), &empty)?;
    for index in &schema.indexes {
        db.save_tree(&index_tree_name(&index.name), &empty)?;
    }
    Ok(crate::Output::Command {
        tag: "DROP TABLE".into(),
    })
}

fn create_index(
    db: &mut Database,
    index_name: &str,
    table: &str,
    column: &str,
) -> Result<crate::Output, SqlError> {
    let mut schema = load_schema(db, table)?;
    let col_type = schema
        .columns
        .iter()
        .find(|(c, _)| c == column)
        .map(|(_, t)| *t)
        .ok_or_else(|| SqlError::UnknownColumn(column.to_string()))?;
    if schema.indexes.iter().any(|i| i.name == index_name) {
        return Err(SqlError::Parse(format!(
            "index '{index_name}' already exists"
        )));
    }

    // Build the index over existing rows.
    let mut index_tree = BTree::empty();
    let data = load_data_tree(db, table)?;
    let scan: Vec<(Vec<u8>, Vec<u8>)> = data.scan(db)?.collect();
    for (key, version) in scan {
        let (_, _, row_bytes) = split_version(&version)?;
        let row = decode_row(&schema, row_bytes)?;
        let idx = schema.column_index(column).unwrap();
        let row_id = u64::from_be_bytes(key[..8].try_into().unwrap());
        if let Some(ikey) = encode_index_key(col_type, &row[idx], row_id)? {
            index_tree.insert(db, &ikey, b"")?;
        }
    }
    db.save_tree(&index_tree_name(index_name), &index_tree)?;

    schema.indexes.push(IndexDef {
        name: index_name.to_string(),
        column: column.to_string(),
    });
    save_schema(db, table, &schema)?;
    Ok(crate::Output::Command {
        tag: "CREATE INDEX".into(),
    })
}

fn drop_index(db: &mut Database, index_name: &str) -> Result<crate::Output, SqlError> {
    // Find the owning table.
    let tree = schema_tree(db)?;
    let tables: Vec<(String, Schema)> = tree
        .scan(db)?
        .map(|(k, v)| {
            Ok((
                String::from_utf8_lossy(&k).into_owned(),
                Schema::decode(&v)?,
            ))
        })
        .collect::<Result<_, DbError>>()?;
    let mut found = false;
    for (table, mut schema) in tables {
        let before = schema.indexes.len();
        schema.indexes.retain(|i| i.name != index_name);
        if schema.indexes.len() != before {
            save_schema(db, &table, &schema)?;
            found = true;
        }
    }
    if !found {
        return Err(SqlError::Parse(format!("unknown index: {index_name}")));
    }
    let empty = BTree::empty();
    db.save_tree(&index_tree_name(index_name), &empty)?;
    Ok(crate::Output::Command {
        tag: "DROP INDEX".into(),
    })
}

// ---------------------------------------------------------------------------
// Index maintenance (called on every row mutation)
// ---------------------------------------------------------------------------

fn index_insert_rows(
    db: &mut Database,
    schema: &Schema,
    values: &[Value],
    row_id: u64,
) -> Result<(), SqlError> {
    for index in &schema.indexes {
        let idx = schema
            .column_index(&index.column)
            .ok_or_else(|| SqlError::UnknownColumn(index.column.clone()))?;
        let col_type = schema.columns[idx].1;
        if let Some(ikey) = encode_index_key(col_type, &values[idx], row_id)? {
            let mut tree = db.open_tree(&index_tree_name(&index.name))?;
            tree.insert(db, &ikey, b"")?;
            db.save_tree(&index_tree_name(&index.name), &tree)?;
        }
    }
    Ok(())
}

fn index_remove_rows(
    db: &mut Database,
    schema: &Schema,
    values: &[Value],
    row_id: u64,
) -> Result<(), SqlError> {
    for index in &schema.indexes {
        let idx = schema
            .column_index(&index.column)
            .ok_or_else(|| SqlError::UnknownColumn(index.column.clone()))?;
        let col_type = schema.columns[idx].1;
        if let Some(ikey) = encode_index_key(col_type, &values[idx], row_id)? {
            let mut tree = db.open_tree(&index_tree_name(&index.name))?;
            tree.delete(db, &ikey)?;
            db.save_tree(&index_tree_name(&index.name), &tree)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DML
// ---------------------------------------------------------------------------

fn insert(
    db: &mut Database,
    txn_id: Option<u64>,
    table: &str,
    rows: Vec<Vec<Expr>>,
) -> Result<crate::Output, SqlError> {
    let txn_id = txn_id.ok_or(SqlError::Locked)?;
    let schema = load_schema(db, table)?;
    let mut data = load_data_tree(db, table)?;
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
        let versioned = stamp_row(txn_id, &encoded);
        data.insert(db, &row_key(row_id), &versioned)?;
        index_insert_rows(db, &schema, &values, row_id)?;
        count += 1;
    }
    save_schema(db, table, &schema)?;
    db.save_tree(&data_tree_name(table), &data)?;
    Ok(crate::Output::Command {
        tag: format!("INSERT 0 {count}"),
    })
}

fn select_stmt(
    db: &mut Database,
    txn_id: Option<u64>,
    select: &Select,
) -> Result<crate::Output, SqlError> {
    let schema = load_schema(db, &select.from)?;

    // Planner: an index scan for a top-level comparison on an indexed
    // column, else a full scan.
    let mut matching: Vec<(u64, Vec<Value>)> =
        if let Some((index, predicate)) = choose_index(&schema, select.filter.as_ref()) {
            index_scan(db, &schema, &select.from, txn_id, &index, &predicate)?
        } else {
            scan_visible(db, &schema, &select.from, txn_id)?
        };

    // Filter applies the full predicate (re-checks the indexed predicate
    // too — correctness never depends on the planner's choice).
    if let Some(filter) = &select.filter {
        let mut kept = Vec::with_capacity(matching.len());
        for entry in matching {
            if eval(filter, &schema, &entry.1)? == Value::Bool(true) {
                kept.push(entry);
            }
        }
        matching = kept;
    }

    if matches!(select.items.first(), Some(SelectItem::CountStar)) {
        return Ok(crate::Output::Query {
            columns: vec!["count".into()],
            column_types: vec![DataType::Int],
            rows: vec![vec![Value::Int(matching.len() as i64)]],
        });
    }

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
    let column_types = indices.iter().map(|&i| schema.columns[i].1).collect();

    // Sort on full rows (ORDER BY may reference unprojected columns).
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
            let ord = cmp_for_order(&a.1[idx], &b.1[idx], descending);
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });

    let mut rows: Vec<Vec<Value>> = matching
        .into_iter()
        .map(|(_, row)| indices.iter().map(|&i| row[i].clone()).collect())
        .collect();
    if let Some(limit) = select.limit {
        rows.truncate(limit as usize);
    }
    Ok(crate::Output::Query {
        columns,
        column_types,
        rows,
    })
}

fn scan_visible(
    db: &mut Database,
    schema: &Schema,
    table: &str,
    txn_id: Option<u64>,
) -> Result<Vec<(u64, Vec<Value>)>, SqlError> {
    let data = load_data_tree(db, table)?;
    let mut rows = Vec::new();
    let scan: Vec<(Vec<u8>, Vec<u8>)> = data.scan(db)?.collect();
    for (key, version) in scan {
        let (xmin, xmax, row_bytes) = split_version(&version)?;
        match txn_id {
            Some(my) => {
                if !visible(&|id| db.txn_status(id), my, xmin, xmax) {
                    continue;
                }
            }
            None => {
                // Auto-commit read: the latest committed state. Rows are
                // committed iff their creator committed; deletes always
                // commit before they become visible, so xmax != 0 hides a
                // row unless the deleting txn is still uncommitted — also
                // correctly hidden (it wrote xmax on a pending version the
                // reader cannot see... it can, but the version's xmax is
                // Active → treated as committed → hmm; handled below).
                let xmin_ok = db.txn_status(xmin) == zdb_core::TxnStatus::Committed;
                if !xmin_ok {
                    continue;
                }
                if xmax != 0 && db.txn_status(xmax) == zdb_core::TxnStatus::Active {
                    // Deleting transaction not committed: old version wins.
                } else if xmax != 0 {
                    continue;
                }
            }
        }
        let row = decode_row(schema, row_bytes)?;
        let row_id = u64::from_be_bytes(key[..8].try_into().unwrap());
        rows.push((row_id, row));
    }
    Ok(rows)
}

/// One comparison predicate, planned against an index.
#[derive(Debug, Clone)]
struct IndexPredicate {
    op: BinOp,
    value: Value,
}

/// Find a top-level conjunct usable by an index on this table.
fn choose_index(schema: &Schema, filter: Option<&Expr>) -> Option<(IndexDef, IndexPredicate)> {
    let filter = filter?;
    let mut conjuncts = Vec::new();
    collect_conjuncts(filter, &mut conjuncts);
    for conjunct in conjuncts {
        if let Expr::Binary(op, l, r) = conjunct {
            let (col_expr, val_expr, op) = match *op {
                BinOp::Eq | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    // Column on either side (`1 = age` support is not needed
                    // for the planner; accept column-left only for now).
                    match (l.as_ref(), r.as_ref()) {
                        (Expr::Column(c), Expr::Literal(v)) => (c, v, op),
                        _ => continue,
                    }
                }
                _ => continue,
            };
            if let Some(index) = schema.indexes.iter().find(|i| &i.column == col_expr) {
                return Some((
                    index.clone(),
                    IndexPredicate {
                        op: *op,
                        value: val_expr.clone(),
                    },
                ));
            }
        }
    }
    None
}

fn collect_conjuncts<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    match expr {
        Expr::Binary(BinOp::And, l, r) => {
            collect_conjuncts(l, out);
            collect_conjuncts(r, out);
        }
        other => out.push(other),
    }
}

/// Fetch rows through a secondary index for one comparison predicate.
fn index_scan(
    db: &mut Database,
    schema: &Schema,
    table: &str,
    txn_id: Option<u64>,
    index: &IndexDef,
    predicate: &IndexPredicate,
) -> Result<Vec<(u64, Vec<Value>)>, SqlError> {
    let idx = schema
        .column_index(&index.column)
        .ok_or_else(|| SqlError::UnknownColumn(index.column.clone()))?;
    let _ = idx;
    let col_type = schema.columns[idx].1;

    let (start, end): (Option<Vec<u8>>, Option<Vec<u8>>) = match predicate.op {
        BinOp::Eq => {
            // Keys are [value][rid], so scanning [value, value+1) covers
            // every row id for this value.
            let prefix = value_prefix(col_type, &predicate.value)?;
            let end = prefix_upper_bound(&prefix);
            (Some(prefix), end)
        }
        BinOp::Gt => {
            let prefix = value_prefix(col_type, &predicate.value)?;
            (prefix_upper_bound(&prefix), None)
        }
        BinOp::Ge => (Some(value_prefix(col_type, &predicate.value)?), None),
        BinOp::Lt => {
            let prefix = value_prefix(col_type, &predicate.value)?;
            (None, Some(prefix))
        }
        BinOp::Le => {
            let prefix = value_prefix(col_type, &predicate.value)?;
            (None, prefix_upper_bound(&prefix))
        }
        _ => unreachable!("planner only plans comparisons"),
    };

    let index_tree = db.open_tree(&index_tree_name(&index.name))?;
    let data = load_data_tree(db, table)?;
    let mut rows = Vec::new();
    let scan: Vec<(Vec<u8>, Vec<u8>)> = index_tree
        .range(db, start.as_deref(), end.as_deref())?
        .collect();
    for (ikey, _) in scan {
        let row_id = u64::from_be_bytes(ikey[ikey.len() - 8..].try_into().unwrap());
        let Some(version) = data.get(db, &row_key(row_id))? else {
            continue;
        };
        let (xmin, xmax, row_bytes) = split_version(&version)?;
        // Index entries carry no txn id: apply visibility after the fetch.
        match txn_id {
            Some(my) => {
                if !visible(&|id| db.txn_status(id), my, xmin, xmax) {
                    continue;
                }
            }
            None => {
                if db.txn_status(xmin) != zdb_core::TxnStatus::Committed {
                    continue;
                }
                if xmax != 0 && db.txn_status(xmax) == zdb_core::TxnStatus::Committed {
                    continue;
                }
            }
        }
        let row = decode_row(schema, row_bytes)?;
        rows.push((row_id, row));
    }
    Ok(rows)
}

/// The value part of an index key (without the row-id suffix).
fn value_prefix(col_type: DataType, value: &Value) -> Result<Vec<u8>, SqlError> {
    // The suffix occupies exactly 8 bytes, so a key with placeholder rid 0
    // starts with the value bytes; extract them.
    let full = encode_index_key(col_type, value, 0)?
        .ok_or_else(|| SqlError::Parse("NULL values are not indexed".into()))?;
    let value_len = full.len() - 8;
    Ok(full[..value_len].to_vec())
}

fn update(
    db: &mut Database,
    txn_id: Option<u64>,
    table: &str,
    assignments: Vec<(String, Expr)>,
    filter: Option<Expr>,
) -> Result<crate::Output, SqlError> {
    let txn_id = txn_id.ok_or(SqlError::Locked)?;
    let schema = load_schema(db, table)?;
    let mut data = load_data_tree(db, table)?;
    let mut updates: Vec<(u64, Vec<Value>)> = Vec::new();
    let scan: Vec<(Vec<u8>, Vec<u8>)> = data.scan(db)?.collect();
    for (key, version) in scan {
        let (xmin, xmax, row_bytes) = split_version(&version)?;
        // Only the latest committed-or-own version may be updated.
        if !visible(&|id| db.txn_status(id), txn_id, xmin, xmax) {
            continue;
        }
        if xmax != 0 && xmax != txn_id {
            continue;
        }
        let row_id = u64::from_be_bytes(key[..8].try_into().unwrap());
        let row = decode_row(&schema, row_bytes)?;
        let keep = match &filter {
            Some(f) => eval(f, &schema, &row)? == Value::Bool(true),
            None => true,
        };
        if !keep {
            continue;
        }
        let mut new_row = row.clone();
        for (column, expr) in &assignments {
            let idx = schema
                .column_index(column)
                .ok_or_else(|| SqlError::UnknownColumn(column.clone()))?;
            new_row[idx] = eval(expr, &schema, &new_row)?;
        }
        updates.push((row_id, new_row));
    }

    let count = updates.len() as u64;
    for (row_id, new_row) in updates {
        let old_values = {
            let version = data.get(db, &row_key(row_id))?.unwrap();
            let (_, _, row_bytes) = split_version(&version)?;
            decode_row(&schema, row_bytes)?
        };
        index_remove_rows(db, &schema, &old_values, row_id)?;

        // Append-only versioning: tombstone the old version, write the new
        // one at a fresh row id (garbage collection is a later phase).
        let old_version = data.get(db, &row_key(row_id))?.unwrap();
        let tombstoned = tombstone(&old_version, txn_id)?;
        data.insert(db, &row_key(row_id), &tombstoned)?;

        let encoded = encode_row(&schema, &new_row)?;
        let new_row_id = load_schema(db, table)?.next_row_id;
        let mut schema = load_schema(db, table)?;
        schema.next_row_id = new_row_id + 1;
        let versioned = stamp_row(txn_id, &encoded);
        data.insert(db, &row_key(new_row_id), &versioned)?;
        index_insert_rows(db, &schema, &new_row, new_row_id)?;
        save_schema(db, table, &schema)?;
    }
    Ok(crate::Output::Command {
        tag: format!("UPDATE {count}"),
    })
}

fn delete(
    db: &mut Database,
    txn_id: Option<u64>,
    table: &str,
    filter: Option<Expr>,
) -> Result<crate::Output, SqlError> {
    let txn_id = txn_id.ok_or(SqlError::Locked)?;
    let schema = load_schema(db, table)?;
    let mut data = load_data_tree(db, table)?;
    let mut doomed: Vec<(u64, Vec<Value>)> = Vec::new();
    let scan: Vec<(Vec<u8>, Vec<u8>)> = data.scan(db)?.collect();
    for (key, version) in scan {
        let (xmin, xmax, row_bytes) = split_version(&version)?;
        if xmax != 0 && xmax != txn_id {
            continue;
        }
        if !visible(&|id| db.txn_status(id), txn_id, xmin, xmax) {
            continue;
        }
        let row = decode_row(&schema, row_bytes)?;
        let keep = match &filter {
            Some(f) => eval(f, &schema, &row)? == Value::Bool(true),
            None => true,
        };
        if keep {
            let row_id = u64::from_be_bytes(key[..8].try_into().unwrap());
            doomed.push((row_id, row));
        }
    }
    let count = doomed.len() as u64;
    for (row_id, values) in doomed {
        let version = data.get(db, &row_key(row_id))?.unwrap();
        let tombstoned = tombstone(&version, txn_id)?;
        data.insert(db, &row_key(row_id), &tombstoned)?;
        index_remove_rows(db, &schema, &values, row_id)?;
    }
    Ok(crate::Output::Command {
        tag: format!("DELETE {count}"),
    })
}

fn show_tables(db: &mut Database) -> Result<crate::Output, SqlError> {
    let tree = schema_tree(db)?;
    let mut names: Vec<String> = tree
        .scan(db)?
        .map(|(k, _)| String::from_utf8_lossy(&k).into_owned())
        .collect();
    names.sort();
    Ok(crate::Output::Query {
        columns: vec!["tables".into()],
        column_types: vec![DataType::Text],
        rows: names.into_iter().map(|n| vec![Value::Text(n)]).collect(),
    })
}

// ---------------------------------------------------------------------------
// Expression evaluation
// ---------------------------------------------------------------------------

/// Constant-fold an expression that must not reference columns (INSERT values).
fn eval_const(expr: &Expr) -> Result<Value, SqlError> {
    match expr {
        Expr::Column(name) => Err(SqlError::Parse(format!(
            "column reference '{name}' is not allowed here; INSERT takes literal values"
        ))),
        other => eval(other, &Schema::default_for_eval(), &[]),
    }
}

fn eval(expr: &Expr, schema: &Schema, row: &[Value]) -> Result<Value, SqlError> {
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
                (Null, _) | (_, Null) => Ok(Bool(false)), // see ADR-0005
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
        (Some('%'), _) => (0..=s.len()).any(|skip| like_rec(&s[skip..], &p[1..])),
        (Some('_'), Some(_)) => like_rec(&s[1..], &p[1..]),
        (Some(&c), Some(&sc)) if c == sc => like_rec(&s[1..], &p[1..]),
        _ => false,
    }
}

impl Schema {
    /// An empty schema for evaluating constant expressions.
    fn default_for_eval() -> Schema {
        Schema {
            columns: Vec::new(),
            next_row_id: 0,
            indexes: Vec::new(),
        }
    }
}
