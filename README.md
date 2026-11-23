# pkgtrack

A lightweight, zero-overhead usage tracker for user-installed (`pacman -Qqe`) packages on Arch Linux with Hyprland and Bash.

Whenever you run a command in an interactive Bash shell or open a GUI window in Hyprland, `pkgtrack` records an event. Aggregating these events gives you an accurate count of how often each explicitly installed package is used and when it was last accessed, helping you identify stale packages and keep your system clean.

---

## Features

- **Tracks Only What Matters**: Monitors only explicitly installed packages (`pacman -Qqe`). Dependencies, libraries, fonts, and themes without binaries are safely partitioned into `untrackable`.
- **Two Signal Sources**:
  1. **Bash prompt hook**: Ultra-lightweight `PROMPT_COMMAND` hook using bash builtins (no subprocesses on prompt display).
  2. **Hyprland listener**: Non-polling IPC socket listener (`.socket2.sock`) for window open events (`openwindow>>`).
- **Crash-Safe Append-Only Logging**: Collectors append single lines to `events.log` without touching SQLite. Aggregation uses atomic file renaming (`events.log` &rarr; `events.log.processing`) with automatic crash recovery.
- **Grace Period Protection**: Packages installed less than N days ago are never marked as stale, protecting new installations.
- **Safety First**: NEVER runs `pacman -R` automatically. Prints suggested removal commands as copy-pasteable text, excluding packages that are required by other installed packages.
- **Zero Heavy Runtime**: Single Rust binary, SQLite with WAL mode, minimal dependencies.

---

## Installation

Build and install the binary using Cargo:

```bash
cargo build --release
cp target/release/pkgtrack ~/.local/bin/
# or: cargo install --path .
```

Ensure `~/.local/bin` (or `~/.cargo/bin`) is in your `$PATH`.

Initialize your package database:

```bash
pkgtrack sync
```

---

## Setup

### 1. Bash Hook (Shell Tracking)

Add this line to your `~/.bashrc`:

```bash
eval "$(pkgtrack init bash)"
```

- **New Kitty / terminal windows** will automatically load the hook.
- For **existing terminal sessions**, run:
  ```bash
  source ~/.bashrc
  ```

#### How it works:
- Uses `PROMPT_COMMAND` with `builtin history 1`.
- Tracks `$HISTCMD` to count every invocation—including identical consecutive commands—while ignoring empty Enter presses.
- Uses `printf '%(%s)T'` builtin for epoch timestamps with zero child process overhead.
- Idempotent: sourcing `~/.bashrc` multiple times will not duplicate hooks or log events twice.
- Strips leading prefixes (`sudo`, `doas`, `env`, `time`, `nohup`, `exec`, `command`, `builtin`) and `VAR=value` assignments. Handles `&&`, `||`, `;`, and `|` command chains.

### 2. Hyprland Watcher (GUI Window Tracking)

Run the watcher in the foreground or as a systemd user service.

To install as a systemd user service:

```bash
mkdir -p ~/.config/systemd/user
cp pkgtrack-watch.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now pkgtrack-watch
```

Check service status:
```bash
systemctl --user status pkgtrack-watch
```

#### How it works:
- Connects to `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`.
- Reacts only to `openwindow>>` events (window opens, not focus changes).
- Resolves the window class against `/usr/share/applications/*.desktop` and `~/.local/share/applications/*.desktop` via `StartupWMClass` or `Exec=` binary name, falling back to lowercase class name.
- Automatically reconnects with exponential backoff if the socket disappears (e.g. after Hyprland restarts), and exits cleanly on SIGTERM/SIGINT.

### 3. Optional History Bootstrap

To seed initial usage counts from your existing `~/.bash_history`:

```bash
pkgtrack bootstrap-history --yes
```

- Scans history commands using the same tokenization logic.
- Idempotent: marked in the database metadata so running it twice will not double-count.
- Leaves `last_used` timestamps unset (`never`) since historical timestamps cannot be reliably determined without prior `HISTTIMEFORMAT`.

### 4. Optional Pacman Hook

