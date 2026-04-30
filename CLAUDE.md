# mc-tui

A small TUI (terminal UI) manager for a local Minecraft Paper / Purpur server.

> This file is the canonical project doc. `README.md` and `AGENTS.md` are symlinks to it — humans, Claude, and other coding agents all read the same source of truth.

## What it does

Manages the boring parts of running a friend-group Minecraft server without leaving the terminal:

- **Worlds** — list every world directory under your server dir, see which is current, switch the active level by writing `level-name` to `server.properties`.
- **Whitelist** — add / remove players (offline-mode UUID is computed automatically).
- **Operators** — add / remove ops, change permission level (1–4) without touching JSON by hand.
- **Config** — browse `server.properties`, edit any value with one keystroke.
- **Logs** — tail the most recent `logs/latest.log`.

It's intentionally a thin layer over the same files Paper/Purpur already write. Stop using `mc-tui` at any time and your server keeps working.

## Install

### Pre-built binaries

GitHub Releases ship binaries for Linux / macOS / Windows on x86_64 and aarch64. Download the archive for your platform, extract, run.

### From source

```bash
cargo install --git https://github.com/<USER>/mc-tui
```

Or clone and build:

```bash
git clone https://github.com/<USER>/mc-tui
cd mc-tui
cargo build --release
./target/release/mc-tui --server-dir /path/to/your/server
```

## Usage

```bash
mc-tui --server-dir /path/to/server
# or via env var
MC_SERVER_DIR=/path/to/server mc-tui
```

The directory must contain `server.properties`. `whitelist.json` and `ops.json` will be created if missing.

### Keys

| Key | Action |
|---|---|
| `1` … `5` | Jump to tab |
| `Tab` / `Shift+Tab` | Cycle tabs |
| `↑` / `↓` | Move selection |
| `Enter` | Switch world / Edit config value |
| `a` | Add (whitelist / op) |
| `d` | Delete (whitelist / op) |
| `←` / `→` | Change op level (Ops tab) |
| `r` | Refresh from disk |
| `q` / `Esc` | Quit |

When a prompt is open: type the value, `Enter` to confirm, `Esc` to cancel.

## Behavior contracts (so you can predict what it touches)

