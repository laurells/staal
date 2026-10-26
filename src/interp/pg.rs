// PostgreSQL for the `db.postgres` capabilities (RUNTIME.md section 12).
//
// A grant lists the databases its capability reaches, by name, each with its server,
// database, user and password (a Secret from the environment); the program names one
// of them. Each operation opens its own connection and closes it, so nothing is held
// between operations and each is one journaled action. Connections use TLS with the
// server's certificate verified against Mozilla's roots and the operator's extra ones,
// and its host name; plain TCP is allowed only to the machine itself.
//
// `query` runs in a READ ONLY transaction, so whatever its SQL says, the server
// refuses to change anything. `execute` runs one statement in a transaction with
// synchronous_commit on, so once it returns the change is durable on the server.
// Either takes exactly one statement (the extended protocol refuses more), with
// parameters sent separately as text, which the server reads as the type it infers
// for each, never spliced into the SQL.
//
// Rows come back as JSON: one object per row, by column name. Integers as numbers,
// booleans, text and enums as strings, numeric as its exact decimal text, floats as
// the shortest decimal text that reads back as the same number, dates and timestamps
// in ISO 8601, uuids in their usual form, json and jsonb as their text, NULL as null.
// Any other type is refused with how to cast it to text in the SQL.

use postgres::types::{FromSql, IsNull, Kind, ToSql, Type};
use serde_json::{json, Map, Value as Json};
use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use super::db::{KEYS_TABLE, MAX_ROWS};

/// One database a grant reaches.
#[derive(Debug, Clone)]
pub struct Database {
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub user: String,
    pub password: Option<String>,
    /// TLS, the server's certificate verified; false only for a server on this machine.
    pub tls: bool,
}

/// The databases one `db.postgres.*` grant reaches, by the names programs use.
#[derive(Debug, Clone, Default)]
pub struct Grant {
    pub databases: BTreeMap<String, Database>,
}

/// How an `execute` ended.
pub enum Executed {
    /// Committed: the rows it changed.
    Done(u64),
    /// Certainly not committed: the server refused it, or it never reached the server.
    Failed(String),
    /// The connection broke while committing: it may or may not have been committed.
    Unknown(String),
}

/// Whether `host` is this machine, the only place plain TCP may go.
pub fn is_local(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

/// Connection settings that apply to every database: the time limits and the TLS
/// roots of the run.
#[derive(Clone)]
pub struct Settings {
    pub connect_timeout: Duration,
    pub timeout: Duration,
    pub tls: Arc<rustls::ClientConfig>,
}

fn connect(name: &str, d: &Database, s: &Settings) -> Result<postgres::Client, String> {
    let mut c = postgres::Config::new();
    c.host(&d.host).port(d.port).dbname(&d.dbname).user(&d.user).application_name("staalc").connect_timeout(s.connect_timeout);
    if let Some(p) = &d.password {
        c.password(p);
    }
    // A statement that runs longer than the run's time limit is cancelled by the server.
    c.options(&format!("-c statement_timeout={}", s.timeout.as_millis().max(1)));
    let client = if d.tls {
        c.ssl_mode(postgres::config::SslMode::Require);
        c.connect(tokio_postgres_rustls::MakeRustlsConnect::new((*s.tls).clone()))
    } else {
        c.ssl_mode(postgres::config::SslMode::Disable);
        c.connect(postgres::NoTls)
    };
    client.map_err(|e| format!("cannot connect to database {name}: {}", describe(&e)))
}

/// An error as the server or the connection gave it, without the client's wrapping.
fn describe(e: &postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => format!("{}: {}", db.severity(), db.message()),
        None => {
            let mut msg = e.to_string();
            let mut src = e.source();
            while let Some(s) = src {
                msg = format!("{msg}: {s}");
                src = s.source();
            }
            msg
        }
    }
}

/// A parameter: the program's text, sent in the text format, which the server reads as
/// the type it inferred for that parameter.
#[derive(Debug)]
struct Param<'a>(&'a str);

impl ToSql for Param<'_> {
    fn to_sql(&self, _ty: &Type, out: &mut bytes::BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        out.extend_from_slice(self.0.as_bytes());
        Ok(IsNull::No)
    }
    fn accepts(_ty: &Type) -> bool {
        true
    }
    fn encode_format(&self, _ty: &Type) -> postgres::types::Format {
        postgres::types::Format::Text
    }
    postgres::types::to_sql_checked!();
}

/// One value of a row, as JSON.
struct Cell(Json);

fn int_be<const N: usize>(raw: &[u8]) -> Result<[u8; N], Box<dyn Error + Sync + Send>> {
    raw.try_into().map_err(|_| "a value of the wrong length".into())
}

