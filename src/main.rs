//! mc-tui — a TUI manager for a local Minecraft Paper/Purpur server.

use std::{
    fs,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use md5::{Digest, Md5};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Terminal,
};
use serde::{Deserialize, Serialize};

// ---------- CLI ----------

#[derive(Parser, Debug)]
#[command(name = "mc-tui", about, version)]
struct Cli {
    /// Path to the Minecraft server directory (must contain server.properties).
    /// If omitted, falls back to the value remembered in $XDG_CONFIG_HOME/mc-tui/state.toml.
    #[arg(short = 'd', long, env = "MC_SERVER_DIR", global = true)]
    server_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// Run the TUI (default if omitted).
    Run,
    /// Scaffold a new server directory: pick MC version + Paper/Purpur, download jar, write start.sh.
    New {
        /// Target directory.
        dir: PathBuf,
        /// Allow scaffolding into a non-empty directory.
        #[arg(long)]
        force: bool,
        /// MC version (e.g. 1.21.4). Defaults to latest.
        #[arg(long)]
        mc_version: Option<String>,
        /// Server type: paper or purpur. Defaults to purpur.
        #[arg(long, value_enum, default_value_t = ServerType::Purpur)]
        server_type: ServerType,
        /// Run server once after scaffolding to generate server.properties, then stop.
        #[arg(long)]
        first_boot: bool,
    },
    /// Render one TUI frame to stdout as text (used for screenshot QA).
    Screenshot {
        /// Tab to render: worlds | whitelist | ops | config | logs | backups | rcon | yaml | ops-panel.
        #[arg(long, default_value = "worlds")]
        tab: String,
        /// Width in cells.
        #[arg(long, default_value_t = 100)]
        width: u16,
        /// Height in cells.
        #[arg(long, default_value_t = 30)]
        height: u16,
        /// Language: en or zh.
        #[arg(long, default_value = "en")]
        lang: String,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ServerType {
    Paper,
    Purpur,
}

impl ServerType {
    fn name(self) -> &'static str {
        match self {
            ServerType::Paper => "paper",
            ServerType::Purpur => "purpur",
        }
    }
}

// ---------- Data layer ----------

#[derive(Debug, Clone)]
struct WorldEntry {
    name: String,
    #[allow(dead_code)]
    path: PathBuf,
    size_bytes: u64,
    last_modified: Option<chrono::DateTime<chrono::Local>>,
    is_current: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WhitelistEntry {
    uuid: String,
    name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpEntry {
    uuid: String,
    name: String,
    level: u8,
    #[serde(rename = "bypassesPlayerLimit", default)]
    bypasses_player_limit: bool,
}

fn offline_uuid(name: &str) -> String {
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

fn dir_size(path: &Path) -> u64 {
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

fn fmt_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut x = n as f64;
    let mut i = 0;
    while x >= 1024.0 && i < UNITS.len() - 1 {
        x /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", x, UNITS[i])
}

fn read_properties(path: &Path) -> Result<Vec<(String, String)>> {
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

fn write_properties(path: &Path, props: &[(String, String)]) -> Result<()> {
    let mut s = String::new();
    s.push_str("#Minecraft server properties\n");
    s.push_str(&format!("#{}\n", chrono::Local::now().to_rfc2822()));
    for (k, v) in props {
        s.push_str(&format!("{}={}\n", k, v));
    }
    fs::write(path, s).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn get_property<'a>(props: &'a [(String, String)], key: &str) -> Option<&'a str> {
    props.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn set_property(props: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some(slot) = props.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value.to_string();
    } else {
        props.push((key.to_string(), value.to_string()));
    }
}

fn scan_worlds(server_dir: &Path, current_level: &str) -> Vec<WorldEntry> {
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
        out.push(WorldEntry { name, path, size_bytes, last_modified, is_current });
    }
    out.sort_by(|a, b| b.is_current.cmp(&a.is_current).then(a.name.cmp(&b.name)));
    out
}

fn read_whitelist(server_dir: &Path) -> Result<Vec<WhitelistEntry>> {
    let path = server_dir.join("whitelist.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn write_whitelist(server_dir: &Path, entries: &[WhitelistEntry]) -> Result<()> {
    let path = server_dir.join("whitelist.json");
    let json = serde_json::to_string_pretty(entries)?;
    fs::write(&path, json)?;
    Ok(())
}

fn read_ops(server_dir: &Path) -> Result<Vec<OpEntry>> {
    let path = server_dir.join("ops.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn write_ops(server_dir: &Path, entries: &[OpEntry]) -> Result<()> {
    let path = server_dir.join("ops.json");
    let json = serde_json::to_string_pretty(entries)?;
    fs::write(&path, json)?;
    Ok(())
}

fn server_running_pid(server_dir: &Path) -> Option<u32> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::everything()),
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let canonical = server_dir.canonicalize().ok();
    for (pid, proc) in sys.processes() {
        let cmd = proc.cmd();
        let has_jar = cmd.iter().any(|s| {
            let s = s.to_string_lossy();
            s.ends_with(".jar")
                && (s.contains("paper") || s.contains("purpur") || s.contains("spigot"))
        });
        if !has_jar {
            continue;
        }
        let cwd = proc.cwd();
        let matches = match (cwd, canonical.as_ref()) {
            (Some(cwd), Some(c)) => cwd == c.as_path(),
            _ => false,
        };
        if matches {
            return Some(pid.as_u32());
        }
    }
    None
}

// ---------- i18n ----------
//
// All user-facing strings live here. UI / status code refers to them as
// `app.lang.s().<field>` (static) or `fmt_<event>(lang, ...)` (parametric).
// New strings: add to `Strings` + populate `EN` and `ZH`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Lang {
    #[default]
    En,
    Zh,
}

impl Lang {
    fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Zh => "zh",
        }
    }
    fn from_code(s: &str) -> Lang {
        match s {
            "zh" | "zh-CN" | "cn" => Lang::Zh,
            _ => Lang::En,
        }
    }
    fn toggle(self) -> Lang {
        match self {
            Lang::En => Lang::Zh,
            Lang::Zh => Lang::En,
        }
    }
    fn s(self) -> &'static Strings {
        match self {
            Lang::En => &EN,
            Lang::Zh => &ZH,
        }
    }
}

#[allow(dead_code)] // some fields populated for future tabs (backups/rcon/yaml)
struct Strings {
    // Status bar
    server_label: &'static str,
    level_label: &'static str,
    dir_label: &'static str,
    status_stopped: &'static str,
    // Tab names
    tab_worlds: &'static str,
    tab_whitelist: &'static str,
    tab_ops: &'static str,
    tab_config: &'static str,
    tab_logs: &'static str,
    tab_backups: &'static str,
    tab_rcon: &'static str,
    tab_ops_panel: &'static str,
    // Pane titles
    title_worlds: &'static str,
    title_whitelist: &'static str,
    title_ops: &'static str,
    title_config: &'static str,
    title_logs_prefix: &'static str, // prefix; full title = `<prefix><path> `
    // Hints
    hint_worlds: &'static str,
    hint_whitelist: &'static str,
    hint_ops: &'static str,
    hint_config: &'static str,
    hint_logs: &'static str,
    // Prompt chrome
    prompt_confirm_cancel: &'static str,
    prompt_label_player: &'static str,
    prompt_label_world: &'static str,
    prompt_label_value: &'static str,
    prompt_label_path: &'static str,
    prompt_title_add_whitelist: &'static str,
    prompt_title_op_player: &'static str,
    prompt_title_new_world: &'static str,
    prompt_title_change_dir: &'static str,
    // Static status / errors
    ready: &'static str,
    refreshed: &'static str,
    cancelled: &'static str,
    err_already_running: &'static str,
    err_not_running: &'static str,
    err_stop_first: &'static str,
    err_already_current_world: &'static str,
    no_logs_yet: &'static str,
    spawn_started: &'static str,
    // Detail-panel headers (used by hover-detail feature)
    detail_header: &'static str,
    detail_default: &'static str,
    detail_range: &'static str,
    detail_no_info: &'static str,
}

const EN: Strings = Strings {
    server_label: "server: ",
    level_label: "level: ",
    dir_label: "dir: ",
    status_stopped: "○ stopped",
    tab_worlds: "Worlds",
    tab_whitelist: "Whitelist",
    tab_ops: "Ops",
    tab_config: "Config",
    tab_logs: "Logs",
    tab_backups: "Backups",
    tab_rcon: "RCON",
    tab_ops_panel: "Server",
    title_worlds: " Worlds (●=current) ",
    title_whitelist: " Whitelist ",
    title_ops: " Operators (←/→ change level) ",
    title_config: " server.properties (Enter = edit) ",
    title_logs_prefix: " Logs — tail of ",
    hint_worlds: "↑/↓ select   Enter switch   N new   S start   X stop   D dir   L lang   r refresh   q quit",
    hint_whitelist: "↑/↓ select   a add   d remove   S start   X stop   D dir   L lang   r refresh   q quit",
    hint_ops: "↑/↓ select   a add   d remove   ←/→ level   S start   X stop   D dir   L lang   q quit",
    hint_config: "↑/↓ select   Enter edit   S start   X stop   D dir   L lang   r refresh   q quit",
    hint_logs: "S start   X stop   D dir   L lang   r refresh   Tab/1-9 tabs   q quit",
    prompt_confirm_cancel: "Enter = confirm    Esc = cancel",
    prompt_label_player: "player name",
    prompt_label_world: "world name",
    prompt_label_value: "value",
    prompt_label_path: "path",
    prompt_title_add_whitelist: "Add to whitelist",
    prompt_title_op_player: "Op a player",
    prompt_title_new_world: "Create new world",
    prompt_title_change_dir: "Switch server-dir",
    ready: "Ready.",
    refreshed: "Refreshed.",
    cancelled: "Cancelled.",
    err_already_running: "✗ Server already running.",
    err_not_running: "✗ Server not running.",
    err_stop_first: "✗ Stop the server first (it's running).",
    err_already_current_world: "→ Already current world.",
    no_logs_yet: "(no logs yet)",
    spawn_started: "→ Spawned start.sh (detached). Waiting for pid…",
    detail_header: "Details",
    detail_default: "Default",
    detail_range: "Range",
    detail_no_info: "(no extra info)",
};

