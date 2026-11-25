//! Database access layer. One code path serves SQLite and MySQL/MariaDB
//! through the sqlx `Any` driver; the schema sticks to text, integer, real
//! and blob columns so behaviour is identical on both engines.

mod messages;
mod misc;
pub mod models;
mod queue;

pub use models::*;

use crate::config::Database as DbConfig;
use crate::utils;
use serde_json::{json, Map, Value as Json};
use sqlx::any::{AnyArguments, AnyPoolOptions, AnyRow, AnyTypeInfoKind};
use sqlx::query::Query;
use sqlx::{Any, AnyPool, Column, Row, ValueRef};
use std::time::Duration;

pub type DbResult<T> = Result<T, DbError>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("{0}")]
    Other(String),
}

impl DbError {
    pub fn is_unique_violation(&self) -> bool {
        match self {
            DbError::Sqlx(sqlx::Error::Database(e)) => {
                e.is_unique_violation() || {
                    let m = e.message().to_ascii_lowercase();
                    m.contains("unique constraint") || m.contains("duplicate entry")
                }
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbKind {
    Sqlite,
    MySql,
}

/// A bindable query parameter.
#[derive(Debug, Clone)]
pub enum P {
    S(Option<String>),
    I(Option<i64>),
    F(Option<f64>),
    B(Option<Vec<u8>>),
}

impl From<&str> for P {
    fn from(v: &str) -> Self {
        P::S(Some(v.to_string()))
    }
}
impl From<String> for P {
    fn from(v: String) -> Self {
        P::S(Some(v))
    }
}
impl From<&String> for P {
    fn from(v: &String) -> Self {
        P::S(Some(v.clone()))
    }
}
impl From<Option<String>> for P {
    fn from(v: Option<String>) -> Self {
        P::S(v)
    }
}
impl From<Option<&str>> for P {
    fn from(v: Option<&str>) -> Self {
        P::S(v.map(str::to_string))
    }
}
impl From<&Option<String>> for P {
    fn from(v: &Option<String>) -> Self {
        P::S(v.clone())
    }
}
impl From<i64> for P {
    fn from(v: i64) -> Self {
        P::I(Some(v))
    }
}
impl From<Option<i64>> for P {
    fn from(v: Option<i64>) -> Self {
        P::I(v)
    }
}
impl From<bool> for P {
    fn from(v: bool) -> Self {
        P::I(Some(v as i64))
    }
}
impl From<f64> for P {
    fn from(v: f64) -> Self {
        P::F(Some(v))
    }
}
impl From<Option<f64>> for P {
    fn from(v: Option<f64>) -> Self {
        P::F(v)
    }
}
impl From<Vec<u8>> for P {
    fn from(v: Vec<u8>) -> Self {
        P::B(Some(v))
    }
}
impl From<Option<Vec<u8>>> for P {
    fn from(v: Option<Vec<u8>>) -> Self {
        P::B(v)
    }
}

#[macro_export]
macro_rules! params {
    ($($x:expr),* $(,)?) => { vec![$($crate::database::P::from($x)),*] };
}

fn bind_all<'q>(
    mut q: Query<'q, Any, AnyArguments<'q>>,
    ps: Vec<P>,
) -> Query<'q, Any, AnyArguments<'q>> {
    for p in ps {
        q = match p {
            P::S(v) => q.bind(v),
            P::I(v) => q.bind(v),
            P::F(v) => q.bind(v),
            P::B(v) => q.bind(v),
        };
    }
    q
}

/// Convert any row into a JSON object (blobs become uppercase hex).
pub fn row_json(row: &AnyRow) -> Json {
    let mut m = Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        let v = match row.try_get_raw(i) {
            Ok(raw) if raw.is_null() => Json::Null,
            Ok(raw) => match raw.type_info().kind() {
                AnyTypeInfoKind::SmallInt
                | AnyTypeInfoKind::Integer
                | AnyTypeInfoKind::BigInt
                | AnyTypeInfoKind::Bool => row
                    .try_get::<i64, _>(i)
                    .map(Json::from)
                    .unwrap_or(Json::Null),
                AnyTypeInfoKind::Real | AnyTypeInfoKind::Double => row
                    .try_get::<f64, _>(i)
                    .map(Json::from)
                    .unwrap_or(Json::Null),
                // MySQL reports long text columns as blobs; only the columns
                // listed in BINARY_COLUMNS hold real binary data.
                AnyTypeInfoKind::Blob => row
                    .try_get::<Vec<u8>, _>(i)
                    .map(|b| {
                        if BINARY_COLUMNS.contains(&col.name()) {
                            Json::String(hex::encode_upper(b))
                        } else {
                            Json::String(String::from_utf8_lossy(&b).into_owned())
                        }
                    })
                    .unwrap_or(Json::Null),
                _ => row
                    .try_get::<String, _>(i)
                    .map(Json::String)
                    .unwrap_or(Json::Null),
            },
            Err(_) => Json::Null,
        };
        m.insert(col.name().to_string(), v);
    }
    Json::Object(m)
}

/// Columns that store binary data (everything else is text or numeric).
const BINARY_COLUMNS: &[&str] = &["payload"];

pub(crate) fn get_s(row: &AnyRow, col: &str) -> Option<String> {
    match row.try_get::<Option<String>, _>(col) {
        Ok(v) => v,
        // MySQL reports long text columns as blobs.
        Err(_) => row
            .try_get::<Option<Vec<u8>>, _>(col)
            .ok()
            .flatten()
            .map(|b| String::from_utf8_lossy(&b).into_owned()),
    }
}
pub(crate) fn get_i(row: &AnyRow, col: &str) -> Option<i64> {
    row.try_get::<Option<i64>, _>(col).ok().flatten()
}
pub(crate) fn get_b(row: &AnyRow, col: &str) -> Option<Vec<u8>> {
    row.try_get::<Option<Vec<u8>>, _>(col).ok().flatten()
}

const MIGRATIONS_SQLITE: &[(i64, &str, &str)] = &[
    (
        1,
        "initial",
        include_str!("../../migrations/sqlite/0001_initial.sql"),
    ),
    (
        2,
        "contacts",
        include_str!("../../migrations/sqlite/0002_contacts.sql"),
    ),
    (
        3,
        "security_events",
        include_str!("../../migrations/sqlite/0003_security_events.sql"),
    ),
];
const MIGRATIONS_MYSQL: &[(i64, &str, &str)] = &[
    (
        1,
        "initial",
        include_str!("../../migrations/mysql/0001_initial.sql"),
    ),
    (
        2,
        "contacts",
        include_str!("../../migrations/mysql/0002_contacts.sql"),
    ),
    (
        3,
        "security_events",
        include_str!("../../migrations/mysql/0003_security_events.sql"),
    ),
];

fn split_statements(sql: &str) -> Vec<String> {
    let cleaned: String = sql
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    cleaned
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[derive(Clone)]
pub struct Db {
    pool: AnyPool,
    kind: DbKind,
    sqlite_path: Option<String>,
}

impl Db {
    /// Open the configured database. The schema is not touched; call
    /// [`Db::migrate`] afterwards.
    pub async fn connect(cfg: &DbConfig) -> DbResult<Self> {
        sqlx::any::install_default_drivers();
        let (kind, url, max) = match cfg.driver.as_str() {
            "mysql" => (DbKind::MySql, cfg.mysql_url.clone(), cfg.max_connections),
            _ => {
                if let Some(parent) = std::path::Path::new(&cfg.sqlite_path).parent() {
                    if !parent.as_os_str().is_empty() && !parent.exists() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            DbError::Other(format!("cannot create {}: {e}", parent.display()))
                        })?;
                    }
                }
                (
                    DbKind::Sqlite,
                    format!("sqlite://{}?mode=rwc", cfg.sqlite_path),
                    cfg.max_connections,
                )
            }
        };
        let wal = cfg.sqlite_wal;
        let pool = AnyPoolOptions::new()
            .max_connections(max.max(1))
            .acquire_timeout(Duration::from_secs(15))
            .after_connect(move |conn, _| {
                Box::pin(async move {
                    if kind == DbKind::Sqlite {
                        for pragma in [
                            "PRAGMA busy_timeout = 10000",
                            "PRAGMA foreign_keys = ON",
                            if wal {
                                "PRAGMA journal_mode = WAL"
                            } else {
                                "PRAGMA journal_mode = DELETE"
                            },
                            "PRAGMA synchronous = NORMAL",
                        ] {
                            sqlx::query(pragma).fetch_all(&mut *conn).await?;
                        }
                    }
                    Ok(())
                })
            })
            .connect(&url)
            .await?;
        Ok(Self {
            pool,
            kind,
            sqlite_path: (kind == DbKind::Sqlite).then(|| cfg.sqlite_path.clone()),
        })
    }

