//! Clean-room reader for SQLite database files, written from the public **Database File Format**
//! document of the SQLite project (<https://www.sqlite.org/fileformat2.html>, public domain;
//! file format 4, unchanged since SQLite 3.0.0). No SQLite source code was consulted.
//!
//! It reads tables, not SQL: [`Database::open`] parses a database held in memory and its schema
//! table, and [`Database::read_table`] returns every row of one table with its column names
//! (taken from the table's `CREATE TABLE` statement). There is no query engine, no index use and
//! no writing back. That is all an importer needs to read another application's project file,
//! such as a DaVinci Resolve `Project.db`.
//!
//! - Table b-trees of any depth (interior and leaf pages), overflow chains, every record serial
//!   type, `INTEGER PRIMARY KEY` row-id aliases, columns added later by `ALTER TABLE` (missing
//!   values read as NULL), UTF-8 and UTF-16 databases, page sizes 512 to 65536 with reserved
//!   bytes.
//! - Every page number, cell offset, varint, record header and overflow chain is bounds-checked;
//!   b-tree depth is capped and page cycles are detected, so damaged files give errors, never
//!   panics or endless loops.
//! - Not supported: `WITHOUT ROWID` tables and virtual tables ([`Error::Unsupported`]), and the
//!   write-ahead log: a database in WAL mode is read as of its last checkpoint
//!   ([`Database::wal_mode`] tells the caller to look for a `-wal` file).
//!
//! [`write::Writer`] builds small rowid-table databases for tests and fixtures.
//!
//! Layer L0: no dependencies beyond `std`, no `unsafe`, builds for `wasm32-unknown-unknown`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod read;
mod schema;
pub mod write;

pub use read::{Database, Row, SchemaEntry, Table};

use std::fmt;

/// The 16-byte header string every database file starts with.
pub const MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Whether `bytes` start like an SQLite database file.
pub fn sniff(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// A column value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    /// Whether this is NULL.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    /// The text of a TEXT value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
    /// An INTEGER value, or a TEXT value that is an integer (SQLite's type affinity lets either
    /// appear in a column).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Integer(i) => Some(*i),
            Value::Text(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
    /// A REAL or INTEGER value.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Real(f) => Some(*f),
            Value::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }
    /// The bytes of a BLOB (or TEXT) value.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Blob(b) => Some(b),
            Value::Text(s) => Some(s.as_bytes()),
            _ => None,
        }
    }
}

/// Why a database could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bytes do not start with the SQLite header string.
    NotSqlite,
    /// The file is damaged or truncated.
    Corrupt(String),
    /// A valid structure this reader does not handle.
    Unsupported(String),
    /// No table of that name in the schema.
    NoTable(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotSqlite => write!(f, "not an SQLite database"),
            Error::Corrupt(m) => write!(f, "damaged SQLite database: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported SQLite feature: {m}"),
            Error::NoTable(t) => write!(f, "no table \"{t}\" in the database"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// An [`Error::Corrupt`] saying `m`.
pub(crate) fn corrupt(m: impl Into<String>) -> Error {
    Error::Corrupt(m.into())
}

/// Read the variable-length integer at `pos` (1–9 bytes, big-endian, seven bits per byte with a
/// continuation bit, all eight bits in a ninth byte). Returns the value and the position after it.
pub(crate) fn read_varint(b: &[u8], pos: usize) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    for i in 0..9 {
        let byte = *b.get(pos.checked_add(i)?)?;
        if i == 8 {
            return Some(((v << 8) | u64::from(byte), pos + 9));
        }
        v = (v << 7) | u64::from(byte & 0x7f);
        if byte < 0x80 {
            return Some((v, pos + i + 1));
        }
    }
    None
}

/// Append `v` as a varint (the shortest encoding).
pub(crate) fn write_varint(v: u64, out: &mut Vec<u8>) {
    if v & 0xff00_0000_0000_0000 != 0 {
        let mut buf = [0u8; 9];
        buf[8] = v as u8;
        let mut x = v >> 8;
        for b in buf[..8].iter_mut().rev() {
            *b = (x as u8 & 0x7f) | 0x80;
            x >>= 7;
        }
        out.extend_from_slice(&buf);
        return;
    }
    let mut groups = Vec::with_capacity(8);
    let mut x = v;
    loop {
        groups.push(x as u8 & 0x7f);
        x >>= 7;
        if x == 0 {
            break;
        }
    }
    // the lowest group goes last, without the continuation bit
    for (i, g) in groups.iter().enumerate().rev() {
        out.push(if i == 0 { *g } else { g | 0x80 });
    }
}

/// How many bytes [`write_varint`] writes for `v`.
pub(crate) fn varint_len(v: u64) -> usize {
    let mut out = Vec::with_capacity(9);
    write_varint(v, &mut out);
    out.len()
}

#[cfg(test)]
mod tests;