const ZH: Strings = Strings {
    server_label: "服务器: ",
    level_label: "世界: ",
    dir_label: "目录: ",
    status_stopped: "○ 已停止",
    tab_worlds: "世界",
    tab_whitelist: "白名单",
    tab_ops: "管理员",
    tab_config: "配置",
    tab_logs: "日志",
    tab_backups: "备份",
    tab_rcon: "RCON",
    tab_ops_panel: "运维",
    title_worlds: " 世界 (●=当前) ",
    title_whitelist: " 白名单 ",
    title_ops: " 管理员 (←/→ 调整级别) ",
    title_config: " server.properties (Enter 编辑) ",
    title_logs_prefix: " 日志 — 末尾来自 ",
    hint_worlds: "↑/↓ 选择   Enter 切换   N 新建   S 启动   X 停止   D 切换目录   L 语言   r 刷新   q 退出",
    hint_whitelist: "↑/↓ 选择   a 添加   d 移除   S 启动   X 停止   D 切换目录   L 语言   r 刷新   q 退出",
    hint_ops: "↑/↓ 选择   a 添加   d 移除   ←/→ 级别   S 启动   X 停止   D 切换目录   L 语言   q 退出",
    hint_config: "↑/↓ 选择   Enter 编辑   S 启动   X 停止   D 切换目录   L 语言   r 刷新   q 退出",
    hint_logs: "S 启动   X 停止   D 切换目录   L 语言   r 刷新   Tab/1-9 切换   q 退出",
    prompt_confirm_cancel: "Enter 确认    Esc 取消",
    prompt_label_player: "玩家名",
    prompt_label_world: "世界名",
    prompt_label_value: "值",
    prompt_label_path: "路径",
    prompt_title_add_whitelist: "加入白名单",
    prompt_title_op_player: "提升为 OP",
    prompt_title_new_world: "创建新世界",
    prompt_title_change_dir: "切换 server-dir",
    ready: "就绪。",
    refreshed: "已刷新。",
    cancelled: "已取消。",
    err_already_running: "✗ 服务器已在运行。",
    err_not_running: "✗ 服务器未运行。",
    err_stop_first: "✗ 请先停止运行中的服务器。",
    err_already_current_world: "→ 已是当前世界。",
    no_logs_yet: "(暂无日志)",
    spawn_started: "→ 已分离启动 start.sh，等待 pid 出现…",
    detail_header: "详情",
    detail_default: "默认",
    detail_range: "取值",
    detail_no_info: "(暂无说明)",
};

// Parametric messages — return owned Strings.

fn tab_name(lang: Lang, id: TabId) -> &'static str {
    let s = lang.s();
    match id {
        TabId::Worlds => s.tab_worlds,
        TabId::Whitelist => s.tab_whitelist,
        TabId::Ops => s.tab_ops,
        TabId::Config => s.tab_config,
        TabId::Logs => s.tab_logs,
    }
}

fn hint_for(lang: Lang, id: TabId) -> &'static str {
    let s = lang.s();
    match id {
        TabId::Worlds => s.hint_worlds,
        TabId::Whitelist => s.hint_whitelist,
        TabId::Ops => s.hint_ops,
        TabId::Config => s.hint_config,
        TabId::Logs => s.hint_logs,
    }
}

fn fmt_status_running(lang: Lang, pid: u32) -> String {
    match lang {
        Lang::En => format!("● running (pid {})", pid),
        Lang::Zh => format!("● 运行中 (pid {})", pid),
    }
}
fn fmt_world_switched(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ Switched to '{}'. Restart the server to load it.", name),
        Lang::Zh => format!("✓ 已切换到 '{}'。请重启服务器以加载。", name),
    }
}
fn fmt_world_created(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ level-name='{}'. Next start will generate the world.", name),
        Lang::Zh => format!("✓ level-name='{}'。下次启动将生成该世界。", name),
    }
}
fn fmt_world_invalid(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✗ Invalid world name: '{}'.", name),
        Lang::Zh => format!("✗ 非法世界名: '{}'。", name),
    }
}
fn fmt_world_exists(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✗ '{}' already exists.", name),
        Lang::Zh => format!("✗ '{}' 已存在。", name),
    }
}
fn fmt_dir_no_properties(lang: Lang, path: &Path) -> String {
    match lang {
        Lang::En => format!("✗ {} has no server.properties.", path.display()),
        Lang::Zh => format!("✗ {} 中没有 server.properties。", path.display()),
    }
}
fn fmt_dir_canon_failed(lang: Lang, path: &Path, err: &str) -> String {
    match lang {
        Lang::En => format!("✗ {}: {}", path.display(), err),
        Lang::Zh => format!("✗ {}：{}", path.display(), err),
    }
}
fn fmt_dir_switched(lang: Lang, path: &Path) -> String {
    match lang {
        Lang::En => format!("✓ Switched to {}.", path.display()),
        Lang::Zh => format!("✓ 已切换到 {}。", path.display()),
    }
}
fn fmt_already_whitelisted(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("→ '{}' already whitelisted.", name),
        Lang::Zh => format!("→ '{}' 已在白名单。", name),
    }
}
fn fmt_whitelist_added(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ Whitelisted {}.", name),
        Lang::Zh => format!("✓ 已加入白名单：{}。", name),
    }
}
fn fmt_whitelist_removed(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ Removed {} from whitelist.", name),
        Lang::Zh => format!("✓ 已从白名单移除：{}。", name),
    }
}
fn fmt_already_op(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("→ '{}' already op.", name),
        Lang::Zh => format!("→ '{}' 已是 OP。", name),
    }
}
fn fmt_op_added(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ Op'd {} (level 4).", name),
        Lang::Zh => format!("✓ 已设为 OP：{}（级别 4）。", name),
    }
}
fn fmt_op_removed(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ De-op'd {}.", name),
        Lang::Zh => format!("✓ 已撤销 OP：{}。", name),
    }
}
fn fmt_op_level_changed(lang: Lang, name: &str, level: u8) -> String {
    match lang {
        Lang::En => format!("✓ {} → level {}.", name, level),
        Lang::Zh => format!("✓ {} → 级别 {}。", name, level),
    }
}
fn fmt_config_saved(lang: Lang, key: &str, value: &str) -> String {
    match lang {
        Lang::En => format!("✓ {} = {}", key, value),
        Lang::Zh => format!("✓ {} = {}", key, value),
    }
}
fn fmt_lang_toggled(lang: Lang) -> String {
    match lang {
        Lang::En => "✓ Language: English.".into(),
        Lang::Zh => "✓ 语言：中文。".into(),
    }
}
fn fmt_start_script_missing(lang: Lang, path: &Path) -> String {
    match lang {
        Lang::En => format!("✗ {} not found. Create a start.sh first.", path.display()),
        Lang::Zh => format!("✗ {} 不存在，请先创建 start.sh。", path.display()),
    }
}
fn fmt_spawn_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::En => format!("✗ Spawn failed: {}", err),
        Lang::Zh => format!("✗ 启动失败: {}", err),
    }
}
fn fmt_kill_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::En => format!("✗ kill failed: {}", err),
        Lang::Zh => format!("✗ kill 失败: {}", err),
    }
}
fn fmt_stop_sent(lang: Lang, pid: u32) -> String {
    match lang {
        Lang::En => format!("→ SIGTERM → pid {}. Waiting for graceful shutdown…", pid),
        Lang::Zh => format!("→ 已发送 SIGTERM → pid {}。等待平滑停服…", pid),
    }
}
fn fmt_log_read_error(lang: Lang, err: &str) -> String {
    match lang {
        Lang::En => format!("(read error: {})", err),
        Lang::Zh => format!("(读取失败: {})", err),
    }
}

/// Pick `en` or `zh` based on `lang`. Kept for ad-hoc spots (rare); prefer
/// `lang.s().<field>` or `fmt_*` helpers above.
#[allow(dead_code)]
fn t<'a>(lang: Lang, en: &'a str, zh: &'a str) -> &'a str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// Chinese annotation for common `server.properties` keys.
/// Returns `None` for unknown keys (caller should fall back to showing only the raw key).
fn property_zh(key: &str) -> Option<&'static str> {
    Some(match key {
        "max-players" => "最大玩家数",
        "view-distance" => "视距",
        "simulation-distance" => "模拟距离",
        "difficulty" => "难度",
        "gamemode" => "默认游戏模式",
        "pvp" => "PVP",
        "hardcore" => "极限模式",
        "online-mode" => "正版验证",
        "white-list" => "启用白名单",
        "enforce-whitelist" => "强制白名单",
        "spawn-protection" => "出生点保护(格)",
        "motd" => "服务器描述(MOTD)",
        "level-name" => "世界名",
        "level-type" => "世界类型",
        "level-seed" => "世界种子",
        "server-port" => "端口",
        "server-ip" => "绑定 IP",
        "max-world-size" => "世界大小上限",
        "allow-flight" => "允许飞行",
        "allow-nether" => "启用下界",
        "spawn-monsters" => "生成怪物",
        "spawn-animals" => "生成动物",
        "spawn-npcs" => "生成村民等 NPC",
        "enable-rcon" => "启用 RCON",
        "rcon.password" => "RCON 密码",
        "rcon.port" => "RCON 端口",
        "enable-query" => "启用 Query",
        "query.port" => "Query 端口",
        "max-tick-time" => "最大 tick 时长",
        "network-compression-threshold" => "网络压缩阈值",
        "op-permission-level" => "OP 默认权限级别",
        "function-permission-level" => "函数权限级别",
        "broadcast-console-to-ops" => "控制台广播给 OP",
        "broadcast-rcon-to-ops" => "RCON 广播给 OP",
        "force-gamemode" => "强制默认模式",
        "use-native-transport" => "使用原生传输",
        "prevent-proxy-connections" => "禁止代理连接",
        "generate-structures" => "生成结构",
        "resource-pack" => "资源包 URL",
        "resource-pack-prompt" => "资源包提示",
        "require-resource-pack" => "强制资源包",
        "hide-online-players" => "隐藏在线玩家",
        "rate-limit" => "速率限制",
        "player-idle-timeout" => "玩家闲置超时(分钟)",
        "max-chained-neighbor-updates" => "链式邻接更新上限",
        "sync-chunk-writes" => "同步区块写入",
        "entity-broadcast-range-percentage" => "实体广播范围 %",
        "log-ips" => "记录 IP",
        "previews-chat" => "聊天预览",
        "enable-status" => "启用状态查询",
        "enable-jmx-monitoring" => "启用 JMX 监控",
        "enable-command-block" => "启用命令方块",
        "enforce-secure-profile" => "强制安全档案",
        "snooper-enabled" => "发送统计信息",
        "max-build-height" => "建筑高度上限",
        "spawn-protection-radius" => "出生点保护半径",
        "accepts-transfers" => "接受转移连接",
        "bug-report-link" => "Bug 反馈链接",
        "debug" => "调试模式",
        "region-file-compression" => "区域文件压缩",
        "text-filtering-config" => "文本过滤配置",
        "text-filtering-version" => "文本过滤版本",
        "pause-when-empty-seconds" => "无人时暂停秒数",
        "enable-code-of-conduct" => "启用行为准则",
        "initial-enabled-packs" => "初始启用数据包",
        "initial-disabled-packs" => "初始禁用数据包",
        _ => return None,
    })
}

