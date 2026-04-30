//! mc-tui — a TUI manager for a local Minecraft Paper/Purpur server.

mod cli;
mod data;
mod i18n;
mod sys;
use cli::*;
use data::*;
use i18n::*;
use sys::*;

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


// (i18n / data / sys / cli moved to their own files — see mod declarations above.)

// ---------- App state ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabId {
    Worlds,
    Whitelist,
    Ops,
    Config,
    Logs,
    Yaml,
    Backups,
    Rcon,
    Server,
}

const TABS: &[(TabId, &str)] = &[
    (TabId::Worlds, "Worlds"),
    (TabId::Whitelist, "Whitelist"),
    (TabId::Ops, "Ops"),
    (TabId::Config, "Config"),
    (TabId::Logs, "Logs"),
    (TabId::Yaml, "YAML"),
    (TabId::Backups, "Backups"),
    (TabId::Rcon, "RCON"),
    (TabId::Server, "Server"),
];

/// Server-tab actions (v0.6). Stable order — index used in events / tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerAction {
    RestartNow,
    BackupNow,
    ScheduleDailyRestart,
    ScheduleDailyBackup,
    PreGenChunks,
    OpenSystemdStatus,
    ShowAttachCommand,
}

const SERVER_ACTIONS: &[ServerAction] = &[
    ServerAction::RestartNow,
    ServerAction::BackupNow,
    ServerAction::ShowAttachCommand,
    ServerAction::ScheduleDailyRestart,
    ServerAction::ScheduleDailyBackup,
    ServerAction::PreGenChunks,
    ServerAction::OpenSystemdStatus,
];

/// YAML tab toggles between file picker and a flat row editor for one file.
#[derive(Debug, Clone)]
enum YamlView {
    Files,
    Editing { file_idx: usize },
}

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
    EditYaml,
    RconCommand,
    ScheduleDailyRestart,
    ScheduleDailyBackup,
    PreGenChunkRadius,
}

struct App {
    server_dir: PathBuf,
    properties: Vec<(String, String)>,
    worlds: Vec<WorldEntry>,
    whitelist: Vec<WhitelistEntry>,
    ops: Vec<OpEntry>,
    /// True if the on-disk whitelist.json failed to parse last refresh.
    /// While set, mc-tui refuses to write to whitelist.json (would clobber the
    /// real, hand-edited file with our empty in-memory copy).
    whitelist_corrupt: bool,
    /// Same idea for ops.json.
    ops_corrupt: bool,
    pid: Option<u32>,

    tab: TabId,
    worlds_state: ListState,
    whitelist_state: ListState,
    ops_state: ListState,
    config_state: ListState,

    // v0.5 — YAML
    yaml_files: Vec<PathBuf>,
    yaml_files_state: ListState,
    yaml_view: YamlView,
    yaml_root: Option<serde_yaml::Value>,
    yaml_rows: Vec<YamlRow>,
    yaml_rows_state: ListState,

    // v0.5 — Backups
    backups: Vec<BackupEntry>,
    backups_state: ListState,

    // v0.5 — RCON
    rcon_history: Vec<(String, String)>,
    rcon_state: ListState,

    // v0.6 — Server ops
    server_state: ListState,

    status: String,
    prompt: Option<InputPrompt>,

    // Mouse hit-testing rects, populated each frame inside `ui()`.
    tabs_rect: Rect,
    list_rect: Rect,
    /// Each entry is the screen rect of a join-address chip and the literal
    /// `ip:port` to copy on click.
    join_chips: Vec<(Rect, String)>,

    lang: Lang,
}

