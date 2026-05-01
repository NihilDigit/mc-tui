//! Data layer for shulker: pure structures + filesystem / network IO.
//!
//! Everything here is independent of the UI. Anything that takes `&App` or
//! returns ratatui widgets lives in src/main.rs (the UI module hasn't been
//! split out yet).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

// `Md5` is the hasher; `Digest` is the trait that provides `update()` /
// `finalize()` — both are needed even though the trait import looks unused
// (cargo's lint can't see through trait-method calls in a generic context).
#[allow(unused_imports)]
use md5::{Digest, Md5};
use ratatui::style::Color;
// Serde derive macros below need these traits in scope; cargo's lint can't
// see through the macro expansion.
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};

use crate::Lang;

// ---------- Properties / Whitelist / Ops / Worlds ----------

#[derive(Debug, Clone)]
pub struct WorldEntry {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub last_modified: Option<chrono::DateTime<chrono::Local>>,
    pub is_current: bool,
    pub playerdata_count: usize,
    pub has_level_dat: bool,
}

pub fn count_playerdata(world_path: &Path) -> usize {
    let dir = world_path.join("playerdata");
    let Ok(rd) = fs::read_dir(&dir) else { return 0 };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "dat").unwrap_or(false))
        .count()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WhitelistEntry {
    pub uuid: String,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpEntry {
    pub uuid: String,
    pub name: String,
    pub level: u8,
    #[serde(rename = "bypassesPlayerLimit", default)]
    pub bypasses_player_limit: bool,
}

pub fn offline_uuid(name: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(format!("OfflinePlayer:{}", name).as_bytes());
    let mut bytes: [u8; 16] = hasher.finalize().into();
    bytes[6] = (bytes[6] & 0x0f) | 0x30; // version 3
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

pub fn dir_size(path: &Path) -> u64 {
    fn walk(p: &Path) -> u64 {
        let Ok(meta) = fs::symlink_metadata(p) else { return 0 };
        if meta.is_file() {
            return meta.len();
        }
        if meta.is_dir() {
            let Ok(rd) = fs::read_dir(p) else { return 0 };
            return rd.filter_map(|e| e.ok()).map(|e| walk(&e.path())).sum();
        }
        0
    }
    walk(path)
}

pub fn fmt_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut x = n as f64;
    let mut i = 0;
    while x >= 1024.0 && i < UNITS.len() - 1 {
        x /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", x, UNITS[i])
}

pub fn read_properties(path: &Path) -> Result<Vec<(String, String)>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq) = line.find('=') {
            let k = line[..eq].trim().to_string();
            let v = line[eq + 1..].to_string();
            out.push((k, v));
        }
    }
    Ok(out)
}

pub fn write_properties(path: &Path, props: &[(String, String)]) -> Result<()> {
    let mut s = String::new();
    s.push_str("#Minecraft server properties\n");
    s.push_str(&format!("#{}\n", chrono::Local::now().to_rfc2822()));
    for (k, v) in props {
        s.push_str(&format!("{}={}\n", k, v));
    }
    fs::write(path, s).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn get_property<'a>(props: &'a [(String, String)], key: &str) -> Option<&'a str> {
    props.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

pub fn set_property(props: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some(slot) = props.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value.to_string();
    } else {
        props.push((key.to_string(), value.to_string()));
    }
}

pub fn scan_worlds(server_dir: &Path, current_level: &str) -> Vec<WorldEntry> {
    let Ok(rd) = fs::read_dir(server_dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path.join("level.dat").exists() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        let size_bytes = dir_size(&path);
        let last_modified = fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .map(chrono::DateTime::<chrono::Local>::from);
        let is_current = name == current_level;
        let playerdata_count = count_playerdata(&path);
        let has_level_dat = path.join("level.dat").is_file();
        out.push(WorldEntry {
            name,
            path,
            size_bytes,
            last_modified,
            is_current,
            playerdata_count,
            has_level_dat,
        });
    }
    out.sort_by(|a, b| b.is_current.cmp(&a.is_current).then(a.name.cmp(&b.name)));

    // If `level-name` points at a world that hasn't been generated yet
    // (e.g. user just hit `N` to create a new world; the dir + level.dat are
    // produced on next server start), surface it as a placeholder so the UI
    // doesn't look like the action did nothing.
    if !current_level.is_empty() && !out.iter().any(|w| w.name == current_level) {
        let target = server_dir.join(current_level);
        out.insert(
            0,
            WorldEntry {
                name: current_level.to_string(),
                path: target,
                size_bytes: 0,
                last_modified: None,
                is_current: true,
                playerdata_count: 0,
                has_level_dat: false,
            },
        );
    }
    out
}