/// The date `days` after 1970-01-01 (proleptic Gregorian), as (year, month, day).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Days from 1970-01-01 to 2000-01-01, Postgres's epoch.
const PG_EPOCH_DAYS: i64 = 10_957;

fn date_text(days: i64) -> Result<String, Box<dyn Error + Sync + Send>> {
    let (y, m, d) = civil(days + PG_EPOCH_DAYS);
    if !(1..=9999).contains(&y) {
        return Err(format!("the date is outside the years 1 to 9999 ({y})").into());
    }
    Ok(format!("{y:04}-{m:02}-{d:02}"))
}

fn timestamp_text(micros: i64, utc: bool) -> Result<String, Box<dyn Error + Sync + Send>> {
    if micros == i64::MAX || micros == i64::MIN {
        return Err("the timestamp is infinity".into());
    }
    let days = micros.div_euclid(86_400_000_000);
    let rest = micros.rem_euclid(86_400_000_000);
    let (h, mi, s, us) = (rest / 3_600_000_000, rest / 60_000_000 % 60, rest / 1_000_000 % 60, rest % 1_000_000);
    let frac = if us == 0 { String::new() } else { format!(".{us:06}").trim_end_matches('0').to_string() };
    Ok(format!("{}T{h:02}:{mi:02}:{s:02}{frac}{}", date_text(days)?, if utc { "Z" } else { "" }))
}

/// Postgres's binary numeric: digits in base 10000, a weight, a sign and a scale.
fn numeric_text(raw: &[u8]) -> Result<String, Box<dyn Error + Sync + Send>> {
    if raw.len() < 8 {
        return Err("a numeric value is too short".into());
    }
    let ndigits = usize::from(u16::from_be_bytes([raw[0], raw[1]]));
    let weight = i64::from(i16::from_be_bytes([raw[2], raw[3]]));
    let sign = u16::from_be_bytes([raw[4], raw[5]]);
    let dscale = usize::from(u16::from_be_bytes([raw[6], raw[7]]));
    match sign {
        0x0000 | 0x4000 => {}
        0xC000 => return Err("the numeric value is NaN".into()),
        _ => return Err("the numeric value is infinite".into()),
    }
    if raw.len() != 8 + 2 * ndigits || dscale > 16_383 {
        return Err("a numeric value of the wrong length".into());
    }
    let digits: Vec<i64> = (0..ndigits).map(|i| i64::from(u16::from_be_bytes([raw[8 + 2 * i], raw[9 + 2 * i]]))).collect();
    if digits.iter().any(|d| *d > 9999) {
        return Err("a numeric digit out of range".into());
    }
    // Groups of four decimal digits: group k is worth 10000^(weight - k).
    let mut int = String::new();
    for k in 0..=weight.max(-1) {
        let g = usize::try_from(k).ok().and_then(|k| digits.get(k)).copied().unwrap_or(0);
        if int.is_empty() {
            if g != 0 {
                int = g.to_string();
            }
        } else {
            int.push_str(&format!("{g:04}"));
        }
    }
    if int.is_empty() {
        int.push('0');
    }
    let mut frac = String::new();
    let mut k = weight + 1;
    while frac.len() < dscale {
        let g = usize::try_from(k).ok().and_then(|k| digits.get(k)).copied().unwrap_or(0);
        frac.push_str(&format!("{g:04}"));
        k += 1;
    }
    frac.truncate(dscale);
    let neg = sign == 0x4000 && (int != "0" || frac.bytes().any(|b| b != b'0'));
    Ok(format!("{}{int}{}{frac}", if neg { "-" } else { "" }, if dscale > 0 { "." } else { "" }))
}