    pub fn kind(&self) -> DbKind {
        self.kind
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub(crate) async fn exec(&self, sql: &str, ps: Vec<P>) -> DbResult<u64> {
        Ok(bind_all(sqlx::query(sql), ps)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub(crate) async fn rows(&self, sql: &str, ps: Vec<P>) -> DbResult<Vec<AnyRow>> {
        Ok(bind_all(sqlx::query(sql), ps).fetch_all(&self.pool).await?)
    }

    pub(crate) async fn row(&self, sql: &str, ps: Vec<P>) -> DbResult<Option<AnyRow>> {
        Ok(bind_all(sqlx::query(sql), ps)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub(crate) async fn json_rows(&self, sql: &str, ps: Vec<P>) -> DbResult<Vec<Json>> {
        Ok(self.rows(sql, ps).await?.iter().map(row_json).collect())
    }

    pub(crate) async fn count(&self, sql: &str, ps: Vec<P>) -> DbResult<i64> {
        Ok(self
            .row(sql, ps)
            .await?
            .and_then(|r| r.try_get::<i64, _>(0).ok())
            .unwrap_or(0))
    }

    pub(crate) async fn tx_exec(
        tx: &mut sqlx::Transaction<'_, Any>,
        sql: &str,
        ps: Vec<P>,
    ) -> DbResult<u64> {
        Ok(bind_all(sqlx::query(sql), ps)
            .execute(&mut **tx)
            .await?
            .rows_affected())
    }

    pub(crate) async fn begin(&self) -> DbResult<sqlx::Transaction<'_, Any>> {
        Ok(self.pool.begin().await?)
    }

    /// Apply pending schema migrations. Returns the versions applied.
    pub async fn migrate(&self) -> DbResult<Vec<i64>> {
        let (list, create) = match self.kind {
            DbKind::Sqlite => (
                MIGRATIONS_SQLITE,
                "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL)",
            ),
            DbKind::MySql => (
                MIGRATIONS_MYSQL,
                "CREATE TABLE IF NOT EXISTS schema_migrations (version BIGINT PRIMARY KEY, name VARCHAR(191) NOT NULL, applied_at VARCHAR(32) NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
            ),
        };
        self.exec(create, vec![]).await?;
        let mut applied = Vec::new();
        for (version, name, sql) in list {
            if self
                .count(
                    "SELECT COUNT(*) FROM schema_migrations WHERE version = ?",
                    params![*version],
                )
                .await?
                > 0
            {
                continue;
            }
            for stmt in split_statements(sql) {
                self.exec(&stmt, vec![]).await.map_err(|e| {
                    DbError::Other(format!("migration {version} ({name}) failed: {e}"))
                })?;
            }
            self.exec(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)",
                params![*version, *name, utils::now()],
            )
            .await?;
            applied.push(*version);
        }
        Ok(applied)
    }

    pub async fn schema_version(&self) -> DbResult<i64> {
        self.count(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            vec![],
        )
        .await
    }

    /// Latest schema version known to this build.
    pub fn latest_version(&self) -> i64 {
        MIGRATIONS_SQLITE.last().map(|m| m.0).unwrap_or(0)
    }

    pub async fn ping(&self) -> bool {
        self.count("SELECT 1", vec![])
            .await
            .map(|v| v == 1)
            .unwrap_or(false)
    }

    /// Run the engine's integrity check.
    pub async fn verify(&self) -> DbResult<Json> {
        match self.kind {
            DbKind::Sqlite => {
                let rows = self.rows("PRAGMA integrity_check", vec![]).await?;
                let results: Vec<String> = rows
                    .iter()
                    .filter_map(|r| r.try_get::<String, _>(0).ok())
                    .collect();
                let ok = results.len() == 1 && results[0] == "ok";
                Ok(
                    json!({"ok": ok, "engine": "sqlite", "schema_version": self.schema_version().await?, "details": results}),
                )
            }
            DbKind::MySql => {
                let v = self.schema_version().await?;
                let n = self.count("SELECT COUNT(*) FROM messages", vec![]).await?;
                Ok(
                    json!({"ok": true, "engine": "mysql", "schema_version": v, "details": [format!("connection ok, {n} messages")]}),
                )
            }
        }
    }

    /// Create a consistent online backup (SQLite only) and verify it.
    pub async fn backup(&self, dir: &str) -> DbResult<String> {
        if self.kind != DbKind::Sqlite {
            return Err(DbError::Other(
                "built-in backup supports SQLite only; use mysqldump for MySQL/MariaDB (see docs/BACKUP-RESTORE.md)".into(),
            ));
        }
        std::fs::create_dir_all(dir)
            .map_err(|e| DbError::Other(format!("cannot create {dir}: {e}")))?;
        let name = format!(
            "messages-{}.db",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        );
        let path = std::path::Path::new(dir).join(name);
        let path_s = path.display().to_string();
        if path_s.contains('\'') {
            return Err(DbError::Other("backup path must not contain quotes".into()));
        }
        self.exec(&format!("VACUUM INTO '{path_s}'"), vec![])
            .await?;
        verify_sqlite_file(&path_s).await?;
        Ok(path_s)
    }

    pub fn sqlite_path(&self) -> Option<&str> {
        self.sqlite_path.as_deref()
    }

    /// Size of the database on disk in bytes (SQLite only).
    pub fn size_bytes(&self) -> Option<u64> {
        let p = self.sqlite_path.as_deref()?;
        let mut total = std::fs::metadata(p).ok()?.len();
        for suffix in ["-wal", "-shm"] {
            total += std::fs::metadata(format!("{p}{suffix}"))
                .map(|m| m.len())
                .unwrap_or(0);
        }
        Some(total)
    }
}

/// Open a SQLite file read-only and run an integrity check.
pub async fn verify_sqlite_file(path: &str) -> DbResult<()> {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{path}?mode=ro"))
        .await?;
    let rows = sqlx::query("PRAGMA integrity_check")
        .fetch_all(&pool)
        .await?;
    let ok = rows.len() == 1
        && rows[0]
            .try_get::<String, _>(0)
            .map(|s| s == "ok")
            .unwrap_or(false);
    let has_schema = sqlx::query("SELECT COUNT(*) FROM schema_migrations")
        .fetch_one(&pool)
        .await
        .is_ok();
    pool.close().await;
    if ok && has_schema {
        Ok(())
    } else {
        Err(DbError::Other(format!("{path} failed verification")))
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// Open a fresh database for tests. Uses MySQL/MariaDB when
    /// `TEST_MYSQL_URL` (a server URL without database name) is set, creating
    /// a uniquely named database per call; SQLite otherwise.
    pub async fn test_db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DbConfig {
            sqlite_path: dir.path().join("test.db").display().to_string(),
            ..Default::default()
        };
        if let Ok(url) = std::env::var("TEST_MYSQL_URL") {
            sqlx::any::install_default_drivers();
            let name = format!("t_{}", utils::new_id().replace('-', ""));
            let admin = AnyPoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .unwrap();
            sqlx::query(&format!("CREATE DATABASE {name} CHARACTER SET utf8mb4"))
                .execute(&admin)
                .await
                .unwrap();
            admin.close().await;
            cfg.driver = "mysql".into();
            cfg.mysql_url = format!("{}/{name}", url.trim_end_matches('/'));
        }
        let db = Db::connect(&cfg).await.unwrap();
        db.migrate().await.unwrap();
        (db, dir)
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::test_db;
    use super::*;

    fn msg(id: &str, direction: &str, status: &str) -> Message {
        Message {
            id: id.into(),
            direction: direction.into(),
            message_type: "sms".into(),
            instance_id: "test".into(),
            status: status.into(),
            parts: 1,
            created_at: utils::now(),
            updated_at: utils::now(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn round_trips_messages_with_nulls_blobs_and_unicode() {
        let (db, _d) = test_db().await;
        assert_eq!(db.schema_version().await.unwrap(), db.latest_version());
        assert!(
            db.migrate().await.unwrap().is_empty(),
            "migrations are idempotent"
        );
        let id = utils::new_id();
        let mut m = msg(&id, "inbound", "received");
        m.sender = Some("+4500000000".into());
        m.body = Some("Héllo — 你好 😀 100% _done_ 'quoted'".into());
        m.payload = Some(vec![0, 1, 2, 255]);
        m.message_class = Some(1);
        m.metadata = Some(json!({"k": [1, 2]}));
        m.fingerprint = Some(format!("fp-{id}"));
        db.insert_message(&m, &[]).await.unwrap();
        let got = db.get_message(&id).await.unwrap().unwrap();
        assert_eq!(got.body, m.body);
        assert_eq!(got.payload, m.payload);
        assert_eq!(got.message_class, Some(1));
        assert_eq!(got.recipient, None);
        assert_eq!(got.metadata, m.metadata);
        assert!(db.fingerprint_known(&format!("fp-{id}")).await.unwrap());
        assert!(!db.fingerprint_known("other").await.unwrap());
        // duplicate fingerprint is rejected by the unique index
        let mut dup = msg(&utils::new_id(), "inbound", "received");
        dup.fingerprint = m.fingerprint.clone();
        assert!(db
            .insert_message(&dup, &[])
            .await
            .unwrap_err()
            .is_unique_violation());
        // LIKE wildcards in the search term are literal
        let q = |s: &str| MessageQuery {
            search: Some(s.into()),
            limit: 10,
            ..Default::default()
        };
        assert_eq!(db.search_messages(&q("100%")).await.unwrap().1, 1);
        assert_eq!(db.search_messages(&q("_done_")).await.unwrap().1, 1);
        assert_eq!(db.search_messages(&q("100_")).await.unwrap().1, 0);
        assert_eq!(db.search_messages(&q("x' OR '1'='1")).await.unwrap().1, 0);
        assert_eq!(db.history(&id).await.unwrap().len(), 1);
        assert!(db.delete_message(&id).await.unwrap());
        assert!(db.get_message(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn generic_rows_kv_and_stats() {
        let (db, _d) = test_db().await;
        db.kv_set("a", "1").await.unwrap();
        db.kv_set("a", "2").await.unwrap();
        db.kv_set("a", "2").await.unwrap();
        assert_eq!(db.kv_get("a").await.unwrap().as_deref(), Some("2"));
        assert_eq!(db.kv_get("zz").await.unwrap(), None);
        db.insert_gnss_fix("m", 55.5, 12.25, Some(10.0), None, None, Some(7), None)
            .await
            .unwrap();
        let rows = db.gnss_history(5).await.unwrap();
        assert_eq!(rows[0]["latitude"], json!(55.5));
        assert_eq!(rows[0]["satellites"], json!(7));
        assert_eq!(rows[0]["speed_kmh"], Json::Null);
        db.insert_message(&msg(&utils::new_id(), "inbound", "received"), &[])
            .await
            .unwrap();
        let s = db.stats().await.unwrap();
        assert_eq!(s["messages"]["received"], json!(1));
        let a = db.activity("2000-01-01", 10).await.unwrap();
        assert_eq!(a[0]["count"], json!(1));
        assert_eq!(a[0]["direction"], json!("inbound"));
        assert!(db.verify().await.unwrap()["ok"].as_bool().unwrap());
        assert!(db.ping().await);
        db.upsert_sim_info(
            "m",
            None,
            Some("8900"),
            None,
            None,
            None,
            Some("READY"),
            None,
        )
        .await
        .unwrap();
        db.upsert_sim_info(
            "m",
            Some("s"),
            Some("8901"),
            None,
            None,
            None,
            Some("READY"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            db.count("SELECT COUNT(*) FROM sim_information", vec![])
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn sqlite_backup_is_verified() {
        let (db, d) = test_db().await;
        if db.kind() != DbKind::Sqlite {
            return;
        }
        db.insert_message(&msg(&utils::new_id(), "inbound", "received"), &[])
            .await
            .unwrap();
        let path = db
            .backup(d.path().join("bk").to_str().unwrap())
            .await
            .unwrap();
        verify_sqlite_file(&path).await.unwrap();
        std::fs::write(d.path().join("junk.db"), b"not a database").unwrap();
        assert!(
            verify_sqlite_file(d.path().join("junk.db").to_str().unwrap())
                .await
                .is_err()
        );
    }
}