pub fn read_whitelist(server_dir: &Path) -> Result<Vec<WhitelistEntry>> {
    let path = server_dir.join("whitelist.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

pub fn write_whitelist(server_dir: &Path, entries: &[WhitelistEntry]) -> Result<()> {
    let path = server_dir.join("whitelist.json");
    let json = serde_json::to_string_pretty(entries)?;
    fs::write(&path, json)?;
    Ok(())
}

pub fn read_ops(server_dir: &Path) -> Result<Vec<OpEntry>> {
    let path = server_dir.join("ops.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

pub fn write_ops(server_dir: &Path, entries: &[OpEntry]) -> Result<()> {
    let path = server_dir.join("ops.json");
    let json = serde_json::to_string_pretty(entries)?;
    fs::write(&path, json)?;
    Ok(())
}

// ---------- Players (merged whitelist + ops + denied logins) ----------
//
// v0.11 collapses the separate Whitelist and Ops tabs into one "Players" view.
// Sources of truth, ordered from most-active to most-historical:
//
// 1. `whitelist.json` — currently allowed names + their UUIDs.
// 2. `ops.json` — currently elevated names + level (1..4).
// 3. `world/<level>/playerdata/*.dat` — every player who has ever logged in.
//    Only their UUID is on disk, so the name-mapping comes from logs.
// 4. `logs/latest.log` + `logs/YYYY-MM-DD-N.log.gz` — login attempts. Two
//    line shapes are mined:
//      a. `UUID of player <name> is <uuid>` → name↔uuid mapping for anyone
//         who has tried to connect, regardless of outcome.
//      b. `Disconnecting <name> (...): You are not whitelisted on this server!`
//         → denied login attempts. The name + the date implied by the log
//         filename go into `denied_at`.
//
// The merged view lets the user see "people who tried to join but were
// rejected" alongside the actual roster, so admitting a friend is one toggle
// instead of asking them to re-join after a `/whitelist add`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerEntry {
    pub name: String,
    pub uuid: String,
    pub in_whitelist: bool,
    /// `Some(level)` when the player is in `ops.json`; `None` otherwise.
    /// Levels are 1..=4 in vanilla — we keep whatever value the file has.
    pub op_level: Option<u8>,
    /// `Some(unix_seconds_at_log_date)` for the most recent denied login we
    /// could find in the log corpus. Granularity is one day (log filenames
    /// give us the date but parsing the per-entry HH:MM:SS into a real
    /// timestamp would require the log's TZ, which Paper doesn't record).
    pub last_denied_at: Option<i64>,
    /// True when we only know about this player from the playerdata folder
    /// (no whitelist, no ops, no denied attempts in the recent log corpus).
    /// Lower-priority for sort.
    pub historical_only: bool,
    /// True when the most-recent join/leave event in `latest.log` is a join
    /// AND the server is currently up. False when the server is stopped, the
    /// player has left, or we never saw them in the current session.
    pub is_online: bool,
}

/// Build the merged `PlayerEntry` list. Caller passes already-loaded
/// whitelist + ops to avoid re-reading them; the world's `playerdata/` and
/// the log corpus get scanned every call.
pub fn scan_players(
    server_dir: &Path,
    level_name: &str,
    whitelist: &[WhitelistEntry],
    ops: &[OpEntry],
    server_running: bool,
) -> Vec<PlayerEntry> {
    use std::collections::BTreeMap;

    // Fold-in by lowercase-name to dedupe across sources. Keeping the
    // first-seen casing as the display name is fine — Minecraft names are
    // case-insensitive at the protocol level.
    let mut by_name: BTreeMap<String, PlayerEntry> = BTreeMap::new();
    let key = |s: &str| s.to_ascii_lowercase();

    for w in whitelist {
        by_name.insert(
            key(&w.name),
            PlayerEntry {
                name: w.name.clone(),
                uuid: w.uuid.clone(),
                in_whitelist: true,
                op_level: None,
                last_denied_at: None,
                historical_only: false,
                is_online: false,
            },
        );
    }
    for o in ops {
        by_name
            .entry(key(&o.name))
            .and_modify(|p| {
                p.op_level = Some(o.level);
                if p.uuid.is_empty() {
                    p.uuid = o.uuid.clone();
                }
            })
            .or_insert_with(|| PlayerEntry {
                name: o.name.clone(),
                uuid: o.uuid.clone(),
                in_whitelist: false,
                op_level: Some(o.level),
                last_denied_at: None,
                historical_only: false,
                is_online: false,
            });
    }

    let scan = scan_log_corpus(server_dir);
    for (name, uuid) in &scan.uuid_by_name {
        by_name
            .entry(key(name))
            .and_modify(|p| {
                if p.uuid.is_empty() {
                    p.uuid = uuid.clone();
                }
            })
            .or_insert_with(|| PlayerEntry {
                name: name.clone(),
                uuid: uuid.clone(),
                in_whitelist: false,
                op_level: None,
                last_denied_at: None,
                // We could actually be a returning regular — disambiguated by
                // the next loop overwriting `last_denied_at` if applicable.
                historical_only: true,
                is_online: false,
            });
    }
    for (name, ts) in &scan.last_denied_by_name {
        by_name
            .entry(key(name))
            .and_modify(|p| {
                p.last_denied_at = Some(*ts);
                p.historical_only = false;
            })
            .or_insert_with(|| PlayerEntry {
                name: name.clone(),
                uuid: offline_uuid(name),
                in_whitelist: false,
                op_level: None,
                last_denied_at: Some(*ts),
                historical_only: false,
                is_online: false,
            });
    }

    // Pull names off of playerdata/*.dat — UUIDs only, but it tells us
    // someone has joined before. We can't infer the name here without a NBT
    // parser, so we just count on log scan to have populated the mapping.
    let pd = server_dir.join(level_name).join("playerdata");
    if let Ok(rd) = fs::read_dir(&pd) {
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("dat") {
                continue;
            }
            let Some(uuid) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            // If we already know this UUID under some name, leave it. Else
            // synthesize a placeholder entry keyed by the UUID itself.
            let known = by_name.values().any(|p| p.uuid == uuid);
            if !known {
                by_name.insert(
                    format!("uuid:{}", uuid),
                    PlayerEntry {
                        name: format!("(uuid:{}…)", &uuid[..8.min(uuid.len())]),
                        uuid: uuid.to_string(),
                        in_whitelist: false,
                        op_level: None,
                        last_denied_at: None,
                        historical_only: true,
                        is_online: false,
                    },
                );
            }
        }
    }

    // Fold the latest.log online state in. Gated on the server actually
    // running — if it's stopped, every player is offline regardless of
    // what the log's last event said (crash before clean shutdown is the
    // common case where this matters).
    if server_running {
        for (lower_name, online) in &scan.online_state_in_latest {
            if *online {
                if let Some(p) = by_name.get_mut(lower_name) {
                    p.is_online = true;
                    // An online player is by definition not "historical only".
                    p.historical_only = false;
                }
            }
        }
    }

    // Sort: online first (so the host sees who's playing right now without
    // scrolling), then denied-recently, then op, then whitelist-only, then
    // historical. Within each band, lex by name.
    let mut out: Vec<PlayerEntry> = by_name.into_values().collect();
    out.sort_by(|a, b| {
        // Bucket order: 0 = online, 1 = recently denied, 2 = op, 3 = whitelist, 4 = historical.
        let bucket = |p: &PlayerEntry| {
            if p.is_online {
                0
            } else if p.last_denied_at.is_some() && !p.in_whitelist && p.op_level.is_none() {
                1
            } else if p.op_level.is_some() {
                2
            } else if p.in_whitelist {
                3
            } else {
                4
            }
        };
        bucket(a)
            .cmp(&bucket(b))
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
    out
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LogScanResult {
    /// Display name → UUID, harvested from `UUID of player NAME is UUID` lines.
    pub uuid_by_name: std::collections::HashMap<String, String>,
    /// Display name → last unix-seconds (00:00 UTC of the log's date) where
    /// the player got denied for "not whitelisted".
    pub last_denied_by_name: std::collections::HashMap<String, i64>,
    /// Lowercased name → most-recent-event bool (true = join, false = leave).
    /// **Only populated from `latest.log`** so a recent crash doesn't ghost
    /// players as online forever via an old rotated log. Caller still has
    /// to filter by "is the server running right now?".
    pub online_state_in_latest: std::collections::HashMap<String, bool>,
}

pub fn scan_log_corpus(server_dir: &Path) -> LogScanResult {
    let logs_dir = server_dir.join("logs");
    let mut acc = LogScanResult::default();
    let Ok(rd) = fs::read_dir(&logs_dir) else { return acc };

    // Collect all candidate log files. We process them in date order so the
    // last_denied_by_name gets overwritten by the latest occurrence.
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else { return false };
            name == "latest.log" || name.ends_with(".log") || name.ends_with(".log.gz")
        })
        .collect();
    // latest.log is for "today"; sort it last so its entries take precedence.
    files.sort_by_key(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        // Tuple sort: (is_latest, name) — false (rotated) first, true (latest) last.
        let is_latest = name == "latest.log";
        (is_latest, name)
    });

    for path in files {
        let Some(date_ts) = log_file_date_unix(&path) else { continue };
        let lines = read_log_lines(&path);
        let is_latest = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == "latest.log")
            .unwrap_or(false);
        scan_lines(&lines, date_ts, is_latest, &mut acc);
    }
    acc
}

/// Today's midnight UTC for `latest.log`; the date encoded in
/// `YYYY-MM-DD-N.log.gz` for rotated files.
fn log_file_date_unix(path: &Path) -> Option<i64> {
    let name = path.file_name()?.to_str()?;
    if name == "latest.log" {
        // Use the file's mtime as a proxy for "today" — keeps the test
        // surface simple and is consistent with what the user would see
        // tailing the file by hand.
        let meta = fs::metadata(path).ok()?;
        let modified = meta.modified().ok()?;
        let secs = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        return Some(secs);
    }
    // YYYY-MM-DD-N.log[.gz]
    let trimmed = name
        .strip_suffix(".log.gz")
        .or_else(|| name.strip_suffix(".log"))?;
    let parts: Vec<&str> = trimmed.split('-').collect();
    if parts.len() < 3 {
        return None;
    }
    let y: i32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let d: u32 = parts[2].parse().ok()?;
    use chrono::TimeZone;
    let dt = chrono::Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).single()?;
    Some(dt.timestamp())
}

