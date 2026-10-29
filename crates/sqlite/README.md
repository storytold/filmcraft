# filmcraft-sqlite

A clean-room, read-only reader for SQLite database files, plus a minimal writer for tests and
fixtures. FilmCraft uses it to read DaVinci Resolve project libraries, whose projects are SQLite
databases (`Project.db`). Layer L0: no dependencies beyond `std`, no `unsafe`, builds for
`wasm32-unknown-unknown`. No SQLite source code was consulted.

## Specification

| Document | Edition | Used for |
|---|---|---|
| SQLite *Database File Format* (<https://www.sqlite.org/fileformat2.html>, public domain) | file format 4 (unchanged since SQLite 3.0.0) | header, page sizes and reserved bytes, table b-tree pages, cell layout and local payload limits, overflow chains, varints, record format and serial types, the schema table, text encodings, `INTEGER PRIMARY KEY` row-id aliases |
| SQLite *Datatypes In SQLite* (type affinity) | — | REAL columns storing whole numbers as integers |

## Reading

```rust
let bytes = std::fs::read("Project.db")?;
let db = filmcraft_sqlite::Database::open(&bytes)?;
let (table, rows) = db.read_table("Sm2Timeline")?;
let name = table.column("Name").unwrap();
for row in &rows {
    println!("{:?}", row.get(name).as_str());
}
```

- Tables only: no SQL, no indexes, no writing back. Column names come from the table's
  `CREATE TABLE` statement; a row written before a column was added reads it as NULL.
- Every page number, cell offset, varint, record header and overflow chain is bounds-checked; the
  b-tree depth is capped and page cycles are detected, so damaged or truncated files return
  `Error::Corrupt`, never panic or loop.
- Not supported: `WITHOUT ROWID` and virtual tables (`Error::Unsupported`); the write-ahead log
  (`Database::wal_mode` says when changes may still be in a `-wal` file next to the database).

## Writing

```rust
let mut w = filmcraft_sqlite::write::Writer::new(4096);
w.table("T", "CREATE TABLE T (name TEXT, n INTEGER)", rows);
let file: Vec<u8> = w.finish();
```

Rowid tables only, laid out as SQLite does (b-trees of any depth, overflow pages, shortest serial
types); SQLite's `integrity_check` accepts the result.

## Tests

- `src/tests.rs`: varints, `CREATE TABLE` parsing (quoting, constraints, row-id aliases,
  `WITHOUT ROWID`), writer → reader round trips at page sizes 512 to 65536 with every value type
  and one to several b-tree levels, overflow chains, columns added later, page cycles; truncated and
  bit-flipped files never panic.
- `tests/oracle.rs` (Python's `sqlite3` module as the oracle; skipped without Python unless
  `FILMCRAFT_REQUIRE_ORACLES=1`): databases written by SQLite (page sizes 512 to 65536, WAL mode,
  deleted rows, overflow, `ALTER TABLE`) read back exactly; databases we write pass
  `integrity_check` and read back exactly in SQLite.
