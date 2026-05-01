# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`shulker` is a small TUI manager for a local Minecraft Paper / Purpur server. User-facing docs (what it does, install, usage, keys) live in [`README.md`](README.md). `AGENTS.md` is a symlink to this file — coding agents and Claude read the same source of truth.

## Behavior contracts (so you can predict what it touches)

- **Worlds tab — switching**: refuses while server is running. Writes `level-name=<chosen>` to `server.properties`. **Drops comments** in `server.properties` (Java properties is quirky and round-tripping comments isn't worth the complexity). Key/value order is preserved.
- **Worlds tab — N (new)**: refuses while server is running. Validates the name (no `/`, `\`, `.`, `..`). Writes `level-name=<name>` only — the world directory + `level.dat` are generated on next server start. The list shows a placeholder entry for the pending world so you can see the state took.
- **Players tab — toggle whitelist (Enter)**: rewrites `whitelist.json` as pretty-printed JSON. UUID for new entries is the offline UUID (Java/Paper offline mode). No-op when `white-list` is disabled in `server.properties`. **Refuses to write if `whitelist.json` failed to parse on read** (would clobber user's broken-but-recoverable file). Same guard for `ops.json`.
- **Players tab — toggle op (`o`)**: rewrites `ops.json`. New ops default to level 4, `bypassesPlayerLimit=false`. `←/→` cycles the level 1↔4 (wraps). `d` purges the selected player from both `whitelist.json` and `ops.json` in one shot.
- **Players tab — `w`**: writes `white-list=true|false` into `server.properties`. shulker does **not** push a `/whitelist reload` to the running server; the change applies on next restart or after a manual reload.
- **Players tab — name discovery**: `whitelist.json` ∪ `ops.json` ∪ `world/<level>/playerdata/*.dat` (UUIDs) ∪ all `logs/latest.log` and `logs/YYYY-MM-DD-N.log.gz`. The log scan harvests `UUID of player NAME is UUID` lines (name↔UUID mapping) and `Disconnecting NAME (...): You are not whitelisted on this server!` lines (denied attempts, dated by the log filename).
- **Settings tab — server.properties edit**: same write path as Worlds (drops comments, preserves order).
- **Settings tab — YAML edit**: full read → mutate `serde_yaml::Value` → write the file. Keeps key order. Preserves nested structure.
- **Logs overlay — read-only**: tails `logs/latest.log` (server view), or reads from `Console::capture_recent` (frpc view — Unix: tmux pane buffer; Windows: 256 KiB in-process ring buffer).
- **Backups (Worlds detail panel)**: read-only display; restore is intentionally not automated (do it by hand to avoid surprises).
- **Server actions — Restart now**: `Console::stop_graceful("stop")` → poll for pid disappearance up to 30 s → `Console::start(...)`.
- **Server actions — Pre-gen chunks**: refuses if `Console` reports the session isn't alive; otherwise sends `chunky world <level>` / `chunky center 0 0` / `chunky radius <r>` / `chunky start` to the server console via `Console::send_line`. Watch progress in the Logs overlay (or `tmux attach` on Unix).
- **Server actions — Schedule daily restart/backup**: routed through `scheduler::schedule_daily`. Linux writes a systemd `--user` unit + timer pair; macOS writes a launchd plist and tries `launchctl bootstrap`; Windows invokes `schtasks /Create /SC DAILY`. shulker does **not** auto-activate the systemd timer — the status bar shows the exact `systemctl --user enable --now …` command to copy.

## Server lifecycle

Since v0.17 shulker runs through a cross-platform `Console` abstraction (`src/console.rs`):

- **Linux & macOS:** detached tmux session — `tmux new-session -d -s shulker-<slug> -c <server-dir> 'bash start.sh'`. Stopping is `send-keys 'stop' Enter` so Minecraft's synchronous shutdown handler runs on the main thread (the only reliable shutdown — SIGTERM races with startup). The JVM survives shulker exit; you can `tmux attach -t shulker-<slug>` from another terminal. macOS users need `brew install tmux`.
- **Windows:** ConPTY via `portable-pty`. shulker spawns `start.bat` with a captured PTY pair, drains stdout into a 256 KiB ring buffer (read by the Logs overlay), and writes `stop\r\n` to the master to shut down. **Closing shulker closes the JVM** — no detach, no attach. Friend-group servers on Windows match the `start.bat` mental model so this tradeoff is intentional. The status bar surfaces this instead of offering an attach command.

The "show attach command" action returns the right thing per platform: tmux command on Unix; a "view in Logs (L)" hint on Windows.

## Detecting whether the server is running

`server_running_pid()` walks the process list (via `sysinfo`) for any Java process whose argv mentions a `paper` / `purpur` / `spigot` jar and whose CWD is the canonical server dir. **It's sticky**: once a pid matches, shulker keeps using it across refreshes as long as the process exists and still looks like our server — this stops the status bar from flickering between pids when `cwd` is briefly unreadable. If the previous pid is gone, shulker re-scans and picks the lowest matching pid for stability. Multiple matches (e.g. you're running two Minecraft servers from the same dir) is unsupported.

## Project layout

```
src/
├── main.rs       App state machine, run loop, mouse / event handlers, main(), screenshot subcommand.
├── cli.rs        Cli + Cmd + ServerType + scaffold_new + Java/curl/Aikar/first-boot helpers.
├── console.rs    Cross-platform "console session" abstraction (Unix=tmux, Windows=ConPTY via portable-pty). Used by both server + frpc lifecycle.
├── scheduler.rs  Daily-task scheduler (Linux=systemd --user, macOS=launchd plist, Windows=schtasks).
├── data.rs       Data structs + filesystem / network IO (worlds, whitelist, ops, properties, backups, YAML walker, NIC discovery via if-addrs, sticky pid detection).
├── i18n.rs       Lang + Strings struct + EN/ZH consts + fmt_* parametric helpers + property_zh annotations + PropertyMeta lookup table.
├── natfrp.rs     Blocking REST client for api.natfrp.com/v4 (UserInfo / Tunnel / Node) + parse helpers — feeds the SakuraFrp tab.
├── sys.rs        state.toml + natfrp.token (0600) persistence, clipboard/url helpers, POSIX shell quote, path/tilde helpers.
└── ui.rs         Every ratatui draw_* function + ui() dispatcher + layout helpers.

Cargo.toml                       Deps: ratatui, crossterm, clap, serde, serde_json, serde_yaml, md-5, chrono, sysinfo, unicode-width, ureq, dirs, arboard, webbrowser, if-addrs; portable-pty (Windows-only).
.github/workflows/release.yml    Tag-triggered release builds for 6 targets.
.github/workflows/test.yml       Push/PR-triggered cargo test on Linux/macOS/Windows.
```

Module dependency rule: **ui ← app/main ← {console, scheduler, i18n, data, sys, cli}**. UI reads `App` fields (they're `pub`) but never mutates business state — disk writes go through `App::*` methods in `main.rs`. Tests live at the bottom of each module under `#[cfg(test)] mod tests`.

## Development

```bash
cargo run -- --server-dir /path/to/your/server
cargo test                                              # Linux suite (~100 unit tests)
cargo build --release
cargo check --target x86_64-pc-windows-gnu              # Confirm Windows backend still compiles (needs mingw-w64-gcc)
```

CI runs `cargo test` on Linux + macOS + Windows on every push (see `.github/workflows/test.yml`). Run a single test locally with `cargo test <name>`; tests are colocated in each module so `cargo test parse_join_event` finds the `data.rs` test directly.

### Visual QA

```bash
cargo run -- --server-dir /mnt/data/mc-server screenshot \
    --tab worlds --lang zh --width 130 --height 32 --select 0 \
    > /tmp/shulker-shot.txt
```

The `screenshot` subcommand dumps one rendered frame to stdout as plain text using `ratatui::backend::TestBackend`. Each module's `cargo test` block plus a `screenshot` pass for the touched tab is the standard QA flow before committing UI work.

### Style

- Multi-module since v0.6. Add a new module when a logical unit grows past ~500 lines or has clear cross-cutting users; otherwise keep it in `main.rs`.
- No `unsafe` outside the macOS scheduler `getuid()` extern; no `unwrap()` on user-facing paths (use `?` and `anyhow::Context`). `unwrap()` in tests is fine.
- Errors bubble to `main` via `anyhow::Result`. No `Box<dyn Error>` decay.
- All UI strings route through `Strings` + `EN`/`ZH` consts in `i18n.rs`, or `fmt_<event>(lang, args...)` for parametric ones. Inline `t(lang, "en", "zh")` is allowed for one-off cases but `Strings` is preferred — keep new translations colocated with old.
- `App` fields are `pub` so `ui.rs` can read them; only `main.rs`'s `impl App` should write them.
- Cross-platform code follows the `#[cfg(unix)]` / `#[cfg(windows)]` split inside one module rather than two parallel files. `console.rs` and `scheduler.rs` are both written this way; mirror that pattern when adding platform-specific paths.

### Tests

~90 unit tests, colocated under `#[cfg(test)] mod tests` in each module. Run a single test with `cargo test <name>`.

If you add a write path to a new file format, add a round-trip test. If you add a parser, give it bad input. Don't write coverage tests (`asserts each variant has a non-empty label`, `asserts every key in some list returns Some`) — they trip on every additive change without catching real bugs. Test branches and contracts, not enumeration.

## For coding agents (Claude, Cursor, etc.)

If you're an LLM working on this repo:

1. **Module boundaries are real.** Adding to `i18n.rs`? Use the `Strings` + EN/ZH pattern. Adding a new tab? UI render goes in `ui.rs`, App state in `main.rs`, persistence in `sys.rs` or `data.rs`. Process / scheduling work goes in `console.rs` / `scheduler.rs`. Don't dump everything in `main.rs`.
2. **Don't add features the user didn't ask for.** No "while we're here" cleanups, no extra tabs, no daemons that watch the server. Keep PRs scoped.
3. **Run `cargo test` before claiming done.** UI changes need manual QA — render a screenshot via the `screenshot` subcommand and inspect it. Say so explicitly when you can't verify visually. For cross-platform changes, run `cargo check --target x86_64-pc-windows-gnu` too.
4. **Don't hardcode paths or user-specific values.** The whole CLI is parameterized via `--server-dir` for a reason; preserve that.
5. **Never commit binaries**, `target/`, `~/.minecraft`, or anything under user server dirs. We DO commit `Cargo.lock` since this is a binary crate.
6. **The `Console` abstraction is the start/stop path.** Don't add SIGTERM as a primary mechanism — we tried, it raced with Paper's startup and left half-dead processes. Sending the literal `stop` text to the JVM's stdin (tmux send-keys on Unix, ConPTY writer on Windows) is what works.
7. **App fields are pub for `ui.rs`, not for free editing.** Keep mutation paths funneled through `impl App` methods so they can update `App::status` and run `refresh_all()` consistently.

## License

MIT. See `LICENSE`.
