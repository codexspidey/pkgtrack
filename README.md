# pkgtrack

A lightweight, zero-overhead usage tracker for user-installed (`pacman -Qqe`) packages on Arch Linux. Works with any interactive Bash shell, with optional Hyprland GUI window tracking.

Whenever you run a command in an interactive Bash shell or open a GUI window under Hyprland, `pkgtrack` records an event. Aggregating these events gives you an accurate count of how often each explicitly installed package is used and when it was last accessed, helping you identify stale packages and decide for yourself what to clean up.

> **Using a different Window Manager / Desktop Environment?** (e.g., KDE Plasma, GNOME, Sway, i3, bspwm): `pkgtrack` works out-of-the-box for all your terminal commands and any GUI apps launched from the shell! The Hyprland watcher daemon is completely optional.

---

## Architecture Overview

`pkgtrack` is designed to be crash-safe, fast, and completely decoupled:

```
[Interactive Bash Hook]  ──(append)─┐
                                    ▼
[Hyprland Window Watcher] ──(append)─► events.log
                                           │
                                           │ (atomic rename on aggregate)
                                           ▼
                                 events.log.processing
                                           │
                                           │ (parse & map to pacman packages)
                                           ▼
                                 SQLite DB (pkgtrack.db)
                                           │
                         ┌─────────────────┼─────────────────┐
                         ▼                 ▼                 ▼
                   pkgtrack top     pkgtrack stale     pkgtrack why
```

- **Collectors never touch SQLite**: Both the shell hook and Hyprland daemon only append simple one-line events to `events.log` (`<epoch> <kind> <name>`), avoiding locking issues and eliminating latency.
- **Atomic rotation & crash recovery**: `aggregate` atomically renames `events.log` &rarr; `events.log.processing` before reading. Any partial or interrupted run is automatically recovered on the next execution.
- **SQLite with WAL mode**: Stores package metadata, binaries mapping, and aggregated usage counts with `PRAGMA foreign_keys=ON` and automatic schema migrations.

---

## Features

- **Tracks Only What Matters**: Monitors only explicitly installed packages (`pacman -Qqe`). Dependencies, base libraries, fonts, and themes without binaries are safely partitioned into `untrackable`.
- **Two Signal Sources**:
  1. **Bash prompt hook**: Ultra-lightweight `PROMPT_COMMAND` hook using bash builtins (zero child process overhead on prompt display).
  2. **Hyprland listener**: Non-polling IPC socket listener (`.socket2.sock`) for window open events (`openwindow>>`).
- **Grace Period Protection**: Packages installed less than N days ago are never marked as stale, protecting new installations.
- **Safety First**: NEVER runs `pacman -R` automatically. Prints suggested removal commands as copy-pasteable text, excluding packages that are required by other installed packages.
- **Zero Heavy Runtime**: Single compiled Rust binary, SQLite with WAL mode, minimal dependencies.

---

## Installation

Clone and build the release binary:

```bash
git clone https://github.com/codexspidey/pkgtrack.git
cd pkgtrack
cargo build --release
cp target/release/pkgtrack ~/.local/bin/
```

Ensure `~/.local/bin` is in your `$PATH`.

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

### 2. (Optional) Hyprland Watcher (GUI Window Tracking)

> **Note for Non-Hyprland Users (KDE Plasma, GNOME, Sway, i3, etc.)**: You can skip this step entirely! The Bash hook alone tracks all terminal and shell-launched commands.

If you use **Hyprland** and want to track GUI applications opened through launchers (like rofi, wofi, or fuzzel), install and enable the provided systemd user unit (from the cloned repository directory):

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

```text
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

---

### Command Examples & Output

#### `pkgtrack top`
Shows the most frequently used packages:
```text
PACKAGE                           COUNT  LAST USED           
──────────────────────────────────────────────────────────────
git                                  38  2026-11-23 15:12    
docker                               34  never               
npm                                   9  never               
vscodium-bin                          7  never               
yay                                   4  never               
```

#### `pkgtrack stale --days 60`
Lists packages not used in the last 60 days (or never used), excluding packages installed recently (within the 60-day grace period):
```text
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

#### `pkgtrack why <pkg>`
Provides detailed package information, including pacman install reason, sizes, dependencies, usage count, and associated binaries:
```text
$ pkgtrack why obs-studio
Package: obs-studio
Required By     : None
Optional For    : None
Installed Size  : 26.77 MiB
Install Reason  : Explicitly installed
Installed: 2026-11-23 13:51
Use count: 0
Last used: never
Binaries: obs, obs-ffmpeg-mux, obs-nvenc-test
```

#### `pkgtrack untrackable`
Identifies explicitly installed packages that do not provide any binaries under `/usr/bin/` (typically fonts, libraries, themes, or header files):
```text
$ pkgtrack untrackable
Explicit packages with no binary in /usr/bin/ (33 packages):
(These are likely libraries, fonts, themes, or header packages)

  archlinux-keyring
  base
  otf-font-awesome
  pipewire-alsa
  pipewire-pulse
  ttf-jetbrains-mono-nerd
  ...
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
- **Shell Aliases & Functions**: Bash aliases (e.g. `alias g=git`) and custom functions are evaluated in the shell environment. `pkgtrack` parses the typed tokens directly. Common wrappers (`sudo`, `doas`, `env`, `exec`, `builtin`) are stripped, but custom aliases without `alias` resolution in Rust are logged as their alias name.
- **Complex Subshells**: Command substitutions inside arguments (e.g. `cmd $(subcmd)`) log the outer command; deep subshell parsing is avoided to keep prompt latency at sub-millisecond speeds.

---

## File Locations (XDG Compliant)

- **Database**: `${XDG_DATA_HOME:-~/.local/share}/pkgtrack/pkgtrack.db`
- **Event Log**: `${XDG_DATA_HOME:-~/.local/share}/pkgtrack/events.log`
- **Processing Temporary**: `${XDG_DATA_HOME:-~/.local/share}/pkgtrack/events.log.processing`