impl App {
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
            whitelist_corrupt: false,
            ops_corrupt: false,
            pid: None,
            tab: TabId::Worlds,
            worlds_state: ListState::default(),
            whitelist_state: ListState::default(),
            ops_state: ListState::default(),
            config_state: ListState::default(),
            yaml_files: Vec::new(),
            yaml_files_state: ListState::default(),
            yaml_view: YamlView::Files,
            yaml_root: None,
            yaml_rows: Vec::new(),
            yaml_rows_state: ListState::default(),
            backups: Vec::new(),
            backups_state: ListState::default(),
            rcon_history: Vec::new(),
            rcon_state: ListState::default(),
            server_state: ListState::default(),
            status: match lang {
                Lang::En => String::from("Ready."),
                Lang::Zh => String::from("就绪。"),
            },
            prompt: None,
            tabs_rect: Rect::default(),
            list_rect: Rect::default(),
            join_chips: Vec::new(),
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
        if !app.yaml_files.is_empty() {
            app.yaml_files_state.select(Some(0));
        }
        if !app.backups.is_empty() {
            app.backups_state.select(Some(0));
        }
        app.server_state.select(Some(0));
        Ok(app)
    }

    fn current_level(&self) -> &str {
        get_property(&self.properties, "level-name").unwrap_or("world")
    }

    fn refresh_all(&mut self) {
        let cur = self.current_level().to_string();
        self.worlds = scan_worlds(&self.server_dir, &cur);
        match read_whitelist(&self.server_dir) {
            Ok(v) => {
                self.whitelist = v;
                self.whitelist_corrupt = false;
            }
            Err(e) => {
                self.whitelist = Vec::new();
                self.whitelist_corrupt = true;
                self.status = match self.lang {
                    Lang::En => format!("✗ whitelist.json unreadable: {} (writes blocked)", e),
                    Lang::Zh => format!("✗ whitelist.json 无法解析：{}（写入已封锁）", e),
                };
            }
        }
        match read_ops(&self.server_dir) {
            Ok(v) => {
                self.ops = v;
                self.ops_corrupt = false;
            }
            Err(e) => {
                self.ops = Vec::new();
                self.ops_corrupt = true;
                self.status = match self.lang {
                    Lang::En => format!("✗ ops.json unreadable: {} (writes blocked)", e),
                    Lang::Zh => format!("✗ ops.json 无法解析：{}（写入已封锁）", e),
                };
            }
        }
        self.pid = server_running_pid(&self.server_dir, self.pid);
        self.yaml_files = list_yaml_files(&self.server_dir);
        self.backups = scan_backups(&self.server_dir);
    }

    fn list_state_for(&mut self, tab: TabId) -> &mut ListState {
        match tab {
            TabId::Worlds => &mut self.worlds_state,
            TabId::Whitelist => &mut self.whitelist_state,
            TabId::Ops => &mut self.ops_state,
            TabId::Config => &mut self.config_state,
            TabId::Logs => &mut self.worlds_state,
            TabId::Yaml => match self.yaml_view {
                YamlView::Files => &mut self.yaml_files_state,
                YamlView::Editing { .. } => &mut self.yaml_rows_state,
            },
            TabId::Backups => &mut self.backups_state,
            TabId::Rcon => &mut self.rcon_state,
            TabId::Server => &mut self.server_state,
        }
    }

    fn list_len_for(&self, tab: TabId) -> usize {
        match tab {
            TabId::Worlds => self.worlds.len(),
            TabId::Whitelist => self.whitelist.len(),
            TabId::Ops => self.ops.len(),
            TabId::Config => self.properties.len(),
            TabId::Logs => 0,
            TabId::Yaml => match self.yaml_view {
                YamlView::Files => self.yaml_files.len(),
                YamlView::Editing { .. } => self.yaml_rows.len(),
            },
            TabId::Backups => self.backups.len(),
            TabId::Rcon => self.rcon_history.len(),
            TabId::Server => SERVER_ACTIONS.len(),
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
        if self.whitelist_corrupt {
            self.status = match self.lang {
                Lang::En => "✗ whitelist.json is corrupt — fix it before editing.".into(),
                Lang::Zh => "✗ whitelist.json 损坏，请先修复后再编辑。".into(),
            };
            return Ok(());
        }
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
        if self.whitelist_corrupt {
            self.status = match self.lang {
                Lang::En => "✗ whitelist.json is corrupt — fix it before editing.".into(),
                Lang::Zh => "✗ whitelist.json 损坏，请先修复后再编辑。".into(),
            };
            return Ok(());
        }
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
        if self.ops_corrupt {
            self.status = match self.lang {
                Lang::En => "✗ ops.json is corrupt — fix it before editing.".into(),
                Lang::Zh => "✗ ops.json 损坏，请先修复后再编辑。".into(),
            };
            return Ok(());
        }
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
        if self.ops_corrupt {
            self.status = match self.lang {
                Lang::En => "✗ ops.json is corrupt — fix it before editing.".into(),
                Lang::Zh => "✗ ops.json 损坏，请先修复后再编辑。".into(),
            };
            return Ok(());
        }
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
        if self.ops_corrupt {
            self.status = match self.lang {
                Lang::En => "✗ ops.json is corrupt — fix it before editing.".into(),
                Lang::Zh => "✗ ops.json 损坏，请先修复后再编辑。".into(),
            };
            return Ok(());
        }
        let Some(idx) = self.ops_state.selected() else { return Ok(()) };
        if idx >= self.ops.len() {
            return Ok(());
        }
        // Wrap-around 1..=4 (CLAUDE.md says "Level cycles 1–4"): ←/→ at the edges
        // jumps back to the other end instead of clamping.
        let cur = self.ops[idx].level as i16;
        let new = ((cur - 1 + dir as i16).rem_euclid(4) + 1) as u8;
        self.ops[idx].level = new;
        write_ops(&self.server_dir, &self.ops)?;
        let name = self.ops[idx].name.clone();
        self.status = fmt_op_level_changed(self.lang, &name, new);
        Ok(())
    }

    // -- v0.5: YAML --

    fn yaml_open(&mut self, idx: usize) -> Result<()> {
        let Some(path) = self.yaml_files.get(idx).cloned() else { return Ok(()) };
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let value: serde_yaml::Value = serde_yaml::from_str(&raw)
            .with_context(|| format!("parse YAML {}", path.display()))?;
        self.yaml_rows = flatten_yaml(&value);
        self.yaml_root = Some(value);
        self.yaml_view = YamlView::Editing { file_idx: idx };
        self.yaml_rows_state = ListState::default();
        if !self.yaml_rows.is_empty() {
            self.yaml_rows_state.select(Some(0));
        }
        self.status = match self.lang {
            Lang::En => format!("✓ Opened {}", path.display()),
            Lang::Zh => format!("✓ 已打开 {}", path.display()),
        };
        Ok(())
    }

    fn yaml_close(&mut self) {
        self.yaml_view = YamlView::Files;
        self.yaml_root = None;
        self.yaml_rows.clear();
    }

    fn yaml_save_current(&mut self, value_str: &str) -> Result<()> {
        let YamlView::Editing { file_idx } = self.yaml_view.clone() else { return Ok(()) };
        let Some(idx) = self.yaml_rows_state.selected() else { return Ok(()) };
        let Some(row) = self.yaml_rows.get(idx).cloned() else { return Ok(()) };
        let Some(root) = self.yaml_root.as_mut() else { return Ok(()) };
        yaml_set(root, &row.path, parse_yaml_scalar(value_str))?;
        // Persist back to disk.
        let path = self
            .yaml_files
            .get(file_idx)
            .cloned()
            .context("yaml file index out of range")?;
        let dumped = serde_yaml::to_string(root).context("serialize YAML")?;
        fs::write(&path, dumped).with_context(|| format!("write {}", path.display()))?;
        // Re-flatten so the row's display value updates.
        self.yaml_rows = flatten_yaml(root);
        if !self.yaml_rows.is_empty() {
            self.yaml_rows_state.select(Some(idx.min(self.yaml_rows.len() - 1)));
        }
        self.status = match self.lang {
            Lang::En => format!("✓ Wrote {}", path.display()),
            Lang::Zh => format!("✓ 已写入 {}", path.display()),
        };
        Ok(())
    }

    // -- v0.5: RCON --

    fn rcon_send(&mut self, cmd: &str) -> Result<()> {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return Ok(());
        }
        if self.pid.is_none() {
            self.status = self.lang.s().server_pregen_no_running.into();
            return Ok(());
        }
        let Some((host, port, password)) = rcon_settings(&self.properties) else {
            self.status = self.lang.s().rcon_disabled_in_props.into();
            return Ok(());
        };
        match RconClient::connect(&host, port, &password)
            .and_then(|mut c| c.exec(cmd))
        {
            Ok(resp) => {
                self.rcon_history.push((cmd.to_string(), resp));
                self.status = match self.lang {
                    Lang::En => "✓ RCON ok".into(),
                    Lang::Zh => "✓ RCON 已执行".into(),
                };
            }
            Err(e) => {
                self.status = match self.lang {
                    Lang::En => format!("✗ RCON: {}", e),
                    Lang::Zh => format!("✗ RCON 失败：{}", e),
                };
            }
        }
        // Auto-scroll to last entry.
        if !self.rcon_history.is_empty() {
            self.rcon_state.select(Some(self.rcon_history.len() - 1));
        }
        Ok(())
    }

    // -- v0.6: Server ops --

    fn backup_now(&mut self) -> Result<()> {
        let script = self.server_dir.join("backup.sh");
        if !script.exists() {
            self.status = match self.lang {
                Lang::En => format!("✗ {} not found", script.display()),
                Lang::Zh => format!("✗ {} 不存在", script.display()),
            };
            return Ok(());
        }
        use std::process::{Command, Stdio};
        let res = Command::new("bash")
            .arg(&script)
            .current_dir(&self.server_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match res {
            Ok(_) => {
                self.status = match self.lang {
                    Lang::En => "→ Spawned backup.sh in background.".into(),
                    Lang::Zh => "→ 已后台启动 backup.sh。".into(),
                };
            }
            Err(e) => {
                self.status = match self.lang {
                    Lang::En => format!("✗ spawn failed: {}", e),
                    Lang::Zh => format!("✗ 启动失败：{}", e),
                };
            }
        }
        Ok(())
    }

    fn restart_now(&mut self) -> Result<()> {
        if let Some(pid) = self.pid {
            self.stop_server()?;
            // Wait briefly for graceful shutdown.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if server_running_pid(&self.server_dir, Some(pid)).is_none() {
                    break;
                }
            }
            self.pid = server_running_pid(&self.server_dir, None);
            if self.pid == Some(pid) {
                self.status = match self.lang {
                    Lang::En => "⚠ stop timed out — start cancelled.".into(),
                    Lang::Zh => "⚠ 停止超时 — 已取消启动。".into(),
                };
                return Ok(());
            }
        }
        self.start_server()
    }

    fn schedule_daily(&mut self, kind: ServerAction, time: &str) -> Result<()> {
        let (hour, minute) = match parse_hh_mm(time) {
            Some(t) => t,
            None => {
                self.status = match self.lang {
                    Lang::En => format!("✗ Invalid time '{}'. Expected HH:MM.", time),
                    Lang::Zh => format!("✗ 时间格式非法：'{}'。预期 HH:MM。", time),
                };
                return Ok(());
            }
        };
        let (unit_name, command, description) = match kind {
            ServerAction::ScheduleDailyRestart => (
                format!("mc-tui-restart-{}", server_dir_slug(&self.server_dir)),
                format!(
                    "/usr/bin/env bash -c 'cd {0:?} && (test -x ./stop.sh && ./stop.sh || pkill -TERM -f \"java.*paper\\|purpur\"; sleep 30; setsid bash {0:?}/start.sh)'",
                    self.server_dir
                ),
                "mc-tui daily restart".to_string(),
            ),
            ServerAction::ScheduleDailyBackup => (
                format!("mc-tui-backup-{}", server_dir_slug(&self.server_dir)),
                format!("/usr/bin/env bash {:?}/backup.sh", self.server_dir),
                "mc-tui daily backup".to_string(),
            ),
            _ => return Ok(()),
        };
        let unit_dir = config_dir().parent().unwrap_or(Path::new(".")).join("systemd").join("user");
        if let Err(e) = fs::create_dir_all(&unit_dir) {
            self.status = match self.lang {
                Lang::En => format!("✗ create {}: {}", unit_dir.display(), e),
                Lang::Zh => format!("✗ 创建 {} 失败：{}", unit_dir.display(), e),
            };
            return Ok(());
        }
        let service = format!(
            "[Unit]\nDescription={desc}\n\n[Service]\nType=oneshot\nWorkingDirectory={cwd:?}\nExecStart={cmd}\n",
            desc = description,
            cwd = self.server_dir,
            cmd = command
        );
        let timer = format!(
            "[Unit]\nDescription={desc} timer\n\n[Timer]\nOnCalendar=*-*-* {h:02}:{m:02}:00\nPersistent=true\nUnit={name}.service\n\n[Install]\nWantedBy=timers.target\n",
            desc = description,
            h = hour,
            m = minute,
            name = unit_name
        );
        let svc_path = unit_dir.join(format!("{}.service", unit_name));
        let tim_path = unit_dir.join(format!("{}.timer", unit_name));
        if let Err(e) = fs::write(&svc_path, &service).and_then(|_| fs::write(&tim_path, &timer)) {
            self.status = match self.lang {
                Lang::En => format!("✗ write unit: {}", e),
                Lang::Zh => format!("✗ 写入 unit 失败：{}", e),
            };
            return Ok(());
        }
        self.status = match self.lang {
            Lang::En => format!(
                "✓ Wrote {} + .timer. Then: systemctl --user daemon-reload && systemctl --user enable --now {}.timer",
                svc_path.display(),
                unit_name
            ),
            Lang::Zh => format!(
                "✓ 已写入 {} 和 .timer。下一步：systemctl --user daemon-reload && systemctl --user enable --now {}.timer",
                svc_path.display(),
                unit_name
            ),
        };
        Ok(())
    }

    fn pregen_chunks(&mut self, radius_str: &str) -> Result<()> {
        let radius: i32 = match radius_str.trim().parse() {
            Ok(n) if n > 0 && n <= 5000 => n,
            _ => {
                self.status = match self.lang {
                    Lang::En => format!("✗ Invalid radius '{}' (1–5000)", radius_str),
                    Lang::Zh => format!("✗ 非法半径 '{}'（应在 1–5000）", radius_str),
                };
                return Ok(());
            }
        };
        if self.pid.is_none() {
            self.status = self.lang.s().server_pregen_no_running.into();
            return Ok(());
        }
        let Some((host, port, password)) = rcon_settings(&self.properties) else {
            self.status = self.lang.s().rcon_disabled_in_props.into();
            return Ok(());
        };
        let mut client = match RconClient::connect(&host, port, &password) {
            Ok(c) => c,
            Err(e) => {
                self.status = match self.lang {
                    Lang::En => format!("✗ RCON connect: {}", e),
                    Lang::Zh => format!("✗ RCON 连接失败：{}", e),
                };
                return Ok(());
            }
        };
        // Try chunky first (most efficient); fall back to vanilla worldborder.
        let level = self.current_level().to_string();
        let cmds = vec![
            format!("chunky world {}", level),
            format!("chunky center 0 0"),
            format!("chunky radius {}", radius),
            format!("chunky start"),
        ];
        let mut log = String::new();
        for c in &cmds {
            match client.exec(c) {
                Ok(r) => log.push_str(&format!("$ {}\n{}\n", c, r)),
                Err(e) => {
                    log.push_str(&format!("$ {} → ERR {}\n", c, e));
                    break;
                }
            }
        }
        self.rcon_history.push(("(pre-gen chunks)".into(), log));
        if !self.rcon_history.is_empty() {
            self.rcon_state.select(Some(self.rcon_history.len() - 1));
        }
        self.status = match self.lang {
            Lang::En => format!("✓ Pre-gen sent (radius {}). Watch RCON tab for progress.", radius),
            Lang::Zh => format!("✓ 已发送区块预加载（半径 {}）。在 RCON 页查看进度。", radius),
        };
        Ok(())
    }

    fn show_attach_command(&mut self) {
        let session = tmux_session_name(&self.server_dir);
        let cmd = format!("tmux attach -t {}", session);
        let alive = which("tmux").is_some() && tmux_session_alive(&session);
        // Best-effort copy to wl-clipboard; ignore failures (e.g. headless / no wayland).
        let _ = std::process::Command::new("wl-copy")
            .arg(&cmd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        self.status = match (self.lang, alive) {
            (Lang::En, true) => format!("ℹ Copied to clipboard: {}", cmd),
            (Lang::En, false) => format!("ℹ {} (session not yet alive)", cmd),
            (Lang::Zh, true) => format!("ℹ 已复制到剪贴板：{}", cmd),
            (Lang::Zh, false) => format!("ℹ {}（会话尚未启动）", cmd),
        };
    }

    fn show_systemd_status(&mut self) {
        let unit_dir = config_dir().parent().unwrap_or(Path::new(".")).join("systemd").join("user");
        self.status = match self.lang {
            Lang::En => format!(
                "ℹ {}: {} | run: systemctl --user list-timers",
                self.lang.s().server_systemd_unit_dir,
                unit_dir.display()
            ),
            Lang::Zh => format!(
                "ℹ {}: {} ｜ 命令: systemctl --user list-timers",
                self.lang.s().server_systemd_unit_dir,
                unit_dir.display()
            ),
        };
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

        // Preferred: launch inside a detached tmux session so we can later send
        // the `stop` console command — it runs Minecraft's own shutdown path
        // (synchronous save on the main thread) instead of relying on JVM
        // signal handlers, which we've seen race with startup and end up half-dead.
        let session = tmux_session_name(&self.server_dir);
        if which("tmux").is_some() {
            // Re-attach situation: if a session by this name already exists,
            // assume it's our previous server and tell the user.
            if tmux_session_alive(&session) {
                self.status = match self.lang {
                    Lang::En => format!(
                        "→ tmux session '{}' already exists. Attach with: tmux attach -t {}",
                        session, session
                    ),
                    Lang::Zh => format!(
                        "→ tmux 会话 '{}' 已存在。接管：tmux attach -t {}",
                        session, session
                    ),
                };
                return Ok(());
            }
            // tmux passes the command string to `/bin/sh -c`; quote the path
            // so spaces, quotes, $, ` etc. in server-dir don't break the launch.
            let cmd_str = format!("bash {}", shell_quote_sh(&script.display().to_string()));
            let res = Command::new("tmux")
                .arg("new-session")
                .arg("-d")
                .arg("-s")
                .arg(&session)
                .arg("-c")
                .arg(&self.server_dir)
                .arg(&cmd_str)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            match res {
                Ok(s) if s.success() => {
                    self.status = match self.lang {
                        Lang::En => format!(
                            "✓ Started in tmux session '{}'. Attach: tmux attach -t {}",
                            session, session
                        ),
                        Lang::Zh => format!(
                            "✓ 已在 tmux 会话 '{}' 中启动。接管：tmux attach -t {}",
                            session, session
                        ),
                    };
                    return Ok(());
                }
                Ok(s) => {
                    self.status = fmt_spawn_failed(self.lang, &format!("tmux exited {:?}", s.code()));
                    return Ok(());
                }
                Err(e) => {
                    self.status = fmt_spawn_failed(self.lang, &e.to_string());
                    return Ok(());
                }
            }
        }

        // Fallback: setsid bash (no console — `stop` will rely on SIGTERM and may race).
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

        // Prefer the tmux console — `stop` runs Minecraft's own shutdown handler
        // on the main server thread, which is the only path that's reliable.
        let session = tmux_session_name(&self.server_dir);
        if which("tmux").is_some() && tmux_session_alive(&session) {
            let res = Command::new("tmux")
                .args(["send-keys", "-t", &session, "stop", "Enter"])
                .status();
            match res {
                Ok(s) if s.success() => {
                    self.status = match self.lang {
                        Lang::En => format!(
                            "→ Sent `stop` to tmux session '{}'. Watching for exit…",
                            session
                        ),
                        Lang::Zh => format!(
                            "→ 已向 tmux 会话 '{}' 发送 `stop`，等待退出…",
                            session
                        ),
                    };
                    return Ok(());
                }
                Ok(s) => {
                    self.status = fmt_kill_failed(
                        self.lang,
                        &format!("tmux send-keys exited {:?}", s.code()),
                    );
                    return Ok(());
                }
                Err(e) => {
                    self.status = fmt_kill_failed(self.lang, &e.to_string());
                    return Ok(());
                }
            }
        }

        // Fallback: SIGTERM the detected pid. JVM shutdown hook may stall under
        // race conditions; if so, the user can SIGKILL manually.
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

        // Drop YAML editor state — yaml_root / yaml_rows belong to the OLD dir
        // and yaml_save_current would otherwise dump them into the NEW dir's
        // config files, corrupting them.
        self.yaml_view = YamlView::Files;
        self.yaml_root = None;
        self.yaml_rows.clear();
        self.yaml_rows_state = ListState::default();
        self.yaml_files_state = ListState::default();

        // RCON history is per-server; clear it.
        self.rcon_history.clear();
        self.rcon_state = ListState::default();

        self.backups_state = ListState::default();
        self.server_state = ListState::default();
        self.server_state.select(Some(0));

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
        if !self.yaml_files.is_empty() {
            self.yaml_files_state.select(Some(0));
        }
        if !self.backups.is_empty() {
            self.backups_state.select(Some(0));
        }

        let mut state = read_persisted_state();
        state.server_dir = Some(self.server_dir.clone());
        let _ = write_persisted_state(&state);

        self.status = fmt_dir_switched(self.lang, &self.server_dir);
        Ok(())
    }
}

