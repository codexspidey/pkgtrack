use anyhow::Result;
use clap::{Parser, Subcommand};
use std::collections::HashMap;

use crate::{collect, db, hypr, sync};

#[derive(Parser)]
#[command(name = "pkgtrack", about = "Track usage of explicitly installed Arch packages")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print the bash shell hook for eval
    Init {
        /// Shell type (currently only bash)
        shell: String,
    },

    /// Rebuild the binary→package map from pacman
    Sync,

    /// Watch Hyprland for GUI window opens (foreground)
    Watch,

    /// Flush event log into the database
    Aggregate,

    /// Show most-used packages
    Top {
        /// Number of packages to show
        #[arg(short, long, default_value = "20")]
        n: usize,
        /// Output JSON instead of table
        #[arg(long)]
        json: bool,
    },

    /// Show packages not used in N days
    Stale {
        /// Days of inactivity to consider stale
        #[arg(long, default_value = "60")]
        days: u64,
        /// Output JSON instead of table
        #[arg(long)]
        json: bool,
    },

    /// Show details about a specific package
    Why {
        /// Package name
        pkg: String,
    },

    /// List explicit packages with no trackable binary
    Untrackable,

    /// Seed usage counts from ~/.bash_history
    BootstrapHistory {
        /// Confirm you want to run this (required)
        #[arg(long)]
        yes: bool,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { shell } => cmd_init(&shell),
        Commands::Sync => {
            let conn = db::open()?;
            sync::run(&conn)
        }
        Commands::Watch => hypr::watch(),
        Commands::Aggregate => {
            let conn = db::open()?;
            collect::aggregate(&conn)
        }
        Commands::Top { n, json } => {
            let conn = db::open()?;
            collect::aggregate(&conn)?;
            cmd_top(&conn, n, json)
        }
        Commands::Stale { days, json } => {
            let conn = db::open()?;
            collect::aggregate(&conn)?;
            cmd_stale(&conn, days, json)
        }
        Commands::Why { pkg } => {
            let conn = db::open()?;
            collect::aggregate(&conn)?;
            cmd_why(&conn, &pkg)
        }
        Commands::Untrackable => {
            let conn = db::open()?;
            cmd_untrackable(&conn)
        }
        Commands::BootstrapHistory { yes } => {
            let conn = db::open()?;
            cmd_bootstrap_history(&conn, yes)
        }
    }
}

fn cmd_init(shell: &str) -> Result<()> {
    if shell != "bash" {
        anyhow::bail!("Only bash is currently supported. Got: {shell}");
    }

    print!("{}", include_str!("bash_hook.sh"));
    Ok(())
}