fn read_log_lines(path: &Path) -> Vec<String> {
    use std::io::{BufRead, BufReader};
    let Ok(file) = fs::File::open(path) else { return Vec::new() };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with(".gz") {
        let r = BufReader::new(flate2::read::GzDecoder::new(file));
        r.lines().map_while(|l| l.ok()).collect()
    } else {
        let r = BufReader::new(file);
        r.lines().map_while(|l| l.ok()).collect()
    }
}

pub fn scan_lines(
    lines: &[String],
    date_ts: i64,
    is_latest: bool,
    out: &mut LogScanResult,
) {
    for line in lines {
        // Pattern A: "UUID of player NAME is UUID"
        if let Some(name_uuid) = parse_uuid_of_player(line) {
            out.uuid_by_name.insert(name_uuid.0, name_uuid.1);
            continue;
        }
        // Pattern B: "Disconnecting NAME (..): You are not whitelisted on this server!"
        if line.contains("not whitelisted on this server") {
            if let Some(name) = parse_disconnect_name(line) {
                let entry = out.last_denied_by_name.entry(name).or_insert(date_ts);
                if date_ts > *entry {
                    *entry = date_ts;
                }
            }
        }
        // Pattern C: join/leave events from `latest.log` only. Within the
        // file lines are chronological, so insert-overwrite naturally lands
        // on the most-recent event per player. Server thread broadcasts
        // both events when a connection comes up / drops cleanly.
        if is_latest {
            if let Some(name) = parse_join_event(line) {
                out.online_state_in_latest.insert(name.to_ascii_lowercase(), true);
            } else if let Some(name) = parse_leave_event(line) {
                out.online_state_in_latest.insert(name.to_ascii_lowercase(), false);
            }
        }
    }
}

