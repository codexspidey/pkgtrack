use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use crate::db;
use crate::parse;

/// Append a single event to the log file.
/// Format: `<epoch_seconds> <kind> <name>`
pub fn log_event(log_path: &std::path::Path, epoch: i64, kind: &str, name: &str) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    writeln!(file, "{epoch} {kind} {name}")?;
    Ok(())
}

/// Run the aggregation: read events.log, map to packages, update counts.
///
/// Uses atomic rename for crash safety:
/// 1. Rename events.log → events.log.processing
/// 2. Read and process
/// 3. Delete the processing file
///
/// If a .processing file already exists (crash recovery), process it first.
pub fn aggregate(conn: &Connection) -> Result<()> {
    let data_dir = db::data_dir()?;
    let log_path = data_dir.join("events.log");
    let proc_path = data_dir.join("events.log.processing");

    // Phase 1: Process any leftover .processing file from a previous crash
    if proc_path.exists() {
        eprintln!("Found leftover processing file, resuming...");
        process_log_file(conn, &proc_path)?;
        fs::remove_file(&proc_path)
            .with_context(|| format!("removing {}", proc_path.display()))?;
    }

    // Phase 2: Atomically rename current log and process it
    if log_path.exists() {
        // Check if log is empty
        let metadata = fs::metadata(&log_path)?;
        if metadata.len() == 0 {
            return Ok(());
        }

        fs::rename(&log_path, &proc_path)
            .with_context(|| "renaming events.log to events.log.processing")?;
        process_log_file(conn, &proc_path)?;
        fs::remove_file(&proc_path)
            .with_context(|| format!("removing {}", proc_path.display()))?;
    }

    Ok(())
}

/// Process a single log file, updating package counts in the database.
fn process_log_file(conn: &Connection, path: &PathBuf) -> Result<()> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);

    // Build a binary → package lookup from the DB
    let bin_to_pkg = build_bin_lookup(conn)?;

    // Accumulate counts: pkg → (count, max_timestamp)
    let mut pkg_stats: HashMap<String, (i64, i64)> = HashMap::new();
    let mut processed = 0u64;
    let mut skipped = 0u64;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse: <epoch> <kind> <name>
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        if parts.len() < 3 {
            skipped += 1;
            continue;
        }

        let epoch: i64 = match parts[0].parse() {
            Ok(e) => e,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let _kind = parts[1]; // "shell" or "gui"
        let name = parts[2];

        // For shell events, extract command words and map each to a package
        let commands = if name.contains(' ') {
            // This shouldn't normally happen since we log single binary names,
            // but handle it defensively
            parse::extract_commands(name)
        } else {
            vec![name.to_string()]
        };

        for cmd in &commands {
            if let Some(pkg) = resolve_binary_to_pkg(cmd, &bin_to_pkg) {
                let entry = pkg_stats.entry(pkg.to_string()).or_insert((0, 0));
                entry.0 += 1;
                entry.1 = entry.1.max(epoch);
                processed += 1;
            }
            // Unknown commands are silently ignored
        }
    }

    if skipped > 0 {
        eprintln!("  Skipped {skipped} malformed log lines");
    }

    // Update the database
    let tx = conn.unchecked_transaction()?;
    for (pkg, (count, last_used)) in &pkg_stats {
        tx.execute(
            "UPDATE packages SET
                use_count = use_count + ?1,
                last_used = MAX(COALESCE(last_used, 0), ?2)
             WHERE name = ?3",
            rusqlite::params![count, last_used, pkg],
        )?;
    }
    tx.commit()?;

    if processed > 0 || skipped > 0 {
        eprintln!("  Aggregated {processed} events for {} packages", pkg_stats.len());
    }

    Ok(())
}

