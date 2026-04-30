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
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
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
    #[arg(short = 'd', long, env = "MC_SERVER_DIR")]
    server_dir: PathBuf,
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
}

impl App {
    fn new(server_dir: PathBuf) -> Result<Self> {
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
            status: String::from("Ready."),
            prompt: None,
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
            self.status = "✗ Stop the server first (it's running).".into();
            return Ok(());
        }
        let Some(idx) = self.worlds_state.selected() else { return Ok(()) };
        let Some(entry) = self.worlds.get(idx) else { return Ok(()) };
        if entry.is_current {
            self.status = "→ Already current world.".into();
            return Ok(());
        }
        let new_name = entry.name.clone();
        set_property(&mut self.properties, "level-name", &new_name);
        write_properties(&self.server_dir.join("server.properties"), &self.properties)?;
        self.status = format!("✓ Switched to '{}'. Restart the server to load it.", new_name);
        self.refresh_all();
        Ok(())
    }

    fn add_whitelist(&mut self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        if self.whitelist.iter().any(|e| e.name == name) {
            self.status = format!("→ '{}' already whitelisted.", name);
            return Ok(());
        }
        self.whitelist.push(WhitelistEntry {
            uuid: offline_uuid(name),
            name: name.to_string(),
        });
        write_whitelist(&self.server_dir, &self.whitelist)?;
        self.status = format!("✓ Whitelisted {}.", name);
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
        self.status = format!("✓ Removed {} from whitelist.", removed.name);
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
            self.status = format!("→ '{}' already op.", name);
            return Ok(());
        }
        self.ops.push(OpEntry {
            uuid: offline_uuid(name),
            name: name.to_string(),
            level: 4,
            bypasses_player_limit: false,
        });
        write_ops(&self.server_dir, &self.ops)?;
        self.status = format!("✓ Op'd {} (level 4).", name);
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
        self.status = format!("✓ De-op'd {}.", removed.name);
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
        self.status = format!("✓ {} → level {}.", self.ops[idx].name, new);
        Ok(())
    }

    fn save_config_value(&mut self, key: &str, value: &str) -> Result<()> {
        set_property(&mut self.properties, key, value);
        write_properties(&self.server_dir.join("server.properties"), &self.properties)?;
        self.status = format!("✓ {} = {}", key, value);
        Ok(())
    }
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
    match app.tab {
        TabId::Worlds => draw_worlds(f, chunks[2], app),
        TabId::Whitelist => draw_whitelist(f, chunks[2], app),
        TabId::Ops => draw_ops(f, chunks[2], app),
        TabId::Config => draw_config(f, chunks[2], app),
        TabId::Logs => draw_logs(f, chunks[2], app),
    }
    draw_hints(f, chunks[3], app);

    if let Some(prompt) = app.prompt.clone() {
        draw_prompt(f, &prompt);
    }
}

fn draw_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let pid_text = match app.pid {
        Some(p) => Span::styled(format!("● running (pid {})", p), Style::default().fg(Color::Green)),
        None => Span::styled("○ stopped", Style::default().fg(Color::DarkGray)),
    };
    let line = Line::from(vec![
        Span::styled("server: ", Style::default().add_modifier(Modifier::DIM)),
        pid_text,
        Span::raw("    "),
        Span::styled("level: ", Style::default().add_modifier(Modifier::DIM)),
        Span::styled(app.current_level(), Style::default().fg(Color::Cyan)),
        Span::raw("    "),
        Span::styled("dir: ", Style::default().add_modifier(Modifier::DIM)),
        Span::raw(app.server_dir.display().to_string()),
    ]);
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(" mc-tui "));
    f.render_widget(p, area);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = TABS
        .iter()
        .enumerate()
        .map(|(i, (_, name))| Line::from(format!(" {} {} ", i + 1, name)))
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
        .block(Block::default().borders(Borders::ALL).title(" Worlds (●=current) "))
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
        .block(Block::default().borders(Borders::ALL).title(" Whitelist "))
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
        .block(Block::default().borders(Borders::ALL).title(" Operators (←/→ change level) "))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut app.ops_state);
}

fn draw_config(f: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .properties
        .iter()
        .map(|(k, v)| {
            let value_color = match v.as_str() {
                "true" => Color::Green,
                "false" => Color::Red,
                _ => Color::Cyan,
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {:35}", k), Style::default().fg(Color::White)),
                Span::raw("= "),
                Span::styled(v, Style::default().fg(value_color)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" server.properties (Enter = edit) "))
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
            Err(e) => format!("(read error: {e})"),
        }
    } else {
        "(no logs yet)".to_string()
    };
    let p = Paragraph::new(body)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Logs — tail of {} ", log_path.display())),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_hints(f: &mut Frame, area: Rect, app: &App) {
    let hint = match app.tab {
        TabId::Worlds => "↑/↓ select   Enter switch   r refresh   Tab/1-5 tabs   q quit",
        TabId::Whitelist => "↑/↓ select   a add   d remove   r refresh   Tab/1-5 tabs   q quit",
        TabId::Ops => "↑/↓ select   a add   d remove   ←/→ level   r refresh   Tab/1-5 tabs   q quit",
        TabId::Config => "↑/↓ select   Enter edit   r refresh   Tab/1-5 tabs   q quit",
        TabId::Logs => "r refresh   Tab/1-5 tabs   q quit",
    };
    let line = Line::from(vec![
        Span::styled(format!(" {} ", hint), Style::default().fg(Color::DarkGray)),
        Span::raw("  │  "),
        Span::styled(&app.status, Style::default().fg(Color::Yellow)),
    ]);
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn draw_prompt(f: &mut Frame, prompt: &InputPrompt) {
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
            "Enter = confirm    Esc = cancel",
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

        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if let Some(mut prompt) = app.prompt.take() {
            match key.code {
                KeyCode::Esc => {
                    app.status = "Cancelled.".into();
                }
                KeyCode::Enter => {
                    let value = prompt.buffer.clone();
                    match prompt.action {
                        PromptAction::AddWhitelist => app.add_whitelist(&value)?,
                        PromptAction::AddOp => app.add_op(&value)?,
                        PromptAction::EditConfig(key) => app.save_config_value(&key, &value)?,
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
                app.status = "Refreshed.".into();
            }
            KeyCode::Up => app.move_selection(-1),
            KeyCode::Down => app.move_selection(1),
            KeyCode::Enter => match app.tab {
                TabId::Worlds => app.switch_world()?,
                TabId::Config => {
                    if let Some(idx) = app.config_state.selected() {
                        if let Some((k, v)) = app.properties.get(idx).cloned() {
                            app.prompt = Some(InputPrompt {
                                title: format!("Edit {}", k),
                                label: "value".into(),
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
                    app.prompt = Some(InputPrompt {
                        title: "Add to whitelist".into(),
                        label: "player name".into(),
                        buffer: String::new(),
                        action: PromptAction::AddWhitelist,
                    });
                }
                TabId::Ops => {
                    app.prompt = Some(InputPrompt {
                        title: "Op a player".into(),
                        label: "player name".into(),
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
            _ => {}
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut app = App::new(cli.server_dir)?;

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
}
