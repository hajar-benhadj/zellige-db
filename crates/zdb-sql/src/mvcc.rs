//! MVCC row versioning and secondary-index key encoding.

use zdb_core::{DbError, TxnStatus};

use crate::types::{DataType, SqlError, Value};

// ---------------------------------------------------------------------------
// Row versions: [xmin u64][xmax u64][row bytes]
// ---------------------------------------------------------------------------

pub(crate) fn stamp_row(xmin: u64, row: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + row.len());
    out.extend_from_slice(&xmin.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(row);
    out
}

/// Mark an existing version as deleted by `xmax`.
pub(crate) fn tombstone(version: &[u8], xmax: u64) -> Result<Vec<u8>, DbError> {
    let mut out = version.to_vec();
    if out.len() < 16 {
        return Err(DbError::Corrupt("row version too short"));
    }
    out[8..16].copy_from_slice(&xmax.to_le_bytes());
    Ok(out)
}

pub(crate) fn split_version(bytes: &[u8]) -> Result<(u64, u64, &[u8]), DbError> {
    if bytes.len() < 16 {
        return Err(DbError::Corrupt("row version too short"));
    }
    let xmin = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let xmax = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    Ok((xmin, xmax, &bytes[16..]))
}

/// Snapshot-visibility rule (ADR-0006):
///
/// - a row version is inserted-visible when its creator is *this*
///   transaction, or committed *before* this transaction began;
/// - it is delete-hidden when the deleter is this transaction, or committed
///   before this transaction began. Deletes committed *after* our snapshot
///   do not affect us — that is the snapshot guarantee.
///
/// Unknown transaction ids count as committed: a crash discards every
/// uncommitted journal record, so anything on disk survived its commit.
pub(crate) fn visible(
    status_of: &dyn Fn(u64) -> TxnStatus,
    my_txn: u64,
    xmin: u64,
    xmax: u64,
) -> bool {
    let inserted_visible =
        xmin == my_txn || (status_of(xmin) == TxnStatus::Committed && xmin < my_txn);
    if !inserted_visible {
        return false;
    }
    if xmax == 0 {
        return true;
    }
    !(xmax == my_txn || (status_of(xmax) == TxnStatus::Committed && xmax < my_txn))
}

// ---------------------------------------------------------------------------
// Index keys: [encoded column value][row id BE u64]
// ---------------------------------------------------------------------------

/// NULLs are not indexed (deviation from SQLite, documented in ADR-0006).
pub(crate) fn encode_index_key(
    column_type: DataType,
    value: &Value,
    row_id: u64,
) -> Result<Option<Vec<u8>>, SqlError> {
    let mut key = match (column_type, value) {
        (_, Value::Null) => return Ok(None),
        (DataType::Int, Value::Int(n)) => (*n ^ i64::MIN).to_be_bytes().to_vec(),
        (DataType::Bool, Value::Bool(b)) => vec![*b as u8],
        (DataType::Text, Value::Text(s)) => s.as_bytes().to_vec(),
        (ty, v) => {
            return Err(SqlError::TypeMismatch {
                column_type: ty.name().into(),
                value_type: v.type_name().into(),
            });
        }
    };
    key.extend_from_slice(&row_id.to_be_bytes());
    Ok(Some(key))
}

/// Smallest key that sorts strictly after every key starting with `prefix`:
/// increment the last byte with carry; an all-`0xFF` tail has no bound.
pub(crate) fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut bound = prefix.to_vec();
    while let Some(&last) = bound.last() {
        if last == 0xFF {
            bound.pop();
        } else {
            *bound.last_mut().unwrap() += 1;
            return Some(bound);
        }
    }
    None // prefix was all 0xFF: unbounded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(id: u64) -> TxnStatus {
        match id {
            1 => TxnStatus::Committed, // old committed txn
            2 => TxnStatus::Active,    // still running
            3 => TxnStatus::Aborted,   // rolled back
            _ => TxnStatus::Committed,
        }
    }

    #[test]
    fn visibility_follows_the_snapshot_rule() {
        // Row inserted by an old committed txn, never deleted: visible to
        // every later snapshot.
        assert!(visible(&status, 10, 1, 0));
        // Row inserted by a still-active other txn: dirty read blocked.
        assert!(!visible(&status, 10, 2, 0));
        // Row inserted by an aborted txn: invisible forever.
        assert!(!visible(&status, 10, 3, 0));
        // Row deleted by a txn that committed BEFORE my snapshot: hidden.
        assert!(!visible(&status, 10, 1, 1));
        // Row deleted by a txn that commits AFTER my snapshot began: I
        // still see it — that is the snapshot guarantee.
        assert!(visible(&status, 5, 1, 6));
        // My own uncommitted delete hides my own view of the row.
        assert!(!visible(&status, 7, 1, 7));
        // My own uncommitted insert is visible to me.
        assert!(visible(&status, 7, 7, 0));
    }

    #[test]
    fn index_keys_sort_by_value_then_row_id() {
        let k1 = encode_index_key(DataType::Int, &Value::Int(-5), 1)
            .unwrap()
            .unwrap();
        let k2 = encode_index_key(DataType::Int, &Value::Int(3), 9)
            .unwrap()
            .unwrap();
        assert!(k1 < k2, "negative ints sort before positives");

        // Sign-flipped big-endian ints keep numeric order in byte order.
        let lo = encode_index_key(DataType::Int, &Value::Int(i64::MIN), 0)
            .unwrap()
            .unwrap();
        let hi = encode_index_key(DataType::Int, &Value::Int(i64::MAX), 0)
            .unwrap()
            .unwrap();
        assert!(lo < hi);
    }

    #[test]
    fn nulls_are_not_indexed() {
        assert!(
            encode_index_key(DataType::Int, &Value::Null, 1)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn prefix_upper_bound_covers_every_continuation() {
        let bound = prefix_upper_bound(b"ab").unwrap();
        let after_ff = prefix_upper_bound(b"ab\xff").unwrap();
        assert!(b"ab".as_slice() < bound.as_slice());
        assert!(b"ab\xff".as_slice() < bound.as_slice());
        assert!(bound.as_slice() <= after_ff.as_slice());
        assert!(b"ac".as_slice() >= bound.as_slice());
        assert_eq!(prefix_upper_bound(&[0xFF, 0xFF]), None);
    }
}
