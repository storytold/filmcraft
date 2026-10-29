use crate::write::Writer;
use crate::*;

/// `n` rows of every value shape: small and large integers, reals and NULLs, blobs, multi-byte
/// text.
fn sample_rows(n: usize) -> Vec<Vec<Value>> {
    (0..n)
        .map(|i| {
            vec![
                Value::Text(format!("{:08x}-uuid-{i}", i * 7919)),
                Value::Integer([0, 1, -1, 127, -129, 40_000, -9_000_000, 3_000_000_000, -(1 << 46), i64::MIN, i64::MAX][i % 11].wrapping_add(i as i64 % 3)),
                if i % 5 == 0 { Value::Null } else { Value::Real(i as f64 * 0.25 - 3.5) },
                Value::Blob((0..(i * 37) % 300).map(|b| b as u8).collect()),
                Value::Text("é日本".repeat(i % 4)),
            ]
        })
        .collect()
}

const SQL: &str = "CREATE TABLE \"T\" (\r\n \"Id\" uuid NOT NULL, \"N\" bigint, \"R\" double precision, \"B\" BLOB, \"S\" character varying,\r\n CONSTRAINT \"T_pkey\" PRIMARY KEY (\"Id\"))";

/// Varints of every length read back as written.
#[test]
fn varints_round_trip() {
    for v in [0u64, 1, 127, 128, 240, 2287, 16383, 16384, 1 << 21, (1 << 56) - 1, 1 << 56, u64::MAX, 0xff00_0000_0000_0001] {
        let mut b = Vec::new();
        write_varint(v, &mut b);
        assert!(b.len() <= 9);
        assert_eq!(read_varint(&b, 0), Some((v, b.len())), "{v}");
    }
    assert_eq!(read_varint(&[0x81], 0), None, "truncated");
}

/// Column names and the row id alias from `CREATE TABLE` statements.
#[test]
fn columns_from_create_table() {
    let c = schema::parse_create_table(SQL).unwrap();
    assert_eq!(c.names, ["Id", "N", "R", "B", "S"]);
    assert_eq!(c.rowid_alias, None);
    let c = schema::parse_create_table("CREATE TABLE x(a INTEGER PRIMARY KEY, \"b c\" TEXT DEFAULT 'x,y', [d] NUMERIC(10, 2), `e`)").unwrap();
    assert_eq!(c.names, ["a", "b c", "d", "e"]);
    assert_eq!(c.rowid_alias, Some(0));
    let c = schema::parse_create_table("create table y (k integer, v, primary key (k))").unwrap();
    assert_eq!(c.rowid_alias, Some(0));
    let c = schema::parse_create_table("CREATE TABLE z (k INTEGER PRIMARY KEY, v) WITHOUT ROWID").unwrap();
    assert!(c.without_rowid);
    assert_eq!(c.rowid_alias, None);
    assert_eq!(schema::parse_create_table("CREATE TABLE q (\"a\"\"b\" int)").unwrap().names, ["a\"b"]);
    assert!(schema::parse_create_table("CREATE TABLE broken (a, b").is_none());
}

/// Tables from one page up to several interior levels read back exactly.
#[test]
fn writer_round_trips_every_shape() {
    // one page, several leaves under one interior page, and several interior levels
    for (page_size, n) in [(4096, 3), (512, 40), (1024, 600), (512, 3000), (65536, 200)] {
        let rows = sample_rows(n);
        let mut w = Writer::new(page_size);
        w.table("T", SQL, rows.clone());
        w.table("Empty", "CREATE TABLE Empty (a)", Vec::new());
        let bytes = w.finish();
        assert!(sniff(&bytes));
        let db = Database::open(&bytes).unwrap();
        assert_eq!(db.page_size(), page_size);
        assert!(!db.wal_mode());
        assert_eq!(db.table_names().collect::<Vec<_>>(), ["T", "Empty"]);
        let (t, got) = db.read_table("t").unwrap();
        assert_eq!(t.column("s"), Some(4));
        assert_eq!(got.len(), n);
        for (i, (g, want)) in got.iter().zip(&rows).enumerate() {
            assert_eq!(g.rowid, i as i64 + 1);
            assert_eq!(&g.values, want, "page size {page_size}, row {i}");
        }
        assert!(db.read_table("Empty").unwrap().1.is_empty());
        assert_eq!(db.table("nope"), Err(Error::NoTable("nope".into())));
    }
}

/// Values larger than a page go to overflow chains and read back whole.
#[test]
fn large_values_use_overflow_chains() {
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let text = "Resolve ".repeat(5000);
    let mut w = Writer::new(1024);
    w.table(
        "Big",
        "CREATE TABLE Big (b BLOB, t TEXT)",
        vec![vec![Value::Blob(big.clone()), Value::Text(text.clone())], vec![Value::Null, Value::Text("small".into())]],
    );
    let bytes = w.finish();
    let db = Database::open(&bytes).unwrap();
    let (_, rows) = db.read_table("Big").unwrap();
    assert_eq!(rows[0].values, vec![Value::Blob(big), Value::Text(text)]);
    assert_eq!(rows[1].get(1).as_str(), Some("small"));
}