// ---------- Persistent state (state.toml) ----------

fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("XDG_CONFIG_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p).join("mc-tui");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("mc-tui");
    }
    PathBuf::from(".mc-tui")
}

fn state_path() -> PathBuf {
    config_dir().join("state.toml")
}

#[derive(Debug, Default, Clone)]
struct PersistedState {
    server_dir: Option<PathBuf>,
    lang: Option<String>,
}

fn read_persisted_state() -> PersistedState {
    let path = state_path();
    let Ok(raw) = fs::read_to_string(&path) else {
        return PersistedState::default();
    };
    let mut state = PersistedState::default();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(eq) = line.find('=') {
            let k = line[..eq].trim();
            let v = line[eq + 1..].trim().trim_matches('"').to_string();
            match k {
                "server_dir" => state.server_dir = Some(PathBuf::from(v)),
                "lang" => state.lang = Some(v),
                _ => {}
            }
        }
    }
    state
}

fn write_persisted_state(state: &PersistedState) -> Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let mut s = String::from("# mc-tui state — auto-managed, hand-edit at your own risk.\n");
    if let Some(dir) = &state.server_dir {
        s.push_str(&format!("server_dir = \"{}\"\n", dir.display()));
    }
    if let Some(lang) = &state.lang {
        s.push_str(&format!("lang = \"{}\"\n", lang));
    }
    fs::write(&path, s).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

// ---------- App state ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabId {
    Worlds,
    Whitelist,
    Ops,
    Config,
    Logs,
}

const TABS: &[(TabId, &str)] = &[
    (TabId::Worlds, "Worlds"),
    (TabId::Whitelist, "Whitelist"),
    (TabId::Ops, "Ops"),
    (TabId::Config, "Config"),
    (TabId::Logs, "Logs"),
];

#[derive(Debug, Clone)]
struct InputPrompt {
    title: String,
    label: String,
    buffer: String,
    action: PromptAction,
}

#[derive(Debug, Clone)]
enum PromptAction {
    AddWhitelist,
    AddOp,
    EditConfig(String),
    NewWorld,
    ChangeServerDir,
}

struct App {
    server_dir: PathBuf,
    properties: Vec<(String, String)>,
    worlds: Vec<WorldEntry>,
    whitelist: Vec<WhitelistEntry>,
    ops: Vec<OpEntry>,
    pid: Option<u32>,

    tab: TabId,
    worlds_state: ListState,
    whitelist_state: ListState,
    ops_state: ListState,
    config_state: ListState,

    status: String,
    prompt: Option<InputPrompt>,

    // Mouse hit-testing rects, populated each frame inside `ui()`.
    tabs_rect: Rect,
    list_rect: Rect,

    lang: Lang,
}

impl App {
    fn new(server_dir: PathBuf) -> Result<Self> {
        Self::new_with_lang(server_dir, Lang::default())
    }

    fn new_with_lang(server_dir: PathBuf, lang: Lang) -> Result<Self> {
        let server_dir = server_dir.canonicalize().with_context(|| {
            format!("server-dir does not exist: {}", server_dir.display())
        })?;
        let properties = read_properties(&server_dir.join("server.properties"))
            .context("read server.properties")?;
        let mut app = App {
            server_dir,
            properties,
            worlds: Vec::new(),
            whitelist: Vec::new(),
            ops: Vec::new(),
            pid: None,
            tab: TabId::Worlds,
            worlds_state: ListState::default(),
            whitelist_state: ListState::default(),
            ops_state: ListState::default(),
            config_state: ListState::default(),
            status: match lang {
                Lang::En => String::from("Ready."),
                Lang::Zh => String::from("就绪。"),
            },
            prompt: None,
            tabs_rect: Rect::default(),
            list_rect: Rect::default(),
            lang,
        };
        app.refresh_all();
        if !app.worlds.is_empty() {
            app.worlds_state.select(Some(0));
        }
        if !app.whitelist.is_empty() {
            app.whitelist_state.select(Some(0));
        }
        if !app.ops.is_empty() {
            app.ops_state.select(Some(0));
        }
        if !app.properties.is_empty() {
            app.config_state.select(Some(0));
        }
        Ok(app)
    }

    fn current_level(&self) -> &str {
        get_property(&self.properties, "level-name").unwrap_or("world")
    }

    fn refresh_all(&mut self) {
        let cur = self.current_level().to_string();
        self.worlds = scan_worlds(&self.server_dir, &cur);
        self.whitelist = read_whitelist(&self.server_dir).unwrap_or_default();
        self.ops = read_ops(&self.server_dir).unwrap_or_default();
        self.pid = server_running_pid(&self.server_dir);
    }

    fn list_state_for(&mut self, tab: TabId) -> &mut ListState {
        match tab {
            TabId::Worlds => &mut self.worlds_state,
            TabId::Whitelist => &mut self.whitelist_state,
            TabId::Ops => &mut self.ops_state,
            TabId::Config => &mut self.config_state,
            TabId::Logs => &mut self.worlds_state,
        }
    }

    fn list_len_for(&self, tab: TabId) -> usize {
        match tab {
            TabId::Worlds => self.worlds.len(),
            TabId::Whitelist => self.whitelist.len(),
            TabId::Ops => self.ops.len(),
            TabId::Config => self.properties.len(),
            TabId::Logs => 0,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.list_len_for(self.tab);
        if len == 0 {
            return;
        }
        let tab = self.tab;
        let state = self.list_state_for(tab);
        let cur = state.selected().unwrap_or(0) as isize;
        let new = (cur + delta).rem_euclid(len as isize) as usize;
        state.select(Some(new));
    }

    fn switch_tab(&mut self, tab: TabId) {
        self.tab = tab;
    }

    fn cycle_tab(&mut self, dir: isize) {
        let cur_idx = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0) as isize;
        let n = TABS.len() as isize;
        let new = (cur_idx + dir).rem_euclid(n) as usize;
        self.tab = TABS[new].0;
    }

    fn switch_world(&mut self) -> Result<()> {
        if self.pid.is_some() {
            self.status = self.lang.s().err_stop_first.into();
            return Ok(());
        }
        let Some(idx) = self.worlds_state.selected() else { return Ok(()) };
        let Some(entry) = self.worlds.get(idx) else { return Ok(()) };
        if entry.is_current {
            self.status = self.lang.s().err_already_current_world.into();
            return Ok(());
        }
        let new_name = entry.name.clone();
        set_property(&mut self.properties, "level-name", &new_name);
        write_properties(&self.server_dir.join("server.properties"), &self.properties)?;
        self.status = fmt_world_switched(self.lang, &new_name);
        self.refresh_all();
        Ok(())
    }

    fn add_whitelist(&mut self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        if self.whitelist.iter().any(|e| e.name == name) {
            self.status = fmt_already_whitelisted(self.lang, name);
            return Ok(());
        }
        self.whitelist.push(WhitelistEntry {
            uuid: offline_uuid(name),
            name: name.to_string(),
        });
        write_whitelist(&self.server_dir, &self.whitelist)?;
        self.status = fmt_whitelist_added(self.lang, name);
        self.refresh_all();
        Ok(())
    }

    fn remove_whitelist(&mut self) -> Result<()> {
        let Some(idx) = self.whitelist_state.selected() else { return Ok(()) };
        if idx >= self.whitelist.len() {
            return Ok(());
        }
        let removed = self.whitelist.remove(idx);
        write_whitelist(&self.server_dir, &self.whitelist)?;
        self.status = fmt_whitelist_removed(self.lang, &removed.name);
        if self.whitelist.is_empty() {
            self.whitelist_state.select(None);
        } else if idx >= self.whitelist.len() {
            self.whitelist_state.select(Some(self.whitelist.len() - 1));
        }
        Ok(())
    }

    fn add_op(&mut self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        if self.ops.iter().any(|e| e.name == name) {
            self.status = fmt_already_op(self.lang, name);
            return Ok(());
        }
        self.ops.push(OpEntry {
            uuid: offline_uuid(name),
            name: name.to_string(),
            level: 4,
            bypasses_player_limit: false,
        });
        write_ops(&self.server_dir, &self.ops)?;
        self.status = fmt_op_added(self.lang, name);
        self.refresh_all();
        Ok(())
    }

    fn remove_op(&mut self) -> Result<()> {
        let Some(idx) = self.ops_state.selected() else { return Ok(()) };
        if idx >= self.ops.len() {
            return Ok(());
        }
        let removed = self.ops.remove(idx);
        write_ops(&self.server_dir, &self.ops)?;
        self.status = fmt_op_removed(self.lang, &removed.name);
        if self.ops.is_empty() {
            self.ops_state.select(None);
        } else if idx >= self.ops.len() {
            self.ops_state.select(Some(self.ops.len() - 1));
        }
        Ok(())
    }

    fn cycle_op_level(&mut self, dir: i8) -> Result<()> {
        let Some(idx) = self.ops_state.selected() else { return Ok(()) };
        if idx >= self.ops.len() {
            return Ok(());
        }
        let cur = self.ops[idx].level as i16;
        let new = (cur + dir as i16).clamp(1, 4) as u8;
        self.ops[idx].level = new;
        write_ops(&self.server_dir, &self.ops)?;
        let name = self.ops[idx].name.clone();
        self.status = fmt_op_level_changed(self.lang, &name, new);
        Ok(())
    }