impl<'a> FromSql<'a> for Cell {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        let text = |raw: &[u8]| std::str::from_utf8(raw).map(str::to_string).map_err(|_| -> Box<dyn Error + Sync + Send> { "text that is not UTF-8".into() });
        let v = match *ty {
            Type::BOOL => json!(raw.first().is_some_and(|b| *b != 0)),
            Type::INT2 => json!(i16::from_be_bytes(int_be(raw)?)),
            Type::INT4 => json!(i32::from_be_bytes(int_be(raw)?)),
            Type::INT8 => json!(i64::from_be_bytes(int_be(raw)?)),
            Type::OID => json!(u32::from_be_bytes(int_be(raw)?)),
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN | Type::JSON => json!(text(raw)?),
            Type::JSONB => match raw.split_first() {
                Some((1, rest)) => json!(text(rest)?),
                _ => return Err("a jsonb value of an unknown version".into()),
            },
            Type::FLOAT4 | Type::FLOAT8 => {
                let f = if *ty == Type::FLOAT4 { f64::from(f32::from_be_bytes(int_be(raw)?)) } else { f64::from_be_bytes(int_be(raw)?) };
                if !f.is_finite() {
                    return Err(format!("the value {f} is not a number").into());
                }
                json!(if *ty == Type::FLOAT4 { format!("{:?}", f as f32) } else { format!("{f:?}") })
            }
            Type::NUMERIC => json!(numeric_text(raw)?),
            Type::DATE => {
                let d = i32::from_be_bytes(int_be(raw)?);
                if d == i32::MAX || d == i32::MIN {
                    return Err("the date is infinity".into());
                }
                json!(date_text(i64::from(d))?)
            }
            Type::TIMESTAMP => json!(timestamp_text(i64::from_be_bytes(int_be(raw)?), false)?),
            Type::TIMESTAMPTZ => json!(timestamp_text(i64::from_be_bytes(int_be(raw)?), true)?),
            Type::UUID => {
                let b: [u8; 16] = int_be(raw)?;
                let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
                json!(format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
            }
            _ if matches!(ty.kind(), Kind::Enum(_)) => json!(text(raw)?),
            Type::BYTEA => return Err("it is bytes, not text; select it as encode(column, 'hex')".into()),
            _ => return Err(format!("its type {} is not read here; cast it to text in the SQL (column::text)", ty.name()).into()),
        };
        Ok(Cell(v))
    }

    fn from_sql_null(_ty: &Type) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Cell(Json::Null))
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }
}

fn database<'g>(grant: &'g Grant, name: &str) -> Result<&'g Database, String> {
    grant.databases.get(name).ok_or_else(|| {
        let names: Vec<&str> = grant.databases.keys().map(String::as_str).collect();
        format!("the grant reaches no database named '{name}' (it reaches {})", names.join(", "))
    })
}

fn statement(client: &mut impl postgres::GenericClient, sql: &str, params: &[String]) -> Result<postgres::Statement, String> {
    let stmt = client.prepare(sql).map_err(|e| describe(&e))?;
    if stmt.params().len() != params.len() {
        return Err(format!("the SQL has {} parameter(s), but {} were given", stmt.params().len(), params.len()));
    }
    Ok(stmt)
}

/// The rows of one statement, run in a READ ONLY transaction, as a JSON array.
pub fn query(grant: &Grant, name: &str, sql: &str, params: &[String], max_bytes: usize, s: &Settings) -> Result<Json, String> {
    let d = database(grant, name)?;
    let mut client = connect(name, d, s)?;
    let mut tx = client.build_transaction().read_only(true).start().map_err(|e| describe(&e))?;
    let stmt = statement(&mut tx, sql, params)?;
    let names: Vec<String> = stmt.columns().iter().map(|c| c.name().to_string()).collect();
    for (i, n) in names.iter().enumerate() {
        if names[..i].contains(n) {
            return Err(format!("two columns are both named '{n}'; name them apart with AS"));
        }
    }
    let bound: Vec<Param> = params.iter().map(|p| Param(p)).collect();
    let mut rows = tx.query_raw(&stmt, bound.iter()).map_err(|e| describe(&e))?;
    let mut out = Vec::new();
    let mut bytes = 0usize;
    use postgres::fallible_iterator::FallibleIterator;
    while let Some(row) = rows.next().map_err(|e| describe(&e))? {
        if out.len() == MAX_ROWS {
            return Err(format!("the query gives more than {MAX_ROWS} rows; ask for fewer (LIMIT)"));
        }
        let mut obj = Map::new();
        for (i, n) in names.iter().enumerate() {
            let Cell(v) = row.try_get::<usize, Cell>(i).map_err(|e| {
                let why = e.source().map(|s| s.to_string()).unwrap_or_else(|| e.to_string());
                format!("column '{n}': {why}")
            })?;
            obj.insert(n.clone(), v);
        }
        let row = Json::Object(obj);
        bytes = bytes.saturating_add(row.to_string().len() + 1);
        if bytes > max_bytes {
            return Err(format!("the query's rows are more than {max_bytes} bytes; ask for fewer"));
        }
        out.push(row);
    }
    drop(rows);
    // Nothing was changed; end the transaction without committing anything.
    let _ = tx.rollback();
    Ok(Json::Array(out))
}

