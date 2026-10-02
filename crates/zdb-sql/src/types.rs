//! Values, column types, and the physical row/schema encodings.

use zdb_core::DbError;

/// A dynamically-typed SQL value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Text(String),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "NULL",
            Value::Bool(_) => "BOOLEAN",
            Value::Int(_) => "INTEGER",
            Value::Text(_) => "TEXT",
        }
    }

    /// Rendered for result tables (`NULL`, `true`, `42`, `'text'`).
    pub fn display(&self) -> String {
        match self {
            Value::Null => "NULL".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Text(s) => s.clone(),
        }
    }
}

/// Column types accepted by `CREATE TABLE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    Int,
    Bool,
    Text,
}

impl DataType {
    fn from_u8(v: u8) -> Option<DataType> {
        match v {
            0 => Some(DataType::Int),
            1 => Some(DataType::Bool),
            2 => Some(DataType::Text),
            _ => None,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            DataType::Int => 0,
            DataType::Bool => 1,
            DataType::Text => 2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            DataType::Int => "INTEGER",
            DataType::Bool => "BOOLEAN",
            DataType::Text => "TEXT",
        }
    }
}

/// A table schema: ordered columns plus the row-id counter.
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    pub columns: Vec<(String, DataType)>,
    pub next_row_id: u64,
}

impl Schema {
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|(c, _)| c == name)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.columns.len() * 12);
        out.push(self.columns.len() as u8);
        out.extend_from_slice(&self.next_row_id.to_le_bytes());
        for (name, ty) in &self.columns {
            out.push(ty.to_u8());
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Schema, DbError> {
        let (&ncols, rest) = bytes
            .split_first()
            .ok_or(DbError::Corrupt("schema: empty"))?;
        if rest.len() < 8 {
            return Err(DbError::Corrupt("schema: missing row counter"));
        }
        let next_row_id = u64::from_le_bytes(rest[..8].try_into().unwrap());
        let mut cols_rest = &rest[8..];
        let mut columns = Vec::with_capacity(ncols as usize);
        for _ in 0..ncols {
            let (&ty, r) = cols_rest
                .split_first()
                .ok_or(DbError::Corrupt("schema: type"))?;
            let (&nlen, r) = r
                .split_first()
                .ok_or(DbError::Corrupt("schema: name len"))?;
            if r.len() < nlen as usize {
                return Err(DbError::Corrupt("schema: name"));
            }
            let name = String::from_utf8(r[..nlen as usize].to_vec())
                .map_err(|_| DbError::Corrupt("schema: name utf8"))?;
            let ty = DataType::from_u8(ty).ok_or(DbError::Corrupt("schema: type id"))?;
            columns.push((name, ty));
            cols_rest = &r[nlen as usize..];
        }
        Ok(Schema {
            columns,
            next_row_id,
        })
    }
}

/// Encode one row: a NULL bitmap followed by the non-NULL values.
pub fn encode_row(schema: &Schema, values: &[Value]) -> Result<Vec<u8>, SqlError> {
    if values.len() != schema.columns.len() {
        return Err(SqlError::ColumnCount {
            expected: schema.columns.len(),
            got: values.len(),
        });
    }
    let bitmap_len = schema.columns.len().div_ceil(8);
    let mut out = vec![0u8; bitmap_len];
    for (i, (value, (_, ty))) in values.iter().zip(schema.columns.iter()).enumerate() {
        match value {
            Value::Null => out[i / 8] |= 1 << (i % 8),
            Value::Bool(b) => {
                expect(*ty, DataType::Bool, value)?;
                out.push(*b as u8);
            }
            Value::Int(n) => {
                expect(*ty, DataType::Int, value)?;
                out.extend_from_slice(&n.to_le_bytes());
            }
            Value::Text(s) => {
                expect(*ty, DataType::Text, value)?;
                out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            }
        }
    }
    Ok(out)
}

/// Decode one row against its schema.
pub fn decode_row(schema: &Schema, bytes: &[u8]) -> Result<Vec<Value>, DbError> {
    let bitmap_len = schema.columns.len().div_ceil(8);
    if bytes.len() < bitmap_len {
        return Err(DbError::Corrupt("row: missing null bitmap"));
    }
    let mut values = Vec::with_capacity(schema.columns.len());
    let mut off = bitmap_len;
    for (i, (_, ty)) in schema.columns.iter().enumerate() {
        if bytes[i / 8] & (1 << (i % 8)) != 0 {
            values.push(Value::Null);
            continue;
        }
        match ty {
            DataType::Bool => {
                let (&b, _) = bytes[off..]
                    .split_first()
                    .ok_or(DbError::Corrupt("row: bool"))?;
                values.push(Value::Bool(b != 0));
                off += 1;
            }
            DataType::Int => {
                if bytes.len() < off + 8 {
                    return Err(DbError::Corrupt("row: int"));
                }
                values.push(Value::Int(i64::from_le_bytes(
                    bytes[off..off + 8].try_into().unwrap(),
                )));
                off += 8;
            }
            DataType::Text => {
                if bytes.len() < off + 4 {
                    return Err(DbError::Corrupt("row: text len"));
                }
                let len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
                if bytes.len() < off + 4 + len {
                    return Err(DbError::Corrupt("row: text"));
                }
                values.push(Value::Text(
                    String::from_utf8(bytes[off + 4..off + 4 + len].to_vec())
                        .map_err(|_| DbError::Corrupt("row: text utf8"))?,
                ));
                off += 4 + len;
            }
        }
    }
    Ok(values)
}

fn expect(actual: DataType, wanted: DataType, value: &Value) -> Result<(), SqlError> {
    if actual != wanted {
        return Err(SqlError::TypeMismatch {
            column_type: actual.name().to_string(),
            value_type: value.type_name().to_string(),
        });
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SqlError {
    #[error("parse error: {0}")]
    Parse(String),

    #[error("unknown table: {0}")]
    UnknownTable(String),

    #[error("table already exists: {0}")]
    DuplicateTable(String),

    #[error("unknown column: {0}")]
    UnknownColumn(String),

    #[error("column count mismatch: expected {expected}, got {got}")]
    ColumnCount { expected: usize, got: usize },

    #[error("type mismatch: column is {column_type}, value is {value_type}")]
    TypeMismatch {
        column_type: String,
        value_type: String,
    },

    #[error("storage error: {0}")]
    Db(#[from] DbError),
}