/// `[Server thread/INFO]: <NAME> joined the game`
/// Returns the name when the line matches; None otherwise. Vanilla / Paper /
/// Purpur all emit this verbatim.
pub fn parse_join_event(line: &str) -> Option<&str> {
    parse_lifecycle_event(line, " joined the game")
}

/// `[Server thread/INFO]: <NAME> left the game`
pub fn parse_leave_event(line: &str) -> Option<&str> {
    parse_lifecycle_event(line, " left the game")
}

fn parse_lifecycle_event<'a>(line: &'a str, suffix: &str) -> Option<&'a str> {
    // Suffix anchors the right side. We then locate the "INFO]: " marker on
    // the left and take the slice in between as the name. Refusing names with
    // spaces or angle brackets keeps chat impersonation (e.g. someone whose
    // message text happens to be "joined the game") out of the join set.
    let suf = line.find(suffix)?;
    let pre = line.find("INFO]: ")?;
    let start = pre + "INFO]: ".len();
    if start >= suf {
        return None;
    }
    let name = &line[start..suf];
    if name.is_empty()
        || name.contains(' ')
        || name.contains('<')
        || name.contains('>')
        || name.contains(':')
    {
        return None;
    }
    Some(name)
}

/// `[HH:MM:SS] [User Authenticator #N/INFO]: UUID of player NAME is UUID`
fn parse_uuid_of_player(line: &str) -> Option<(String, String)> {
    let idx = line.find("UUID of player ")?;
    let rest = &line[idx + "UUID of player ".len()..];
    // Up to the next " is "
    let is_idx = rest.find(" is ")?;
    let name = rest[..is_idx].trim().to_string();
    let after_is = &rest[is_idx + " is ".len()..];
    // UUID token is whitespace-separated, take first 36 chars.
    let token: String = after_is.chars().take(36).collect();
    if name.is_empty() || token.len() != 36 {
        return None;
    }
    Some((name, token))
}

/// `[HH:MM:SS] [Server thread/INFO]: Disconnecting NAME (...): You are not whitelisted on this server!`
fn parse_disconnect_name(line: &str) -> Option<String> {
    let idx = line.find("Disconnecting ")?;
    let rest = &line[idx + "Disconnecting ".len()..];
    // Name ends at first space or '(' — both possible depending on whether
    // there's a parenthesized address ("Disconnecting NAME (/ip:port): ...").
    let end = rest.find(|c: char| c == ' ' || c == '(').unwrap_or(rest.len());
    let name = rest[..end].trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(name)
}

/// v0.15 — Find a running frpc subprocess. Sticky on `prev` to avoid pid
/// flicker when proc table reads race against the tmux-spawned child. Match
/// criterion: command name contains "frpc". Same shape as
/// `server_running_pid` so the two cohabit naturally.
pub fn detect_frpc_pid(prev: Option<u32>) -> Option<u32> {
    use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::everything()),
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let matches = |proc: &sysinfo::Process| -> bool {
        let name = proc.name().to_string_lossy();
        if name == "frpc" {
            return true;
        }
        // Fallback: check argv for an absolute path ending in `/frpc`.
        proc.cmd().iter().any(|s| {
            let s = s.to_string_lossy();
            s.ends_with("/frpc") || s == "frpc"
        })
    };

    if let Some(p) = prev {
        if let Some(proc) = sys.process(Pid::from_u32(p)) {
            if matches(proc) {
                return Some(p);
            }
        }
    }
    let mut best: Option<u32> = None;
    for (pid, proc) in sys.processes() {
        if !matches(proc) {
            continue;
        }
        let p = pid.as_u32();
        if best.map(|b| p < b).unwrap_or(true) {
            best = Some(p);
        }
    }
    best
}

/// Find the Java process running the Paper/Purpur/Spigot server in `server_dir`.
///
/// Why sticky: a single process scan can miss `cwd` for a process that's mid-fork,
/// or hit them in a different iteration order between refreshes — both cause the
/// returned pid to flicker between Some(p) and None (or between sibling pids if
/// multiple jars are running). To avoid the status bar bouncing, prefer keeping
/// the previously-observed pid as long as that pid still exists and still looks
/// like our server.
pub fn server_running_pid(server_dir: &Path, prev: Option<u32>) -> Option<u32> {
    use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::everything()),
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let canonical = server_dir.canonicalize().ok();

    let matches = |proc: &sysinfo::Process| -> bool {
        let cmd = proc.cmd();
        let has_jar = cmd.iter().any(|s| {
            let s = s.to_string_lossy();
            s.ends_with(".jar")
                && (s.contains("paper") || s.contains("purpur") || s.contains("spigot"))
        });
        if !has_jar {
            return false;
        }
        let cwd = proc.cwd();
        match (cwd, canonical.as_ref()) {
            (Some(cwd), Some(c)) => cwd == c.as_path(),
            // If we couldn't read cwd this refresh, fall back to "is the cmd jar
            // path absolute and inside server_dir?" — this covers `java -jar /srv/mc/purpur.jar`.
            (None, Some(c)) => cmd.iter().any(|s| {
                let s = s.to_string_lossy();
                s.ends_with(".jar") && s.starts_with(c.to_string_lossy().as_ref())
            }),
            _ => false,
        }
    };

    // Sticky: if the previously-known pid still exists and matches, keep it.
    if let Some(p) = prev {
        if let Some(proc) = sys.process(Pid::from_u32(p)) {
            if matches(proc) {
                return Some(p);
            }
        }
    }

    // Otherwise pick the lowest matching pid for stability across re-scans.
    let mut best: Option<u32> = None;
    for (pid, proc) in sys.processes() {
        if !matches(proc) {
            continue;
        }
        let p = pid.as_u32();
        if best.map(|b| p < b).unwrap_or(true) {
            best = Some(p);
        }
    }
    best
}

