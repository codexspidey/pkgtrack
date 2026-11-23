# pkgtrack

A lightweight package usage tracker ,i built it so that i can keep track of the packages that im using so that during an update i have a fair idea in terms of what packages are getting updated and do i even use them ?

Whenever you run a command in Bash or open a GUI window in Hyprland, `pkgtrack` counts the invocation and updates its timestamp. Use it to find stale packages and see what you can safely remove.

---

## Quick Start

### 1. Install & Sync

```bash
git clone https://github.com/codexspidey/pkgtrack.git
cd pkgtrack
cargo build --release
cp target/release/pkgtrack ~/.local/bin/

pkgtrack sync
```

### 2. Enable Shell Tracking

Add to your `~/.bashrc`:

```bash
eval "$(pkgtrack init bash)"
```

Then reload your shell:
```bash
source ~/.bashrc
```

*(Optional) Seed counts from existing history:*
```bash
pkgtrack bootstrap-history --yes
```

### 3. (Optional) Hyprland GUI Tracking

If you use Hyprland and want to track GUI apps opened via app launchers (rofi, wofi, etc.):

```bash
mkdir -p ~/.config/systemd/user
cp pkgtrack-watch.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now pkgtrack-watch
```
*(Non-Hyprland users can skip this step—shell tracking works everywhere).*

---

## Command Reference

| Command | Description |
| :--- | :--- |
| `pkgtrack top [-n 20] [--json]` | Show most frequently used packages |
| `pkgtrack stale [--days 60] [--json]` | List packages unused for N days + suggested removal command |
| `pkgtrack why <package>` | View install reason, size, dependencies, usage count, and binaries |
| `pkgtrack untrackable` | List explicit packages without `/usr/bin` binaries (fonts, libs, themes) |
| `pkgtrack sync` | Rebuild binary &rarr; package map after system updates |
| `pkgtrack aggregate` | Manually flush `events.log` into the database (called automatically by `top`/`stale`) |
| `pkgtrack bootstrap-history --yes` | One-time import of command counts from `~/.bash_history` |
| `pkgtrack init bash` | Print the bash hook script |

---

## Examples

### View most-used packages
```bash
$ pkgtrack top
PACKAGE                           COUNT  LAST USED           
──────────────────────────────────────────────────────────────
git                                  38  2026-11-23 15:12    
docker                               34  never               
npm                                   9  never               
vscodium-bin                          7  never               
```

### Find stale packages to remove
Packages installed less than `--days` ago are automatically protected by a grace period:
```bash
$ pkgtrack stale --days 60
Packages not used in 60 days (or never used), installed before cutoff:

PACKAGE                      INSTALLED  COUNT  LAST USED               SIZE
────────────────────────────────────────────────────────────────────────────
onboard                     2026-09-23      0  never               28.0 MiB
fish                        2026-09-12      0  never               18.2 MiB
awww                        2026-06-21      0  never               10.6 MiB
...
less                        2026-09-24      0  never              337.7 KiB ⚠

⚠  The following stale packages are required by other packages:
   less: required by man-db

Suggested removal (review carefully before running!):
  sudo pacman -Rns onboard fish awww
```
*(Required packages with `⚠` are automatically excluded from the suggested removal command. `pkgtrack` never removes packages automatically).*

### Inspect a specific package
```bash
$ pkgtrack why obs-studio
Package: obs-studio
Required By     : None
Installed Size  : 26.77 MiB
Install Reason  : Explicitly installed
Installed: 2026-11-23 13:51
Use count: 1
Last used: 2026-11-23 15:22
Binaries: obs, obs-ffmpeg-mux, obs-nvenc-test
```

---

## How It Works

- **Zero prompt latency**: The Bash hook uses builtins (`printf '%(%s)T'`, `$HISTCMD`)—zero child processes spawned on prompt display.
- **Append-only & crash-safe**: Collectors append simple events to `~/.local/share/pkgtrack/events.log` without touching SQLite. Queries atomically rotate the log before updating `pkgtrack.db`.
- **Database location**: `${XDG_DATA_HOME:-~/.local/share}/pkgtrack/`
