//! Postgres wire protocol (protocol v3, simple query mode).
//!
//! The goal of phase 6: a stock `psql` connects to ZelligeDB and works.
//! Message flow per connection:
//!
//! ```text
//! client → SSLRequest?  → server: 'N' (no SSL, plain TCP)
//! client → Startup      → server: AuthenticationOk, ParameterStatus*,
//!                                 BackendKeyData, ReadyForQuery
//! loop:   client 'Q'    → server: 'T' RowDescription
//!                                 'D' DataRow*
//!                                 'C' CommandComplete | 'E' ErrorResponse
//!                                 'Z' ReadyForQuery
//!         client 'X'    → close
//! ```
//!
//! All integers are big-endian (network order). A message is a type byte
//! followed by an i32 length that counts itself but not the type byte.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use zdb_core::DbError;
use zdb_sql::types::SqlError;
use zdb_sql::{Output, SqlEngine};

/// OIDs of the Postgres built-in types we emit (text format).
const OID_BOOL: u32 = 16;
const OID_INT8: u32 = 20;
const OID_TEXT: u32 = 25;

const PROTOCOL_V3: i32 = 196_608;
const SSL_REQUEST: i32 = 80_877_103;

/// Accept connections forever, one thread per session.
pub fn serve(engine: SqlEngine, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!("ZelligeDB serving on 127.0.0.1:{port} — connect with: psql -h 127.0.0.1 -p {port}");
    serve_on(listener, engine)
}

/// Serve over an existing listener (tests bind an ephemeral port).
pub fn serve_on(listener: TcpListener, engine: SqlEngine) -> std::io::Result<()> {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let engine = engine.clone();
        std::thread::spawn(move || {
            let _ = handle_connection(engine, stream);
        });
    }
    Ok(())
}

fn handle_connection(mut engine: SqlEngine, mut stream: TcpStream) -> std::io::Result<()> {
    if !startup_handshake(&mut stream)? {
        return Ok(()); // client closed during handshake
    }

    authentication_ok(&mut stream)?;
    parameter_status(&mut stream, "server_version", "16.0 ZelligeDB")?;
    parameter_status(&mut stream, "client_encoding", "UTF8")?;
    parameter_status(&mut stream, "DateStyle", "ISO, MDY")?;
    backend_key_data(&mut stream, 1, 0x5A_44_42_31)?;
    ready_for_query(&mut stream, b'I')?;

    loop {
        let (msg_type, body) = match read_message(&mut stream) {
            Ok(Some(msg)) => msg,
            Ok(None) => return Ok(()), // clean EOF
            Err(_) => return Ok(()),
        };
        match msg_type {
            b'Q' => {
                let sql = String::from_utf8_lossy(&body)
                    .trim_end_matches('\0')
                    .to_string();
                let session_txn = engine.in_txn();
                let result = engine.execute_batch(&sql, |output| {
                    let _ = send_output(&mut stream, &output);
                });
                if let Err(e) = result {
                    error_response(&mut stream, &e)?;
                }
                let status = if engine.in_txn() { b'T' } else { b'I' };
                let _ = session_txn; // reserved: distinguish 'E' failed state
                ready_for_query(&mut stream, status)?;
            }
            b'X' => return Ok(()),
            b'S' => {
                // Sync: only meaningful inside extended query mode, which
                // v0.1 does not implement; acknowledge politely.
                ready_for_query(&mut stream, if engine.in_txn() { b'T' } else { b'I' })?;
            }
            other => {
                error_response(
                    &mut stream,
                    &SqlError::Parse(format!(
                        "unsupported message type '{}{}' — this server implements simple query mode only",
                        (other as char).escape_default(),
                        if other.is_ascii_graphic() {
                            ""
                        } else {
                            " (non-printable)"
                        }
                    )),
                )?;
                ready_for_query(&mut stream, if engine.in_txn() { b'T' } else { b'I' })?;
            }
        }
    }
}

/// Handle SSLRequest + Startup. Returns false if the client vanished.
fn startup_handshake(stream: &mut TcpStream) -> std::io::Result<bool> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
        Err(e) => return Err(e),
    }
    let len = i32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len - 4];
    stream.read_exact(&mut body)?;
    let mut code = i32::from_be_bytes(body[..4].try_into().unwrap());

    if code == SSL_REQUEST {
        // No TLS in v0.1: decline, then read the real startup packet.
        stream.write_all(b"N")?;
        stream.read_exact(&mut len_buf)?;
        let len = i32::from_be_bytes(len_buf) as usize;
        body = vec![0u8; len - 4];
        stream.read_exact(&mut body)?;
        code = i32::from_be_bytes(body[..4].try_into().unwrap());
    }
    if code != PROTOCOL_V3 {
        error_response_raw(
            stream,
            "FATAL",
            "28000",
            &format!("unsupported protocol {code}"),
        )?;
        return Ok(false);
    }
    Ok(true)
}

