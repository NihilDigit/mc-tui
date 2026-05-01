//! Data layer for mc-tui: pure structures + filesystem / network IO.
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
        NicKind::Docker => Color::DarkGray,
        NicKind::Loopback => Color::DarkGray,
    }
}

/// Parse `ip -4 -o addr show` output. Each non-secondary line looks like:
///   `3: wlan0    inet 10.128.177.76/11 brd ... scope global ...`
/// We pull out interface name and IP, classify, and return.
pub fn detect_interfaces() -> Vec<NicInfo> {
    use std::process::Command;
    let out = Command::new("ip")
        .args(["-4", "-o", "addr", "show"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut result = Vec::new();
    for line in text.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() < 4 || toks[2] != "inet" {
            continue;
        }
        let name = toks[1].trim_end_matches(':').to_string();
        let Some(ip_part) = toks[3].split('/').next() else { continue };
        let Ok(ip) = ip_part.parse::<std::net::Ipv4Addr>() else { continue };
        let kind = classify_iface(&name, &ip);
        result.push(NicInfo { name, ip, kind });
    }
    result.sort_by_key(|n| (nic_kind_priority(n.kind), n.name.clone()));
    result
}

// ---------- SakuraFrp launcher Docker probe ----------
//
// SakuraFrp's official launcher ships as a Docker container that runs frpc
// inside. mc-tui shows whether that container is running and offers
// start / stop / restart actions — the actual frpc + tunnel config lives in
// the SakuraFrp web console (https://www.natfrp.com), we don't replicate that.
//
// All commands shell out to `docker` (no sudo); status messages bubble up to
// the App.status hint line. Probing on every refresh adds a single ~80ms
// `docker inspect` call which is acceptable for a TUI refreshed on key events.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DockerState {
    Running,
    /// Container exists but is not running (created/exited/paused/dead/restarting).
    Stopped,
    /// Container does not exist on this host.
    Missing,
    /// `docker` binary not found, or daemon unreachable.
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct SakuraFrpDocker {
    pub state: DockerState,
}

pub fn detect_sakurafrp_docker(container: &str) -> SakuraFrpDocker {
    use std::process::Command;
    let out = Command::new("docker")
        .args(["inspect", "--format", "{{.State.Status}}", container])
        .output();
    let Ok(out) = out else {
        return SakuraFrpDocker { state: DockerState::Unknown };
    };
    if !out.status.success() {
        // `docker inspect` prints "No such object" to stderr and exits 1
        // when the container doesn't exist.
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("No such object") || stderr.contains("not found") {
            return SakuraFrpDocker { state: DockerState::Missing };
        }
        return SakuraFrpDocker { state: DockerState::Unknown };
    }
    let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
    SakuraFrpDocker { state: parse_docker_state(&status) }
}

/// Map `docker inspect`'s `.State.Status` string to our enum. Pure, testable.
pub fn parse_docker_state(s: &str) -> DockerState {
    match s.trim() {
        "running" => DockerState::Running,
        "" => DockerState::Unknown,
        // created / exited / paused / restarting / dead / removing — all "not running"
        _ => DockerState::Stopped,
    }
}

/// Run `docker <verb> <container>` synchronously and return stdout/stderr in a
/// single Result so the caller can feed it straight to App.status.
pub fn docker_lifecycle(verb: &str, container: &str) -> Result<String> {
    use std::process::Command;
    let out = Command::new("docker")
        .args([verb, container])
        .output()
        .with_context(|| format!("spawn docker {}", verb))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        anyhow::bail!("docker {} {}: {}", verb, container, err)
    }
}

// ---------- v0.5: backup scanner ----------
//
// Look for archive files in standard backup locations and present them as a
// time-sorted list. Backup *creation* belongs to v0.6 (scheduled/ad-hoc); this
// is just the read side: discover, sort, present.

// ---------- Backup scanner ----------

pub struct BackupEntry {
    pub name: String,
    /// Kept for future restore action; the current draw_backups only shows name/size/age.
    #[allow(dead_code)]
    pub path: PathBuf,
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

