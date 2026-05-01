# shulker

[![Test](https://github.com/NihilDigit/shulker/actions/workflows/test.yml/badge.svg)](https://github.com/NihilDigit/shulker/actions/workflows/test.yml)
[![Release](https://img.shields.io/github/v/release/NihilDigit/shulker?display_name=tag&sort=semver)](https://github.com/NihilDigit/shulker/releases/latest)
[![License](https://img.shields.io/github/license/NihilDigit/shulker)](LICENSE)

A small TUI (terminal UI) manager for a local Minecraft Paper / Purpur server.

`shulker` manages the boring parts of running a friend-group MC server without leaving the terminal — switching worlds, editing whitelist / ops / `server.properties` / `paper-global.yml`, scheduling daily restarts and backups, pre-generating chunks, watching the live log, and bringing tunnels up/down through SakuraFrp.

It's intentionally a thin layer over the same files Paper / Purpur already write. Stop using `shulker` at any time and your server keeps working.

## Install

### One-line install (recommended)

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/NihilDigit/shulker/main/scripts/install.sh | sh

# Windows (PowerShell)
irm https://raw.githubusercontent.com/NihilDigit/shulker/main/scripts/install.ps1 | iex
```

The script detects your platform, fetches the latest release, and drops `shulker` into `~/.local/bin` (Linux/macOS) or `%LOCALAPPDATA%\shulker` (Windows). Override the install dir with `SHULKER_INSTALL_DIR=...`; pin a version with `SHULKER_VERSION=v1.0.0`. (The legacy `MC_TUI_INSTALL_DIR` / `MC_TUI_VERSION` names still work as fallbacks.)

### Pre-built binaries (manual)

GitHub Releases ship binaries for Linux / macOS / Windows on x86_64 and aarch64. Download the archive for your platform, extract, run.

### From source

```bash
cargo install --git https://github.com/NihilDigit/shulker
```

### Platform notes

- **Linux:** install `tmux` from your distro's package manager (`pacman -S tmux` / `apt install tmux`).
- **macOS:** install `tmux` via Homebrew (`brew install tmux`). Config dir is `~/Library/Application Support/shulker/` (per Apple's HIG).
- **Windows:** **closing shulker stops the Minecraft server** — there's no `tmux attach`-style detach on Windows. Drive the server through the in-app Overview tab + Logs overlay. Config dir is `%APPDATA%\shulker\`.

## Quickstart

First-time setup, end-to-end:

```bash
# 1. Scaffold a fresh server (detects Java, fetches the jar, writes eula + start script).
shulker new ~/mc-server

# 2. Open the TUI against it. Subsequent runs remember the last --server-dir.
shulker --server-dir ~/mc-server

# 3. Inside the TUI: press S to start, ? for the complete key legend, q to quit.
```

`shulker new` flags: `--server-type paper|purpur` (default purpur), `--mc-version 1.21.4` (default: latest), `--first-boot` (run the server once to generate `server.properties`), `--force` (allow non-empty target dir).

If you already have a server directory, skip step 1.

## Usage

```bash
shulker --server-dir /path/to/server
# or via env var
MC_SERVER_DIR=/path/to/server shulker
# without either, shulker uses the last server-dir it remembers.
shulker
```

The directory must contain `server.properties`. `whitelist.json` and `ops.json` are created on demand.

## Tabs

Five top-level views:

- **1 Overview** — server status, recent activity, world / player snapshot, join-address bar, server-lifecycle actions.
- **2 Players** — unified roster from `whitelist.json` ∪ `ops.json` ∪ `world/playerdata/*.dat` ∪ rolling log corpus. Names rejected with "not whitelisted" surface a `denied YYYY-MM-DD` marker.
- **3 Worlds** — list every world, see which is current, switch the active level by writing `level-name`.
- **4 Settings** — file picker into `server.properties` and every Paper / Purpur YAML. Right-side detail panel shows default + range + restart-required + bilingual description.
- **5 Network** — SakuraFrp account + tunnel list live from `api.natfrp.com/v4`; manage tunnels and shulker's directly-managed `frpc` subprocess. Always-visible NIC list (collapsed by default).

The **join-address bar** is always visible between the status row and the tab bar — click the chip to copy `<ip>:<port>` to the system clipboard.

## Keys

`?` opens the in-app help overlay with the complete legend. The essentials:

### Global

| Key | Action |
|---|---|
| `1` … `5` | Jump to tab |
| `Tab` / `Shift+Tab` | Cycle tabs |
| `↑` / `↓` | Move selection |
| `Enter` | Confirm / activate selection |
| `Esc` | Close overlay; back out of sub-views |
| `S` / `X` / `R` | Start / stop / restart the server |
| `B` | Run `backup.sh` now |
| `L` | Open fullscreen log overlay |
| `Y` | Copy share-text to clipboard |
| `?` | Help overlay |
| `:` | Command palette (schedule, pre-gen chunks, attach, scheduler paths) |
| `D` | Switch `--server-dir` at runtime |
| `T` | Toggle 中 / EN |
| `r` | Refresh from disk (and SakuraFrp API on Network) |
| `q` | Quit |

### Players tab

| Key | Action |
|---|---|
| `Enter` | Toggle whitelist |
| `o` | Toggle op for selected player |
| `←` / `→` | Cycle op level (1↔4, wraps) |
| `d` | Purge from whitelist + ops |
| `a` | Add player (whitelist-add prompt) |
| `w` | Toggle the `white-list` setting |

### Worlds tab

| Key | Action |
|---|---|
| `Enter` | Switch active world (refuses while server is running) |
| `N` | New world (writes `level-name`; server generates the dir on next start) |

### Network tab

| Key | Action |
|---|---|
| `Enter` | Copy tunnel public address |
| `t` | Set SakuraFrp API token |
| `i` | One-key setup wizard |
| `c` / `m` / `d` | Create / migrate / delete tunnel |
| `e` / `x` | Enable / disable selected tunnel (frpc subprocess) |
| `o` | Open natfrp.com in browser |
| `A` | Manual SakuraFrp address override |
| `n` | Toggle the NIC list |

### Logs overlay

| Key | Action |
|---|---|
| `↑` / `↓` | Scroll |
| `PgUp` / `PgDn` / `Home` / `End` | Page / boundary scroll |
| `Esc` | Close |

**Mouse:** click tab bar, list rows, or the join chip (chip → clipboard).
**In a prompt:** type the value, `Enter` to confirm, `Esc` to cancel.

## Configuration

shulker stores state in your OS-native config dir (Linux `~/.config/shulker/`, macOS `~/Library/Application Support/shulker/`, Windows `%APPDATA%\shulker\`):

- `state.toml` — last-used `--server-dir`, language, SakuraFrp manual address override, frpc enabled tunnel ids.
- `natfrp.token` (mode `0600`) — SakuraFrp API token; set via `t` on the Network tab. Never written to logs or `state.toml`.
- `frpc` — the SakuraFrp `frpc` binary, downloaded on demand by the one-key setup wizard (`i` on Network).

Delete any of these to reset the corresponding state — shulker recreates them as needed.

## See also

Developer / agent documentation (architecture, module layout, behavior contracts, contribution style) lives in [`CLAUDE.md`](CLAUDE.md). `AGENTS.md` is a symlink to the same file.

## License

MIT. See `LICENSE`.
