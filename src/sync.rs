use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;
use std::process::Command;

/// Binary map result: (binary→package map, list of conflicting binaries).
type BinaryMapResult = (HashMap<String, String>, Vec<(String, Vec<String>)>);

/// Run `pkgtrack sync`: rebuild the binary→package map from pacman.
pub fn run(conn: &Connection) -> Result<()> {
    eprintln!("Syncing packages from pacman...");

    let explicit = get_explicit_packages()?;
    eprintln!("  Found {} explicitly installed packages", explicit.len());

    let install_dates = get_install_dates(&explicit)?;
    let (bin_map, conflicts) = get_binary_map(&explicit)?;
    eprintln!("  Found {} binaries across all packages", bin_map.len());

    if !conflicts.is_empty() {
        eprintln!(
            "  Warning: {} binaries claimed by multiple packages:",
            conflicts.len()
        );
        for (bin, pkgs) in &conflicts {
            eprintln!("    {bin}: {}", pkgs.join(", "));
        }
    }

    let tx = conn.unchecked_transaction()?;

    // Upsert packages (never reset existing counts)
    for pkg in &explicit {
        let installed_at = install_dates.get(pkg.as_str()).copied();
        tx.execute(
            "INSERT INTO packages (name, installed_at, use_count)
             VALUES (?1, ?2, 0)
             ON CONFLICT(name) DO UPDATE SET installed_at = ?2",
            rusqlite::params![pkg, installed_at],
        )?;
    }

    // Remove packages no longer installed
    if !explicit.is_empty() {
        let placeholders: Vec<String> =
            (0..explicit.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "DELETE FROM packages WHERE name NOT IN ({})",
            placeholders.join(", ")
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = explicit
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        tx.execute(&sql, params.as_slice())?;
    }

    // Rebuild binaries table
    tx.execute("DELETE FROM binaries", [])?;
    for (bin, pkg) in &bin_map {
        tx.execute(
            "INSERT INTO binaries (bin, pkg) VALUES (?1, ?2)",
            rusqlite::params![bin, pkg],
        )?;
    }

    tx.commit()?;

    let with_bins: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT pkg) FROM binaries",
        [],
        |row| row.get(0),
    )?;
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM packages",
        [],
        |row| row.get(0),
    )?;
    eprintln!(
        "  {with_bins} packages have binaries, {} are untrackable",
        total - with_bins
    );

    eprintln!("Sync complete.");
    Ok(())
}

fn get_explicit_packages() -> Result<Vec<String>> {
    let output = Command::new("pacman")
        .args(["-Qqe"])
        .output()
        .context("running pacman -Qqe")?;

    if !output.status.success() {
        anyhow::bail!(
            "pacman -Qqe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8(output.stdout)?
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

fn get_install_dates(packages: &[String]) -> Result<HashMap<&str, i64>> {
    let mut dates = HashMap::new();

    let output = Command::new("pacman")
        .arg("-Qi")
        .args(packages)
        .output()
        .context("running pacman -Qi")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_pkg: Option<&str> = None;

    for line in stdout.lines() {
        if let Some(name) = line.strip_prefix("Name") {
            let name = name.trim_start_matches([' ', ':']).trim();
            current_pkg = packages
                .iter()
                .find(|p| p.as_str() == name)
                .map(|p| p.as_str());
        } else if let Some(date_str) = line.strip_prefix("Install Date") {
            let date_str = date_str.trim_start_matches([' ', ':']).trim();
            if let Some(pkg) = current_pkg
                && let Ok(epoch) = parse_pacman_date(date_str)
            {
                dates.insert(pkg, epoch);
            }
        }
    }

    Ok(dates)
}

fn parse_pacman_date(s: &str) -> Result<i64> {
    let output = Command::new("date")
        .args(["-d", s, "+%s"])
        .output()
        .context("parsing date")?;

    if !output.status.success() {
        anyhow::bail!("failed to parse date: {s}");
    }

    let epoch: i64 = String::from_utf8(output.stdout)?
        .trim()
        .parse()
        .context("parsing epoch from date")?;

    Ok(epoch)
}

fn get_binary_map(packages: &[String]) -> Result<BinaryMapResult> {
    let mut all_bins: HashMap<String, Vec<String>> = HashMap::new();

    let output = Command::new("pacman")
        .arg("-Ql")
        .args(packages)
        .output()
        .context("running pacman -Ql")?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        let Some((pkg, path)) = line.split_once(' ') else {
            continue;
        };
        let path = path.trim();

        let Some(bin_name) = path.strip_prefix("/usr/bin/") else {
            continue;
        };
        if bin_name.is_empty() || bin_name.contains('/') {
            continue;
        }

        // Keep only regular files and symlinks
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            let ft = meta.file_type();
            if !ft.is_file() && !ft.is_symlink() {
                continue;
            }
        }

        all_bins
            .entry(bin_name.to_string())
            .or_default()
            .push(pkg.to_string());
    }

    let mut map = HashMap::new();
    let mut conflicts = Vec::new();

    for (bin, pkgs) in all_bins {
        let unique_pkgs: Vec<String> = {
            let mut v = pkgs;
            v.sort();
            v.dedup();
            v
        };

        if unique_pkgs.len() > 1 {
            conflicts.push((bin.clone(), unique_pkgs.clone()));
            if let Ok(owner) = resolve_owner(&bin) {
                map.insert(bin, owner);
            } else {
                map.insert(bin, unique_pkgs[0].clone());
            }
        } else {
            map.insert(bin, unique_pkgs.into_iter().next().unwrap());
        }
    }

    Ok((map, conflicts))
}

fn resolve_owner(bin: &str) -> Result<String> {
    let path = format!("/usr/bin/{bin}");
    let output = Command::new("pacman")
        .args(["-Qo", &path])
        .output()
        .context("running pacman -Qo")?;

    if !output.status.success() {
        anyhow::bail!("pacman -Qo failed for {path}");
    }

    let stdout = String::from_utf8(output.stdout)?;
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "by" && i > 0 && parts[i - 1] == "owned"
            && let Some(pkg) = parts.get(i + 1)
        {
            return Ok(pkg.to_string());
        }
    }

    anyhow::bail!("could not parse pacman -Qo output: {stdout}")
}