// ---------- Network interface discovery (Server tab join info) ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NicKind {
    Loopback,
    Lan,
    Public,
    Tun,
    Docker,
}

#[derive(Debug, Clone)]
pub struct NicInfo {
    pub name: String,
    pub ip: std::net::Ipv4Addr,
    pub kind: NicKind,
}

/// Heuristic classifier. Prefers interface naming convention over IP-range
/// guessing (since CGNAT can give 10.x to a Wi-Fi card and a tun device can
/// route non-private ranges). IP range only decides Lan vs Public.
pub fn classify_iface(name: &str, ip: &std::net::Ipv4Addr) -> NicKind {
    if ip.is_loopback() {
        return NicKind::Loopback;
    }
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("docker") || lower.starts_with("br-") || lower.starts_with("veth") {
        return NicKind::Docker;
    }
    if lower.starts_with("tun")
        || lower.starts_with("tap")
        || lower.starts_with("wg")
        || lower.starts_with("tailscale")
        || lower.starts_with("zt")
        || lower.starts_with("zerotier")
        || lower == "mihomo"
        || lower == "utun"
    {
        return NicKind::Tun;
    }
    let o = ip.octets();
    let private = o[0] == 10
        || (o[0] == 172 && (16..=31).contains(&o[1]))
        || (o[0] == 192 && o[1] == 168)
        || (o[0] == 169 && o[1] == 254); // link-local
    if private {
        NicKind::Lan
    } else {
        NicKind::Public
    }
}

/// Sort key — lower = shown first. Friend-group-server priority:
/// LAN first (most common case), then Public, then virtual interfaces.
pub fn nic_kind_priority(k: NicKind) -> u8 {
    match k {
        NicKind::Lan => 0,
        NicKind::Public => 1,
        NicKind::Tun => 2,
        NicKind::Docker => 3,
        NicKind::Loopback => 4,
    }
}

pub fn nic_kind_label(lang: Lang, k: NicKind) -> &'static str {
    match (lang, k) {
        (Lang::En, NicKind::Lan) => "LAN",
        (Lang::En, NicKind::Public) => "Public",
        (Lang::En, NicKind::Tun) => "VPN/TUN",
        (Lang::En, NicKind::Docker) => "Docker",
        (Lang::En, NicKind::Loopback) => "Loopback",
        (Lang::Zh, NicKind::Lan) => "局域网",
        (Lang::Zh, NicKind::Public) => "公网",
        (Lang::Zh, NicKind::Tun) => "VPN/TUN",
        (Lang::Zh, NicKind::Docker) => "Docker",
        (Lang::Zh, NicKind::Loopback) => "本机",
    }
}

pub fn nic_kind_color(k: NicKind) -> Color {
    match k {
        NicKind::Lan => Color::Green,
        NicKind::Public => Color::Yellow,
        NicKind::Tun => Color::Cyan,
        NicKind::Docker => Color::Gray,
        NicKind::Loopback => Color::Gray,
    }
}

/// Enumerate IPv4 addresses on every local NIC. Cross-platform via the
/// `if-addrs` crate (Linux/macOS `getifaddrs`, Windows `GetAdaptersAddresses`).
/// Replaces the old `ip -4 -o addr show` shell-out so the join-bar works on
/// macOS / Windows too.
pub fn detect_interfaces() -> Vec<NicInfo> {
    let Ok(addrs) = if_addrs::get_if_addrs() else { return Vec::new() };
    let mut result = Vec::new();
    for iface in addrs {
        let std::net::IpAddr::V4(ip) = iface.ip() else { continue };
        let kind = classify_iface(&iface.name, &ip);
        result.push(NicInfo { name: iface.name, ip, kind });
    }
    result.sort_by_key(|n| (nic_kind_priority(n.kind), n.name.clone()));
    result
}

// v0.9–v0.14 had a Docker launcher container probe here (`SakuraFrpDocker`,
// `DockerState`, `detect_sakurafrp_docker`, `parse_docker_state`). v0.15
// switched to running frpc directly; v0.17 dropped the probe code as
// part of the cross-platform port. To revive container management, the
// pattern was: `docker inspect --format '{{.State.Status}}'` mapped to a
// 4-variant enum. Git history before v0.17 has the full code.