    fn save_config_value(&mut self, key: &str, value: &str) -> Result<()> {
        set_property(&mut self.properties, key, value);
        write_properties(&self.server_dir.join("server.properties"), &self.properties)?;
        self.status = fmt_config_saved(self.lang, key, value);
        Ok(())
    }

    // -- v0.2: lifecycle --

    fn start_server(&mut self) -> Result<()> {
        if self.pid.is_some() {
            self.status = self.lang.s().err_already_running.into();
            return Ok(());
        }
        let script = self.server_dir.join("start.sh");
        if !script.exists() {
            self.status = fmt_start_script_missing(self.lang, &script);
            return Ok(());
        }
        use std::process::{Command, Stdio};
        // Use `setsid` on unix to detach the child into its own session so closing mc-tui doesn't HUP it.
        // Fall back to plain bash on platforms without setsid.
        let mut cmd = if cfg!(unix) && which("setsid").is_some() {
            let mut c = Command::new("setsid");
            c.arg("bash").arg(&script);
            c
        } else {
            let mut c = Command::new("bash");
            c.arg(&script);
            c
        };
        cmd.current_dir(&self.server_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match cmd.spawn() {
            Ok(_) => self.status = self.lang.s().spawn_started.into(),
            Err(e) => self.status = fmt_spawn_failed(self.lang, &e.to_string()),
        }
        Ok(())
    }

    fn stop_server(&mut self) -> Result<()> {
        let Some(pid) = self.pid else {
            self.status = self.lang.s().err_not_running.into();
            return Ok(());
        };
        use std::process::Command;
        #[cfg(unix)]
        let res = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
        #[cfg(not(unix))]
        let res = Command::new("taskkill")
            .arg("/PID")
            .arg(pid.to_string())
            .status();
        match res {
            Ok(_) => self.status = fmt_stop_sent(self.lang, pid),
            Err(e) => self.status = fmt_kill_failed(self.lang, &e.to_string()),
        }
        Ok(())
    }

    // -- v0.2: create new world --

    fn create_new_world(&mut self, name: &str) -> Result<()> {
        if self.pid.is_some() {
            self.status = self.lang.s().err_stop_first.into();
            return Ok(());
        }
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        if name.contains('/')
            || name.contains('\\')
            || name == "."
            || name == ".."
            || name.contains('\0')
        {
            self.status = fmt_world_invalid(self.lang, name);
            return Ok(());
        }
        let target = self.server_dir.join(name);
        if target.exists() {
            self.status = fmt_world_exists(self.lang, name);
            return Ok(());
        }
        set_property(&mut self.properties, "level-name", name);
        write_properties(&self.server_dir.join("server.properties"), &self.properties)?;
        self.status = fmt_world_created(self.lang, name);
        self.refresh_all();
        Ok(())
    }

    // -- v0.3: language toggle --

    fn toggle_lang(&mut self) {
        self.lang = self.lang.toggle();
        let mut state = read_persisted_state();
        state.lang = Some(self.lang.code().to_string());
        let _ = write_persisted_state(&state);
        self.status = fmt_lang_toggled(self.lang);
    }

    // -- v0.2: change server-dir --

    fn change_server_dir(&mut self, raw: &str) -> Result<()> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let path = expand_tilde(trimmed);
        if !path.join("server.properties").exists() {
            self.status = fmt_dir_no_properties(self.lang, &path);
            return Ok(());
        }
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                self.status = fmt_dir_canon_failed(self.lang, &path, &e.to_string());
                return Ok(());
            }
        };
        self.server_dir = canonical;
        self.properties = read_properties(&self.server_dir.join("server.properties"))?;
        self.refresh_all();

        self.worlds_state = ListState::default();
        if !self.worlds.is_empty() {
            self.worlds_state.select(Some(0));
        }
        self.whitelist_state = ListState::default();
        if !self.whitelist.is_empty() {
            self.whitelist_state.select(Some(0));
        }
        self.ops_state = ListState::default();
        if !self.ops.is_empty() {
            self.ops_state.select(Some(0));
        }
        self.config_state = ListState::default();
        if !self.properties.is_empty() {
            self.config_state.select(Some(0));
        }

        let mut state = read_persisted_state();
        state.server_dir = Some(self.server_dir.clone());
        let _ = write_persisted_state(&state);

        self.status = fmt_dir_switched(self.lang, &self.server_dir);
        Ok(())
    }
}

fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(prog);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    if p == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(p)
}

// ---------- UI ----------

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(f.area());

    draw_status_bar(f, chunks[0], app);
    draw_tabs(f, chunks[1], app);
    app.tabs_rect = chunks[1];
    app.list_rect = chunks[2];
    match app.tab {
        TabId::Worlds => draw_worlds(f, chunks[2], app),
        TabId::Whitelist => draw_whitelist(f, chunks[2], app),
        TabId::Ops => draw_ops(f, chunks[2], app),
        TabId::Config => draw_config(f, chunks[2], app),
        TabId::Logs => draw_logs(f, chunks[2], app),
    }
    draw_hints(f, chunks[3], app);

    if let Some(prompt) = app.prompt.clone() {
        draw_prompt(f, &prompt, app.lang);
    }
}