/// An `INTEGER PRIMARY KEY` reads as the row id, and rows written before a column was added read it
/// as NULL.
#[test]
fn rowid_alias_and_added_columns() {
    let mut w = Writer::new(4096);
    // rows written before `c` was added have two values; the alias is stored as NULL
    w.table(
        "A",
        "CREATE TABLE A (id INTEGER PRIMARY KEY, b TEXT, c TEXT)",
        vec![vec![Value::Null, Value::Text("x".into())], vec![Value::Null, Value::Text("y".into()), Value::Text("z".into())]],
    );
    let bytes = w.finish();
    let db = Database::open(&bytes).unwrap();
    let (t, rows) = db.read_table("A").unwrap();
    assert_eq!(t.rowid_alias, Some(0));
    assert_eq!(rows[0].get(0), &Value::Integer(1));
    assert_eq!(rows[1].get(0), &Value::Integer(2));
    assert!(rows[0].get(2).is_null());
    assert_eq!(rows[1].get(2).as_str(), Some("z"));
}

/// Files that are not SQLite, and tables this reader does not read (`WITHOUT ROWID`), are errors.
#[test]
fn rejects_what_it_cannot_read() {
    assert_eq!(Database::open(b"not a database").err(), Some(Error::NotSqlite));
    let mut w = Writer::new(4096);
    w.table("W", "CREATE TABLE W (k INTEGER PRIMARY KEY, v) WITHOUT ROWID", vec![vec![Value::Integer(1), Value::Null]]);
    let bytes = w.finish();
    let db = Database::open(&bytes).unwrap();
    assert!(matches!(db.table("W"), Err(Error::Unsupported(_))));
    // a bad page size
    let mut bad = bytes.clone();
    bad[16..18].copy_from_slice(&1000u16.to_be_bytes());
    assert!(matches!(Database::open(&bad), Err(Error::Corrupt(_))));
}

/// A b-tree whose pages point back at each other is an error, not an endless walk.
#[test]
fn page_cycles_are_errors() {
    let mut w = Writer::new(512);
    w.table("T", SQL, sample_rows(200));
    let mut bytes = w.finish();
    let db = Database::open(&bytes).unwrap();
    let root = db.schema().iter().find(|e| e.name == "T").unwrap().root as usize;
    // point the root's right-most child back at the root
    let at = (root - 1) * 512;
    assert_eq!(bytes[at], 0x05, "the table needs an interior root for this test");
    bytes[at + 8..at + 12].copy_from_slice(&(root as u32).to_be_bytes());
    let db = Database::open(&bytes).unwrap();
    assert!(matches!(db.read_table("T"), Err(Error::Corrupt(_))));
}

/// Truncated and bit-flipped files are errors, never a panic.
#[test]
fn damaged_files_never_panic() {
    let mut w = Writer::new(512);
    w.table("T", SQL, sample_rows(120));
    w.table("Big", "CREATE TABLE Big (b)", vec![vec![Value::Blob(vec![7; 5000])]]);
    let bytes = w.finish();
    let read_all = |b: &[u8]| {
        if let Ok(db) = Database::open(b) {
            let names: Vec<String> = db.table_names().map(str::to_string).collect();
            for n in names {
                let _ = db.read_table(&n);
            }
        }
    };
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for cut in (0..bytes.len()).step_by(97) {
        let b = &bytes[..cut];
        assert!(std::panic::catch_unwind(|| read_all(b)).is_ok(), "truncated at {cut}");
    }
    for round in 0..400 {
        let mut b = bytes.clone();
        for _ in 0..1 + round % 8 {
            let i = (rnd() as usize) % b.len();
            b[i] ^= 1 << (rnd() % 8);
        }
        assert!(std::panic::catch_unwind(|| read_all(&b)).is_ok(), "bit flips, round {round}");
    }
}

/// No interior b-tree page but page 1 is written without cells (SQLite reads one as corrupt),
/// even when packing the interior pages of a level leaves a single child over.
#[test]
fn interior_pages_always_hold_a_key() {
    for n in 100..=500 {
        let rows: Vec<Vec<Value>> = (0..n).map(|i| vec![Value::Blob(vec![i as u8; 200])]).collect();
        let mut w = Writer::new(512);
        w.table("T", "CREATE TABLE T (b BLOB)", rows);
        let bytes = w.finish();
        let db = Database::open(&bytes).unwrap();
        assert_eq!(db.read_table("T").unwrap().1.len(), n, "{n} rows");
        for (k, page) in bytes.chunks(512).enumerate().skip(1) {
            if page[0] == 0x05 {
                assert!(u16::from_be_bytes([page[3], page[4]]) > 0, "{n} rows: interior page {} has no cells", k + 1);
            }
        }
    }
}