A pacman post-transaction hook is provided at `pkgtrack.hook`.

```bash
sudo cp pkgtrack.hook /etc/pacman.d/hooks/
```

> **Design Choice**: Pacman hooks run as `root` during system updates. Writing directly to a regular user's `~/.local/share/pkgtrack/pkgtrack.db` from root can cause permission conflicts, ownership issues, and requires hardcoding the target user account. Therefore, `pkgtrack.hook` defaults to manual-only or a template using `sudo -u <username>`. We recommend running `pkgtrack sync` manually when desired, or adjusting the hook with your username as documented inside `pkgtrack.hook`.

---

## CLI Reference

```
pkgtrack init bash               # Output bash shell hook for eval in ~/.bashrc
pkgtrack sync                    # Rebuild binary → package mapping from pacman
pkgtrack watch                   # Hyprland window event listener (foreground daemon)
pkgtrack aggregate               # Flush events.log into SQLite database
pkgtrack top [-n 20] [--json]    # Show most used packages
pkgtrack stale [--days 60] [--json] # Show packages unused for N days
pkgtrack why <pkg>               # Detailed package info (install reason, size, dependencies)
pkgtrack untrackable             # List explicit packages without /usr/bin binaries
pkgtrack bootstrap-history --yes # Seed usage counts from ~/.bash_history
```

`top`, `stale`, and `why` automatically call `aggregate` before querying, ensuring fresh data.

### Example Outputs

#### `pkgtrack top`
```
PACKAGE                           COUNT  LAST USED           
──────────────────────────────────────────────────────────────
git                                  37  2026-11-23 14:31    
docker                               35  never               
npm                                   9  never               
vscodium-bin                          7  never               
```

#### `pkgtrack stale --days 60`
```
Packages not used in 60 days (or never used), installed before cutoff:

PACKAGE                      INSTALLED  COUNT  LAST USED               SIZE
────────────────────────────────────────────────────────────────────────────
onboard                     2026-09-23      0  never               28.0 MiB
fish                        2026-09-12      0  never               18.2 MiB
awww                        2026-06-21      0  never               10.6 MiB
thunar                      2026-09-14      0  never                9.6 MiB
...
sddm                        2026-06-19      0  never                5.3 MiB ⚠
less                        2026-09-24      0  never              337.7 KiB ⚠

⚠  The following stale packages are required by other packages:
   less: required by man-db
   sddm: required by sddm-astronaut-theme

Suggested removal (review carefully before running!):
  sudo pacman -Rns onboard fish awww thunar ...
```

---

## Scope & Limitations

### What is tracked:
- Explicitly installed packages (`pacman -Qqe`).
- Executables in `/usr/bin/` associated with those packages.
- Bash commands executed interactively.
- GUI application window launches in Hyprland.

### What is NOT tracked (by design):
- System dependencies, base packages, fonts, icon themes, shared libraries (see `pkgtrack untrackable`).
- Background cron jobs, timers, systemd services, or non-interactive scripts.
- Shells other than Bash (e.g. Zsh, Fish).
- Automated package removal (pkgtrack is strictly an advisory tool).

### Known Limitations:
- **Shell Aliases & Functions**: Bash aliases (e.g. `alias g=git`) and custom functions are evaluated in the shell environment. `pkgtrack` parses the typed tokens directly. Common wrappers (`sudo`, `env`) are stripped, but custom aliases without `alias` resolution in Rust are logged as their alias name.
- **Complex Subshells**: Command substitutions inside arguments (e.g. `cmd $(subcmd)`) log the outer command; deep subshell parsing is avoided to keep prompt latency at sub-millisecond speeds.

---

## File Locations (XDG Compliant)

- **Database**: `${XDG_DATA_HOME:-~/.local/share}/pkgtrack/pkgtrack.db`
- **Event Log**: `${XDG_DATA_HOME:-~/.local/share}/pkgtrack/events.log`
- **Processing Temporary**: `${XDG_DATA_HOME:-~/.local/share}/pkgtrack/events.log.processing`