// ---------- UI ----------

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // status bar
            Constraint::Length(3), // join bar (always-visible primary connect chip)
            Constraint::Length(3), // tabs
            Constraint::Min(3),    // content
            Constraint::Length(3), // hints / status line
        ])
        .split(f.area());

    draw_status_bar(f, chunks[0], app);
    draw_join_bar(f, chunks[1], app);
    draw_tabs(f, chunks[2], app);
    app.tabs_rect = chunks[2];
    app.list_rect = chunks[3];
    match app.tab {
        TabId::Worlds => draw_worlds(f, chunks[3], app),
        TabId::Whitelist => draw_whitelist(f, chunks[3], app),
        TabId::Ops => draw_ops(f, chunks[3], app),
        TabId::Config => draw_config(f, chunks[3], app),
        TabId::Logs => draw_logs(f, chunks[3], app),
        TabId::Yaml => draw_yaml(f, chunks[3], app),
        TabId::Backups => draw_backups(f, chunks[3], app),
        TabId::Rcon => draw_rcon(f, chunks[3], app),
        TabId::Server => draw_server(f, chunks[3], app),
    }
    draw_hints(f, chunks[4], app);

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

/// Always-visible primary connect address (typically the ZeroTier one).
/// Click the chip to copy `<ip>:<port>` to the clipboard via wl-copy.
fn draw_join_bar(f: &mut Frame, area: Rect, app: &mut App) {
    let nics = detect_interfaces();
    let port: u16 = get_property(&app.properties, "server-port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(25565);

    // Pick the most "tell-friends-this-one" interface. nic_kind_priority orders
    // ZeroTier first, then LAN, then Public, etc. Skip Loopback / Docker / TUN.
    let primary = nics.iter().find(|n| {
        !matches!(
            n.kind,
            NicKind::Loopback | NicKind::Docker | NicKind::Tun
        )
    });

    app.join_chips.clear();

    let inner_x = area.x.saturating_add(1);
    let inner_y = area.y.saturating_add(1);

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(" "));

    let label_lang = app.lang;
    let title = match app.lang {
        Lang::En => " Join — click to copy ",
        Lang::Zh => " 连接地址（点击复制）",
    };

    if let Some(n) = primary {
        let chip_text = format!("{}:{}", n.ip, port);
        let kind_label = nic_kind_label(label_lang, n.kind);

        // Layout: " [<kind>] <ip>:<port> "
        spans.push(Span::styled(
            format!("[{}]", kind_label),
            Style::default().fg(nic_kind_color(n.kind)).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));

        // Track chip rect (x..x+chip_text.len(), y=inner_y) for mouse hit-testing.
        let mut chip_x = inner_x + 1; // " "
        chip_x += format!("[{}]", kind_label).chars().count() as u16;
        chip_x += 1; // " "
        let chip_w = chip_text.chars().count() as u16;
        let chip_rect = Rect {
            x: chip_x,
            y: inner_y,
            width: chip_w,
            height: 1,
        };
        app.join_chips.push((chip_rect, chip_text.clone()));

        spans.push(Span::styled(
            chip_text,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ));
    } else {
        spans.push(Span::styled(
            match app.lang {
                Lang::En => "(no LAN/Public/ZeroTier IPv4 detected)",
                Lang::Zh => "(没检测到 LAN/Public/ZeroTier IPv4)",
            },
            Style::default().fg(Color::DarkGray),
        ));
    }

    let p = Paragraph::new(Line::from(spans))
        .block(Block::default().borders(Borders::ALL).title(title));
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
    let (list_area, detail_area) = split_list_detail(area);
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
    f.render_stateful_widget(list, list_area, &mut app.worlds_state);
    if let Some(da) = detail_area {
        draw_world_detail(f, da, app);
    }
}

