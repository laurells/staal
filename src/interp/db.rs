// SQLite for the `db.sqlite` capabilities (RUNTIME.md section 12).
//
// The broker has checked the database's canonical path against the grant; fsx checks
// the path as every `fs` path is checked, and SQLite opens it refusing any symbolic
// link in it. SQLite opens files by path, so a process that can rename folders inside
// the granted ones could still race that check; the folders should be writable only
// by those who run the program. Each operation opens its own connection and closes
// it, so nothing is held between operations and each is one journaled action.
//
// `query` opens the database read-only: whatever its SQL says, it cannot change the
// file. `execute` runs one statement in a transaction, with `synchronous=FULL`, so
// once it returns the change is on disk. Either takes one statement only, with no
// ATTACH (which would open another file) and no extensions. Parameters are bound,
// never spliced into the SQL.
//
// Rows come back as JSON: one object per row, by column name; INTEGER as a number,
// TEXT as a string, NULL as null, REAL as the shortest decimal text that reads back
// as the same number. A BLOB is refused: it is not text.

#[cfg(target_os = "linux")]
use rusqlite::limits::Limit;
use rusqlite::types::ValueRef;
use rusqlite::Connection;
#[cfg(target_os = "linux")]
use rusqlite::OpenFlags;
use serde_json::{json, Map, Value as Json};

/// The most rows one query may give.
pub const MAX_ROWS: usize = 10_000;

/// How long an operation waits for another process's lock on the database.
#[cfg(target_os = "linux")]
const BUSY_MS: u64 = 5_000;

/// The table `execute` keeps its idempotency keys in, when its grant asks for it.
pub const KEYS_TABLE: &str = "_staal_keys";

fn open(path: &str, write: bool) -> Result<Connection, String> {
    #[cfg(target_os = "linux")]
    {
        super::fsx::database_check(path, write)?;
        let mut flags = OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        flags |= if write { OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE } else { OpenFlags::SQLITE_OPEN_READ_ONLY };
        let conn = Connection::open_with_flags(path, flags).map_err(|e| format!("'{path}': {e}"))?;
        conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0).map_err(|e| format!("'{path}': {e}"))?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_MS)).map_err(|e| format!("'{path}': {e}"))?;
        // Schema code (triggers, views) may not call functions with side effects.
        conn.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA synchronous = FULL;").map_err(|e| format!("'{path}': {e}"))?;
        Ok(conn)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, write);
        Err("db.sqlite runs on Linux only so far".into())
    }
}

/// One statement prepared, refusing more than one.
fn prepare<'c>(conn: &'c Connection, path: &str, sql: &str) -> Result<rusqlite::Statement<'c>, String> {
    conn.prepare(sql).map_err(|e| match e {
        rusqlite::Error::MultipleStatement => "the SQL holds more than one statement; give one at a time".to_string(),
        e => format!("'{path}': {e}"),
    })
}

