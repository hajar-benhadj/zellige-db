//! A hand-written wire-protocol client, used to test the server the same
//! way real clients talk to it: raw messages over TCP, no helper crates.
//!
//! Covers: SSL decline, startup, AuthenticationOk → ReadyForQuery, simple
//! queries (RowDescription/DataRow/CommandComplete), psql-style tags, the
//! single-writer lock surfacing as an ErrorResponse, and transaction
//! status in ReadyForQuery.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::Ordering;

use zdb_sql::SqlEngine;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::AtomicU64;
        use std::time::{SystemTime, UNIX_EPOCH};
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("zdb-{tag}-{}-{id}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

/// Spawn the server on an ephemeral port; returns (addr, port).
fn spawn_server(tag: &str) -> (TempDir, String, u16) {
    let dir = TempDir::new(tag);
    let engine = SqlEngine::create(dir.path().join("wire.zdb")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || zdb_serverless_serve(listener, engine));
    (dir, "127.0.0.1".to_string(), port)
}

// The binary crate cannot be imported by tests; replicate its entry point.
fn zdb_serverless_serve(listener: std::net::TcpListener, engine: SqlEngine) {
    let _ = zdb_server::serve::serve_on(listener, engine);
}

struct Client {
    stream: TcpStream,
}

impl Client {
    fn connect(addr: &str, port: u16) -> Self {
        let mut stream = TcpStream::connect((addr, port)).unwrap();
        // Try SSL first: the server must decline with a bare 'N'.
        let mut ssl = Vec::new();
        ssl.extend_from_slice(&8i32.to_be_bytes());
        ssl.extend_from_slice(&80_877_103i32.to_be_bytes());
        stream.write_all(&ssl).unwrap();
        let mut n = [0u8; 1];
        stream.read_exact(&mut n).unwrap();
        assert_eq!(n[0], b'N', "server must decline SSL");

        // Startup packet: len, protocol 3.0, "user\0zellige\0", terminator.
        let mut startup = Vec::new();
        let params: &[&[u8]] = &[b"user\0", b"zellige\0"];
        let mut body = Vec::new();
        body.extend_from_slice(&196_608i32.to_be_bytes());
        for p in params {
            body.extend_from_slice(p);
        }
        body.push(0);
        startup.extend_from_slice(&((body.len() as i32 + 4).to_be_bytes()));
        startup.extend_from_slice(&body);
        stream.write_all(&startup).unwrap();

        let mut client = Client { stream };
        // Consume handshake: R(0), S*, K, then Z.
        let mut saw_auth_ok = false;
        loop {
            let (t, body) = client.read_message();
            match t {
                b'R' => {
                    assert_eq!(&body[..4], &0i32.to_be_bytes(), "AuthenticationOk");
                    saw_auth_ok = true;
                }
                b'S' | b'K' => {}
                b'Z' => break,
                other => panic!("unexpected handshake message {other}"),
            }
        }
        assert!(saw_auth_ok);
        client
    }

    fn read_message(&mut self) -> (u8, Vec<u8>) {
        let mut header = [0u8; 5];
        self.stream.read_exact(&mut header).unwrap();
        let len = i32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
        let mut body = vec![0u8; len - 4];
        self.stream.read_exact(&mut body).unwrap();
        (header[0], body)
    }

    /// Send a simple Query and collect responses until ReadyForQuery.
    /// Returns (rows, command_tags, error_messages, txn_status).
    fn query(&mut self, sql: &str) -> (Vec<Vec<String>>, Vec<String>, Vec<String>, u8) {
        let mut body = sql.as_bytes().to_vec();
        body.push(0);
        let mut frame = vec![b'Q'];
        frame.extend_from_slice(&((body.len() as i32 + 4).to_be_bytes()));
        frame.extend_from_slice(&body);
        self.stream.write_all(&frame).unwrap();

        let mut rows = Vec::new();
        let mut tags = Vec::new();
        let mut errors = Vec::new();
        let status;
        let mut ncols = 0usize;
        loop {
            let (t, body) = self.read_message();
            match t {
                b'T' => ncols = u16::from_be_bytes(body[..2].try_into().unwrap()) as usize,
                b'D' => {
                    let n = u16::from_be_bytes(body[..2].try_into().unwrap()) as usize;
                    assert_eq!(n, ncols);
                    let mut off = 2;
                    let mut row = Vec::new();
                    for _ in 0..n {
                        let cell_len = i32::from_be_bytes(body[off..off + 4].try_into().unwrap());
                        off += 4;
                        if cell_len < 0 {
                            row.push("NULL".to_string());
                        } else {
                            let cell =
                                String::from_utf8(body[off..off + cell_len as usize].to_vec())
                                    .unwrap();
                            off += cell_len as usize;
                            row.push(cell);
                        }
                    }
                    rows.push(row);
                }
                b'C' => {
                    let tag = String::from_utf8(body[..body.len() - 1].to_vec()).unwrap();
                    tags.push(tag);
                }
                b'E' => {
                    // Extract the M field.
                    let mut off = 0;
                    while off < body.len() && body[off] != 0 {
                        let field = body[off] as char;
                        off += 1;
                        let end = body[off..].iter().position(|&b| b == 0).unwrap() + off;
                        if field == 'M' {
                            errors.push(String::from_utf8(body[off..end].to_vec()).unwrap());
                        }
                        off = end + 1;
                    }
                }
                b'Z' => {
                    status = body[0];
                    break;
                }
                b'S' | b'K' | b'R' => {}
                other => panic!("unexpected message {other}"),
            }
        }
        (rows, tags, errors, status)
    }

    fn terminate(mut self) {
        let _ = self.stream.write_all(b"X\x00\x00\x00\x04");
        let _ = self.stream.flush();
    }
}

#[test]
fn psql_wire_end_to_end() {
    let (_dir, addr, port) = spawn_server("wire-e2e");
    let mut client = Client::connect(&addr, port);

    client.query("CREATE TABLE users (id INTEGER, name TEXT, active BOOLEAN)");
    client.query("INSERT INTO users VALUES (1, 'hajar', TRUE), (2, 'amine', FALSE)");

    let (rows, tags, errors, status) =
        client.query("SELECT id, name, active FROM users WHERE id = 2");
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    assert_eq!(rows, vec![vec!["2", "amine", "f"]]);
    assert_eq!(tags, vec!["SELECT 1"]);
    assert_eq!(status, b'I', "idle after auto-commit statement");

    // Transaction status flows through ReadyForQuery.
    client.query("BEGIN");
    let (_, _, _, status) = client.query("SELECT 1");
    assert_eq!(status, b'T', "in-transaction status");
    client.query("COMMIT");

    // psql-style error reporting with a code, and no poisoned connection.
    let (_, _, errors, status) = client.query("SELECT * FROM missing_table");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("missing_table"), "{errors:?}");
    assert_eq!(status, b'I');
    let (rows, _, _, _) = client.query("SELECT COUNT(*) FROM users");
    assert_eq!(rows, vec![vec!["2"]]);

    client.terminate();
}

#[test]
fn wire_lock_and_two_sessions() {
    let (_dir, addr, port) = spawn_server("wire-lock");
    let mut a = Client::connect(&addr, port);
    let mut b = Client::connect(&addr, port);

    a.query("CREATE TABLE t (id INTEGER)");
    a.query("INSERT INTO t VALUES (1)");

    a.query("BEGIN");
    a.query("INSERT INTO t VALUES (2)");

    // Session B cannot write while A holds the storage transaction.
    let (_, _, errors, _) = b.query("INSERT INTO t VALUES (3)");
    assert_eq!(errors.len(), 1, "expected a lock error");
    assert!(errors[0].to_lowercase().contains("locked"), "{errors:?}");

    // B can still read, and never sees A's uncommitted row.
    let (rows, _, _, _) = b.query("SELECT COUNT(*) FROM t");
    assert_eq!(rows, vec![vec!["1"]]);

    a.query("COMMIT");
    let (rows, _, _, _) = b.query("SELECT COUNT(*) FROM t");
    assert_eq!(rows, vec![vec!["2"]]);

    a.terminate();
    b.terminate();
}