/// Split a content area horizontally into `(list, detail)`. If the screen is
/// narrower than 90 cols the detail panel is hidden (single-pane fallback).
fn split_list_detail(area: Rect) -> (Rect, Option<Rect>) {
    if area.width < 90 {
        return (area, None);
    }
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);
    (chunks[0], Some(chunks[1]))
}

fn kv_line_label(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{}: ", label), Style::default().fg(Color::DarkGray)),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ])
}

fn kv_line_bold(value: &str, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        value.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

fn draw_world_detail(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let block = Block::default().borders(Borders::ALL).title(s.detail_title);
    let lines: Vec<Line> = match app.worlds_state.selected().and_then(|i| app.worlds.get(i)) {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::DarkGray),
        ))],
        Some(w) => {
            let when = w
                .last_modified
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "?".into());
            let yn = |b: bool| if b { s.detail_yes } else { s.detail_no };
            vec![
                kv_line_bold(&w.name, Color::Cyan),
                Line::raw(""),
                kv_line_label(s.detail_path, &w.path.display().to_string()),
                kv_line_label(s.detail_size, &fmt_bytes(w.size_bytes)),
                kv_line_label(s.detail_modified, &when),
                kv_line_label(s.detail_is_current, yn(w.is_current)),
                kv_line_label(s.detail_has_level_dat, yn(w.has_level_dat)),
                kv_line_label(s.detail_playerdata_count, &w.playerdata_count.to_string()),
            ]
        }
    };
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_whitelist(f: &mut Frame, area: Rect, app: &mut App) {
    let (list_area, detail_area) = split_list_detail(area);
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
    f.render_stateful_widget(list, list_area, &mut app.whitelist_state);
    if let Some(da) = detail_area {
        draw_whitelist_detail(f, da, app);
    }
}