- **Worlds tab — switching**: refuses while server is running. Writes `level-name=<chosen>` to `server.properties`. **Drops comments** in `server.properties` (Java properties is quirky and round-tripping comments isn't worth the complexity). Key/value order is preserved.
- **Whitelist tab — add/remove**: rewrites `whitelist.json` as pretty-printed JSON. UUID for new entries is the offline UUID (`md5("OfflinePlayer:" + name)`, version 3 + RFC 4122 variant bits), the same one vanilla / Paper computes for offline-mode players.
- **Ops tab — add/remove/level**: rewrites `ops.json`. New ops default to level 4, `bypassesPlayerLimit=false`. Level cycles 1–4.
- **Config tab — edit**: same `server.properties` write path as Worlds.
- **Logs tab — read-only**: tails `logs/latest.log`.

mc-tui never starts/stops the server itself. Run your `start.sh` / `systemctl` separately.

## Detecting whether the server is running

`server_running_pid()` walks the process list (via `sysinfo`) for any Java process whose argv mentions a `paper`/`purpur`/`spigot` `.jar` and whose CWD is the canonical server dir. If you start the server in an unusual way (Docker, weird wrappers), detection might miss it; the worst case is mc-tui lets you change `level-name` while the server is up — which is bad. PRs welcome to harden this.

## Project layout

```
src/main.rs    Everything. Single-binary CLI + TUI.
Cargo.toml     Deps: ratatui, crossterm, clap, serde, serde_json, serde_yaml, md-5, chrono, sysinfo.
tests/         (none yet — unit tests live in `mod tests` at the bottom of main.rs)
.github/workflows/release.yml   Tag-triggered release builds.
```

There's no premature module split. When `main.rs` becomes painful to navigate, split it then, not before.

## Development

```bash
cargo run -- --server-dir /path/to/your/server
cargo test       # unit tests for properties parser, UUID, JSON roundtrip, etc.
cargo build --release
```

### Style

- One `main.rs`. Don't introduce `mod foo; mod bar;` until there's a concrete reason. (~700 lines now; ~1500 is still fine in one file for a TUI of this scope.)
- No `unsafe`, no `unwrap()` on user-facing paths (use `?` and `anyhow::Context`). `unwrap()` in tests is fine.
- Errors bubble to `main` via `anyhow::Result`. No `Box<dyn Error>` decay.
- TUI rendering is pure: `fn ui(f, app)` should not mutate disk. All disk writes go through `App::*` action methods that also update `App::status`.

### Tests

Unit tests are at the bottom of `src/main.rs` under `#[cfg(test)] mod tests`. Coverage:

- Offline UUID: format / version bits / determinism
- `server.properties` round-trip (read → mutate → write → re-read)
- `whitelist.json` and `ops.json` round-trip
- `fmt_bytes` examples

If you add a write path to a new file format, add a round-trip test. They're cheap and catch the dumb bugs.

## For coding agents (Claude, Cursor, etc.)

If you're an LLM working on this repo:

1. **Don't extract modules unless asked.** The whole point of single-file is that you can read it top-to-bottom.
2. **Don't add features the user didn't ask for.** No "while we're here" cleanups, no extra tabs, no daemons that watch the server. Keep PRs scoped.
3. **Run `cargo test` before claiming done.** UI changes need manual QA — say so explicitly when you can't verify visually.
4. **Don't hardcode paths or user-specific values.** The whole CLI is parameterized via `--server-dir` for a reason; preserve that.
5. **Never commit binaries, lockfiles for libraries** (we DO commit `Cargo.lock` since this is a binary crate), `target/`, or anything under `~/.minecraft`.

When you propose changes, use the same anyhow-everywhere, single-file convention. If a function gets > 80 lines, that's a fine signal it might want to be extracted — but talk to the human first.

## Roadmap

Tracked here instead of GitHub issues for now (low overhead). Add to top of list, mark with date when shipped.

### v0.2 — interactivity

- [ ] **Server lifecycle from TUI**: `S` to start (spawns `start.sh`), `X` to stop (SIGTERM the detected pid + wait for graceful shutdown). Status bar shows progress.
- [ ] **Create new world**: `N` in Worlds tab → prompt for name → write `level-name=<name>` so next start generates a fresh world.
- [ ] **Mouse support**: `EnableMouseCapture` is already on; wire click handlers for tab bar + list rows.
- [ ] **Server-dir switcher**: `D` to open prompt for a new server-dir, validate (must contain `server.properties`), reload all state.
- [ ] **Persist last-good server-dir**: write `$XDG_CONFIG_HOME/mc-tui/state.toml`. Without `--server-dir`, restore from there. If no remembered dir and no `--server-dir`, error out clearly.

### v0.3 — i18n

- [ ] Add `Lang` enum (`En`, `Zh`). Toggle with `L`. Translate UI strings, hint bar, prompt labels, status messages.
- [ ] Translate common `server.properties` keys into Chinese annotations in the Config tab (`max-players` → `最大玩家数`, `view-distance` → `视距`, etc.). Keep raw key visible — translation is annotation, not replacement.

### v0.4 — server scaffolder

- [ ] Wizard for `mc-tui new <dir>`:
  - Check Java ≥ 25 in `$PATH` (parse `java -version`). Error if missing/too old.
  - Pick MC version + server type (Paper / Purpur) via Modrinth/Purpur API.
  - Download jar, write `eula.txt`, write `start.sh` with Aikar's flags + heap based on detected RAM.
  - Optionally first-boot to generate `server.properties`, then stop.
- [ ] Refuse to scaffold into a non-empty dir without `--force`.

### v0.5 — beyond

- [ ] Edit `paper-global.yml` / `paper-world-defaults.yml` / `purpur.yml` (huge YAMLs, needs nested editor).
- [ ] Backup tab — list `/path/to/backups/`, restore with confirmation.
- [ ] RCON bridge — send commands to running server without attaching to console.

### v0.6+ — stability gate

The CI workflow at `.github/workflows/release.yml` is already in place but **no tag has been pushed yet**. First release (binary publish) is intentionally held until the project hits a stable version (~v0.6). To cut a release: `git tag v0.6.0 && git push --tags` — that triggers builds for the 6 targets and uploads to GitHub Releases.

## License

MIT. See `LICENSE`.