fn draw_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let pid_text = match app.pid {
        Some(p) => Span::styled(fmt_status_running(app.lang, p), Style::default().fg(Color::Green)),
        None => Span::styled(s.status_stopped, Style::default().fg(Color::DarkGray)),
    };
    let line = Line::from(vec![
        Span::styled(s.server_label, Style::default().add_modifier(Modifier::DIM)),
        pid_text,
        Span::raw("    "),
        Span::styled(s.level_label, Style::default().add_modifier(Modifier::DIM)),
        Span::styled(app.current_level().to_string(), Style::default().fg(Color::Cyan)),
        Span::raw("    "),
        Span::styled(s.dir_label, Style::default().add_modifier(Modifier::DIM)),
        Span::raw(app.server_dir.display().to_string()),
    ]);
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(" mc-tui "));
    f.render_widget(p, area);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = TABS
        .iter()
        .enumerate()
        .map(|(i, (id, _en))| {
            Line::from(format!(" {} {} ", i + 1, tab_name(app.lang, *id)))
        })
        .collect();
    let selected = TABS.iter().position(|(t, _)| *t == app.tab).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL))
        .select(selected)
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_worlds(f: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .worlds
        .iter()
        .map(|w| {
            let mark = if w.is_current { "●" } else { " " };
            let when = w
                .last_modified
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {} ", mark), Style::default().fg(Color::Green)),
                Span::styled(format!("{:30}", w.name), Style::default().fg(Color::White)),
                Span::styled(
                    format!("{:>10}  ", fmt_bytes(w.size_bytes)),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(when, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(app.lang.s().title_worlds))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut app.worlds_state);
}

fn draw_whitelist(f: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .whitelist
        .iter()
        .map(|e| {
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {:20} ", e.name), Style::default().fg(Color::White)),
                Span::styled(&e.uuid, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(app.lang.s().title_whitelist))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut app.whitelist_state);
}

fn draw_ops(f: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .ops
        .iter()
        .map(|e| {
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {:20} ", e.name), Style::default().fg(Color::White)),
                Span::styled(format!("level {} ", e.level), Style::default().fg(Color::Yellow)),
                Span::styled(&e.uuid, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(app.lang.s().title_ops))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut app.ops_state);
}

fn draw_config(f: &mut Frame, area: Rect, app: &mut App) {
    let lang = app.lang;
    let items: Vec<ListItem> = app
        .properties
        .iter()
        .map(|(k, v)| {
            let value_color = match v.as_str() {
                "true" => Color::Green,
                "false" => Color::Red,
                _ => Color::Cyan,
            };
            let mut spans = vec![
                Span::styled(format!(" {:35}", k), Style::default().fg(Color::White)),
                Span::raw("= "),
                Span::styled(v.clone(), Style::default().fg(value_color)),
            ];
            // In zh mode, append a dim Chinese annotation if we know one for this key.
            if lang == Lang::Zh {
                if let Some(annot) = property_zh(k) {
                    spans.push(Span::raw("    "));
                    spans.push(Span::styled(
                        format!("// {}", annot),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(app.lang.s().title_config),
        )
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut app.config_state);
}

fn draw_logs(f: &mut Frame, area: Rect, app: &App) {
    let log_path = app.server_dir.join("logs/latest.log");
    let body = if log_path.exists() {
        match fs::read_to_string(&log_path) {
            Ok(s) => {
                let lines: Vec<&str> = s.lines().collect();
                let n = lines.len();
                let take = (area.height as usize).saturating_sub(2).max(1);
                let start = n.saturating_sub(take);
                lines[start..].join("\n")
            }
            Err(e) => fmt_log_read_error(app.lang, &e.to_string()),
        }
    } else {
        app.lang.s().no_logs_yet.to_string()
    };
    let title = format!("{}{} ", app.lang.s().title_logs_prefix, log_path.display());
    let p = Paragraph::new(body)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_hints(f: &mut Frame, area: Rect, app: &App) {
    let hint = hint_for(app.lang, app.tab);
    let line = Line::from(vec![
        Span::styled(format!(" {} ", hint), Style::default().fg(Color::DarkGray)),
        Span::raw("  │  "),
        Span::styled(&app.status, Style::default().fg(Color::Yellow)),
    ]);
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn draw_prompt(f: &mut Frame, prompt: &InputPrompt, lang: Lang) {
    let area = centered_rect(60, 5, f.area());
    f.render_widget(ratatui::widgets::Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", prompt.title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = vec![
        Line::from(vec![
            Span::styled(format!("{}: ", prompt.label), Style::default().fg(Color::White)),
            Span::styled(&prompt.buffer, Style::default().fg(Color::Yellow)),
            Span::styled(
                "█",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
            ),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            lang.s().prompt_confirm_cancel,
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn centered_rect(w_pct: u16, h_lines: u16, area: Rect) -> Rect {
    let w = area.width.saturating_mul(w_pct) / 100;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h_lines)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h_lines.min(area.height),
    }
}

// ---------- Main loop ----------

fn run<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if !event::poll(Duration::from_millis(500))? {
            app.pid = server_running_pid(&app.server_dir);
            continue;
        }

        let ev = event::read()?;
        let key = match ev {
            Event::Key(k) => k,
            Event::Mouse(me) => {
                handle_mouse(app, me);
                continue;
            }
            _ => continue,
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if let Some(mut prompt) = app.prompt.take() {
            match key.code {
                KeyCode::Esc => {
                    app.status = app.lang.s().cancelled.into();
                }
                KeyCode::Enter => {
                    let value = prompt.buffer.clone();
                    match prompt.action {
                        PromptAction::AddWhitelist => app.add_whitelist(&value)?,
                        PromptAction::AddOp => app.add_op(&value)?,
                        PromptAction::EditConfig(key) => app.save_config_value(&key, &value)?,
                        PromptAction::NewWorld => app.create_new_world(&value)?,
                        PromptAction::ChangeServerDir => app.change_server_dir(&value)?,
                    }
                }
                KeyCode::Backspace => {
                    prompt.buffer.pop();
                    app.prompt = Some(prompt);
                }
                KeyCode::Char(c) => {
                    prompt.buffer.push(c);
                    app.prompt = Some(prompt);
                }
                _ => {
                    app.prompt = Some(prompt);
                }
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('1') => app.switch_tab(TabId::Worlds),
            KeyCode::Char('2') => app.switch_tab(TabId::Whitelist),
            KeyCode::Char('3') => app.switch_tab(TabId::Ops),
            KeyCode::Char('4') => app.switch_tab(TabId::Config),
            KeyCode::Char('5') => app.switch_tab(TabId::Logs),
            KeyCode::Tab => app.cycle_tab(1),
            KeyCode::BackTab => app.cycle_tab(-1),
            KeyCode::Char('r') => {
                app.refresh_all();
                app.status = app.lang.s().refreshed.into();
            }
            KeyCode::Up => app.move_selection(-1),
            KeyCode::Down => app.move_selection(1),
            KeyCode::Enter => match app.tab {
                TabId::Worlds => app.switch_world()?,
                TabId::Config => {
                    if let Some(idx) = app.config_state.selected() {
                        if let Some((k, v)) = app.properties.get(idx).cloned() {
                            let title = match app.lang {
                                Lang::En => format!("Edit {}", k),
                                Lang::Zh => format!("编辑 {}", k),
                            };
                            app.prompt = Some(InputPrompt {
                                title,
                                label: app.lang.s().prompt_label_value.into(),
                                buffer: v,
                                action: PromptAction::EditConfig(k),
                            });
                        }
                    }
                }
                _ => {}
            },
            KeyCode::Char('a') => match app.tab {
                TabId::Whitelist => {
                    let s = app.lang.s();
                    app.prompt = Some(InputPrompt {
                        title: s.prompt_title_add_whitelist.into(),
                        label: s.prompt_label_player.into(),
                        buffer: String::new(),
                        action: PromptAction::AddWhitelist,
                    });
                }
                TabId::Ops => {
                    let s = app.lang.s();
                    app.prompt = Some(InputPrompt {
                        title: s.prompt_title_op_player.into(),
                        label: s.prompt_label_player.into(),
                        buffer: String::new(),
                        action: PromptAction::AddOp,
                    });
                }
                _ => {}
            },
            KeyCode::Char('d') => match app.tab {
                TabId::Whitelist => app.remove_whitelist()?,
                TabId::Ops => app.remove_op()?,
                _ => {}
            },
            KeyCode::Left => {
                if app.tab == TabId::Ops {
                    app.cycle_op_level(-1)?;
                }
            }
            KeyCode::Right => {
                if app.tab == TabId::Ops {
                    app.cycle_op_level(1)?;
                }
            }
            // v0.2 new keys
            KeyCode::Char('S') => app.start_server()?,
            KeyCode::Char('X') => app.stop_server()?,
            // v0.3 language toggle
            KeyCode::Char('L') => app.toggle_lang(),
            KeyCode::Char('N') => {
                if app.tab == TabId::Worlds {
                    let s = app.lang.s();
                    app.prompt = Some(InputPrompt {
                        title: s.prompt_title_new_world.into(),
                        label: s.prompt_label_world.into(),
                        buffer: String::new(),
                        action: PromptAction::NewWorld,
                    });
                }
            }
            KeyCode::Char('D') => {
                let s = app.lang.s();
                app.prompt = Some(InputPrompt {
                    title: s.prompt_title_change_dir.into(),
                    label: s.prompt_label_path.into(),
                    buffer: app.server_dir.display().to_string(),
                    action: PromptAction::ChangeServerDir,
                });
            }
            _ => {}
        }
    }
}

fn handle_mouse(app: &mut App, me: MouseEvent) {
    if !matches!(me.kind, MouseEventKind::Down(MouseButton::Left)) {
        return;
    }
    let (col, row) = (me.column, me.row);

    if rect_contains(app.tabs_rect, col, row) {
        // ratatui Tabs widget renders titles as " 1 Worlds " " │ " " 2 Whitelist " ...
        // Compute cumulative widths to find which tab was clicked.
        let inner_x = app.tabs_rect.x.saturating_add(1);
        if col < inner_x {
            return;
        }
        let dx = col - inner_x;
        let mut x: u16 = 0;
        for (i, (id, name)) in TABS.iter().enumerate() {
            // Title format: " {idx} {name} " (matches draw_tabs).
            let title_len = format!(" {} {} ", i + 1, name).chars().count() as u16;
            let divider_len: u16 = if i + 1 < TABS.len() { 3 } else { 0 }; // ratatui Tabs default divider " │ "
            if dx >= x && dx < x + title_len {
                app.tab = *id;
                return;
            }
            x = x + title_len + divider_len;
        }
        return;
    }

    if rect_contains(app.list_rect, col, row) {
        // Block has 1-cell border; rows render at y+1..y+height-1.
        let inner_y = app.list_rect.y.saturating_add(1);
        let inner_h = app.list_rect.height.saturating_sub(2);
        if row < inner_y {
            return;
        }
        let row_in_list = (row - inner_y) as usize;
        if row_in_list >= inner_h as usize {
            return;
        }
        let tab = app.tab;
        let len = app.list_len_for(tab);
        if len == 0 {
            return;
        }
        let state = app.list_state_for(tab);
        let target = state.offset() + row_in_list;
        if target < len {
            state.select(Some(target));
        }
    }
}

fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x.saturating_add(r.width) && y >= r.y && y < r.y.saturating_add(r.height)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Sub-commands dispatch first.
    match cli.command {
        Some(Cmd::New {
            dir,
            force,
            mc_version,
            server_type,
            first_boot,
        }) => {
            return scaffold_new(&dir, force, mc_version.as_deref(), server_type, first_boot);
        }
        Some(Cmd::Screenshot {
            tab,
            width,
            height,
            lang,
        }) => {
            let server_dir = resolve_server_dir(cli.server_dir.clone())?;
            return render_screenshot(&server_dir, &tab, width, height, &lang);
        }
        Some(Cmd::Run) | None => {}
    }

    let server_dir = resolve_server_dir(cli.server_dir)?;
    let mut state = read_persisted_state();
    let lang = state.lang.as_deref().map(Lang::from_code).unwrap_or_default();
    let mut app = App::new_with_lang(server_dir.clone(), lang)?;

    // Persist this dir as last-good.
    state.server_dir = Some(server_dir);
    if state.lang.is_none() {
        state.lang = Some(lang.code().to_string());
    }
    let _ = write_persisted_state(&state);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    res
}

fn resolve_server_dir(cli_arg: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = cli_arg {
        return Ok(p);
    }
    let state = read_persisted_state();
    if let Some(p) = state.server_dir {
        if p.join("server.properties").exists() {
            eprintln!("(using remembered server-dir: {})", p.display());
            return Ok(p);
        }
        anyhow::bail!(
            "remembered server-dir {} no longer has server.properties — pass --server-dir",
            p.display()
        );
    }
    anyhow::bail!(
        "no --server-dir given and no remembered dir at {} — pass --server-dir or MC_SERVER_DIR",
        state_path().display()
    );
}

// ---------- v0.4: server scaffolder (`mc-tui new <dir>`) ----------
//
// Bootstraps a fresh server directory:
//   - check Java >= 25
//   - resolve latest MC version (or use --mc-version)
//   - download Paper or Purpur jar via curl
//   - write eula.txt, start.sh (Aikar's flags + heap from detected RAM)
//   - optionally first-boot to generate server.properties, then SIGTERM
//
// CLI tool (not TUI) — emits progress to stderr.

fn scaffold_new(
    dir: &Path,
    force: bool,
    mc_version: Option<&str>,
    server_type: ServerType,
    first_boot: bool,
) -> Result<()> {
    // 1. validate target directory
    if dir.exists() {
        if !dir.is_dir() {
            anyhow::bail!("{} exists and is not a directory", dir.display());
        }
        let non_empty = fs::read_dir(dir)
            .with_context(|| format!("read {}", dir.display()))?
            .next()
            .is_some();
        if non_empty && !force {
            anyhow::bail!(
                "{} is not empty — pass --force to scaffold anyway",
                dir.display()
            );
        }
    } else {
        fs::create_dir_all(dir)
            .with_context(|| format!("create {}", dir.display()))?;
    }

    // 2. java check (warn-only — user may want to scaffold first, install Java later)
    match detect_java_major_version() {
        Ok(Some(v)) => {
            eprintln!("→ detected Java {}", v);
            if v < 25 {
                eprintln!(
                    "⚠ Paper/Purpur for MC 1.21.4+ require Java 21+, latest builds prefer Java 25. \
                     Your Java {} may be too old.",
                    v
                );
            }
        }
        Ok(None) => eprintln!("⚠ could not parse `java -version` output. Continuing anyway."),
        Err(e) => eprintln!("⚠ no `java` on PATH ({}). Install Java before starting the server.", e),
    }

    // 3. resolve version
    let version = match mc_version {
        Some(v) => v.to_string(),
        None => {
            eprintln!("→ resolving latest MC version for {}…", server_type.name());
            resolve_latest_version(server_type)?
        }
    };
    eprintln!("→ MC version: {}", version);

    // 4. download jar
    let jar_name = format!("{}.jar", server_type.name());
    let jar_path = dir.join(&jar_name);
    let url = build_download_url(server_type, &version)?;
    eprintln!("→ downloading {} → {}", url, jar_path.display());
    download_url(&url, &jar_path)?;
    let jar_size = fs::metadata(&jar_path)
        .map(|m| m.len())
        .unwrap_or(0);
    eprintln!("✓ downloaded {} ({})", jar_name, fmt_bytes(jar_size));

    // 5. eula.txt — only the user can ethically agree, but `mc-tui new` is interactive intent.
    //    We write `eula=true` since the user invoked us with explicit intent to run a server.
    fs::write(dir.join("eula.txt"), "# generated by mc-tui new\neula=true\n")
        .context("write eula.txt")?;
    eprintln!("✓ wrote eula.txt");

    // 6. start.sh with Aikar's flags + heap based on RAM
    let heap_mb = recommended_heap_mb();
    let start_sh = aikar_start_script(&jar_name, heap_mb);
    let start_path = dir.join("start.sh");
    fs::write(&start_path, start_sh).context("write start.sh")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&start_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&start_path, perms)?;
    }
    eprintln!("✓ wrote start.sh (heap: {}M)", heap_mb);

    // 7. optional first-boot to generate server.properties
    if first_boot {
        eprintln!("→ first-boot: starting server briefly to generate config files…");
        first_boot_run(dir, &jar_name, heap_mb)?;
        eprintln!("✓ first-boot complete");
    } else {
        eprintln!(
            "ℹ skip first-boot (pass --first-boot to auto-generate server.properties on this run)"
        );
    }

    // 8. summary
    eprintln!();
    eprintln!("✓ scaffolded {}", dir.display());
    eprintln!("Next:");
    eprintln!("  cd {} && ./start.sh", dir.display());
    eprintln!("  mc-tui --server-dir {}", dir.display());
    Ok(())
}

fn detect_java_major_version() -> Result<Option<u32>> {
    let out = std::process::Command::new("java")
        .arg("-version")
        .output()
        .context("spawn `java -version`")?;
    // `java -version` writes to stderr, e.g. `openjdk version "21.0.4" 2024-07-16` or `... "1.8.0_392"`.
    let text = String::from_utf8_lossy(&out.stderr);
    Ok(parse_java_major(&text))
}

fn parse_java_major(text: &str) -> Option<u32> {
    // Look for the first quoted version string and extract the major.
    let start = text.find('"')?;
    let after = &text[start + 1..];
    let end = after.find('"')?;
    let ver = &after[..end];
    // "1.8.0_392" → major 8 ; "21.0.4" → 21 ; "25" → 25.
    let mut parts = ver.split('.').filter(|s| !s.is_empty());
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        // legacy form 1.x.y → major is x
        let second: u32 = parts.next()?.parse().ok()?;
        Some(second)
    } else {
        Some(first)
    }
}

fn resolve_latest_version(st: ServerType) -> Result<String> {
    let url = match st {
        ServerType::Paper => "https://api.papermc.io/v2/projects/paper",
        ServerType::Purpur => "https://api.purpurmc.org/v2/purpur",
    };
    let body = curl_text(url)?;
    let v: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("parse JSON from {}", url))?;
    let arr = v
        .get("versions")
        .and_then(|x| x.as_array())
        .with_context(|| format!("no `versions` array in {}", url))?;
    let last = arr
        .last()
        .and_then(|x| x.as_str())
        .with_context(|| "empty versions array")?;
    Ok(last.to_string())
}

fn build_download_url(st: ServerType, version: &str) -> Result<String> {
    match st {
        ServerType::Paper => {
            // 1. list builds
            let builds_url = format!(
                "https://api.papermc.io/v2/projects/paper/versions/{}/builds",
                version
            );
            let body = curl_text(&builds_url)?;
            let v: serde_json::Value = serde_json::from_str(&body)
                .with_context(|| format!("parse builds JSON from {}", builds_url))?;
            let builds = v
                .get("builds")
                .and_then(|b| b.as_array())
                .context("no builds array")?;
            // pick the highest-numbered build that is `default` channel (or last as fallback)
            let chosen = builds
                .iter()
                .rev()
                .find(|b| b.get("channel").and_then(|c| c.as_str()) == Some("default"))
                .or_else(|| builds.last())
                .context("no builds available")?;
            let build_num = chosen
                .get("build")
                .and_then(|x| x.as_u64())
                .context("missing build number")?;
            let filename = chosen
                .get("downloads")
                .and_then(|d| d.get("application"))
                .and_then(|a| a.get("name"))
                .and_then(|n| n.as_str())
                .context("missing download filename")?;
            Ok(format!(
                "https://api.papermc.io/v2/projects/paper/versions/{}/builds/{}/downloads/{}",
                version, build_num, filename
            ))
        }
        ServerType::Purpur => Ok(format!(
            "https://api.purpurmc.org/v2/purpur/{}/latest/download",
            version
        )),
    }
}

fn curl_text(url: &str) -> Result<String> {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "-A",
            "mc-tui/0.1 (https://github.com/)",
            url,
        ])
        .output()
        .context("spawn curl")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("curl GET {} failed: {}", url, stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn download_url(url: &str, target: &Path) -> Result<()> {
    let status = std::process::Command::new("curl")
        .args([
            "-fSL",
            "-A",
            "mc-tui/0.1",
            "--progress-bar",
            "-o",
        ])
        .arg(target)
        .arg(url)
        .status()
        .context("spawn curl")?;
    if !status.success() {
        anyhow::bail!("curl download failed for {}", url);
    }
    Ok(())
}

fn recommended_heap_mb() -> u32 {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let sys = System::new_with_specifics(
        RefreshKind::new().with_memory(MemoryRefreshKind::new().with_ram()),
    );
    // sysinfo 0.32: total_memory() returns bytes.
    let total_bytes = sys.total_memory();
    let total_mb = (total_bytes / 1024 / 1024) as u32;
    // Heuristic: leave ~2 GB for OS, give half of remaining RAM to the JVM, capped at 8 GB.
    // Friend-group servers rarely benefit from > 8 GB heap.
    let usable = total_mb.saturating_sub(2048);
    let heap = (usable / 2).clamp(2 * 1024, 8 * 1024);
    heap
}

fn aikar_start_script(jar: &str, heap_mb: u32) -> String {
    // Aikar's flags: https://docs.papermc.io/paper/aikars-flags
    // (Tuned for low GC pauses on a friend-group server.)
    format!(
        r#"#!/usr/bin/env bash
# Generated by mc-tui new. Edit freely.
set -e
cd "$(dirname "$0")"

JAR="{jar}"
HEAP={heap_mb}M

exec java \
    -Xms$HEAP -Xmx$HEAP \
    -XX:+UseG1GC \
    -XX:+ParallelRefProcEnabled \
    -XX:MaxGCPauseMillis=200 \
    -XX:+UnlockExperimentalVMOptions \
    -XX:+DisableExplicitGC \
    -XX:+AlwaysPreTouch \
    -XX:G1NewSizePercent=30 \
    -XX:G1MaxNewSizePercent=40 \
    -XX:G1HeapRegionSize=8M \
    -XX:G1ReservePercent=20 \
    -XX:G1HeapWastePercent=5 \
    -XX:G1MixedGCCountTarget=4 \
    -XX:InitiatingHeapOccupancyPercent=15 \
    -XX:G1MixedGCLiveThresholdPercent=90 \
    -XX:G1RSetUpdatingPauseTimePercent=5 \
    -XX:SurvivorRatio=32 \
    -XX:+PerfDisableSharedMem \
    -XX:MaxTenuringThreshold=1 \
    -Dusing.aikars.flags=https://mcflags.emc.gs \
    -Daikars.new.flags=true \
    -jar "$JAR" --nogui
"#,
        jar = jar,
        heap_mb = heap_mb
    )
}

fn first_boot_run(dir: &Path, jar: &str, heap_mb: u32) -> Result<()> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new("java")
        .arg(format!("-Xms{}M", heap_mb))
        .arg(format!("-Xmx{}M", heap_mb))
        .arg("-jar")
        .arg(jar)
        .arg("--nogui")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn java for first-boot")?;

    let server_props = dir.join("server.properties");
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if server_props.exists() {
            // Server has written initial config. Give it a few more seconds, then stop.
            std::thread::sleep(Duration::from_secs(4));
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    // SIGTERM the child.
    let pid = child.id();
    #[cfg(unix)]
    {
        let _ = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill")
            .arg("/PID")
            .arg(pid.to_string())
            .status();
    }
    // Wait for graceful exit (best effort).
    let wait_deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < wait_deadline {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(500)),
            Err(_) => break,
        }
    }
    // If still alive, force kill.
    let _ = child.kill();

    if !server_props.exists() {
        anyhow::bail!(
            "first-boot timed out: no {} after 120s",
            server_props.display()
        );
    }
    Ok(())
}

// ---------- v0.5: backup scanner ----------
//
// Look for archive files in standard backup locations and present them as a
// time-sorted list. Backup *creation* belongs to v0.6 (scheduled/ad-hoc); this
// is just the read side: discover, sort, present.

#[derive(Debug, Clone)]
struct BackupEntry {
    name: String,
    path: PathBuf,
    size_bytes: u64,
    modified: Option<chrono::DateTime<chrono::Local>>,
}

fn is_backup_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.zst")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".zip")
        || lower.ends_with(".7z")
}

fn backup_dir_candidates(server_dir: &Path) -> Vec<PathBuf> {
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

fn scan_backups(server_dir: &Path) -> Vec<BackupEntry> {
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
// prompt → `parse_yaml_scalar` → `yaml_set(..., path, new_value)` → write.

#[derive(Debug, Clone)]
enum YamlSeg {
    Key(String),
    Index(usize),
}

#[derive(Debug, Clone)]
enum YamlDisplay {
    Branch, // mapping or sequence
    Scalar(String),
}

#[derive(Debug, Clone)]
struct YamlRow {
    indent: u8,
    path: Vec<YamlSeg>,
    label: String,
    value: YamlDisplay,
}

fn flatten_yaml(v: &serde_yaml::Value) -> Vec<YamlRow> {
    let mut out = Vec::new();
    let mut path = Vec::new();
    walk_yaml(v, 0, &mut path, &mut out);
    out
}

fn walk_yaml(
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

fn yaml_scalar_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => "null".to_string(),
        _ => serde_yaml::to_string(v).unwrap_or_default().trim().to_string(),
    }
}

fn yaml_set(
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

fn parse_yaml_scalar(input: &str) -> serde_yaml::Value {
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
fn list_yaml_files(server_dir: &Path) -> Vec<PathBuf> {
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

// ---------- v0.5: RCON client ----------
//
// Source-of-truth: https://wiki.vg/RCON
// Packet: i32_le length | i32_le request_id | i32_le type | body (utf8) | 0x00 | 0x00
// Types we use: 3 = LOGIN, 2 = COMMAND / AUTH_RESPONSE, 0 = COMMAND_RESPONSE.
// Auth failure echoes request_id = -1.

const RCON_TYPE_LOGIN: i32 = 3;
const RCON_TYPE_COMMAND: i32 = 2;
#[allow(dead_code)]
const RCON_TYPE_RESPONSE: i32 = 0;

struct RconClient {
    stream: std::net::TcpStream,
    next_id: i32,
}

impl RconClient {
    fn connect(host: &str, port: u16, password: &str) -> Result<Self> {
        use std::net::ToSocketAddrs;
        use std::time::Duration;
        let addr = (host, port)
            .to_socket_addrs()
            .with_context(|| format!("resolve {}:{}", host, port))?
            .next()
            .with_context(|| format!("no addrs for {}:{}", host, port))?;
        let stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5))
            .with_context(|| format!("connect {}:{}", host, port))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut c = RconClient { stream, next_id: 1 };
        c.auth(password)?;
        Ok(c)
    }

    fn auth(&mut self, password: &str) -> Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        self.send_packet(id, RCON_TYPE_LOGIN, password.as_bytes())?;
        // Some servers send an empty COMMAND_RESPONSE first; loop until we see AUTH_RESPONSE.
        for _ in 0..3 {
            let (rid, ty, _body) = self.recv_packet()?;
            if ty == RCON_TYPE_COMMAND {
                if rid == -1 {
                    anyhow::bail!("RCON auth failed (wrong password?)");
                }
                if rid == id {
                    return Ok(());
                }
            }
            // ignore stray RESPONSE_VALUE packets
        }
        anyhow::bail!("RCON: never got auth response");
    }

    fn exec(&mut self, cmd: &str) -> Result<String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send_packet(id, RCON_TYPE_COMMAND, cmd.as_bytes())?;
        let (_rid, _ty, body) = self.recv_packet()?;
        Ok(body)
    }

    fn send_packet(&mut self, id: i32, ty: i32, body: &[u8]) -> Result<()> {
        use std::io::Write;
        let len: i32 = (10 + body.len()) as i32; // id(4) + ty(4) + body + 0x00 + 0x00
        let mut packet = Vec::with_capacity(4 + len as usize);
        packet.extend_from_slice(&len.to_le_bytes());
        packet.extend_from_slice(&id.to_le_bytes());
        packet.extend_from_slice(&ty.to_le_bytes());
        packet.extend_from_slice(body);
        packet.push(0);
        packet.push(0);
        self.stream.write_all(&packet)?;
        self.stream.flush()?;
        Ok(())
    }

    fn recv_packet(&mut self) -> Result<(i32, i32, String)> {
        use std::io::Read;
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf)?;
        let len = i32::from_le_bytes(len_buf);
        if !(10..=4096).contains(&len) {
            anyhow::bail!("invalid rcon packet length: {}", len);
        }
        let mut payload = vec![0u8; len as usize];
        self.stream.read_exact(&mut payload)?;
        let id = i32::from_le_bytes(payload[0..4].try_into().unwrap());
        let ty = i32::from_le_bytes(payload[4..8].try_into().unwrap());
        // body ends at the first NUL after offset 8.
        let body_bytes = &payload[8..];
        let body_end = body_bytes.iter().position(|b| *b == 0).unwrap_or(body_bytes.len());
        let body = String::from_utf8_lossy(&body_bytes[..body_end]).to_string();
        Ok((id, ty, body))
    }
}

/// Read RCON connect info from `server.properties`. Returns (host, port, password).
/// Host falls back to `127.0.0.1` if `server-ip` is empty (Paper default).
fn rcon_settings(props: &[(String, String)]) -> Option<(String, u16, String)> {
    let enabled = get_property(props, "enable-rcon").map(|v| v == "true").unwrap_or(false);
    if !enabled {
        return None;
    }
    let port: u16 = get_property(props, "rcon.port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(25575);
    let password = get_property(props, "rcon.password")
        .map(|s| s.to_string())
        .unwrap_or_default();
    let host_raw = get_property(props, "server-ip").unwrap_or("");
    let host = if host_raw.is_empty() || host_raw == "0.0.0.0" {
        "127.0.0.1".to_string()
    } else {
        host_raw.to_string()
    };
    Some((host, port, password))
}

fn render_screenshot(
    server_dir: &Path,
    tab: &str,
    width: u16,
    height: u16,
    lang: &str,
) -> Result<()> {
    use ratatui::backend::TestBackend;
    let lang = Lang::from_code(lang);
    let mut app = App::new_with_lang(server_dir.to_path_buf(), lang)?;
    app.tab = match tab.to_ascii_lowercase().as_str() {
        "worlds" => TabId::Worlds,
        "whitelist" => TabId::Whitelist,
        "ops" => TabId::Ops,
        "config" => TabId::Config,
        "logs" => TabId::Logs,
        other => anyhow::bail!("unknown tab: {}", other),
    };
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|f| ui(f, &mut app))?;
    let buffer = terminal.backend().buffer().clone();
    // Render buffer cells to plain text (one line per row).
    // ratatui stores a wide char (CJK, fullwidth) in one cell and pads the next cell
    // with an empty/space symbol; advance by the rendered width so we don't double up.
    use unicode_width::UnicodeWidthStr;
    for y in 0..buffer.area.height {
        let mut line = String::new();
        let mut x = 0;
        while x < buffer.area.width {
            let cell = &buffer[(x, y)];
            let sym = cell.symbol();
            line.push_str(sym);
            let w = UnicodeWidthStr::width(sym).max(1) as u16;
            x = x.saturating_add(w);
        }
        let trimmed = line.trim_end();
        println!("{}", trimmed);
    }
    Ok(())
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_uuid_format_and_version_bits() {
        // Algorithm: md5("OfflinePlayer:" + name), then set version (3) and variant bits.
        // Format must be 8-4-4-4-12 hex digits. Char 14 must be '3' (version 3).
        // Char 19 must be 8/9/a/b (RFC 4122 variant).
        for name in ["Alice", "Bob", "Steve_42", "测试用户"] {
            let u = offline_uuid(name);
            assert_eq!(u.len(), 36, "uuid length for {name}");
            assert_eq!(&u[8..9], "-");
            assert_eq!(&u[13..14], "-");
            assert_eq!(&u[14..15], "3", "version-3 bit for {name}");
            assert_eq!(&u[18..19], "-");
            let variant = u.chars().nth(19).unwrap();
            assert!("89ab".contains(variant), "variant bit for {name}: got {variant}");
            assert_eq!(&u[23..24], "-");
        }
    }

    #[test]
    fn offline_uuid_is_deterministic() {
        // Same input -> same output across calls.
        assert_eq!(offline_uuid("Spencer"), offline_uuid("Spencer"));
        assert_ne!(offline_uuid("Spencer"), offline_uuid("spencer"));
    }

    #[test]
    fn properties_roundtrip_preserves_kv_order() {
        let dir = tempdir();
        let p = dir.join("server.properties");
        fs::write(
            &p,
            "# comment\nfoo=bar\nbaz=qux\n# another\nempty=\n",
        )
        .unwrap();
        let mut props = read_properties(&p).unwrap();
        assert_eq!(props.len(), 3);
        assert_eq!(props[0], ("foo".to_string(), "bar".to_string()));
        assert_eq!(props[1], ("baz".to_string(), "qux".to_string()));
        assert_eq!(props[2], ("empty".to_string(), "".to_string()));
        set_property(&mut props, "foo", "42");
        set_property(&mut props, "newkey", "hello");
        write_properties(&p, &props).unwrap();
        let reread = read_properties(&p).unwrap();
        assert_eq!(reread[0], ("foo".to_string(), "42".to_string()));
        assert_eq!(reread.last().unwrap(), &("newkey".to_string(), "hello".to_string()));
    }

    #[test]
    fn whitelist_roundtrip() {
        let dir = tempdir();
        let entries = vec![WhitelistEntry {
            uuid: offline_uuid("Alice"),
            name: "Alice".to_string(),
        }];
        write_whitelist(&dir, &entries).unwrap();
        let read = read_whitelist(&dir).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "Alice");
    }

    #[test]
    fn ops_roundtrip() {
        let dir = tempdir();
        let entries = vec![OpEntry {
            uuid: offline_uuid("Bob"),
            name: "Bob".to_string(),
            level: 4,
            bypasses_player_limit: false,
        }];
        write_ops(&dir, &entries).unwrap();
        let read = read_ops(&dir).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "Bob");
        assert_eq!(read[0].level, 4);
    }

    #[test]
    fn fmt_bytes_examples() {
        assert_eq!(fmt_bytes(0), "0.0 B");
        assert_eq!(fmt_bytes(1023), "1023.0 B");
        assert_eq!(fmt_bytes(1024), "1.0 KB");
        assert_eq!(fmt_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(fmt_bytes(1024_u64.pow(3)), "1.0 GB");
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "mc-tui-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn expand_tilde_replaces_home() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let p = expand_tilde("~/foo/bar");
        assert!(p.starts_with(&home), "expected {} to start with {}", p.display(), home);
        let p = expand_tilde("/abs/path");
        assert_eq!(p, PathBuf::from("/abs/path"));
        let p = expand_tilde("~");
        assert_eq!(p, PathBuf::from(&home));
    }

    #[test]
    fn persisted_state_roundtrip() {
        let dir = tempdir();
        let state_file = dir.join("state.toml");
        // Write directly so we exercise the parser.
        fs::write(
            &state_file,
            "# header\nserver_dir = \"/srv/mc\"\nlang = \"zh\"\n",
        )
        .unwrap();
        // Reuse parser via a tiny shim that mimics read_persisted_state but takes a path.
        let raw = fs::read_to_string(&state_file).unwrap();
        let mut server_dir: Option<PathBuf> = None;
        let mut lang: Option<String> = None;
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(eq) = line.find('=') {
                let k = line[..eq].trim();
                let v = line[eq + 1..].trim().trim_matches('"').to_string();
                match k {
                    "server_dir" => server_dir = Some(PathBuf::from(v)),
                    "lang" => lang = Some(v),
                    _ => {}
                }
            }
        }
        assert_eq!(server_dir, Some(PathBuf::from("/srv/mc")));
        assert_eq!(lang.as_deref(), Some("zh"));
    }

    #[test]
    fn lang_codes_roundtrip() {
        assert_eq!(Lang::from_code("zh"), Lang::Zh);
        assert_eq!(Lang::from_code("en"), Lang::En);
        assert_eq!(Lang::from_code(""), Lang::En);
        assert_eq!(Lang::from_code("zh-CN"), Lang::Zh);
        assert_eq!(Lang::Zh.code(), "zh");
        assert_eq!(Lang::En.code(), "en");
        assert_eq!(Lang::En.toggle(), Lang::Zh);
        assert_eq!(Lang::Zh.toggle(), Lang::En);
    }

    #[test]
    fn property_zh_covers_common_keys() {
        for key in [
            "max-players",
            "view-distance",
            "difficulty",
            "gamemode",
            "pvp",
            "online-mode",
            "white-list",
            "motd",
            "level-name",
            "server-port",
        ] {
            assert!(property_zh(key).is_some(), "missing zh annotation for {key}");
        }
        // Unknown keys should return None, not a fallback string.
        assert!(property_zh("not-a-real-key").is_none());
    }

    #[test]
    fn strings_struct_fields_nonempty_in_both_langs() {
        // Spot-check a few fields — important ones we know we render.
        for s in [&EN, &ZH] {
            assert!(!s.ready.is_empty());
            assert!(!s.refreshed.is_empty());
            assert!(!s.tab_worlds.is_empty());
            assert!(!s.hint_worlds.is_empty());
            assert!(!s.title_logs_prefix.is_empty());
            assert!(!s.prompt_confirm_cancel.is_empty());
        }
    }

    #[test]
    fn fmt_helpers_return_lang_appropriate_strings() {
        let en = fmt_world_switched(Lang::En, "test");
        let zh = fmt_world_switched(Lang::Zh, "test");
        assert!(en.contains("Switched"));
        assert!(zh.contains("已切换"));
        assert!(en != zh);

        let en = fmt_status_running(Lang::En, 42);
        let zh = fmt_status_running(Lang::Zh, 42);
        assert!(en.contains("running"));
        assert!(zh.contains("运行中"));
        assert!(en.contains("42") && zh.contains("42"));
    }

    #[test]
    fn parse_java_major_handles_modern_and_legacy() {
        let text = r#"openjdk version "21.0.4" 2024-07-16
OpenJDK Runtime Environment ..."#;
        assert_eq!(parse_java_major(text), Some(21));

        let text = r#"java version "1.8.0_392"
Java(TM) SE Runtime Environment ..."#;
        assert_eq!(parse_java_major(text), Some(8));

        let text = r#"openjdk version "25" 2025-09-15"#;
        assert_eq!(parse_java_major(text), Some(25));

        assert_eq!(parse_java_major("nope"), None);
    }

    #[test]
    fn aikar_script_contains_essentials() {
        let s = aikar_start_script("paper.jar", 4096);
        assert!(s.contains("paper.jar"));
        assert!(s.contains("4096M"));
        assert!(s.contains("UseG1GC"));
        assert!(s.contains("--nogui"));
        assert!(s.contains("aikars.new.flags"));
    }

    #[test]
    fn server_type_names() {
        assert_eq!(ServerType::Paper.name(), "paper");
        assert_eq!(ServerType::Purpur.name(), "purpur");
    }

    #[test]
    fn recommended_heap_in_sane_range() {
        let h = recommended_heap_mb();
        assert!(h >= 2048, "heap should be at least 2GB, got {}M", h);
        assert!(h <= 8192, "heap should be capped at 8GB, got {}M", h);
    }

    #[test]
    fn is_backup_file_recognises_common_archives() {
        for n in [
            "world-2024-01-01.tar.gz",
            "snap.tgz",
            "snap.tar.zst",
            "world.zip",
            "snap.tar.xz",
            "snap.7z",
            "snap.TAR.GZ", // case-insensitive
        ] {
            assert!(is_backup_file(n), "expected {} to be recognised", n);
        }
        for n in ["world.dat", "log.txt", "snap.tar"] {
            assert!(!is_backup_file(n), "expected {} NOT recognised", n);
        }
    }

    #[test]
    fn rcon_settings_disabled_returns_none() {
        let props = vec![
            ("enable-rcon".into(), "false".into()),
            ("rcon.port".into(), "25575".into()),
            ("rcon.password".into(), "secret".into()),
        ];
        assert!(rcon_settings(&props).is_none());
    }

    #[test]
    fn rcon_settings_enabled_returns_defaults() {
        let props = vec![
            ("enable-rcon".into(), "true".into()),
            ("rcon.port".into(), "12345".into()),
            ("rcon.password".into(), "hunter2".into()),
            ("server-ip".into(), "".into()),
        ];
        let (host, port, pw) = rcon_settings(&props).unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 12345);
        assert_eq!(pw, "hunter2");
    }

    #[test]
    fn rcon_packet_roundtrip_in_memory() {
        // Verify our packet framing: build a packet, re-parse the header fields.
        let body = b"list";
        let id: i32 = 7;
        let ty: i32 = RCON_TYPE_COMMAND;
        let len: i32 = (10 + body.len()) as i32;
        let mut packet = Vec::new();
        packet.extend_from_slice(&len.to_le_bytes());
        packet.extend_from_slice(&id.to_le_bytes());
        packet.extend_from_slice(&ty.to_le_bytes());
        packet.extend_from_slice(body);
        packet.push(0);
        packet.push(0);
        assert_eq!(packet.len(), 4 + len as usize);
        let parsed_len = i32::from_le_bytes(packet[0..4].try_into().unwrap());
        let parsed_id = i32::from_le_bytes(packet[4..8].try_into().unwrap());
        let parsed_ty = i32::from_le_bytes(packet[8..12].try_into().unwrap());
        assert_eq!(parsed_len, len);
        assert_eq!(parsed_id, id);
        assert_eq!(parsed_ty, ty);
        // Body terminator
        assert_eq!(packet[packet.len() - 1], 0);
        assert_eq!(packet[packet.len() - 2], 0);
    }

    #[test]
    fn scan_backups_finds_archives_in_local_dir() {
        let dir = tempdir();
        let backups = dir.join("backups");
        fs::create_dir_all(&backups).unwrap();
        fs::write(backups.join("snap-1.tar.gz"), b"x").unwrap();
        fs::write(backups.join("snap-2.zip"), b"y").unwrap();
        fs::write(backups.join("not-a-backup.txt"), b"z").unwrap();
        // Need a `server.properties` so that the dir looks like a real server-dir
        fs::write(dir.join("server.properties"), b"").unwrap();
        let out = scan_backups(&dir);
        let names: Vec<_> = out.iter().map(|b| b.name.clone()).collect();
        assert!(names.contains(&"snap-1.tar.gz".to_string()));
        assert!(names.contains(&"snap-2.zip".to_string()));
        assert!(!names.contains(&"not-a-backup.txt".to_string()));
    }

    #[test]
    fn yaml_flatten_walks_nested_mapping() {
        let yaml = r#"
chunks:
  view-distance: 10
  simulation-distance: 8
players:
  - name: Alice
    level: 1
"#;
        let v: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        let rows = flatten_yaml(&v);
        // Expect: chunks (branch), chunks.view-distance (10), chunks.sim-distance (8),
        // players (branch), players[0] (branch), players[0].name (Alice), players[0].level (1)
        assert!(rows.len() >= 6, "got {} rows", rows.len());
        let labels: Vec<_> = rows.iter().map(|r| r.label.clone()).collect();
        assert!(labels.contains(&"chunks".to_string()));
        assert!(labels.contains(&"view-distance".to_string()));
        assert!(labels.contains(&"name".to_string()));
        assert!(labels.iter().any(|l| l == "[0]"));
    }

    #[test]
    fn yaml_set_modifies_leaf() {
        let yaml = "view-distance: 10\n";
        let mut v: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        yaml_set(
            &mut v,
            &[YamlSeg::Key("view-distance".into())],
            parse_yaml_scalar("32"),
        )
        .unwrap();
        let dumped = serde_yaml::to_string(&v).unwrap();
        assert!(dumped.contains("view-distance: 32"), "got: {}", dumped);
    }

    #[test]
    fn parse_yaml_scalar_typing() {
        assert!(matches!(parse_yaml_scalar("true"), serde_yaml::Value::Bool(true)));
        assert!(matches!(parse_yaml_scalar("False"), serde_yaml::Value::Bool(false)));
        assert!(matches!(parse_yaml_scalar("null"), serde_yaml::Value::Null));
        assert!(matches!(parse_yaml_scalar("~"), serde_yaml::Value::Null));
        match parse_yaml_scalar("42") {
            serde_yaml::Value::Number(n) => assert_eq!(n.as_i64(), Some(42)),
            _ => panic!("expected number"),
        }
        match parse_yaml_scalar("hello world") {
            serde_yaml::Value::String(s) => assert_eq!(s, "hello world"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn build_download_url_for_purpur_is_simple() {
        let url = build_download_url(ServerType::Purpur, "1.21.4").unwrap();
        assert_eq!(url, "https://api.purpurmc.org/v2/purpur/1.21.4/latest/download");
    }

    #[test]
    fn rect_contains_basic() {
        let r = Rect { x: 10, y: 20, width: 30, height: 5 };
        assert!(rect_contains(r, 10, 20));
        assert!(rect_contains(r, 39, 24));
        assert!(!rect_contains(r, 9, 20));
        assert!(!rect_contains(r, 40, 20));
        assert!(!rect_contains(r, 10, 25));
    }
}