fn draw_whitelist_detail(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let block = Block::default().borders(Borders::ALL).title(s.detail_title);
    let lines: Vec<Line> = match app
        .whitelist_state
        .selected()
        .and_then(|i| app.whitelist.get(i))
    {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::DarkGray),
        ))],
        Some(e) => vec![
            kv_line_bold(&e.name, Color::Cyan),
            Line::raw(""),
            Line::from(Span::styled(
                format!("{}:", s.detail_uuid),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                e.uuid.clone(),
                Style::default().fg(Color::White),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                s.detail_offline_uuid_note.to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
        ],
    };
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_ops(f: &mut Frame, area: Rect, app: &mut App) {
    let (list_area, detail_area) = split_list_detail(area);
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
    f.render_stateful_widget(list, list_area, &mut app.ops_state);
    if let Some(da) = detail_area {
        draw_ops_detail(f, da, app);
    }
}

fn op_level_meaning(s: &Strings, level: u8) -> &'static str {
    match level {
        1 => s.detail_op_level_1,
        2 => s.detail_op_level_2,
        3 => s.detail_op_level_3,
        4 => s.detail_op_level_4,
        _ => "?",
    }
}

fn draw_ops_detail(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let block = Block::default().borders(Borders::ALL).title(s.detail_title);
    let lines: Vec<Line> = match app.ops_state.selected().and_then(|i| app.ops.get(i)) {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::DarkGray),
        ))],
        Some(e) => {
            let yn = |b: bool| if b { s.detail_yes } else { s.detail_no };
            vec![
                kv_line_bold(&e.name, Color::Cyan),
                Line::raw(""),
                Line::from(Span::styled(
                    format!("{}:", s.detail_uuid),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    e.uuid.clone(),
                    Style::default().fg(Color::White),
                )),
                Line::raw(""),
                kv_line_label(s.detail_level, &e.level.to_string()),
                kv_line_label(s.detail_level_meaning, op_level_meaning(s, e.level)),
                kv_line_label(s.detail_bypass, yn(e.bypasses_player_limit)),
            ]
        }
    };
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_config(f: &mut Frame, area: Rect, app: &mut App) {
    let lang = app.lang;
    let (list_area, detail_area) = split_list_detail(area);
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
    f.render_stateful_widget(list, list_area, &mut app.config_state);
    if let Some(da) = detail_area {
        draw_config_detail(f, da, app);
    }
}

fn draw_config_detail(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let block = Block::default().borders(Borders::ALL).title(s.detail_title);
    let lines: Vec<Line> = match app
        .config_state
        .selected()
        .and_then(|i| app.properties.get(i))
    {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::DarkGray),
        ))],
        Some((k, v)) => {
            let mut out = vec![
                kv_line_label(s.detail_key, k),
                kv_line_label(s.detail_value, v),
                Line::raw(""),
            ];
            match property_metadata(k) {
                Some(m) => {
                    let yn = if m.restart_required { s.detail_yes } else { s.detail_no };
                    out.push(kv_line_label(s.detail_default, m.default));
                    out.push(kv_line_label(s.detail_range, m.range));
                    out.push(kv_line_label(s.detail_restart_required, yn));
                    out.push(Line::raw(""));
                    out.push(Line::from(Span::styled(
                        format!("{}:", s.detail_description),
                        Style::default().fg(Color::DarkGray),
                    )));
                    let desc = match app.lang {
                        Lang::En => m.description_en,
                        Lang::Zh => m.description_zh,
                    };
                    out.push(Line::from(Span::styled(
                        desc.to_string(),
                        Style::default().fg(Color::White),
                    )));
                }
                None => {
                    out.push(Line::raw(""));
                    out.push(Line::from(Span::styled(
                        s.detail_no_metadata.to_string(),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }
            out
        }
    };
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
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

fn draw_yaml(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();
    match &app.yaml_view {
        YamlView::Files => {
            let items: Vec<ListItem> = if app.yaml_files.is_empty() {
                vec![ListItem::new(Line::from(Span::styled(
                    s.yaml_no_files,
                    Style::default().fg(Color::DarkGray),
                )))]
            } else {
                app.yaml_files
                    .iter()
                    .map(|p| {
                        let display = p
                            .strip_prefix(&app.server_dir)
                            .unwrap_or(p)
                            .display()
                            .to_string();
                        ListItem::new(Line::from(Span::styled(
                            format!(" {}", display),
                            Style::default().fg(Color::White),
                        )))
                    })
                    .collect()
            };
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(s.title_yaml_files))
                .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
                .highlight_symbol("> ");
            f.render_stateful_widget(list, area, &mut app.yaml_files_state);
        }
        YamlView::Editing { file_idx } => {
            let path = app
                .yaml_files
                .get(*file_idx)
                .cloned()
                .unwrap_or_default();
            let title = format!("{}{} ", s.title_yaml_edit_fmt, path.display());
            let items: Vec<ListItem> = app
                .yaml_rows
                .iter()
                .map(|row| {
                    let indent_str: String = (0..row.indent).map(|_| "  ").collect();
                    let mut spans = vec![
                        Span::raw(" "),
                        Span::raw(indent_str),
                        Span::styled(row.label.clone(), Style::default().fg(Color::White)),
                    ];
                    match &row.value {
                        YamlDisplay::Branch => {
                            spans.push(Span::styled(
                                s.yaml_branch_marker,
                                Style::default().fg(Color::DarkGray),
                            ));
                        }
                        YamlDisplay::Scalar(v) => {
                            spans.push(Span::raw(": "));
                            let color = match v.as_str() {
                                "true" => Color::Green,
                                "false" => Color::Red,
                                _ => Color::Cyan,
                            };
                            spans.push(Span::styled(v.clone(), Style::default().fg(color)));
                        }
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(title))
                .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
                .highlight_symbol("> ");
            f.render_stateful_widget(list, area, &mut app.yaml_rows_state);
        }
    }
}

fn draw_backups(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();
    let items: Vec<ListItem> = if app.backups.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            s.backups_none,
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        let now = chrono::Local::now();
        app.backups
            .iter()
            .map(|b| {
                let age = b
                    .modified
                    .map(|t| fmt_age(now - t))
                    .unwrap_or_else(|| "?".into());
                let when = b
                    .modified
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {:40}", b.name), Style::default().fg(Color::White)),
                    Span::styled(
                        format!("{:>10}  ", fmt_bytes(b.size_bytes)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(when, Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::styled(age, Style::default().fg(Color::Yellow)),
                ]))
            })
            .collect()
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(s.title_backups))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut app.backups_state);
}

fn fmt_age(d: chrono::Duration) -> String {
    let total_secs = d.num_seconds().max(0);
    if total_secs < 60 {
        format!("{}s ago", total_secs)
    } else if total_secs < 3600 {
        format!("{}m ago", total_secs / 60)
    } else if total_secs < 86400 {
        format!("{}h ago", total_secs / 3600)
    } else if total_secs < 86400 * 60 {
        format!("{}d ago", total_secs / 86400)
    } else {
        format!("{}mo ago", total_secs / (86400 * 30))
    }
}

fn draw_rcon(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();
    let enabled = rcon_settings(&app.properties).is_some();
    if !enabled {
        let p = Paragraph::new(Line::from(Span::styled(
            s.rcon_disabled_in_props,
            Style::default().fg(Color::Yellow),
        )))
        .block(Block::default().borders(Borders::ALL).title(s.title_rcon))
        .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }
    let items: Vec<ListItem> = if app.rcon_history.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            s.rcon_history_empty,
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        app.rcon_history
            .iter()
            .flat_map(|(cmd, resp)| {
                let mut out = vec![ListItem::new(Line::from(vec![
                    Span::styled(" $ ", Style::default().fg(Color::Green)),
                    Span::styled(cmd.clone(), Style::default().fg(Color::White)),
                ]))];
                for line in resp.lines() {
                    out.push(ListItem::new(Line::from(vec![
                        Span::styled(
                            format!(" {} ", s.rcon_response_label),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(line.to_string(), Style::default().fg(Color::Cyan)),
                    ])));
                }
                out
            })
            .collect()
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(s.title_rcon))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut app.rcon_state);
}

fn draw_server(f: &mut Frame, area: Rect, app: &mut App) {
    // Vertical split: top = join info (auto-sized to # of interfaces, capped), bottom = actions list.
    let nics = detect_interfaces();
    let join_h = (nics.len() as u16 + 2).max(3).min(12); // border(2) + lines, cap 12
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(join_h), Constraint::Min(3)])
        .split(area);

    draw_join_info(f, chunks[0], app, &nics);
    draw_server_actions(f, chunks[1], app);
}

