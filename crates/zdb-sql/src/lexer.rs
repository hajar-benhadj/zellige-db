//! SQL lexer: text → tokens.
//!
//! Case-insensitive keywords, case-sensitive identifiers, single-quoted
//! strings with `''` escapes, `--` line comments, i64 integer literals.

use crate::types::SqlError;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Ident(String),
    Number(i64),
    Str(String),
    Symbol(Symbol),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Symbol {
    LParen,
    RParen,
    Comma,
    Semicolon,
    Star,
    Eq,
    Ne, // <> or !=
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Slash,
    Percent,
}

pub fn lex(input: &str) -> Result<Vec<Token>, SqlError> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'(' => push(&mut tokens, Symbol::LParen, &mut i),
            b')' => push(&mut tokens, Symbol::RParen, &mut i),
            b',' => push(&mut tokens, Symbol::Comma, &mut i),
            b';' => push(&mut tokens, Symbol::Semicolon, &mut i),
            b'*' => push(&mut tokens, Symbol::Star, &mut i),
            b'+' => push(&mut tokens, Symbol::Plus, &mut i),
            b'-' => push(&mut tokens, Symbol::Minus, &mut i),
            b'/' => push(&mut tokens, Symbol::Slash, &mut i),
            b'%' => push(&mut tokens, Symbol::Percent, &mut i),
            b'=' => push(&mut tokens, Symbol::Eq, &mut i),
            b'<' => {
                if i + 1 < bytes.len() && (bytes[i + 1] == b'>' || bytes[i + 1] == b'=') {
                    let sym = if bytes[i + 1] == b'>' {
                        Symbol::Ne
                    } else {
                        Symbol::Le
                    };
                    tokens.push(Token::Symbol(sym));
                    i += 2;
                } else {
                    push(&mut tokens, Symbol::Lt, &mut i);
                }
            }
            b'>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::Symbol(Symbol::Ge));
                    i += 2;
                } else {
                    push(&mut tokens, Symbol::Gt, &mut i);
                }
            }
            b'!' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::Symbol(Symbol::Ne));
                    i += 2;
                } else {
                    return Err(SqlError::Parse(format!("unexpected '!' at byte {i}")));
                }
            }
            b'\'' => {
                // single-quoted string, '' escapes a quote
                let mut s = String::new();
                i += 1;
                loop {
                    if i >= bytes.len() {
                        return Err(SqlError::Parse("unterminated string literal".into()));
                    }
                    if bytes[i] == b'\'' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                            s.push('\'');
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        let ch_len = utf8_len(bytes[i]);
                        s.push_str(
                            std::str::from_utf8(&bytes[i..i + ch_len])
                                .map_err(|_| SqlError::Parse("invalid utf-8".into()))?,
                        );
                        i += ch_len;
                    }
                }
                tokens.push(Token::Str(s));
            }
            b'0'..=b'9' => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let n: i64 = input[start..i].parse().map_err(|_| {
                    SqlError::Parse(format!("number out of range: {}", &input[start..i]))
                })?;
                tokens.push(Token::Number(n));
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                tokens.push(Token::Ident(input[start..i].to_string()));
            }
            _ if b >= 0x80 => {
                let ch_len = utf8_len(b);
                let s = std::str::from_utf8(&bytes[i..i + ch_len])
                    .map_err(|_| SqlError::Parse("invalid utf-8".into()))?;
                if s.chars().all(|c| c.is_alphabetic() || c == '_') {
                    tokens.push(Token::Ident(s.to_string()));
                    i += ch_len;
                } else {
                    return Err(SqlError::Parse(format!("unexpected character at byte {i}")));
                }
            }
            _ => return Err(SqlError::Parse(format!("unexpected character at byte {i}"))),
        }
    }
    Ok(tokens)
}

fn push(tokens: &mut Vec<Token>, sym: Symbol, i: &mut usize) {
    tokens.push(Token::Symbol(sym));
    *i += 1;
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}