fn cmd_top(conn: &rusqlite::Connection, n: usize, json: bool) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT name, use_count, last_used FROM packages
         WHERE use_count > 0
         ORDER BY use_count DESC
         LIMIT ?1",
    )?;

    let rows: Vec<(String, i64, Option<i64>)> = stmt
        .query_map([n as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    if json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|(name, count, last)| {
                serde_json::json!({
                    "name": name,
                    "use_count": count,
                    "last_used": last,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else if rows.is_empty() {
        println!("No usage data yet. Run some commands and try again.");
    } else {
        println!("{:<30} {:>8}  {:<20}", "PACKAGE", "COUNT", "LAST USED");
        println!("{}", "─".repeat(62));
        for (name, count, last) in &rows {
            let last_str = last
                .map(format_epoch)
                .unwrap_or_else(|| "never".to_string());
            println!("{:<30} {:>8}  {:<20}", name, count, last_str);
        }
    }

    Ok(())
}

/// Stale package record from the database.
pub struct StalePackage {
    pub name: String,
    pub installed_at: Option<i64>,
    pub use_count: i64,
    pub last_used: Option<i64>,
}

/// Query stale packages from the database given a cutoff timestamp.
pub fn query_stale_packages(
    conn: &rusqlite::Connection,
    cutoff_epoch: i64,
) -> Result<Vec<StalePackage>> {
    let mut stmt = conn.prepare(
        "SELECT p.name, p.installed_at, p.use_count, p.last_used
         FROM packages p
         WHERE (p.use_count = 0 OR p.last_used < ?1 OR p.last_used IS NULL)
           AND (p.installed_at IS NULL OR p.installed_at < ?1)
           AND EXISTS (SELECT 1 FROM binaries b WHERE b.pkg = p.name)
         ORDER BY p.use_count ASC, p.name ASC",
    )?;

    let rows = stmt
        .query_map([cutoff_epoch], |row| {
            Ok(StalePackage {
                name: row.get(0)?,
                installed_at: row.get(1)?,
                use_count: row.get(2)?,
                last_used: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// Data for a stale package entry with size.
struct StaleEntry {
    name: String,
    installed_at: Option<i64>,
    use_count: i64,
    last_used: Option<i64>,
    size: u64,
}

fn cmd_stale(conn: &rusqlite::Connection, days: u64, json: bool) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let cutoff = now - (days as i64 * 86400);

    let rows = query_stale_packages(conn, cutoff)?;

    let mut entries: Vec<StaleEntry> = rows
        .into_iter()
        .map(|pkg| {
            let size = get_installed_size(&pkg.name).unwrap_or(0);
            StaleEntry {
                name: pkg.name,
                installed_at: pkg.installed_at,
                use_count: pkg.use_count,
                last_used: pkg.last_used,
                size,
            }
        })
        .collect();

    // Sort by size descending
    entries.sort_by_key(|e| std::cmp::Reverse(e.size));

    // Check for packages that are required by others
    let pkg_names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let required_pkgs = check_required_by(&pkg_names)?;

    if json {
        let items: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "installed_at": e.installed_at,
                    "use_count": e.use_count,
                    "last_used": e.last_used,
                    "installed_size": e.size,
                    "required_by": required_pkgs.get(e.name.as_str()),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else if entries.is_empty() {
        println!("No stale packages found (threshold: {days} days).");
    } else {
        println!(
            "Packages not used in {days} days (or never used), installed before cutoff:"
        );
        println!();
        println!(
            "{:<25} {:>12}  {:>5}  {:<16}  {:>10}",
            "PACKAGE", "INSTALLED", "COUNT", "LAST USED", "SIZE"
        );
        println!("{}", "─".repeat(76));

        let mut safe_to_remove = Vec::new();
        for e in &entries {
            let inst_str = e
                .installed_at
                .map(format_date)
                .unwrap_or_else(|| "unknown".to_string());
            let last_str = e
                .last_used
                .map(format_epoch)
                .unwrap_or_else(|| "never".to_string());
            let size_str = format_size(e.size);

            let marker = if required_pkgs.contains_key(e.name.as_str()) {
                " ⚠"
            } else {
                ""
            };

            println!(
                "{:<25} {:>12}  {:>5}  {:<16}  {:>10}{}",
                e.name, inst_str, e.use_count, last_str, size_str, marker
            );

            if !required_pkgs.contains_key(e.name.as_str()) {
                safe_to_remove.push(e.name.as_str());
            }
        }

        if !required_pkgs.is_empty() {
            println!();
            println!("⚠  The following stale packages are required by other packages:");
            for (pkg, deps) in &required_pkgs {
                println!("   {pkg}: required by {}", deps.join(", "));
            }
        }

        if !safe_to_remove.is_empty() {
            println!();
            println!("Suggested removal (review carefully before running!):");
            println!("  sudo pacman -Rns {}", safe_to_remove.join(" "));
        }
    }

    Ok(())
}

fn cmd_why(conn: &rusqlite::Connection, pkg: &str) -> Result<()> {
    let result = conn.query_row(
        "SELECT name, installed_at, use_count, last_used FROM packages WHERE name = ?1",
        [pkg],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        },
    );

    let (name, installed_at, use_count, last_used) = match result {
        Ok(r) => r,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            eprintln!("Package '{pkg}' not found in pkgtrack DB. Run `pkgtrack sync` first.");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    println!("Package: {name}");

    if let Ok(output) = std::process::Command::new("pacman")
        .args(["-Qi", &name])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.starts_with("Install Reason")
                || line.starts_with("Installed Size")
                || line.starts_with("Required By")
                || line.starts_with("Optional For")
            {
                println!("{}", line.trim());
            }
        }
    }

    let inst_str = installed_at
        .map(format_epoch)
        .unwrap_or_else(|| "unknown".to_string());
    println!("Installed: {inst_str}");
    println!("Use count: {use_count}");

    let last_str = last_used
        .map(format_epoch)
        .unwrap_or_else(|| "never".to_string());
    println!("Last used: {last_str}");

    let mut stmt = conn.prepare("SELECT bin FROM binaries WHERE pkg = ?1 ORDER BY bin")?;
    let bins: Vec<String> = stmt
        .query_map([&name], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    if bins.is_empty() {
        println!("Binaries: (none — untrackable)");
    } else {
        println!("Binaries: {}", bins.join(", "));
    }

    Ok(())
}

fn cmd_untrackable(conn: &rusqlite::Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT p.name FROM packages p
         WHERE NOT EXISTS (SELECT 1 FROM binaries b WHERE b.pkg = p.name)
         ORDER BY p.name",
    )?;

    let rows: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    if rows.is_empty() {
        println!("All explicit packages have at least one binary.");
    } else {
        println!(
            "Explicit packages with no binary in /usr/bin/ ({} packages):",
            rows.len()
        );
        println!("(These are likely libraries, fonts, themes, or header packages)");
        println!();
        for name in &rows {
            println!("  {name}");
        }
    }

    Ok(())
}

fn cmd_bootstrap_history(conn: &rusqlite::Connection, yes: bool) -> Result<()> {
    if !yes {
        println!("This will seed usage counts from ~/.bash_history.");
        println!("Run with --yes to confirm.");
        println!();
        println!("Note: This is idempotent — running it twice won't double-count.");
        return Ok(());
    }

    if db::get_meta(conn, "history_bootstrapped").is_some() {
        println!("History bootstrap has already been run. Skipping to prevent double-counting.");
        return Ok(());
    }

    let hist_path = std::env::var("HISTFILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("HOME not set");
            std::path::PathBuf::from(home).join(".bash_history")
        });

    if !hist_path.exists() {
        anyhow::bail!("History file not found: {}", hist_path.display());
    }

    let contents = std::fs::read_to_string(&hist_path)?;
    let bin_to_pkg = build_bin_lookup(conn)?;

    let mut pkg_counts: HashMap<String, i64> = HashMap::new();
    let mut total_lines = 0u64;
    let mut matched = 0u64;

    for line in contents.lines() {
        let line = line.trim();

        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        total_lines += 1;

        let commands = crate::parse::extract_commands(line);
        for cmd in &commands {
            if let Some(pkg) = bin_to_pkg.get(cmd.as_str()) {
                *pkg_counts.entry(pkg.clone()).or_insert(0) += 1;
                matched += 1;
            }
        }
    }

    let tx = conn.unchecked_transaction()?;
    for (pkg, count) in &pkg_counts {
        tx.execute(
            "UPDATE packages SET use_count = use_count + ?1 WHERE name = ?2",
            rusqlite::params![count, pkg],
        )?;
    }

    db::set_meta(&tx, "history_bootstrapped", "1")?;
    tx.commit()?;

    println!("Processed {total_lines} history lines.");
    println!("Matched {matched} commands to {} packages.", pkg_counts.len());
    println!("Counts updated (last_used not set — no reliable timestamps).");

    Ok(())
}

fn build_bin_lookup(conn: &rusqlite::Connection) -> Result<HashMap<String, String>> {
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

fn get_installed_size(pkg: &str) -> Result<u64> {
    let output = std::process::Command::new("pacman")
        .args(["-Qi", pkg])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(val) = line.strip_prefix("Installed Size") {
            let val = val.trim_start_matches([' ', ':']).trim();
            return parse_size(val);
        }
    }

    Ok(0)
}

fn parse_size(s: &str) -> Result<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 2 {
        return Ok(0);
    }

    let num: f64 = parts[0].parse().unwrap_or(0.0);
    let multiplier = match parts[1] {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };

    Ok((num * multiplier) as u64)
}

fn check_required_by(packages: &[&str]) -> Result<HashMap<String, Vec<String>>> {
    let mut required = HashMap::new();

    for pkg in packages {
        let output = std::process::Command::new("pacman")
            .args(["-Qi", pkg])
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(val) = line.strip_prefix("Required By") {
                let val = val.trim_start_matches([' ', ':']).trim();
                if val != "None" {
                    let deps: Vec<String> =
                        val.split_whitespace().map(String::from).collect();
                    if !deps.is_empty() {
                        required.insert(pkg.to_string(), deps);
                    }
                }
            }
        }
    }

    Ok(required)
}

fn format_epoch(epoch: i64) -> String {
    let output = std::process::Command::new("date")
        .args(["-d", &format!("@{epoch}"), "+%Y-%m-%d %H:%M"])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => epoch.to_string(),
    }
}

fn format_date(epoch: i64) -> String {
    let output = std::process::Command::new("date")
        .args(["-d", &format!("@{epoch}"), "+%Y-%m-%d"])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => epoch.to_string(),
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_stale_date_logic() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test_stale.db");
        let conn = crate::db::open_at(&db_path).unwrap();

        let now = 1_700_000_000i64;
        let days = 60;
        let cutoff = now - (days * 86400);

        // 1. Old package, never used -> STALE
        conn.execute(
            "INSERT INTO packages (name, installed_at, use_count, last_used)
             VALUES ('old-unused', ?1, 0, NULL)",
            [cutoff - 100],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO binaries (bin, pkg) VALUES ('old-unused', 'old-unused')",
            [],
        )
        .unwrap();

        // 2. Newly installed package (< 60 days ago), never used -> NOT STALE (grace period)
        conn.execute(
            "INSERT INTO packages (name, installed_at, use_count, last_used)
             VALUES ('new-unused', ?1, 0, NULL)",
            [cutoff + 100],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO binaries (bin, pkg) VALUES ('new-unused', 'new-unused')",
            [],
        )
        .unwrap();

        // 3. Old package, used recently -> NOT STALE
        conn.execute(
            "INSERT INTO packages (name, installed_at, use_count, last_used)
             VALUES ('used-recent', ?1, 5, ?2)",
            [cutoff - 1000, cutoff + 50],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO binaries (bin, pkg) VALUES ('used-recent', 'used-recent')",
            [],
        )
        .unwrap();

        // 4. Old package, used before cutoff -> STALE
        conn.execute(
            "INSERT INTO packages (name, installed_at, use_count, last_used)
             VALUES ('used-stale', ?1, 2, ?2)",
            [cutoff - 1000, cutoff - 50],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO binaries (bin, pkg) VALUES ('used-stale', 'used-stale')",
            [],
        )
        .unwrap();

        // 5. Untrackable package (no binaries), old and unused -> NOT STALE (untrackable excluded)
        conn.execute(
            "INSERT INTO packages (name, installed_at, use_count, last_used)
             VALUES ('libfoo', ?1, 0, NULL)",
            [cutoff - 1000],
        )
        .unwrap();

        let stale = query_stale_packages(&conn, cutoff).unwrap();
        let stale_names: Vec<String> = stale.into_iter().map(|s| s.name).collect();

        assert!(stale_names.contains(&"old-unused".to_string()));
        assert!(stale_names.contains(&"used-stale".to_string()));
        assert!(
            !stale_names.contains(&"new-unused".to_string()),
            "Grace period must protect new installs"
        );
        assert!(
            !stale_names.contains(&"used-recent".to_string()),
            "Recently used must not be stale"
        );
        assert!(
            !stale_names.contains(&"libfoo".to_string()),
            "Packages with no binaries must not be in stale"
        );
    }
}