fn bind(stmt: &mut rusqlite::Statement<'_>, params: &[String]) -> Result<(), String> {
    let want = stmt.parameter_count();
    if want != params.len() {
        return Err(format!("the SQL has {want} parameter(s), but {} were given", params.len()));
    }
    for (i, p) in params.iter().enumerate() {
        stmt.raw_bind_parameter(i + 1, p).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The rows of one read-only statement, as a JSON array of objects.
pub fn query(path: &str, sql: &str, params: &[String], max_bytes: usize) -> Result<Json, String> {
    let conn = open(path, false)?;
    let mut stmt = prepare(&conn, path, sql)?;
    if !stmt.readonly() {
        return Err("`query` runs only statements that read; use a db.sqlite.execute capability to change the database".into());
    }
    let names: Vec<String> = stmt.column_names().map(String::from).collect();
    for (i, n) in names.iter().enumerate() {
        if names[..i].contains(n) {
            return Err(format!("two columns are both named '{n}'; name them apart with AS"));
        }
    }
    bind(&mut stmt, params)?;
    let mut rows = stmt.raw_query();
    let mut out = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().map_err(|e| format!("'{path}': {e}"))? {
        if out.len() == MAX_ROWS {
            return Err(format!("the query gives more than {MAX_ROWS} rows; ask for fewer (LIMIT)"));
        }
        let mut obj = Map::new();
        for (i, n) in names.iter().enumerate() {
            let v = match row.get_ref(i).map_err(|e| e.to_string())? {
                ValueRef::Null => Json::Null,
                ValueRef::Integer(n) => json!(n),
                ValueRef::Real(f) => json!(real_text(f)?),
                ValueRef::Text(t) => json!(std::str::from_utf8(t).map_err(|_| format!("column '{n}' holds text that is not UTF-8"))?),
                ValueRef::Blob(_) => return Err(format!("column '{n}' holds a BLOB, which is not text; select it as hex(...)")),
            };
            obj.insert(n.clone(), v);
        }
        let row = Json::Object(obj);
        bytes = bytes.saturating_add(row.to_string().len() + 1);
        if bytes > max_bytes {
            return Err(format!("the query's rows are more than {max_bytes} bytes; ask for fewer"));
        }
        out.push(row);
    }
    Ok(Json::Array(out))
}

/// A REAL as text: the shortest decimal that reads back as the same number.
fn real_text(f: f64) -> Result<String, String> {
    if !f.is_finite() {
        return Err(format!("a REAL value {f} is not a number"));
    }
    Ok(format!("{f:?}"))
}

/// Runs one statement that changes the database, in a transaction; gives how many rows
/// it changed. With `key`, the key is recorded in the same transaction, so whether the
/// statement happened can be known later (`executed`).
pub fn execute(path: &str, sql: &str, params: &[String], key: Option<&str>) -> Result<u64, String> {
    let mut conn = open(path, true)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| format!("'{path}': {e}"))?;
    if let Some(k) = key {
        tx.execute_batch(&format!("CREATE TABLE IF NOT EXISTS {KEYS_TABLE} (key TEXT PRIMARY KEY, changes INTEGER NOT NULL)"))
            .map_err(|e| format!("'{path}': {e}"))?;
        if let Some(n) = recorded(&tx, k, path)? {
            return Ok(n);
        }
    }
    let changes = {
        let mut stmt = prepare(&tx, path, sql)?;
        if stmt.readonly() {
            return Err("`execute` runs statements that change the database; use a db.sqlite.query capability to read".into());
        }
        bind(&mut stmt, params)?;
        stmt.raw_execute().map_err(|e| format!("'{path}': {e}"))?
    };
    // A statement that changes nothing may still have run: the change count is what
    // SQLite reports for it.
    let changes = changes as u64;
    if let Some(k) = key {
        tx.execute(&format!("INSERT INTO {KEYS_TABLE} (key, changes) VALUES (?1, ?2)"), rusqlite::params![k, changes as i64])
            .map_err(|e| format!("'{path}': {e}"))?;
    }
    tx.commit().map_err(|e| format!("'{path}': {e}"))?;
    Ok(changes)
}

fn recorded(conn: &Connection, key: &str, path: &str) -> Result<Option<u64>, String> {
    let mut stmt = conn.prepare(&format!("SELECT changes FROM {KEYS_TABLE} WHERE key = ?1")).map_err(|e| format!("'{path}': {e}"))?;
    let mut rows = stmt.query([key]).map_err(|e| format!("'{path}': {e}"))?;
    match rows.next().map_err(|e| format!("'{path}': {e}"))? {
        Some(r) => Ok(Some(r.get::<_, i64>(0).map_err(|e| e.to_string())? as u64)),
        None => Ok(None),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn scratch(name: &str) -> String {
        let d = std::env::temp_dir().join(format!("staal-db-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap().join("t.db").to_string_lossy().into_owned()
    }

    #[test]
    fn statements_change_the_database_and_queries_only_read_it() {
        let db = scratch("basic");
        assert!(query(&db, "SELECT 1", &[], 1 << 20).unwrap_err().contains("does not exist"));
        execute(&db, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score REAL)", &[], None).unwrap();
        assert_eq!(execute(&db, "INSERT INTO t (name, score) VALUES (?1, ?2)", &["ada".into(), "1.5".into()], None), Ok(1));
        assert_eq!(execute(&db, "INSERT INTO t (name, score) VALUES (?1, NULL)", &["bola".into()], None), Ok(1));
        let rows = query(&db, "SELECT id, name, score FROM t ORDER BY id", &[], 1 << 20).unwrap();
        assert_eq!(rows, json!([{ "id": 1, "name": "ada", "score": "1.5" }, { "id": 2, "name": "bola", "score": null }]));
        // A text parameter compares with an INTEGER column by the column's affinity.
        assert_eq!(query(&db, "SELECT name FROM t WHERE id = ?1", &["2".into()], 1 << 20).unwrap(), json!([{ "name": "bola" }]));
        // Read-only means read-only, whatever the SQL.
        assert!(query(&db, "DELETE FROM t", &[], 1 << 20).unwrap_err().contains("only statements that read"));
        assert!(execute(&db, "SELECT * FROM t", &[], None).unwrap_err().contains("change the database"));
        // One statement; parameters counted; no ATTACH; no BLOBs; no duplicate names.
        assert!(execute(&db, "DELETE FROM t; DROP TABLE t", &[], None).unwrap_err().contains("more than one statement"));
        assert!(query(&db, "SELECT ?1", &[], 1 << 20).unwrap_err().contains("1 parameter(s), but 0"));
        let other = db.replace("t.db", "other.db");
        assert!(execute(&db, &format!("ATTACH DATABASE '{other}' AS o"), &[], None).is_err());
        assert!(!std::path::Path::new(&other).exists());
        assert!(query(&db, "SELECT x'00' AS b", &[], 1 << 20).unwrap_err().contains("BLOB"));
        assert!(query(&db, "SELECT 1 AS a, 2 AS a", &[], 1 << 20).unwrap_err().contains("both named 'a'"));
        assert!(query(&db, "SELECT name FROM t", &[], 10).unwrap_err().contains("more than 10 bytes"));
        // A failed statement changes nothing: its transaction is rolled back.
        assert!(execute(&db, "INSERT INTO t (id, name) VALUES (1, 'dup')", &[], None).is_err());
        assert_eq!(query(&db, "SELECT count(*) AS n FROM t", &[], 1 << 20).unwrap(), json!([{ "n": 2 }]));
    }

    #[test]
    fn a_keyed_statement_happens_once() {
        let db = scratch("keys");
        execute(&db, "CREATE TABLE t (n INTEGER)", &[], None).unwrap();
        assert_eq!(execute(&db, "INSERT INTO t VALUES (1)", &[], Some("run-1")), Ok(1));
        // The same key again: it already happened, and is not done twice.
        assert_eq!(execute(&db, "INSERT INTO t VALUES (1)", &[], Some("run-1")), Ok(1));
        assert_eq!(execute(&db, "INSERT INTO t VALUES (1)", &[], Some("run-2")), Ok(1));
        assert_eq!(query(&db, "SELECT count(*) AS n FROM t", &[], 1 << 20).unwrap(), json!([{ "n": 2 }]));
        // A statement that fails records no key: it can be made again.
        assert!(execute(&db, "INSERT INTO nowhere VALUES (1)", &[], Some("run-3")).is_err());
        assert_eq!(query(&db, &format!("SELECT count(*) AS n FROM {KEYS_TABLE}"), &[], 1 << 20).unwrap(), json!([{ "n": 2 }]));
    }

    #[test]
    fn links_are_never_followed() {
        let db = scratch("links");
        execute(&db, "CREATE TABLE t (n INTEGER)", &[], None).unwrap();
        let link = db.replace("t.db", "link.db");
        std::os::unix::fs::symlink(&db, &link).unwrap();
        assert!(query(&link, "SELECT 1", &[], 1 << 20).unwrap_err().contains("symbolic link"));
        let hard = db.replace("t.db", "hard.db");
        std::fs::hard_link(&db, &hard).unwrap();
        assert!(query(&hard, "SELECT 1", &[], 1 << 20).unwrap_err().contains("hard links"));
        std::fs::remove_file(&hard).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", format!("{db}-journal")).unwrap();
        assert!(execute(&db, "INSERT INTO t VALUES (1)", &[], None).unwrap_err().contains("-journal' is a symbolic link"));
    }
}