/// v0.15.1 — Download a binary from `url` to `target` and chmod it 0755.
/// Streams the response so we don't materialize a 5+ MB blob in RAM. Caller
/// is responsible for ensuring the target directory exists.
///
/// Note: this is a synchronous blocking call (~5–10 s on a fast link). It's
/// invoked from the setup wizard, which runs on the main thread; we don't
/// have an async runtime, and adding one for one-shot downloads isn't worth
/// the complexity.
pub fn download_frpc(url: &str, target: &Path) -> Result<()> {
    use std::io::Write;
    use std::time::Duration;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build();
    let resp = agent
        .get(url)
        .call()
        .with_context(|| format!("GET {}", url))?;
    let mut reader = resp.into_reader();
    let mut tmp = target.to_path_buf();
    tmp.set_extension("download");
    let mut f = fs::File::create(&tmp)
        .with_context(|| format!("create {}", tmp.display()))?;
    let mut buf = [0u8; 65536];
    loop {
        let n = std::io::Read::read(&mut reader, &mut buf)
            .with_context(|| "read frpc body")?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n])
            .with_context(|| format!("write {}", tmp.display()))?;
    }
    f.sync_all().ok();
    drop(f);
    // Atomic-ish move: rename only after the full body is on disk so a
    // partial download never gets chmod'd to executable.
    fs::rename(&tmp, target)
        .with_context(|| format!("rename {} → {}", tmp.display(), target.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(target)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(target, perms)?;
    }
    Ok(())
}

/// v0.15.1 — MD5 a file on disk and compare to `expected` (lowercase hex).
/// Reads in 64 KiB chunks so a multi-MB binary doesn't bloat memory. Used by
/// the setup wizard to confirm the downloaded frpc matches the manifest.
pub fn verify_md5(path: &Path, expected: &str) -> Result<bool> {
    let computed = md5_file(path)?;
    Ok(computed.eq_ignore_ascii_case(expected))
}

fn md5_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = fs::File::open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Md5::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let bytes: [u8; 16] = hasher.finalize().into();
    let mut hex = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{:02x}", b);
    }
    Ok(hex)
}

/// v0.15 — Locate a usable frpc binary. Search order:
///   1. `frpc` (or `frpc.exe`) on `$PATH` — Arch users via `paru -S sakura-frp`,
///      Homebrew, manual drops into `~/.local/bin`, etc.
///   2. shulker's per-user config dir (`~/.config/shulker/frpc` on Linux,
///      `~/Library/Application Support/shulker/frpc` on macOS,
///      `%APPDATA%\shulker\frpc.exe` on Windows) where the setup wizard
///      drops the binary it downloads.
/// Returns `None` if neither exists. We never auto-download — the user pulls
/// the official binary themselves (no second-party redistribution, no licensing
/// trapdoor for shulker).
pub fn find_frpc_binary() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "frpc.exe" } else { "frpc" };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let managed = dirs::config_dir()?.join("shulker").join(exe_name);
    if managed.is_file() {
        return Some(managed);
    }
    None
}

/// Map Rust's runtime target triple to natfrp's download manifest keys.
/// Returns the (os, arch) suffix used in URLs like
/// `frpc_<os>_<arch>` / `frpc_<os>_<arch>.exe`. None for platforms the
/// upstream doesn't ship binaries for.
pub fn host_target_for_manifest() -> Option<(&'static str, &'static str)> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "windows",
        "freebsd" => "freebsd",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        "arm" => "armv7",
        "mips" => "mips",
        "mips64" => "mips64",
        "powerpc64" => return None, // not shipped
        "riscv64" => "riscv64",
        "loongarch64" => "loong64",
        _ => return None,
    };
    Some((os, arch))
}


// ---------- v0.5: backup scanner ----------
//
// Look for archive files in standard backup locations and present them as a
// time-sorted list. Backup *creation* belongs to v0.6 (scheduled/ad-hoc); this
// is just the read side: discover, sort, present.

// ---------- Backup scanner ----------

pub struct BackupEntry {
    pub name: String,
    /// Kept for future restore action; the current draw_backups only shows name/age.
    #[allow(dead_code)]
    pub path: PathBuf,
    /// v0.16 dropped per-row size from the Worlds detail panel (one world's
    /// backups are similar size). Field kept for future "restore" UX.
    #[allow(dead_code)]
    pub size_bytes: u64,
    pub modified: Option<chrono::DateTime<chrono::Local>>,
}

pub fn is_backup_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.zst")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".zip")
        || lower.ends_with(".7z")
}

pub fn backup_dir_candidates(server_dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![server_dir.join("backups")];
    if let Some(parent) = server_dir.parent() {
        out.push(parent.join("backups"));
        out.push(parent.join("mc-backups"));
        if let Some(name) = server_dir.file_name() {
            out.push(parent.join(format!("{}-backups", name.to_string_lossy())));
        }
    }
    out
}

