//! Oracle tests against the SQLite library itself, through Python's `sqlite3` module: databases
//! written by SQLite are read back by our reader, and databases written by our writer pass
//! SQLite's `integrity_check` and read back identically. Skipped when Python is not installed
//! (`FILMCRAFT_REQUIRE_ORACLES=1` makes that a failure).

use std::path::PathBuf;
use std::process::Command;

use filmcraft_sqlite::write::Writer;
use filmcraft_sqlite::{Database, Value};

/// A Python with the `sqlite3` module, if one is installed.
fn python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|p| Command::new(p).args(["-c", "import sqlite3"]).output().is_ok_and(|o| o.status.success()))
}

/// Skip an oracle test (a failure under `FILMCRAFT_REQUIRE_ORACLES`).
fn skip(why: &str) {
    if std::env::var_os("FILMCRAFT_REQUIRE_ORACLES").is_some() {
        panic!("{why}");
    }
    eprintln!("skipping: {why}");
}

/// Where the oracle tests write their databases.
fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/sqlite");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// SQLite writes a database with every page and value shape; we read it.
#[test]
fn reads_databases_written_by_sqlite() {
    let Some(py) = python() else { return skip("python with sqlite3 not found") };
    for (page_size, journal) in [(512, "delete"), (4096, "delete"), (65536, "delete"), (1024, "wal")] {
        let path = dir().join(format!("oracle-{page_size}-{journal}.db"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let script = r#"
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute(f"pragma page_size={sys.argv[2]}")
db.execute(f"pragma journal_mode={sys.argv[3]}")
db.execute('CREATE TABLE "Sm2TiItem" ("Sm2TiItem_id" uuid NOT NULL, "Name" character varying, "Start" character varying, "N" bigint, "R" double precision, "B" BLOB, CONSTRAINT "pk" PRIMARY KEY ("Sm2TiItem_id"))')
db.execute('CREATE TABLE a (id INTEGER PRIMARY KEY, v TEXT)')
for i in range(3000):
    db.execute('insert into Sm2TiItem values (?,?,?,?,?,?)', (f'id-{i}', 'clip é' * (i % 7), str(86400 + i), [0, 1, -5, 300, -70000, 2**40, -2**50][i % 7] * (i + 1), None if i % 4 == 0 else i / 3, bytes(range(256)) * (i % 50)))
db.execute('insert into a (v) values (?)', ('x' * 70000,))
db.execute('insert into a values (1000, ?)', ('y',))
db.execute('delete from Sm2TiItem where rowid % 10 = 3')
db.execute('alter table a add column w')
db.execute('insert into a values (2000, ?, ?)', ('z', 5))
db.commit()
if sys.argv[3] == 'wal':
    db.execute('pragma wal_checkpoint(TRUNCATE)')
db.close()
"#;
        let out = Command::new(py).args(["-c", script, path.to_str().unwrap(), &page_size.to_string(), journal]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let bytes = std::fs::read(&path).unwrap();
        let db = Database::open(&bytes).unwrap();
        assert_eq!(db.page_size(), page_size);
        assert_eq!(db.wal_mode(), journal == "wal");
        let (t, rows) = db.read_table("Sm2TiItem").unwrap();
        assert_eq!(t.columns, ["Sm2TiItem_id", "Name", "Start", "N", "R", "B"]);
        let kept: Vec<usize> = (0..3000).filter(|i| (i + 1) % 10 != 3).collect();
        assert_eq!(rows.len(), kept.len());
        for (row, &i) in rows.iter().zip(&kept) {
            assert_eq!(row.get(0).as_str(), Some(format!("id-{i}").as_str()));
            assert_eq!(row.get(1).as_str(), Some("clip é".repeat(i % 7).as_str()));
            assert_eq!(row.get(2).as_i64(), Some(86400 + i as i64));
            assert_eq!(row.get(3), &Value::Integer([0i64, 1, -5, 300, -70000, 1 << 40, -(1 << 50)][i % 7] * (i as i64 + 1)));
            assert_eq!(row.get(4), &if i % 4 == 0 { Value::Null } else { Value::Real(i as f64 / 3.0) });
            assert_eq!(row.get(5).as_bytes().map(<[u8]>::len), Some(256 * (i % 50)));
        }
        let (t, rows) = db.read_table("a").unwrap();
        assert_eq!(t.rowid_alias, Some(0));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get(0), &Value::Integer(1));
        assert_eq!(rows[0].get(1).as_str().map(str::len), Some(70000));
        assert!(rows[0].get(2).is_null());
        assert_eq!(rows[2].values, vec![Value::Integer(2000), Value::Text("z".into()), Value::Integer(5)]);
    }
}

/// Our writer's files pass SQLite's integrity check and read back the same values.
#[test]
fn sqlite_reads_databases_we_write() {
    let Some(py) = python() else { return skip("python with sqlite3 not found") };
    for page_size in [512, 4096] {
        let rows: Vec<Vec<Value>> = (0..2000)
            .map(|i| vec![Value::Text(format!("r{i}")), Value::Integer(i * 1_000_003 - 7_000_000), Value::Blob(vec![i as u8; (i as usize * 13) % 3000])])
            .collect();
        let mut w = Writer::new(page_size);
        w.table("T", "CREATE TABLE T (name TEXT, n INTEGER, b BLOB)", rows);
        let path = dir().join(format!("written-{page_size}.db"));
        std::fs::write(&path, w.finish()).unwrap();
        let script = r#"
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
assert db.execute('pragma integrity_check').fetchone()[0] == 'ok'
rows = db.execute('select rowid, name, n, length(b), b from T order by rowid').fetchall()
assert len(rows) == 2000, len(rows)
for i, (rowid, name, n, blen, b) in enumerate(rows):
    assert rowid == i + 1 and name == f'r{i}' and n == i * 1000003 - 7000000, (rowid, name, n)
    assert blen == (i * 13) % 3000 and b == bytes([i % 256]) * blen
print('ok')
"#;
        let out = Command::new(py).args(["-c", script, path.to_str().unwrap()]).output().unwrap();
        assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "ok", "page size {page_size}: {}", String::from_utf8_lossy(&out.stderr));
    }
}