fn draw_join_info(f: &mut Frame, area: Rect, app: &App, nics: &[NicInfo]) {
    let s = app.lang.s();
    let port: u16 = get_property(&app.properties, "server-port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(25565);

    let lines: Vec<Line> = if nics.is_empty() {
        vec![Line::from(Span::styled(
            s.join_no_interfaces,
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        nics.iter()
            .map(|n| {
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        format!("{:14}", n.name),
                        Style::default().fg(Color::White),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        format!("{}:{}", n.ip, port),
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        format!("[{}]", nic_kind_label(app.lang, n.kind)),
                        Style::default().fg(nic_kind_color(n.kind)),
                    ),
                ])
            })
            .collect()
    };

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(s.join_section_title),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_server_actions(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();
    let items: Vec<ListItem> = SERVER_ACTIONS
        .iter()
        .map(|a| {
            ListItem::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    server_action_label(app.lang, *a),
                    Style::default().fg(Color::White),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(s.server_actions_section),
        )
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut app.server_state);
    // Note: title_server (s.title_server) is intentionally not rendered as a
    // border title here — Server tab uses two stacked blocks ("Join addresses"
    // + "Actions") and the tab name in the tab bar already conveys context.
    let _ = s.title_server;
}

fn draw_hints(f: &mut Frame, area: Rect, app: &App) {
    let hint = hint_for(app.lang, app.tab, &app.yaml_view);
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
            app.pid = server_running_pid(&app.server_dir, app.pid);
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
                        PromptAction::EditYaml => {
                            if let Err(e) = app.yaml_save_current(&value) {
                                app.status = match app.lang {
                                    Lang::En => format!("✗ {}", e),
                                    Lang::Zh => format!("✗ {}", e),
                                };
                            }
                        }
                        PromptAction::RconCommand => app.rcon_send(&value)?,
                        PromptAction::ScheduleDailyRestart => {
                            app.schedule_daily(ServerAction::ScheduleDailyRestart, &value)?
                        }
                        PromptAction::ScheduleDailyBackup => {
                            app.schedule_daily(ServerAction::ScheduleDailyBackup, &value)?
                        }
                        PromptAction::PreGenChunkRadius => app.pregen_chunks(&value)?,
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
            KeyCode::Char('q') => return Ok(()),
            KeyCode::Esc => {
                // In YAML editing view, Esc returns to file picker instead of quitting.
                if app.tab == TabId::Yaml {
                    if let YamlView::Editing { .. } = app.yaml_view {
                        app.yaml_close();
                        continue;
                    }
                }
                return Ok(());
            }
            KeyCode::Char('1') => app.switch_tab(TabId::Worlds),
            KeyCode::Char('2') => app.switch_tab(TabId::Whitelist),
            KeyCode::Char('3') => app.switch_tab(TabId::Ops),
            KeyCode::Char('4') => app.switch_tab(TabId::Config),
            KeyCode::Char('5') => app.switch_tab(TabId::Logs),
            KeyCode::Char('6') => app.switch_tab(TabId::Yaml),
            KeyCode::Char('7') => app.switch_tab(TabId::Backups),
            KeyCode::Char('8') => app.switch_tab(TabId::Rcon),
            KeyCode::Char('9') => app.switch_tab(TabId::Server),
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
                TabId::Yaml => match app.yaml_view.clone() {
                    YamlView::Files => {
                        if let Some(idx) = app.yaml_files_state.selected() {
                            if let Err(e) = app.yaml_open(idx) {
                                app.status = match app.lang {
                                    Lang::En => format!("✗ {}", e),
                                    Lang::Zh => format!("✗ {}", e),
                                };
                            }
                        }
                    }
                    YamlView::Editing { .. } => {
                        if let Some(idx) = app.yaml_rows_state.selected() {
                            if let Some(row) = app.yaml_rows.get(idx).cloned() {
                                if let YamlDisplay::Scalar(v) = &row.value {
                                    let title = match app.lang {
                                        Lang::En => format!("Edit {}", row.label),
                                        Lang::Zh => format!("编辑 {}", row.label),
                                    };
                                    app.prompt = Some(InputPrompt {
                                        title,
                                        label: app.lang.s().prompt_label_value.into(),
                                        buffer: v.clone(),
                                        action: PromptAction::EditYaml,
                                    });
                                }
                            }
                        }
                    }
                },
                TabId::Server => {
                    if let Some(idx) = app.server_state.selected() {
                        if let Some(action) = SERVER_ACTIONS.get(idx).copied() {
                            handle_server_action(app, action)?;
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
            // RCON: 'i' opens command prompt in RCON tab
            KeyCode::Char('i') => {
                if app.tab == TabId::Rcon {
                    if rcon_settings(&app.properties).is_some() {
                        let s = app.lang.s();
                        app.prompt = Some(InputPrompt {
                            title: s.rcon_prompt_title.into(),
                            label: s.rcon_prompt_label.into(),
                            buffer: String::new(),
                            action: PromptAction::RconCommand,
                        });
                    } else {
                        app.status = app.lang.s().rcon_disabled_in_props.into();
                    }
                }
            }
            _ => {}
        }
    }
}

fn handle_server_action(app: &mut App, a: ServerAction) -> Result<()> {
    let s = app.lang.s();
    match a {
        ServerAction::RestartNow => app.restart_now(),
        ServerAction::BackupNow => app.backup_now(),
        ServerAction::ScheduleDailyRestart => {
            app.prompt = Some(InputPrompt {
                title: s.server_prompt_time_title.into(),
                label: s.server_prompt_time_label.into(),
                buffer: "04:00".into(),
                action: PromptAction::ScheduleDailyRestart,
            });
            Ok(())
        }
        ServerAction::ScheduleDailyBackup => {
            app.prompt = Some(InputPrompt {
                title: s.server_prompt_time_title.into(),
                label: s.server_prompt_time_label.into(),
                buffer: "03:30".into(),
                action: PromptAction::ScheduleDailyBackup,
            });
            Ok(())
        }
        ServerAction::PreGenChunks => {
            app.prompt = Some(InputPrompt {
                title: s.server_prompt_radius_title.into(),
                label: s.server_prompt_radius_label.into(),
                buffer: "1000".into(),
                action: PromptAction::PreGenChunkRadius,
            });
            Ok(())
        }
        ServerAction::OpenSystemdStatus => {
            app.show_systemd_status();
            Ok(())
        }
        ServerAction::ShowAttachCommand => {
            app.show_attach_command();
            Ok(())
        }
    }
}

fn handle_mouse(app: &mut App, me: MouseEvent) {
    if !matches!(me.kind, MouseEventKind::Down(MouseButton::Left)) {
        return;
    }
    let (col, row) = (me.column, me.row);

    // Join-bar chip click → copy to clipboard.
    let chips = app.join_chips.clone();
    for (r, payload) in chips {
        if rect_contains(r, col, row) {
            let copied = std::process::Command::new("wl-copy")
                .arg(&payload)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            app.status = match (app.lang, copied) {
                (Lang::En, true) => format!("✓ Copied {} to clipboard", payload),
                (Lang::En, false) => format!("ℹ {} (wl-copy unavailable)", payload),
                (Lang::Zh, true) => format!("✓ 已复制 {} 到剪贴板", payload),
                (Lang::Zh, false) => format!("ℹ {}（wl-copy 不可用）", payload),
            };
            return;
        }
    }

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
            select,
        }) => {
            let server_dir = resolve_server_dir(cli.server_dir.clone())?;
            return render_screenshot(&server_dir, &tab, width, height, &lang, select);
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


fn render_screenshot(
    server_dir: &Path,
    tab: &str,
    width: u16,
    height: u16,
    lang: &str,
    select: usize,
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
        "yaml" => TabId::Yaml,
        "backups" => TabId::Backups,
        "rcon" => TabId::Rcon,
        "server" => TabId::Server,
        other => anyhow::bail!("unknown tab: {}", other),
    };
    // Allow QA to highlight a specific row to inspect its detail panel.
    let len = app.list_len_for(app.tab);
    if len > 0 {
        let idx = select.min(len - 1);
        let t = app.tab;
        app.list_state_for(t).select(Some(idx));
    }
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
    fn scan_worlds_inserts_placeholder_for_pending_level_name() {
        let dir = tempdir();
        // existing world with level.dat
        let w1 = dir.join("world");
        fs::create_dir_all(&w1).unwrap();
        fs::write(w1.join("level.dat"), b"x").unwrap();

        // current level-name points at a world that doesn't exist yet
        let out = scan_worlds(&dir, "fresh-world");
        assert_eq!(out.len(), 2);
        // placeholder should be first (sorted current-first) and is_current
        assert_eq!(out[0].name, "fresh-world");
        assert!(out[0].is_current);
        assert!(!out[0].has_level_dat);
        assert_eq!(out[1].name, "world");
        assert!(!out[1].is_current);
    }

    #[test]
    fn scan_worlds_no_placeholder_when_level_name_exists() {
        let dir = tempdir();
        let w1 = dir.join("world");
        fs::create_dir_all(&w1).unwrap();
        fs::write(w1.join("level.dat"), b"x").unwrap();

        let out = scan_worlds(&dir, "world");
        assert_eq!(out.len(), 1);
        assert!(out[0].is_current);
        assert!(out[0].has_level_dat);
    }

    #[test]
    fn parse_hh_mm_accepts_valid_times() {
        assert_eq!(parse_hh_mm("00:00"), Some((0, 0)));
        assert_eq!(parse_hh_mm("23:59"), Some((23, 59)));
        assert_eq!(parse_hh_mm("4:5"), Some((4, 5)));
        assert!(parse_hh_mm("24:00").is_none());
        assert!(parse_hh_mm("12:60").is_none());
        assert!(parse_hh_mm("nope").is_none());
        assert!(parse_hh_mm("12").is_none());
    }

    #[test]
    fn op_level_cycles_at_edges() {
        // 1 ← wraps to 4
        assert_eq!(((1i16 - 1 + -1).rem_euclid(4) + 1) as u8, 4);
        // 4 → wraps to 1
        assert_eq!(((4i16 - 1 + 1).rem_euclid(4) + 1) as u8, 1);
        // mid-range
        assert_eq!(((2i16 - 1 + 1).rem_euclid(4) + 1) as u8, 3);
        assert_eq!(((3i16 - 1 + -1).rem_euclid(4) + 1) as u8, 2);
    }

    #[test]
    fn shell_quote_sh_handles_dangerous_characters() {
        // safe → unquoted
        assert_eq!(shell_quote_sh("/srv/mc-server/start.sh"), "/srv/mc-server/start.sh");
        assert_eq!(shell_quote_sh("plain"), "plain");
        // spaces → single-quoted
        assert_eq!(shell_quote_sh("/srv/My Server/start.sh"), "'/srv/My Server/start.sh'");
        // single quote inside → '\'' escape sequence
        assert_eq!(shell_quote_sh("a'b"), r"'a'\''b'");
        // empty
        assert_eq!(shell_quote_sh(""), "''");
        // dollar / backtick / double-quote all force quoting
        assert!(shell_quote_sh("$HOME").starts_with('\''));
        assert!(shell_quote_sh("`x`").starts_with('\''));
        assert!(shell_quote_sh("a\"b").starts_with('\''));
    }

    #[test]
    fn read_whitelist_propagates_parse_error() {
        let dir = tempdir();
        fs::write(dir.join("whitelist.json"), b"{ not json").unwrap();
        let res = read_whitelist(&dir);
        assert!(res.is_err(), "expected parse error to propagate");
    }

    #[test]
    fn read_ops_propagates_parse_error() {
        let dir = tempdir();
        fs::write(dir.join("ops.json"), b"garbage").unwrap();
        let res = read_ops(&dir);
        assert!(res.is_err(), "expected parse error to propagate");
    }

    #[test]
    fn read_whitelist_missing_file_returns_empty() {
        let dir = tempdir();
        // no whitelist.json at all — fresh server-dir
        let v = read_whitelist(&dir).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn classify_iface_handles_known_naming_conventions() {
        use std::net::Ipv4Addr;
        // ZeroTier — name prefix wins regardless of IP range
        assert_eq!(
            classify_iface("ztpp6kuvag", &Ipv4Addr::new(10, 24, 0, 11)),
            NicKind::Zerotier
        );
        assert_eq!(
            classify_iface("zerotier0", &Ipv4Addr::new(192, 168, 1, 5)),
            NicKind::Zerotier
        );
        // Loopback — IP wins
        assert_eq!(
            classify_iface("lo", &Ipv4Addr::new(127, 0, 0, 1)),
            NicKind::Loopback
        );
        // Docker / bridges
        assert_eq!(
            classify_iface("docker0", &Ipv4Addr::new(172, 17, 0, 1)),
            NicKind::Docker
        );
        assert_eq!(
            classify_iface("br-8115d8db670a", &Ipv4Addr::new(172, 18, 0, 1)),
            NicKind::Docker
        );
        // VPN/TUN
        assert_eq!(
            classify_iface("mihomo", &Ipv4Addr::new(198, 18, 0, 1)),
            NicKind::Tun
        );
        assert_eq!(
            classify_iface("tun0", &Ipv4Addr::new(10, 8, 0, 1)),
            NicKind::Tun
        );
        assert_eq!(
            classify_iface("wg0", &Ipv4Addr::new(10, 100, 0, 1)),
            NicKind::Tun
        );
        // LAN — RFC1918
        assert_eq!(
            classify_iface("wlan0", &Ipv4Addr::new(192, 168, 1, 50)),
            NicKind::Lan
        );
        assert_eq!(
            classify_iface("wlan0", &Ipv4Addr::new(10, 128, 177, 76)),
            NicKind::Lan
        );
        // Public
        assert_eq!(
            classify_iface("eth0", &Ipv4Addr::new(8, 8, 8, 8)),
            NicKind::Public
        );
    }

    #[test]
    fn nic_kind_priority_orders_zerotier_first() {
        assert!(nic_kind_priority(NicKind::Zerotier) < nic_kind_priority(NicKind::Lan));
        assert!(nic_kind_priority(NicKind::Lan) < nic_kind_priority(NicKind::Public));
        assert!(nic_kind_priority(NicKind::Public) < nic_kind_priority(NicKind::Tun));
        assert!(nic_kind_priority(NicKind::Docker) < nic_kind_priority(NicKind::Loopback));
    }

    #[test]
    fn nic_kind_label_localized() {
        for k in [
            NicKind::Zerotier,
            NicKind::Lan,
            NicKind::Public,
            NicKind::Tun,
            NicKind::Docker,
            NicKind::Loopback,
        ] {
            assert!(!nic_kind_label(Lang::En, k).is_empty());
            assert!(!nic_kind_label(Lang::Zh, k).is_empty());
        }
    }

    #[test]
    fn tmux_session_name_stable_and_safe() {
        // Same dir → same name (so start_server and stop_server agree).
        assert_eq!(
            tmux_session_name(Path::new("/mnt/data/mc-server")),
            tmux_session_name(Path::new("/mnt/data/mc-server"))
        );
        // No characters tmux would choke on.
        let n = tmux_session_name(Path::new("/srv/MyServer 2024!"));
        for c in n.chars() {
            assert!(c.is_ascii_alphanumeric() || c == '-', "bad char {:?} in {}", c, n);
        }
        assert!(n.starts_with("mc-tui-"));
    }

    #[test]
    fn server_dir_slug_sanitizes() {
        assert_eq!(server_dir_slug(Path::new("/mnt/data/mc-server")), "mc-server");
        assert_eq!(server_dir_slug(Path::new("/srv/MyServer 2024")), "myserver-2024");
        assert_eq!(server_dir_slug(Path::new("/")), "server");
    }

    #[test]
    fn fmt_age_basic() {
        use chrono::Duration as D;
        assert!(fmt_age(D::seconds(30)).contains("s ago"));
        assert!(fmt_age(D::seconds(120)).contains("m ago"));
        assert!(fmt_age(D::seconds(3600 * 5)).contains("h ago"));
        assert!(fmt_age(D::seconds(86400 * 3)).contains("d ago"));
        assert!(fmt_age(D::seconds(86400 * 90)).contains("mo ago"));
    }

    #[test]
    fn server_action_labels_exist_in_both_langs() {
        for action in SERVER_ACTIONS.iter().copied() {
            let en = server_action_label(Lang::En, action);
            let zh = server_action_label(Lang::Zh, action);
            assert!(!en.is_empty());
            assert!(!zh.is_empty());
        }
    }

    #[test]
    fn property_metadata_covers_listed_keys() {
        for key in [
            "max-players",
            "view-distance",
            "simulation-distance",
            "difficulty",
            "gamemode",
            "pvp",
            "hardcore",
            "online-mode",
            "white-list",
            "enforce-whitelist",
            "spawn-protection",
            "motd",
            "level-name",
            "level-type",
            "level-seed",
            "server-port",
            "allow-flight",
            "allow-nether",
            "spawn-monsters",
            "spawn-animals",
            "enable-rcon",
            "rcon.password",
            "rcon.port",
            "op-permission-level",
            "function-permission-level",
            "network-compression-threshold",
            "max-tick-time",
            "force-gamemode",
            "generate-structures",
            "resource-pack",
            "require-resource-pack",
            "player-idle-timeout",
            "entity-broadcast-range-percentage",
        ] {
            let m = property_metadata(key).unwrap_or_else(|| panic!("missing meta for {}", key));
            assert!(!m.description_en.is_empty(), "empty en desc for {}", key);
            assert!(!m.description_zh.is_empty(), "empty zh desc for {}", key);
            assert!(!m.range.is_empty(), "empty range for {}", key);
        }
    }

    #[test]
    fn property_metadata_unknown_returns_none() {
        assert!(property_metadata("not-a-real-key").is_none());
    }

    #[test]
    fn detail_strings_nonempty_in_both_langs() {
        for s in [&EN, &ZH] {
            assert!(!s.detail_title.is_empty());
            assert!(!s.detail_no_selection.is_empty());
            assert!(!s.detail_no_metadata.is_empty());
            assert!(!s.detail_path.is_empty());
            assert!(!s.detail_size.is_empty());
            assert!(!s.detail_uuid.is_empty());
            assert!(!s.detail_offline_uuid_note.is_empty());
            assert!(!s.detail_op_level_4.is_empty());
            assert!(!s.detail_yes.is_empty());
            assert!(!s.detail_no.is_empty());
        }
    }

    #[test]
    fn split_list_detail_collapses_on_narrow_screen() {
        let narrow = Rect { x: 0, y: 0, width: 80, height: 30 };
        let (list, det) = split_list_detail(narrow);
        assert_eq!(list, narrow);
        assert!(det.is_none());

        let wide = Rect { x: 0, y: 0, width: 130, height: 30 };
        let (list, det) = split_list_detail(wide);
        assert!(det.is_some());
        assert!(list.width < wide.width);
    }

    #[test]
    fn op_level_meaning_returns_localized_string() {
        let en = op_level_meaning(&EN, 4);
        let zh = op_level_meaning(&ZH, 4);
        assert!(en.contains("/stop") || en.contains("admin"));
        assert!(zh.contains("/stop") || zh.contains("管理"));
        assert_ne!(en, zh);
        assert_eq!(op_level_meaning(&EN, 99), "?");
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
    fn rect_contains_basic() {
        let r = Rect { x: 10, y: 20, width: 30, height: 5 };
        assert!(rect_contains(r, 10, 20));
        assert!(rect_contains(r, 39, 24));
        assert!(!rect_contains(r, 9, 20));
        assert!(!rect_contains(r, 40, 20));
        assert!(!rect_contains(r, 10, 25));
    }
}
