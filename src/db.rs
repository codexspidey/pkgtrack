use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Current schema version.
const SCHEMA_VERSION: i64 = 1;

/// Return the pkgtrack data directory, creating it if needed.
pub fn data_dir() -> Result<PathBuf> {
    let base = match std::env::var("XDG_DATA_HOME") {
        Ok(val) if !val.trim().is_empty() => PathBuf::from(val),
        _ => {
            let home = std::env::var("HOME").context("HOME not set")?;
            PathBuf::from(home).join(".local").join("share")
        }
    };
    let dir = base.join("pkgtrack");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating data dir {}", dir.display()))?;
    Ok(dir)
}

/// Path to events.log
pub fn log_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("events.log"))
}

/// Path to database
pub fn db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("pkgtrack.db"))
}

/// Open (or create) the database, run migrations, return connection.
pub fn open() -> Result<Connection> {
    let path = db_path()?;
    open_at(&path)
}

/// Open a database at a specific path (useful for testing).
pub fn open_at(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("opening database {}", path.display()))?;

    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    // Create meta table if it doesn't exist
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);",
    )?;

    let version = get_meta_int(conn, "schema_version").unwrap_or(0);

    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS packages (
                name TEXT PRIMARY KEY,
                installed_at INTEGER,
                use_count INTEGER NOT NULL DEFAULT 0,
                last_used INTEGER
            );
            CREATE TABLE IF NOT EXISTS binaries (
                bin TEXT PRIMARY KEY,
                pkg TEXT NOT NULL REFERENCES packages(name) ON DELETE CASCADE
            );",
        )?;
        set_meta_int(conn, "schema_version", 1)?;
    }

    // Future migrations go here: if version < 2 { ... }

    let current = get_meta_int(conn, "schema_version").unwrap_or(0);
    if current != SCHEMA_VERSION {
        anyhow::bail!(
            "unexpected schema version {current}, expected {SCHEMA_VERSION}"
        );
    }

    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        [key],
        |row| row.get(0),
    )
    .ok()
}

pub fn get_meta_int(conn: &Connection, key: &str) -> Option<i64> {
    get_meta(conn, key).and_then(|v| v.parse().ok())
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

pub fn set_meta_int(conn: &Connection, key: &str, value: i64) -> Result<()> {
    set_meta(conn, key, &value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migration_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrate(&conn).unwrap();

        // Check tables exist
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='packages'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='binaries'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Check schema version
        assert_eq!(get_meta_int(&conn, "schema_version"), Some(1));
    }

    #[test]
    fn test_migration_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap(); // should not fail
        assert_eq!(get_meta_int(&conn, "schema_version"), Some(1));
    }

    #[test]
    fn test_meta_operations() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();

        set_meta(&conn, "foo", "bar").unwrap();
        assert_eq!(get_meta(&conn, "foo"), Some("bar".to_string()));

        set_meta_int(&conn, "num", 42).unwrap();
        assert_eq!(get_meta_int(&conn, "num"), Some(42));

        // Update existing key
        set_meta(&conn, "foo", "baz").unwrap();
        assert_eq!(get_meta(&conn, "foo"), Some("baz".to_string()));
    }
}