/// Resolve a command or path to a package name, handling paths and symlinks where cheap.
fn resolve_binary_to_pkg<'a>(cmd: &'a str, bin_to_pkg: &'a HashMap<String, String>) -> Option<&'a str> {
    if let Some(pkg) = bin_to_pkg.get(cmd) {
        return Some(pkg.as_str());
    }

    let base = if cmd.contains('/') {
        cmd.rsplit('/').next().unwrap_or(cmd)
    } else {
        cmd
    };

    if let Some(pkg) = bin_to_pkg.get(base) {
        return Some(pkg.as_str());
    }

    // If it was a path, try canonicalizing
    if cmd.contains('/') {
        if let Ok(canon) = std::fs::canonicalize(cmd)
            && let Some(name) = canon.file_name().and_then(|n| n.to_str())
            && let Some(pkg) = bin_to_pkg.get(name)
        {
            return Some(pkg.as_str());
        }
    } else {
        let usr_bin_path = format!("/usr/bin/{cmd}");
        if let Ok(canon) = std::fs::canonicalize(&usr_bin_path)
            && let Some(name) = canon.file_name().and_then(|n| n.to_str())
            && let Some(pkg) = bin_to_pkg.get(name)
        {
            return Some(pkg.as_str());
        }
    }

    None
}

/// Build a binary name → package name lookup from the database.
fn build_bin_lookup(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT bin, pkg FROM binaries")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut map = HashMap::new();
    for row in rows {
        let (bin, pkg) = row?;
        map.insert(bin, pkg);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_test_db() -> (Connection, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = db::open_at(&db_path).unwrap();

        // Insert test data
        conn.execute(
            "INSERT INTO packages (name, installed_at) VALUES ('vim', 1000000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO packages (name, installed_at) VALUES ('git', 1000000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO binaries (bin, pkg) VALUES ('vim', 'vim')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO binaries (bin, pkg) VALUES ('vimdiff', 'vim')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO binaries (bin, pkg) VALUES ('git', 'git')",
            [],
        )
        .unwrap();

        (conn, dir)
    }

    #[test]
    fn test_process_log_file() {
        let (conn, dir) = setup_test_db();
        let log_path = dir.path().join("events.log");

        let mut f = fs::File::create(&log_path).unwrap();
        writeln!(f, "1700000000 shell vim").unwrap();
        writeln!(f, "1700000001 shell git").unwrap();
        writeln!(f, "1700000002 shell vim").unwrap();
        writeln!(f, "1700000003 shell unknown_cmd").unwrap();
        writeln!(f, "bad line").unwrap();
        drop(f);

        process_log_file(&conn, &log_path.to_path_buf()).unwrap();

        let (count, last): (i64, i64) = conn
            .query_row(
                "SELECT use_count, last_used FROM packages WHERE name = 'vim'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(last, 1700000002);

        let (count, last): (i64, i64) = conn
            .query_row(
                "SELECT use_count, last_used FROM packages WHERE name = 'git'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(last, 1700000001);
    }

    #[test]
    fn test_aggregate_crash_recovery() {
        let (conn, dir) = setup_test_db();

        // Simulate a crash: .processing file exists but events.log doesn't
        let proc_path = dir.path().join("events.log.processing");
        let mut f = fs::File::create(&proc_path).unwrap();
        writeln!(f, "1700000000 shell vim").unwrap();
        drop(f);

        // We can't easily test aggregate() since it uses data_dir(),
        // but we can test process_log_file
        process_log_file(&conn, &proc_path.to_path_buf()).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT use_count FROM packages WHERE name = 'vim'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_incremental_aggregation() {
        let (conn, dir) = setup_test_db();
        let log_path = dir.path().join("events.log");

        // First batch
        let mut f = fs::File::create(&log_path).unwrap();
        writeln!(f, "1700000000 shell vim").unwrap();
        drop(f);
        process_log_file(&conn, &log_path.to_path_buf()).unwrap();

        // Second batch
        let mut f = fs::File::create(&log_path).unwrap();
        writeln!(f, "1700000010 shell vim").unwrap();
        drop(f);
        process_log_file(&conn, &log_path.to_path_buf()).unwrap();

        let (count, last): (i64, i64) = conn
            .query_row(
                "SELECT use_count, last_used FROM packages WHERE name = 'vim'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(last, 1700000010);
    }
}