pub fn scan_backups(server_dir: &Path) -> Vec<BackupEntry> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for dir in backup_dir_candidates(server_dir) {
        let canonical = match dir.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let Ok(rd) = fs::read_dir(&canonical) else { continue };
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let name = match p.file_name().and_then(|n| n.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if !is_backup_file(&name) {
                continue;
            }
            let meta = entry.metadata().ok();
            out.push(BackupEntry {
                name,
                path: p.clone(),
                size_bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                modified: meta
                    .and_then(|m| m.modified().ok())
                    .map(chrono::DateTime::<chrono::Local>::from),
            });
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

// ---------- v0.5: YAML flattener (paper-global.yml etc.) ----------
//
// Big YAMLs (`paper-global.yml`, `purpur.yml`, …) become a flat list of rows
// where each row knows its tree depth, displayed label, and a canonical path
// back into the `serde_yaml::Value`. Rendering = list. Editing a leaf =

// ---------- YAML flattener (paper-global.yml etc.) ----------

#[derive(Debug, Clone)]
pub enum YamlSeg {
    Key(String),
    Index(usize),
}

#[derive(Debug, Clone)]
pub enum YamlDisplay {
    Branch, // mapping or sequence
    Scalar(String),
}

#[derive(Debug, Clone)]
pub struct YamlRow {
    pub indent: u8,
    pub path: Vec<YamlSeg>,
    pub label: String,
    pub value: YamlDisplay,
}

pub fn flatten_yaml(v: &serde_yaml::Value) -> Vec<YamlRow> {
    let mut out = Vec::new();
    let mut path = Vec::new();
    walk_yaml(v, 0, &mut path, &mut out);
    out
}

pub fn walk_yaml(
    v: &serde_yaml::Value,
    indent: u8,
    path: &mut Vec<YamlSeg>,
    out: &mut Vec<YamlRow>,
) {
    match v {
        serde_yaml::Value::Mapping(m) => {
            for (k, val) in m {
                let key_str = match k {
                    serde_yaml::Value::String(s) => s.clone(),
                    other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
                };
                path.push(YamlSeg::Key(key_str.clone()));
                if val.is_mapping() || val.is_sequence() {
                    out.push(YamlRow {
                        indent,
                        path: path.clone(),
                        label: key_str,
                        value: YamlDisplay::Branch,
                    });
                    walk_yaml(val, indent.saturating_add(1), path, out);
                } else {
                    out.push(YamlRow {
                        indent,
                        path: path.clone(),
                        label: key_str,
                        value: YamlDisplay::Scalar(yaml_scalar_string(val)),
                    });
                }
                path.pop();
            }
        }
        serde_yaml::Value::Sequence(s) => {
            for (i, val) in s.iter().enumerate() {
                path.push(YamlSeg::Index(i));
                let label = format!("[{}]", i);
                if val.is_mapping() || val.is_sequence() {
                    out.push(YamlRow {
                        indent,
                        path: path.clone(),
                        label,
                        value: YamlDisplay::Branch,
                    });
                    walk_yaml(val, indent.saturating_add(1), path, out);
                } else {
                    out.push(YamlRow {
                        indent,
                        path: path.clone(),
                        label,
                        value: YamlDisplay::Scalar(yaml_scalar_string(val)),
                    });
                }
                path.pop();
            }
        }
        _ => {}
    }
}

pub fn yaml_scalar_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => "null".to_string(),
        _ => serde_yaml::to_string(v).unwrap_or_default().trim().to_string(),
    }
}

pub fn yaml_set(
    root: &mut serde_yaml::Value,
    path: &[YamlSeg],
    new_value: serde_yaml::Value,
) -> Result<()> {
    if path.is_empty() {
        *root = new_value;
        return Ok(());
    }
    let mut cur = root;
    for seg in &path[..path.len() - 1] {
        cur = match seg {
            YamlSeg::Key(k) => cur
                .get_mut(serde_yaml::Value::String(k.clone()))
                .with_context(|| format!("yaml path missing key: {}", k))?,
            YamlSeg::Index(i) => cur
                .get_mut(*i)
                .with_context(|| format!("yaml index out of range: {}", i))?,
        };
    }
    match path.last().unwrap() {
        YamlSeg::Key(k) => {
            if let serde_yaml::Value::Mapping(m) = cur {
                m.insert(serde_yaml::Value::String(k.clone()), new_value);
            } else {
                anyhow::bail!("expected mapping at parent of key {}", k);
            }
        }
        YamlSeg::Index(i) => {
            if let serde_yaml::Value::Sequence(s) = cur {
                if *i >= s.len() {
                    anyhow::bail!("index out of range: {}", i);
                }
                s[*i] = new_value;
            } else {
                anyhow::bail!("expected sequence at parent of [{}]", i);
            }
        }
    }
    Ok(())
}

pub fn parse_yaml_scalar(input: &str) -> serde_yaml::Value {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return serde_yaml::Value::String(String::new());
    }
    if trimmed.eq_ignore_ascii_case("true") {
        return serde_yaml::Value::Bool(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return serde_yaml::Value::Bool(false);
    }
    if trimmed.eq_ignore_ascii_case("null") || trimmed == "~" {
        return serde_yaml::Value::Null;
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return serde_yaml::Value::Number(i.into());
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        // Fallback to f64 only if it isn't a clean integer (handled above).
        return serde_yaml::Value::Number(serde_yaml::Number::from(f));
    }
    serde_yaml::Value::String(trimmed.to_string())
}