fn authentication_ok(stream: &mut TcpStream) -> std::io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&0i32.to_be_bytes()); // AuthenticationOk
    write_message(stream, b'R', &body)
}

fn parameter_status(stream: &mut TcpStream, name: &str, value: &str) -> std::io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(name.as_bytes());
    body.push(0);
    body.extend_from_slice(value.as_bytes());
    body.push(0);
    write_message(stream, b'S', &body)
}

fn backend_key_data(stream: &mut TcpStream, pid: u32, secret: u32) -> std::io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&pid.to_be_bytes());
    body.extend_from_slice(&secret.to_be_bytes());
    write_message(stream, b'K', &body)
}

fn ready_for_query(stream: &mut TcpStream, status: u8) -> std::io::Result<()> {
    write_message(stream, b'Z', &[status])
}

fn send_output(stream: &mut TcpStream, output: &Output) -> std::io::Result<()> {
    match output {
        Output::Command { tag } => command_complete(stream, tag),
        Output::Query {
            columns,
            column_types,
            rows,
        } => {
            row_description(stream, columns, column_types)?;
            for row in rows {
                data_row(stream, row)?;
            }
            command_complete(stream, &format!("SELECT {}", rows.len()))
        }
    }
}

fn row_description(
    stream: &mut TcpStream,
    columns: &[String],
    types: &[zdb_sql::types::DataType],
) -> std::io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&(columns.len() as u16).to_be_bytes());
    for (i, name) in columns.iter().enumerate() {
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(&0u32.to_be_bytes()); // table oid
        body.extend_from_slice(&0u16.to_be_bytes()); // column attr
        let oid = match types.get(i) {
            Some(zdb_sql::types::DataType::Int) => OID_INT8,
            Some(zdb_sql::types::DataType::Bool) => OID_BOOL,
            _ => OID_TEXT,
        };
        body.extend_from_slice(&oid.to_be_bytes());
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&(-1i32).to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes()); // text format
    }
    write_message(stream, b'T', &body)
}

fn data_row(stream: &mut TcpStream, row: &[zdb_sql::types::Value]) -> std::io::Result<()> {
    use zdb_sql::types::Value;
    let mut body = Vec::new();
    body.extend_from_slice(&(row.len() as u16).to_be_bytes());
    for value in row {
        match value {
            Value::Null => body.extend_from_slice(&(-1i32).to_be_bytes()),
            Value::Bool(b) => {
                let text = if *b { "t" } else { "f" };
                body.extend_from_slice(&(text.len() as i32).to_be_bytes());
                body.extend_from_slice(text.as_bytes());
            }
            Value::Int(n) => {
                let text = n.to_string();
                body.extend_from_slice(&(text.len() as i32).to_be_bytes());
                body.extend_from_slice(text.as_bytes());
            }
            Value::Text(s) => {
                body.extend_from_slice(&(s.len() as i32).to_be_bytes());
                body.extend_from_slice(s.as_bytes());
            }
        }
    }
    write_message(stream, b'D', &body)
}

fn command_complete(stream: &mut TcpStream, tag: &str) -> std::io::Result<()> {
    let mut body = tag.as_bytes().to_vec();
    body.push(0);
    write_message(stream, b'C', &body)
}

fn error_response(stream: &mut TcpStream, error: &SqlError) -> std::io::Result<()> {
    let severity = "ERROR";
    let code = match error {
        SqlError::Locked => "55P03", // lock_not_available
        SqlError::UnknownTable(_) => "42P01",
        SqlError::DuplicateTable(_) => "42P07",
        SqlError::UnknownColumn(_) => "42703",
        SqlError::Parse(_) => "42601",
        SqlError::Db(DbError::ChecksumMismatch { .. } | DbError::Corrupt(_)) => "XX001",
        _ => "XX000",
    };
    error_response_raw(stream, severity, code, &error.to_string())
}

fn error_response_raw(
    stream: &mut TcpStream,
    severity: &str,
    code: &str,
    message: &str,
) -> std::io::Result<()> {
    let mut body = Vec::new();
    for (field, value) in [
        ('S', severity),
        ('V', severity),
        ('C', code),
        ('M', message),
    ] {
        body.push(field as u8);
        body.extend_from_slice(value.as_bytes());
        body.push(0);
    }
    body.push(0); // terminator
    write_message(stream, b'E', &body)
}

fn write_message(stream: &mut TcpStream, msg_type: u8, body: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(1 + 4 + body.len());
    frame.push(msg_type);
    frame.extend_from_slice(&((body.len() as i32 + 4).to_be_bytes()));
    frame.extend_from_slice(body);
    stream.write_all(&frame)?;
    stream.flush()
}

/// Read one frontend message: `Some((type, body))`, or `None` on EOF.
fn read_message(stream: &mut TcpStream) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut header = [0u8; 5];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let msg_type = header[0];
    let len = i32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
    if len < 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message length below 4",
        ));
    }
    let mut body = vec![0u8; len - 4];
    stream.read_exact(&mut body)?;
    Ok(Some((msg_type, body)))
}