/// Runs one statement that changes the database, in a transaction; gives how many rows
/// it changed. With `key`, the key is recorded in the same transaction, so a statement
/// already committed under that key is not run again: its count is given instead.
pub fn execute(grant: &Grant, name: &str, sql: &str, params: &[String], key: Option<&str>, s: &Settings) -> Executed {
    let d = match database(grant, name) {
        Ok(d) => d,
        Err(e) => return Executed::Failed(e),
    };
    let mut client = match connect(name, d, s) {
        Ok(c) => c,
        Err(e) => return Executed::Failed(e),
    };
    let (tx, changed) = match run_execute(&mut client, sql, params, key) {
        Ok(r) => r,
        Err(e) => return Executed::Failed(e),
    };
    match tx.commit() {
        Ok(()) => Executed::Done(changed),
        // The server answered: it refused the commit, and rolled it back.
        Err(e) if e.as_db_error().is_some() => Executed::Failed(describe(&e)),
        // No answer: the commit may have happened.
        Err(e) => Executed::Unknown(format!("the connection to database {name} failed while committing: {}", describe(&e))),
    }
}

/// Everything of an `execute` before COMMIT. A failure here leaves the database as it
/// was: the transaction is rolled back, by the server or when the connection closes.
fn run_execute<'c>(client: &'c mut postgres::Client, sql: &str, params: &[String], key: Option<&str>) -> Result<(postgres::Transaction<'c>, u64), String> {
    let mut tx = client.transaction().map_err(|e| describe(&e))?;
    tx.batch_execute("SET LOCAL synchronous_commit = on").map_err(|e| describe(&e))?;
    if let Some(k) = key {
        tx.batch_execute(&format!("CREATE TABLE IF NOT EXISTS {KEYS_TABLE} (key text PRIMARY KEY, changes bigint NOT NULL)"))
            .map_err(|e| describe(&e))?;
        let found = tx.query_opt(&format!("SELECT changes FROM {KEYS_TABLE} WHERE key = $1"), &[&k]).map_err(|e| describe(&e))?;
        if let Some(row) = found {
            let n: i64 = row.try_get(0).map_err(|e| describe(&e))?;
            return Ok((tx, u64::try_from(n).unwrap_or(0)));
        }
    }
    let stmt = statement(&mut tx, sql, params)?;
    let bound: Vec<Param> = params.iter().map(|p| Param(p)).collect();
    let refs: Vec<&(dyn ToSql + Sync)> = bound.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    let changed = tx.execute(&stmt, &refs).map_err(|e| describe(&e))?;
    if let Some(k) = key {
        tx.execute(&format!("INSERT INTO {KEYS_TABLE} (key, changes) VALUES ($1, $2)"), &[&k, &i64::try_from(changed).unwrap_or(i64::MAX)])
            .map_err(|e| describe(&e))?;
    }
    Ok((tx, changed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_the_calendar_s() {
        assert_eq!(date_text(0).unwrap(), "2000-01-01");
        assert_eq!(date_text(-1).unwrap(), "1999-12-31");
        assert_eq!(date_text(59).unwrap(), "2000-02-29");
        assert_eq!(date_text(366).unwrap(), "2001-01-01");
        assert_eq!(date_text(-10_957).unwrap(), "1970-01-01");
        assert_eq!(timestamp_text(0, true).unwrap(), "2000-01-01T00:00:00Z");
        assert_eq!(timestamp_text(-1, false).unwrap(), "1999-12-31T23:59:59.999999");
        assert_eq!(timestamp_text(86_400_000_000 + 3_723_500_000, false).unwrap(), "2000-01-02T01:02:03.5");
    }

    fn numeric(ndigits: &[u16], weight: i16, neg: bool, dscale: u16) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(ndigits.len() as u16).to_be_bytes());
        v.extend_from_slice(&weight.to_be_bytes());
        v.extend_from_slice(&(if neg { 0x4000u16 } else { 0 }).to_be_bytes());
        v.extend_from_slice(&dscale.to_be_bytes());
        for d in ndigits {
            v.extend_from_slice(&d.to_be_bytes());
        }
        v
    }

    #[test]
    fn numerics_are_exact() {
        assert_eq!(numeric_text(&numeric(&[1234, 5678], 0, false, 4)).unwrap(), "1234.5678");
        assert_eq!(numeric_text(&numeric(&[12, 3456, 7800], 1, true, 2)).unwrap(), "-123456.78");
        assert_eq!(numeric_text(&numeric(&[5000], -1, false, 2)).unwrap(), "0.50");
        assert_eq!(numeric_text(&numeric(&[1], -2, false, 8)).unwrap(), "0.00000001");
        assert_eq!(numeric_text(&numeric(&[1], 2, false, 0)).unwrap(), "100000000");
        assert_eq!(numeric_text(&numeric(&[], 0, false, 2)).unwrap(), "0.00");
        assert_eq!(numeric_text(&numeric(&[], 0, true, 0)).unwrap(), "0");
        assert!(numeric_text(&[0, 0, 0, 0, 0xC0, 0, 0, 0]).is_err());
    }
}