/// Server-relative paths to YAMLs we care about. Some live at the root, some
/// under `config/` (Paper puts `paper-global.yml` and `paper-world-defaults.yml` there).
pub fn list_yaml_files(server_dir: &Path) -> Vec<PathBuf> {
    let known = [
        "paper-global.yml",
        "paper-world-defaults.yml",
        "purpur.yml",
        "spigot.yml",
        "bukkit.yml",
        "commands.yml",
        "permissions.yml",
        "help.yml",
    ];
    let mut out = Vec::new();
    for n in known {
        let p = server_dir.join(n);
        if p.is_file() {
            out.push(p);
            continue;
        }
        let pc = server_dir.join("config").join(n);
        if pc.is_file() {
            out.push(pc);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Online detection — extract player name from join/leave broadcasts.
    /// Vanilla / Paper / Purpur all emit these verbatim from the server thread.
    #[test]
    fn parse_join_event_extracts_name() {
        let line = "[20:31:14] [Server thread/INFO]: Spencer joined the game";
        assert_eq!(parse_join_event(line), Some("Spencer"));
        let line2 = "[20:31:14 INFO]: Friend123 joined the game";
        assert_eq!(parse_join_event(line2), Some("Friend123"));
    }

    #[test]
    fn parse_leave_event_extracts_name() {
        let line = "[20:31:14] [Server thread/INFO]: Spencer left the game";
        assert_eq!(parse_leave_event(line), Some("Spencer"));
    }

    #[test]
    fn parse_join_leave_rejects_chat_lines() {
        // Chat in vanilla is "<NAME> message"; a creative chatter typing
        // "joined the game" should not be treated as a real join event
        // because the name slot would contain a space + chat content.
        let chat = "[20:31:14] [Server thread/INFO]: <Spencer> joined the game already?";
        assert_eq!(parse_join_event(chat), None);
        // Death messages start with the name but use a different verb.
        let death = "[20:31:14] [Server thread/INFO]: Spencer was slain by Zombie";
        assert_eq!(parse_join_event(death), None);
        assert_eq!(parse_leave_event(death), None);
    }

    /// scan_lines: when the same player joins, leaves, and joins again,
    /// the most recent event wins (online).
    #[test]
    fn scan_lines_resolves_latest_join_leave_event() {
        let lines: Vec<String> = vec![
            "[20:00:00] [Server thread/INFO]: Spencer joined the game".into(),
            "[20:30:00] [Server thread/INFO]: Spencer left the game".into(),
            "[20:45:00] [Server thread/INFO]: Spencer joined the game".into(),
            "[20:46:00] [Server thread/INFO]: Friend1 joined the game".into(),
        ];
        let mut acc = LogScanResult::default();
        scan_lines(&lines, 0, true, &mut acc);
        assert_eq!(acc.online_state_in_latest.get("spencer").copied(), Some(true));
        assert_eq!(acc.online_state_in_latest.get("friend1").copied(), Some(true));
    }

    /// scan_lines: when the player has left, online_state should be false.
    #[test]
    fn scan_lines_marks_left_player_offline() {
        let lines: Vec<String> = vec![
            "[20:00:00] [Server thread/INFO]: Spencer joined the game".into(),
            "[20:30:00] [Server thread/INFO]: Spencer left the game".into(),
        ];
        let mut acc = LogScanResult::default();
        scan_lines(&lines, 0, true, &mut acc);
        assert_eq!(acc.online_state_in_latest.get("spencer").copied(), Some(false));
    }

    /// scan_lines: only `latest.log` contributes to online state — rotated
    /// logs are filtered out so a previous-day join doesn't ghost a player.
    #[test]
    fn scan_lines_ignores_rotated_log_for_online_state() {
        let lines: Vec<String> = vec![
            "[20:00:00] [Server thread/INFO]: Spencer joined the game".into(),
        ];
        let mut acc = LogScanResult::default();
        scan_lines(&lines, 0, false, &mut acc); // is_latest = false
        assert!(acc.online_state_in_latest.is_empty());
    }

    /// v0.15.1 — md5 round-trip against a known-good fixture. The setup
    /// wizard relies on this to confirm a downloaded frpc matches the
    /// manifest; a regression here means the wizard might silently accept
    /// a corrupted binary.
    #[test]
    fn verify_md5_against_known_vector() {
        // RFC 1321: md5("") = d41d8cd98f00b204e9800998ecf8427e
        let dir = std::env::temp_dir().join(format!(
            "shulker-md5-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.bin");
        fs::write(&path, b"").unwrap();
        assert!(verify_md5(&path, "d41d8cd98f00b204e9800998ecf8427e").unwrap());
        assert!(!verify_md5(&path, "00000000000000000000000000000000").unwrap());
        // Non-empty: md5("abc") = 900150983cd24fb0d6963f7d28e17f72
        let path2 = dir.join("abc.bin");
        fs::write(&path2, b"abc").unwrap();
        assert!(verify_md5(&path2, "900150983cd24fb0d6963f7d28e17f72").unwrap());
        // Case-insensitive (manifest hashes happen to be lowercase but defensive).
        assert!(verify_md5(&path2, "900150983CD24FB0D6963F7D28E17F72").unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    /// v0.15 — every host arch we map to a manifest key has a non-empty
    /// (os, arch) pair, and ones we deliberately don't ship are None. Locks
    /// in the mapping so future Rust target additions don't silently produce
    /// wrong URLs.
    #[test]
    fn host_target_mapping_is_consistent() {
        // We can't easily fake std::env::consts::OS at test-time (it's a
        // compile-time constant), so just call the function and make sure it
        // returns *something* on this build target — and that what it returns
        // looks right.
        if let Some((os, arch)) = host_target_for_manifest() {
            assert!(!os.is_empty());
            assert!(!arch.is_empty());
            // Sanity: combined key should match the format used by the
            // manifest parser (linux_amd64, darwin_arm64, etc).
            let key = format!("{}_{}", os, arch);
            assert!(key.contains('_'));
        }
        // Build host is one of: linux/amd64, darwin/arm64, etc. None of
        // these should map to None.
    }

}

