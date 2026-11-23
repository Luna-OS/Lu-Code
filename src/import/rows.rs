//! Zeilen ohne festes Schema lesen und schreiben: Der Import nimmt aus jeder Tabelle der Quelle
//! die Spalten, die es auch hier gibt – so stört es nicht, wenn nrzCode inzwischen Spalten
//! dazubekommen hat oder noch welche fehlen (dann gilt hier die Vorgabe der Spalte).

use sqlx::sqlite::{SqliteConnection, SqliteRow};
use sqlx::{Column, Row, TypeInfo, ValueRef};
use std::collections::{BTreeMap, HashSet};

/// Wert einer Spalte.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Val {
    pub fn int(&self) -> Option<i64> {
        match self {
            Val::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Val::Text(s) => s,
            _ => "",
        }
    }
}

impl From<i64> for Val {
    fn from(v: i64) -> Self {
        Val::Int(v)
    }
}

impl From<Option<i64>> for Val {
    fn from(v: Option<i64>) -> Self {
        v.map(Val::Int).unwrap_or(Val::Null)
    }
}

impl From<&str> for Val {
    fn from(v: &str) -> Self {
        Val::Text(v.to_string())
    }
}

impl From<String> for Val {
    fn from(v: String) -> Self {
        Val::Text(v)
    }
}

/// Eine Zeile: Spalte → Wert.
pub type Rec = BTreeMap<String, Val>;

/// Kurzformen für den Zugriff auf eine Zeile.
pub trait RecExt {
    fn id(&self, col: &str) -> Option<i64>;
    fn str(&self, col: &str) -> &str;
    fn put(&mut self, col: &str, v: impl Into<Val>);
}

impl RecExt for Rec {
    fn id(&self, col: &str) -> Option<i64> {
        self.get(col).and_then(Val::int)
    }

    fn str(&self, col: &str) -> &str {
        self.get(col).map(Val::text).unwrap_or("")
    }

    /// Setzt den Wert nur, wenn die Quelle die Spalte kennt – sonst gilt die Vorgabe des Ziels.
    fn put(&mut self, col: &str, v: impl Into<Val>) {
        if let Some(slot) = self.get_mut(col) {
            *slot = v.into();
        }
    }
}

fn rec_from(row: &SqliteRow) -> Result<Rec, sqlx::Error> {
    let mut rec = Rec::new();
    for (i, col) in row.columns().iter().enumerate() {
        let raw = row.try_get_raw(i)?;
        let v = if raw.is_null() {
            Val::Null
        } else {
            let kind = raw.type_info().name().to_string();
            match kind.as_str() {
                "INTEGER" | "BOOLEAN" => Val::Int(row.try_get_unchecked::<i64, _>(i)?),
                "REAL" | "NUMERIC" => Val::Real(row.try_get_unchecked::<f64, _>(i)?),
                "BLOB" => Val::Blob(row.try_get_unchecked::<Vec<u8>, _>(i)?),
                _ => Val::Text(row.try_get_unchecked::<String, _>(i)?),
            }
        };
        rec.insert(col.name().to_string(), v);
    }
    Ok(rec)
}

/// Tabellen der Datenbank.
pub async fn tables(conn: &mut SqliteConnection) -> Result<HashSet<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table'").fetch_all(&mut *conn).await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

/// Spalten einer Tabelle.
pub async fn columns(conn: &mut SqliteConnection, table: &str) -> Result<HashSet<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info(?)").bind(table).fetch_all(&mut *conn).await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

/// Alle Zeilen einer Tabelle, nach `order` sortiert. `table` und `order` sind feste Namen aus
/// dem Programm, nie Eingaben.
pub async fn read_all(conn: &mut SqliteConnection, table: &str, order: &str) -> Result<Vec<Rec>, sqlx::Error> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT * FROM \"{table}\" ORDER BY {order}"))).fetch_all(&mut *conn).await?;
    rows.iter().map(rec_from).collect()
}

/// Nächster Block einer grossen Tabelle: Zeilen mit `id > after`, höchstens `limit`.
pub async fn read_chunk(conn: &mut SqliteConnection, table: &str, after: i64, limit: i64) -> Result<Vec<Rec>, sqlx::Error> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT * FROM \"{table}\" WHERE id > ? ORDER BY id LIMIT ?")))
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?;
    rows.iter().map(rec_from).collect()
}

/// Zeile einfügen – nur Spalten, die das Ziel kennt. `or_ignore`: bei schon vorhandenem
/// Schlüssel nichts tun. Gibt die neue Zeilen-ID zurück (`None`: nichts eingefügt).
pub async fn insert(conn: &mut SqliteConnection, table: &str, rec: &Rec, dest: &HashSet<String>, or_ignore: bool) -> Result<Option<i64>, sqlx::Error> {
    let cols: Vec<&String> = rec.keys().filter(|c| dest.contains(*c)).collect();
    if cols.is_empty() {
        return Ok(None);
    }
    let names = cols.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
    let marks = vec!["?"; cols.len()].join(", ");
    let verb = if or_ignore { "INSERT OR IGNORE" } else { "INSERT" };
    let mut q = sqlx::query(sqlx::AssertSqlSafe(format!("{verb} INTO \"{table}\" ({names}) VALUES ({marks})")));
    for c in &cols {
        q = match &rec[*c] {
            Val::Null => q.bind(None::<i64>),
            Val::Int(i) => q.bind(*i),
            Val::Real(f) => q.bind(*f),
            Val::Text(s) => q.bind(s.clone()),
            Val::Blob(b) => q.bind(b.clone()),
        };
    }
    let res = q.execute(&mut *conn).await?;
    Ok((res.rows_affected() > 0).then(|| res.last_insert_rowid()))
}
