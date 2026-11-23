use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::db;

/// Global flag for signal handling.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Run the Hyprland event watcher.
pub fn watch() -> Result<()> {
    let log_path = db::log_path()?;

    // Build desktop file lookup
    let desktop_map = build_desktop_map()?;
    eprintln!(
        "Loaded {} desktop file mappings",
        desktop_map.len()
    );

    // Install signal handlers
    unsafe {
        libc::signal(libc::SIGTERM, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, handle_signal as *const () as libc::sighandler_t);
    }

    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);

    while !SHUTDOWN.load(Ordering::Relaxed) {
        match connect_and_listen(&log_path, &desktop_map) {
            Ok(()) => break,
            Err(e) => {
                if SHUTDOWN.load(Ordering::Relaxed) {
                    break;
                }
                eprintln!("Connection lost: {e}. Reconnecting in {backoff:?}...");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }

    eprintln!("Hyprland watcher stopped.");
    Ok(())
}

/// Get the Hyprland IPC socket path.
fn socket_path() -> Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .context("XDG_RUNTIME_DIR not set")?;
    let instance = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .context("HYPRLAND_INSTANCE_SIGNATURE not set (is Hyprland running?)")?;

    Ok(PathBuf::from(runtime_dir)
        .join("hypr")
        .join(instance)
        .join(".socket2.sock"))
}

/// Connect to the Hyprland socket and process events.
fn connect_and_listen(
    log_path: &std::path::Path,
    desktop_map: &HashMap<String, String>,
) -> Result<()> {
    let path = socket_path()?;
    eprintln!("Connecting to {}", path.display());

    let stream = UnixStream::connect(&path)
        .with_context(|| format!("connecting to {}", path.display()))?;

    // Set a read timeout so we can check the shutdown flag periodically
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;

    let reader = BufReader::new(stream);

    for line in reader.lines() {
        if SHUTDOWN.load(Ordering::Relaxed) {
            return Ok(());
        }

        let line = match line {
            Ok(l) => l,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        // Hyprland event format: "eventname>>data"
        if let Some(data) = line.strip_prefix("openwindow>>") {
            // Format: addr,workspace,class,title
            let parts: Vec<&str> = data.splitn(4, ',').collect();
            if parts.len() >= 3 {
                let class = parts[2];
                if class.is_empty() {
                    continue;
                }

                let bin_name = resolve_class(class, desktop_map);
                let epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;

                if let Err(e) = crate::collect::log_event(log_path, epoch, "gui", &bin_name) {
                    eprintln!("Failed to log event: {e}");
                }

                eprintln!("  window: {class} → {bin_name}");
            }
        }
    }

    Ok(())
}

/// Resolve a window class to a binary name using .desktop files.
fn resolve_class(class: &str, desktop_map: &HashMap<String, String>) -> String {
    let class_lower = class.to_lowercase();

    if let Some(bin) = desktop_map.get(&class_lower) {
        return bin.clone();
    }

    // Fall back to lowercase class name
    class_lower
}

/// Build a map from lowercase WMClass / desktop-id → binary name.
fn build_desktop_map() -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();

    let dirs = [
        PathBuf::from("/usr/share/applications"),
        dirs_desktop_user(),
    ];

    for dir in &dirs {
        if !dir.exists() {
            continue;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }

            if let Ok(contents) = std::fs::read_to_string(&path) {
                parse_desktop_file(&contents, &path, &mut map);
            }
        }
    }

    Ok(map)
}

/// Parse a single .desktop file and add entries to the map.
fn parse_desktop_file(
    contents: &str,
    path: &std::path::Path,
    map: &mut HashMap<String, String>,
) {
    let mut exec_bin: Option<String> = None;
    let mut wm_class: Option<String> = None;
    let mut in_desktop_entry = false;

    for line in contents.lines() {
        let line = line.trim();

        if line == "[Desktop Entry]" {
            in_desktop_entry = true;
            continue;
        }

        if line.starts_with('[') {
            in_desktop_entry = false;
            continue;
        }

        if !in_desktop_entry {
            continue;
        }

        if let Some(val) = line.strip_prefix("Exec=") {
            let val = val.trim();
            let first_word = val.split_whitespace().next().unwrap_or(val);
            let bin = first_word.rsplit('/').next().unwrap_or(first_word);
            exec_bin = Some(bin.to_string());
        }

        if let Some(val) = line.strip_prefix("StartupWMClass=") {
            wm_class = Some(val.trim().to_string());
        }
    }

    let bin = match exec_bin {
        Some(b) => b,
        None => return,
    };

    if let Some(ref wm) = wm_class {
        map.insert(wm.to_lowercase(), bin.clone());
    }

    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        map.insert(stem.to_lowercase(), bin);
    }
}

fn dirs_desktop_user() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home".to_string());
    let data_home = std::env::var("XDG_DATA_HOME")
        .unwrap_or_else(|_| format!("{home}/.local/share"));
    PathBuf::from(data_home).join("applications")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_desktop_file() {
        let contents = r#"[Desktop Entry]
Name=Firefox
Exec=/usr/lib/firefox/firefox %u
StartupWMClass=firefox
Type=Application

[Desktop Action new-window]
Exec=firefox --new-window
"#;
        let mut map = HashMap::new();
        parse_desktop_file(contents, std::path::Path::new("firefox.desktop"), &mut map);

        assert_eq!(map.get("firefox"), Some(&"firefox".to_string()));
    }

    #[test]
    fn test_parse_desktop_file_no_wmclass() {
        let contents = r#"[Desktop Entry]
Name=My App
Exec=myapp --start
Type=Application
"#;
        let mut map = HashMap::new();
        parse_desktop_file(contents, std::path::Path::new("myapp.desktop"), &mut map);

        assert_eq!(map.get("myapp"), Some(&"myapp".to_string()));
    }

    #[test]
    fn test_resolve_class() {
        let mut map = HashMap::new();
        map.insert("firefox".to_string(), "firefox".to_string());
        map.insert("code-oss".to_string(), "code".to_string());

        assert_eq!(resolve_class("firefox", &map), "firefox");
        assert_eq!(resolve_class("Firefox", &map), "firefox");
        assert_eq!(resolve_class("code-oss", &map), "code");
        assert_eq!(resolve_class("unknown-app", &map), "unknown-app");
    }
}
