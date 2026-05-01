//! mc-tui — a TUI manager for a local Minecraft Paper/Purpur server.

mod cli;
mod data;
mod i18n;
mod natfrp;
mod sys;
mod ui;
use cli::*;
use data::*;
use i18n::*;
use sys::*;
use ui::ui;

use std::{
    collections::HashMap,
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
pub enum TabId {
    Worlds,
    /// v0.11 — replaces the separate `Whitelist` and `Ops` tabs. Single roster
    /// view: every name surfaces once, with toggleable whitelist + op flags.
    Players,
    Config,
    Logs,
    Yaml,
    Backups,
    Server,
    SakuraFrp,
}

const TABS: &[(TabId, &str)] = &[
    (TabId::Worlds, "Worlds"),
    (TabId::Players, "Players"),
    (TabId::Config, "Config"),
    (TabId::Logs, "Logs"),
    (TabId::Yaml, "YAML"),
    (TabId::Backups, "Backups"),
    (TabId::Server, "Server"),
    (TabId::SakuraFrp, "SakuraFrp"),
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
    // v0.8 — surface the SakuraFrp tunnel address in the join-bar.
    SetSakuraFrpAddress,
    // v0.9 — manage the SakuraFrp launcher Docker container directly.
    // Container name persisted in state.toml (default `natfrp-service`).
    SetSakuraFrpContainer,
    SakuraFrpStart,
    SakuraFrpStop,
    SakuraFrpRestart,
    SakuraFrpShowLogs,
}

const SERVER_ACTIONS: &[ServerAction] = &[
    ServerAction::RestartNow,
    ServerAction::BackupNow,
    ServerAction::ShowAttachCommand,
    ServerAction::ScheduleDailyRestart,
    ServerAction::ScheduleDailyBackup,
    ServerAction::PreGenChunks,
    ServerAction::OpenSystemdStatus,
    ServerAction::SetSakuraFrpAddress,
    ServerAction::SakuraFrpStart,
    ServerAction::SakuraFrpStop,
    ServerAction::SakuraFrpRestart,
    ServerAction::SakuraFrpShowLogs,
    ServerAction::SetSakuraFrpContainer,
];

/// Default Docker container name for the SakuraFrp launcher image. The user
/// can override it in state.toml or via the Server tab prompt; this is what
/// the official launcher install ends up named when run with `docker run …
/// --name natfrp-service natfrp.com/launcher`.
const DEFAULT_SAKURAFRP_CONTAINER: &str = "natfrp-service";

/// YAML tab toggles between file picker and a flat row editor for one file.
#[derive(Debug, Clone)]
enum YamlView {
    Files,
    Editing { file_idx: usize },
}

#[derive(Debug, Clone)]
struct InputPrompt {
    pub title: String,
    pub label: String,
    pub buffer: String,
    pub action: PromptAction,
}

#[derive(Debug, Clone)]
enum PromptAction {
    AddWhitelist,
    EditConfig(String),
    NewWorld,
    ChangeServerDir,
    EditYaml,
    ScheduleDailyRestart,
    ScheduleDailyBackup,
    PreGenChunkRadius,
    SetSakuraFrpAddress,
    SetSakuraFrpContainer,
    /// v0.10 — input the SakuraFrp API token. Persisted to natfrp.token (0600).
    SetNatfrpToken,
}

struct App {
    pub server_dir: PathBuf,
    pub properties: Vec<(String, String)>,
    pub worlds: Vec<WorldEntry>,
    pub whitelist: Vec<WhitelistEntry>,
    pub ops: Vec<OpEntry>,
    /// True if the on-disk whitelist.json failed to parse last refresh.
    /// While set, mc-tui refuses to write to whitelist.json (would clobber the
    /// real, hand-edited file with our empty in-memory copy).
    pub whitelist_corrupt: bool,
    /// Same idea for ops.json.
    pub ops_corrupt: bool,
    pub pid: Option<u32>,

    pub tab: TabId,
    pub worlds_state: ListState,
    pub players: Vec<PlayerEntry>,
    pub players_state: ListState,
    /// Mirrors `white-list` in `server.properties`. The Players tab top row
    /// toggles this; when false, the whitelist column is hidden and Enter
    /// becomes a no-op (op toggles still work).
    pub whitelist_enabled: bool,
    pub config_state: ListState,

    // v0.5 — YAML
    pub yaml_files: Vec<PathBuf>,
    pub yaml_files_state: ListState,
    pub yaml_view: YamlView,
    pub yaml_root: Option<serde_yaml::Value>,
    pub yaml_rows: Vec<YamlRow>,
    pub yaml_rows_state: ListState,

    // v0.5 — Backups
    pub backups: Vec<BackupEntry>,
    pub backups_state: ListState,

    // v0.6 — Server ops
    pub server_state: ListState,

    // v0.8 — SakuraFrp tunnel public address (display-only). Persisted in
    // state.toml. The actual frpc service is managed by the SakuraFrp client.
    pub sakurafrp_address: Option<String>,
    // v0.9 — Docker container name for the SakuraFrp launcher + cached
    // last-probed state.
    pub sakurafrp_container: String,
    pub sakurafrp_docker: SakuraFrpDocker,

    // v0.10 — SakuraFrp API integration. Token is read from
    // ~/.config/mc-tui/natfrp.token (0600); other fields are populated by
    // refresh_natfrp() on demand (first tab visit + user-triggered refresh).
    // We never call the API from refresh_all() — would block the redraw loop.
    pub natfrp_token: Option<String>,
    pub natfrp_user: Option<natfrp::UserInfo>,
    pub natfrp_tunnels: Vec<natfrp::Tunnel>,
    pub natfrp_nodes: HashMap<u64, natfrp::Node>,
    pub natfrp_last_error: Option<String>,
    pub natfrp_state: ListState,
    /// True after the first refresh_natfrp() in this session — gates auto-load
    /// on first tab visit so we don't keep re-firing requests.
    pub natfrp_loaded: bool,

    // v0.12 — Sparkle/mihomo presence indicator. Pure cache; refresh_all() walks
    // proc table once per refresh tick. Surfaced on the SakuraFrp tab as a dim
    // warning line, since the user has explicitly said the auto-kill behavior
    // is off-limits.
    pub mihomo_running: bool,

    pub status: String,
    pub prompt: Option<InputPrompt>,

    // Mouse hit-testing rects, populated each frame inside `ui()`.
    pub tabs_rect: Rect,
    pub list_rect: Rect,
    /// Each entry is the screen rect of a join-address chip and the literal
    /// `ip:port` to copy on click.
    pub join_chips: Vec<(Rect, String)>,

    pub lang: Lang,
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
            players: Vec::new(),
            players_state: ListState::default(),
            whitelist_enabled: false,
            config_state: ListState::default(),
            yaml_files: Vec::new(),
            yaml_files_state: ListState::default(),
            yaml_view: YamlView::Files,
            yaml_root: None,
            yaml_rows: Vec::new(),
            yaml_rows_state: ListState::default(),
            backups: Vec::new(),
            backups_state: ListState::default(),
            server_state: ListState::default(),
            // Pull persisted SakuraFrp address so the join-bar shows the
            // public entry from the start of the session.
            sakurafrp_address: read_persisted_state().sakurafrp_address,
            sakurafrp_container: read_persisted_state()
                .sakurafrp_container
                .unwrap_or_else(|| DEFAULT_SAKURAFRP_CONTAINER.to_string()),
            sakurafrp_docker: SakuraFrpDocker::default(),
            natfrp_token: read_natfrp_token(),
            natfrp_user: None,
            natfrp_tunnels: Vec::new(),
            natfrp_nodes: HashMap::new(),
            natfrp_last_error: None,
            natfrp_state: ListState::default(),
            natfrp_loaded: false,
            mihomo_running: false,
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
        if !app.players.is_empty() {
            app.players_state.select(Some(0));
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
        self.sakurafrp_docker = detect_sakurafrp_docker(&self.sakurafrp_container);
        self.mihomo_running = mihomo_running();
        self.whitelist_enabled = get_property(&self.properties, "white-list")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        self.players = scan_players(
            &self.server_dir,
            &cur,
            &self.whitelist,
            &self.ops,
        );
        // Keep selection in range after the merge.
        if let Some(i) = self.players_state.selected() {
            if i >= self.players.len() {
                if self.players.is_empty() {
                    self.players_state.select(None);
                } else {
                    self.players_state.select(Some(self.players.len() - 1));
                }
            }
        } else if !self.players.is_empty() {
            self.players_state.select(Some(0));
        }
    }

    fn list_state_for(&mut self, tab: TabId) -> &mut ListState {
        match tab {
            TabId::Worlds => &mut self.worlds_state,
            TabId::Players => &mut self.players_state,
            TabId::Config => &mut self.config_state,
            TabId::Logs => &mut self.worlds_state,
            TabId::Yaml => match self.yaml_view {
                YamlView::Files => &mut self.yaml_files_state,
                YamlView::Editing { .. } => &mut self.yaml_rows_state,
            },
            TabId::Backups => &mut self.backups_state,
            TabId::Server => &mut self.server_state,
            TabId::SakuraFrp => &mut self.natfrp_state,
        }
    }

    fn list_len_for(&self, tab: TabId) -> usize {
        match tab {
            TabId::Worlds => self.worlds.len(),
            TabId::Players => self.players.len(),
            TabId::Config => self.properties.len(),
            TabId::Logs => 0,
            TabId::Yaml => match self.yaml_view {
                YamlView::Files => self.yaml_files.len(),
                YamlView::Editing { .. } => self.yaml_rows.len(),
            },
            TabId::Backups => self.backups.len(),
            TabId::Server => SERVER_ACTIONS.len(),
            TabId::SakuraFrp => self.natfrp_tunnels.len(),
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
        // Lazy-load SakuraFrp data the first time the user opens that tab.
        // Subsequent refreshes are user-driven via 'r' to keep the network
        // off the hot redraw path.
        if tab == TabId::SakuraFrp && !self.natfrp_loaded && self.natfrp_token.is_some() {
            self.refresh_natfrp();
        }
    }

    /// Hits api.natfrp.com for /user/info + /tunnels + /nodes. Blocking — only
    /// call from user-initiated paths (tab open, `r`). Errors land in
    /// `natfrp_last_error` already translated to the active language; partial
    /// success is allowed (e.g. user info loads but tunnels fail) so the UI
    /// shows what it can. We surface only the *first* error — a 401 triggered
    /// by user_info will also fail tunnels and nodes; chaining all three would
    /// just spam the status bar.
    fn refresh_natfrp(&mut self) {
        let Some(token) = self.natfrp_token.clone() else {
            self.natfrp_last_error = Some(self.lang.s().sf_user_no_token.to_string());
            return;
        };
        self.status = self.lang.s().sf_refreshing.to_string();
        self.natfrp_loaded = true;
        let client = natfrp::Client::new(token);

        let mut first_err: Option<natfrp::NatfrpError> = None;
        match client.user_info() {
            Ok(u) => self.natfrp_user = Some(u),
            Err(e) => first_err = first_err.or(Some(e)),
        }
        match client.tunnels() {
            Ok(ts) => {
                self.natfrp_tunnels = ts;
                if self.natfrp_state.selected().is_none() && !self.natfrp_tunnels.is_empty() {
                    self.natfrp_state.select(Some(0));
                }
            }
            Err(e) => first_err = first_err.or(Some(e)),
        }
        // Only fetch /nodes once per session — the list rarely changes.
        if self.natfrp_nodes.is_empty() {
            match client.nodes() {
                Ok(n) => self.natfrp_nodes = n,
                Err(e) => first_err = first_err.or(Some(e)),
            }
        }

        match first_err {
            None => {
                self.natfrp_last_error = None;
                self.status = self.lang.s().refreshed.to_string();
            }
            Some(e) => {
                let msg = translate_natfrp_error(self.lang, &e);
                self.natfrp_last_error = Some(msg.clone());
                self.status = msg;
            }
        }
    }

    /// v0.12 — fire `xdg-open` on the SakuraFrp access-key page so the user
    /// doesn't have to remember the URL or click around the dashboard. Falls
    /// back to copying the URL via wl-copy when xdg-open isn't available
    /// (headless / no portal). The deep link lands on the access-key tab.
    fn open_natfrp_dashboard(&mut self) {
        const URL: &str = "https://www.natfrp.com/user/edit/auth#info-key";
        let opened = std::process::Command::new("xdg-open")
            .arg(URL)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if opened {
            self.status = match self.lang {
                Lang::En => format!("✓ Opened {} in your browser.", URL),
                Lang::Zh => format!("✓ 已在浏览器打开 {}。", URL),
            };
            return;
        }
        // Best-effort clipboard fallback. Tell the user even when wl-copy
        // succeeds — they may have expected a browser to pop.
        let copied = std::process::Command::new("wl-copy")
            .arg(URL)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        self.status = match (self.lang, copied) {
            (Lang::En, true) => {
                format!("ℹ xdg-open unavailable; copied {} to clipboard.", URL)
            }
            (Lang::En, false) => {
                format!("ℹ Open this URL manually: {}", URL)
            }
            (Lang::Zh, true) => {
                format!("ℹ xdg-open 不可用；已把 {} 复制到剪贴板。", URL)
            }
            (Lang::Zh, false) => {
                format!("ℹ 请手动打开此 URL：{}", URL)
            }
        };
    }

    /// True when launcher container is up but the user has at least one tunnel
    /// the API knows about — the (rare) "everything is configured but nothing
    /// is moving" state we don't need to nag about. False otherwise → show the
    /// `sf_launcher_hint` so the user knows where to go next. Treats Unknown
    /// as "no docker / no opinion" → no hint, since the user might be on a
    /// host where docker isn't the launcher transport.
    fn launcher_hint_applicable(&self) -> bool {
        if self.natfrp_tunnels.is_empty() {
            return false;
        }
        matches!(
            self.sakurafrp_docker.state,
            data::DockerState::Stopped | data::DockerState::Missing
        )
    }

    fn set_natfrp_token(&mut self, token: &str) -> Result<()> {
        let token = token.trim().to_string();
        if token.is_empty() {
            // Treat empty input as "clear the token".
            let path = sys::natfrp_token_path();
            let _ = std::fs::remove_file(&path);
            self.natfrp_token = None;
            self.natfrp_user = None;
            self.natfrp_tunnels.clear();
            self.natfrp_loaded = false;
            self.natfrp_last_error = None;
            self.status = match self.lang {
                Lang::En => "✓ SakuraFrp token cleared.".into(),
                Lang::Zh => "✓ 已清除 SakuraFrp token。".into(),
            };
            return Ok(());
        }
        write_natfrp_token(&token).context("write natfrp.token")?;
        self.natfrp_token = Some(token);
        self.status = self.lang.s().sf_token_saved.to_string();
        // Force a full refresh now that we have credentials.
        self.natfrp_loaded = false;
        self.natfrp_user = None;
        self.natfrp_tunnels.clear();
        self.refresh_natfrp();
        Ok(())
    }

    /// Public address derived from API state — first online tcp tunnel wins.
    /// Falls back to the user-set `sakurafrp_address` from state.toml so the
    /// pre-v0.10 manual workflow still works when the user has no token.
    pub fn effective_sakurafrp_address(&self) -> Option<String> {
        for t in &self.natfrp_tunnels {
            if t.kind == "tcp" {
                if let Some(addr) = natfrp::public_address(t, &self.natfrp_nodes) {
                    return Some(addr);
                }
            }
        }
        self.sakurafrp_address.clone()
    }

    fn copy_selected_tunnel_address(&mut self) {
        let s = self.lang.s();
        let Some(idx) = self.natfrp_state.selected() else {
            self.status = s.sf_no_selected_tunnel.to_string();
            return;
        };
        let Some(t) = self.natfrp_tunnels.get(idx) else {
            self.status = s.sf_no_selected_tunnel.to_string();
            return;
        };
        let Some(addr) = natfrp::public_address(t, &self.natfrp_nodes) else {
            self.status = match self.lang {
                Lang::En => format!("✗ Cannot resolve public address for tunnel #{}", t.id),
                Lang::Zh => format!("✗ 无法解析隧道 #{} 的公网地址", t.id),
            };
            return;
        };
        let copied = std::process::Command::new("wl-copy")
            .arg(&addr)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        self.status = match (self.lang, copied) {
            (Lang::En, true) => format!("✓ Copied {} to clipboard", addr),
            (Lang::En, false) => format!("ℹ {} (wl-copy unavailable)", addr),
            (Lang::Zh, true) => format!("✓ 已复制 {} 到剪贴板", addr),
            (Lang::Zh, false) => format!("ℹ {}（wl-copy 不可用）", addr),
        };
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

    /// Remove `name` from `whitelist.json`. Caller decides whether the player
    /// is or isn't actually present; we silently no-op when they're not.
    fn remove_from_whitelist_by_name(&mut self, name: &str) -> Result<()> {
        if self.whitelist_corrupt {
            self.status = match self.lang {
                Lang::En => "✗ whitelist.json is corrupt — fix it before editing.".into(),
                Lang::Zh => "✗ whitelist.json 损坏，请先修复后再编辑。".into(),
            };
            return Ok(());
        }
        let before = self.whitelist.len();
        self.whitelist
            .retain(|e| !e.name.eq_ignore_ascii_case(name));
        if self.whitelist.len() != before {
            write_whitelist(&self.server_dir, &self.whitelist)?;
            self.status = fmt_whitelist_removed(self.lang, name);
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

    fn remove_op_by_name(&mut self, name: &str) -> Result<()> {
        if self.ops_corrupt {
            self.status = match self.lang {
                Lang::En => "✗ ops.json is corrupt — fix it before editing.".into(),
                Lang::Zh => "✗ ops.json 损坏，请先修复后再编辑。".into(),
            };
            return Ok(());
        }
        let before = self.ops.len();
        self.ops.retain(|e| !e.name.eq_ignore_ascii_case(name));
        if self.ops.len() != before {
            write_ops(&self.server_dir, &self.ops)?;
            self.status = fmt_op_removed(self.lang, name);
        }
        Ok(())
    }

    fn set_op_level_by_name(&mut self, name: &str, level: u8) -> Result<()> {
        if self.ops_corrupt {
            self.status = match self.lang {
                Lang::En => "✗ ops.json is corrupt — fix it before editing.".into(),
                Lang::Zh => "✗ ops.json 损坏，请先修复后再编辑。".into(),
            };
            return Ok(());
        }
        if let Some(o) = self.ops.iter_mut().find(|e| e.name.eq_ignore_ascii_case(name)) {
            o.level = level;
            write_ops(&self.server_dir, &self.ops)?;
            self.status = fmt_op_level_changed(self.lang, name, level);
        }
        Ok(())
    }

    /// v0.11 — Players tab actions, all keyed by `players_state` selection.

    fn selected_player_name(&self) -> Option<String> {
        let idx = self.players_state.selected()?;
        Some(self.players.get(idx)?.name.clone())
    }

    fn toggle_whitelist_for_selected(&mut self) -> Result<()> {
        if !self.whitelist_enabled {
            self.status = match self.lang {
                Lang::En => "✗ Whitelist is disabled — press `w` to enable it.".into(),
                Lang::Zh => "✗ 白名单未启用 — 按 w 开启。".into(),
            };
            return Ok(());
        }
        let Some(name) = self.selected_player_name() else { return Ok(()) };
        let in_wl = self
            .whitelist
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case(&name));
        if in_wl {
            self.remove_from_whitelist_by_name(&name)?;
        } else {
            self.add_whitelist(&name)?;
        }
        self.refresh_all();
        self.reselect_player(&name);
        Ok(())
    }

    fn toggle_op_for_selected(&mut self) -> Result<()> {
        let Some(name) = self.selected_player_name() else { return Ok(()) };
        let is_op = self.ops.iter().any(|e| e.name.eq_ignore_ascii_case(&name));
        if is_op {
            self.remove_op_by_name(&name)?;
        } else {
            self.add_op(&name)?;
        }
        self.refresh_all();
        self.reselect_player(&name);
        Ok(())
    }

    fn cycle_op_level_for_selected(&mut self, dir: i8) -> Result<()> {
        let Some(name) = self.selected_player_name() else { return Ok(()) };
        let cur = self
            .ops
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(&name))
            .map(|e| e.level as i16);
        let Some(cur) = cur else {
            // Not currently op — do nothing rather than silently elevating.
            self.status = match self.lang {
                Lang::En => "→ Player is not op (press `o` to op them first).".into(),
                Lang::Zh => "→ 玩家不是 OP（先按 o 提升）。".into(),
            };
            return Ok(());
        };
        let new = ((cur - 1 + dir as i16).rem_euclid(4) + 1) as u8;
        self.set_op_level_by_name(&name, new)?;
        self.refresh_all();
        self.reselect_player(&name);
        Ok(())
    }

    /// `d` — full purge from whitelist + ops. Useful on the Players tab as a
    /// single-keystroke "kick this person out of every roster".
    fn remove_selected_player(&mut self) -> Result<()> {
        let Some(name) = self.selected_player_name() else { return Ok(()) };
        self.remove_from_whitelist_by_name(&name)?;
        self.remove_op_by_name(&name)?;
        self.refresh_all();
        self.reselect_player(&name);
        Ok(())
    }

    /// `w` — flip `white-list` in `server.properties` and write the file.
    /// Note: Paper / Purpur honor live changes only after `/whitelist reload`
    /// or a server restart; we don't push a console command here.
    fn toggle_whitelist_enabled(&mut self) -> Result<()> {
        let new = !self.whitelist_enabled;
        set_property(&mut self.properties, "white-list", if new { "true" } else { "false" });
        write_properties(&self.server_dir.join("server.properties"), &self.properties)?;
        self.whitelist_enabled = new;
        self.status = match (self.lang, new) {
            (Lang::En, true) => "✓ Whitelist enabled (restart or /whitelist reload to apply).".into(),
            (Lang::En, false) => "✓ Whitelist disabled (restart or /whitelist reload to apply).".into(),
            (Lang::Zh, true) => "✓ 已启用白名单（重启或 /whitelist reload 后生效）。".into(),
            (Lang::Zh, false) => "✓ 已停用白名单（重启或 /whitelist reload 后生效）。".into(),
        };
        Ok(())
    }

    /// After a refresh that may have re-sorted `players`, keep the cursor on
    /// the row that the user just acted on (by name).
    fn reselect_player(&mut self, name: &str) {
        if let Some(i) = self
            .players
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(name))
        {
            self.players_state.select(Some(i));
        }
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
        let session = tmux_session_name(&self.server_dir);
        if which("tmux").is_none() || !tmux_session_alive(&session) {
            self.status = match self.lang {
                Lang::En => format!(
                    "✗ tmux session '{}' is not alive — start the server with S first.",
                    session
                ),
                Lang::Zh => format!("✗ tmux 会话 '{}' 不存在 — 请先按 S 启动服务器。", session),
            };
            return Ok(());
        }
        let level = self.current_level().to_string();
        let cmds = [
            format!("chunky world {}", level),
            "chunky center 0 0".to_string(),
            format!("chunky radius {}", radius),
            "chunky start".to_string(),
        ];
        use std::process::Command;
        for c in &cmds {
            let res = Command::new("tmux")
                .args(["send-keys", "-t", &session, c, "Enter"])
                .status();
            match res {
                Ok(s) if s.success() => {}
                Ok(s) => {
                    self.status = match self.lang {
                        Lang::En => format!("✗ tmux send-keys exited {:?}", s.code()),
                        Lang::Zh => format!("✗ tmux send-keys 退出码 {:?}", s.code()),
                    };
                    return Ok(());
                }
                Err(e) => {
                    self.status = match self.lang {
                        Lang::En => format!("✗ tmux send-keys: {}", e),
                        Lang::Zh => format!("✗ tmux send-keys 失败：{}", e),
                    };
                    return Ok(());
                }
            }
        }
        self.status = match self.lang {
            Lang::En => format!(
                "✓ Pre-gen sent (radius {}). Attach with `tmux attach -t {}` to watch.",
                radius, session
            ),
            Lang::Zh => format!(
                "✓ 已发送区块预加载（半径 {}）。`tmux attach -t {}` 查看进度。",
                radius, session
            ),
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

    // ---- v0.8: SakuraFrp helpers ----

    fn set_sakurafrp_address(&mut self, value: &str) -> Result<()> {
        let trimmed = value.trim();
        let mut state = read_persisted_state();
        if trimmed.is_empty() {
            self.sakurafrp_address = None;
            state.sakurafrp_address = None;
            self.status = match self.lang {
                Lang::En => "✓ Cleared SakuraFrp address.".into(),
                Lang::Zh => "✓ 已清除 SakuraFrp 地址。".into(),
            };
        } else {
            self.sakurafrp_address = Some(trimmed.to_string());
            state.sakurafrp_address = Some(trimmed.to_string());
            self.status = match self.lang {
                Lang::En => format!("✓ SakuraFrp address set: {}", trimmed),
                Lang::Zh => format!("✓ SakuraFrp 地址已保存：{}", trimmed),
            };
        }
        let _ = write_persisted_state(&state);
        Ok(())
    }

    fn set_sakurafrp_container(&mut self, value: &str) -> Result<()> {
        let trimmed = value.trim();
        let mut state = read_persisted_state();
        if trimmed.is_empty() {
            // Empty input = reset to default rather than clear (we always
            // need *something* to probe; default is the canonical install).
            self.sakurafrp_container = DEFAULT_SAKURAFRP_CONTAINER.to_string();
            state.sakurafrp_container = None;
            self.status = match self.lang {
                Lang::En => format!("✓ Reset SakuraFrp container to {}.", DEFAULT_SAKURAFRP_CONTAINER),
                Lang::Zh => format!("✓ SakuraFrp 容器名重置为 {}。", DEFAULT_SAKURAFRP_CONTAINER),
            };
        } else {
            self.sakurafrp_container = trimmed.to_string();
            state.sakurafrp_container = Some(trimmed.to_string());
            self.status = match self.lang {
                Lang::En => format!("✓ SakuraFrp container set: {}", trimmed),
                Lang::Zh => format!("✓ SakuraFrp 容器名已保存：{}", trimmed),
            };
        }
        let _ = write_persisted_state(&state);
        // Re-probe immediately so the join-bar marker updates.
        self.sakurafrp_docker = detect_sakurafrp_docker(&self.sakurafrp_container);
        Ok(())
    }

    fn run_frp_lifecycle(&mut self, verb: &str) {
        let container = self.sakurafrp_container.clone();
        match docker_lifecycle(verb, &container) {
            Ok(_) => {
                self.status = match self.lang {
                    Lang::En => format!("✓ docker {} {}", verb, container),
                    Lang::Zh => format!("✓ docker {} {} 完成", verb, container),
                };
            }
            Err(e) => {
                self.status = match self.lang {
                    Lang::En => format!("✗ docker {} failed: {}", verb, e),
                    Lang::Zh => format!("✗ docker {} 失败：{}", verb, e),
                };
            }
        }
        // Re-probe so the marker reflects the new state.
        self.sakurafrp_docker = detect_sakurafrp_docker(&self.sakurafrp_container);
    }

    fn show_frp_logs_command(&mut self) {
        let cmd = format!("docker logs -f --tail 50 {}", self.sakurafrp_container);
        let copied = std::process::Command::new("wl-copy")
            .arg(&cmd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|st| st.success())
            .unwrap_or(false);
        self.status = match (self.lang, copied) {
            (Lang::En, true) => format!("ℹ Copied to clipboard: {}", cmd),
            (Lang::En, false) => format!("ℹ {} (wl-copy unavailable)", cmd),
            (Lang::Zh, true) => format!("ℹ 已复制到剪贴板：{}", cmd),
            (Lang::Zh, false) => format!("ℹ {}（wl-copy 不可用）", cmd),
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

        self.backups_state = ListState::default();
        self.server_state = ListState::default();
        self.server_state.select(Some(0));

        self.refresh_all();

        self.worlds_state = ListState::default();
        if !self.worlds.is_empty() {
            self.worlds_state.select(Some(0));
        }
        self.players_state = ListState::default();
        if !self.players.is_empty() {
            self.players_state.select(Some(0));
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
                        PromptAction::ScheduleDailyRestart => {
                            app.schedule_daily(ServerAction::ScheduleDailyRestart, &value)?
                        }
                        PromptAction::ScheduleDailyBackup => {
                            app.schedule_daily(ServerAction::ScheduleDailyBackup, &value)?
                        }
                        PromptAction::PreGenChunkRadius => app.pregen_chunks(&value)?,
                        PromptAction::SetSakuraFrpAddress => app.set_sakurafrp_address(&value)?,
                        PromptAction::SetSakuraFrpContainer => app.set_sakurafrp_container(&value)?,
                        PromptAction::SetNatfrpToken => app.set_natfrp_token(&value)?,
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
            KeyCode::Char('2') => app.switch_tab(TabId::Players),
            KeyCode::Char('3') => app.switch_tab(TabId::Config),
            KeyCode::Char('4') => app.switch_tab(TabId::Logs),
            KeyCode::Char('5') => app.switch_tab(TabId::Yaml),
            KeyCode::Char('6') => app.switch_tab(TabId::Backups),
            KeyCode::Char('7') => app.switch_tab(TabId::Server),
            KeyCode::Char('8') => app.switch_tab(TabId::SakuraFrp),
            KeyCode::Tab => app.cycle_tab(1),
            KeyCode::BackTab => app.cycle_tab(-1),
            KeyCode::Char('r') => {
                app.refresh_all();
                if app.tab == TabId::SakuraFrp {
                    app.refresh_natfrp();
                } else {
                    app.status = app.lang.s().refreshed.into();
                }
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
                TabId::SakuraFrp => app.copy_selected_tunnel_address(),
                TabId::Players => app.toggle_whitelist_for_selected()?,
                _ => {}
            },
            KeyCode::Char('a') => {
                if app.tab == TabId::Players {
                    let s = app.lang.s();
                    app.prompt = Some(InputPrompt {
                        title: s.prompt_title_add_whitelist.into(),
                        label: s.prompt_label_player.into(),
                        buffer: String::new(),
                        action: PromptAction::AddWhitelist,
                    });
                }
            }
            KeyCode::Char('d') => {
                if app.tab == TabId::Players {
                    app.remove_selected_player()?;
                }
            }
            KeyCode::Char('o') => match app.tab {
                TabId::Players => app.toggle_op_for_selected()?,
                TabId::SakuraFrp => app.open_natfrp_dashboard(),
                _ => {}
            },
            KeyCode::Char('w') => {
                if app.tab == TabId::Players {
                    app.toggle_whitelist_enabled()?;
                    app.refresh_all();
                }
            }
            KeyCode::Left => {
                if app.tab == TabId::Players {
                    app.cycle_op_level_for_selected(-1)?;
                }
            }
            KeyCode::Right => {
                if app.tab == TabId::Players {
                    app.cycle_op_level_for_selected(1)?;
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
            KeyCode::Char('t') if app.tab == TabId::SakuraFrp => {
                let s = app.lang.s();
                app.prompt = Some(InputPrompt {
                    title: s.sf_prompt_token_title.into(),
                    label: s.sf_prompt_token_label.into(),
                    buffer: String::new(),
                    action: PromptAction::SetNatfrpToken,
                });
            }
            _ => {}
        }
    }
}

/// Map a `NatfrpError` to a localized status-bar / error-line string. Centralized
/// so v0.13's write-path (create / migrate / delete tunnel) can reuse the same
/// translations without re-deriving the same prose.
fn translate_natfrp_error(lang: Lang, err: &natfrp::NatfrpError) -> String {
    match err {
        natfrp::NatfrpError::Unauthorized => lang.s().sf_err_unauthorized.to_string(),
        natfrp::NatfrpError::Forbidden => lang.s().sf_err_forbidden.to_string(),
        natfrp::NatfrpError::ServerError(code) => fmt_sf_err_server(lang, *code),
        natfrp::NatfrpError::HttpError(code) => fmt_sf_err_http(lang, *code),
        natfrp::NatfrpError::Network(detail) => fmt_sf_err_network(lang, detail),
        natfrp::NatfrpError::Parse(detail) => fmt_sf_err_parse(lang, detail),
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
        ServerAction::SetSakuraFrpAddress => {
            app.prompt = Some(InputPrompt {
                title: s.frp_prompt_address_title.into(),
                label: s.frp_prompt_address_label.into(),
                buffer: app.sakurafrp_address.clone().unwrap_or_default(),
                action: PromptAction::SetSakuraFrpAddress,
            });
            Ok(())
        }
        ServerAction::SetSakuraFrpContainer => {
            app.prompt = Some(InputPrompt {
                title: s.frp_prompt_container_title.into(),
                label: s.frp_prompt_container_label.into(),
                buffer: app.sakurafrp_container.clone(),
                action: PromptAction::SetSakuraFrpContainer,
            });
            Ok(())
        }
        ServerAction::SakuraFrpStart => {
            app.run_frp_lifecycle("start");
            Ok(())
        }
        ServerAction::SakuraFrpStop => {
            app.run_frp_lifecycle("stop");
            Ok(())
        }
        ServerAction::SakuraFrpRestart => {
            app.run_frp_lifecycle("restart");
            Ok(())
        }
        ServerAction::SakuraFrpShowLogs => {
            app.show_frp_logs_command();
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
        // No border now (single-row tab bar), so titles start at tabs_rect.x.
        let inner_x = app.tabs_rect.x;
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
        "players" | "whitelist" | "ops" => TabId::Players,
        "config" => TabId::Config,
        "logs" => TabId::Logs,
        "yaml" => TabId::Yaml,
        "backups" => TabId::Backups,
        "server" => TabId::Server,
        "sakurafrp" | "frp" | "natfrp" => TabId::SakuraFrp,
        other => anyhow::bail!("unknown tab: {}", other),
    };
    // SakuraFrp tab fetches from the network on first visit. The screenshot
    // path bypasses switch_tab(), so trigger refresh_natfrp() explicitly here
    // — gives QA the same content the user would see after entering the tab.
    if app.tab == TabId::SakuraFrp && app.natfrp_token.is_some() {
        app.refresh_natfrp();
    }
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
    use crate::ui::{fmt_age, op_level_meaning, split_list_detail};

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
            "# header\n\
             server_dir = \"/srv/mc\"\n\
             lang = \"zh\"\n\
             sakurafrp_address = \"cn-sh-bgp.frp.one:23456\"\n",
        )
        .unwrap();
        // Reuse parser via a tiny shim that mimics read_persisted_state but takes a path.
        let raw = fs::read_to_string(&state_file).unwrap();
        let mut server_dir: Option<PathBuf> = None;
        let mut lang: Option<String> = None;
        let mut sakurafrp_address: Option<String> = None;
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
                    "sakurafrp_address" => sakurafrp_address = Some(v),
                    _ => {}
                }
            }
        }
        assert_eq!(server_dir, Some(PathBuf::from("/srv/mc")));
        assert_eq!(lang.as_deref(), Some("zh"));
        assert_eq!(sakurafrp_address.as_deref(), Some("cn-sh-bgp.frp.one:23456"));
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

    /// v0.12 — every NatfrpError variant maps to a distinct, language-appropriate
    /// message, and the action hint (press t / pkill sparkle / wait + r) is
    /// preserved in both languages. Regressions here would silently revert the
    /// onboarding fix back to the pre-v0.12 "✗ user_info: GET /user/info" mess.
    #[test]
    fn natfrp_error_translation_covers_every_variant() {
        use natfrp::NatfrpError as E;
        let cases = [
            E::Unauthorized,
            E::Forbidden,
            E::ServerError(503),
            E::HttpError(404),
            E::Network("dns failed".into()),
            E::Parse("bad json".into()),
        ];
        let mut en_msgs = Vec::new();
        let mut zh_msgs = Vec::new();
        for e in &cases {
            let en = translate_natfrp_error(Lang::En, e);
            let zh = translate_natfrp_error(Lang::Zh, e);
            assert!(!en.is_empty());
            assert!(!zh.is_empty());
            assert_ne!(en, zh, "EN/ZH should differ for {:?}", e);
            en_msgs.push(en);
            zh_msgs.push(zh);
        }
        // 401 → mentions `t` (paste a token); 403 → mentions permissions.
        assert!(en_msgs[0].contains('t'));
        assert!(zh_msgs[0].contains('t'));
        assert!(en_msgs[1].to_lowercase().contains("permission"));
        assert!(zh_msgs[1].contains("权限"));
        // 5xx vs 4xx leak the actual code so the user can grep the dashboard.
        assert!(en_msgs[2].contains("503"));
        assert!(zh_msgs[3].contains("404"));
        // Network suggests the mihomo workaround.
        assert!(en_msgs[4].contains("sparkle"));
        assert!(zh_msgs[4].contains("sparkle"));
        // Distinct messages per variant within the same language.
        let unique: std::collections::HashSet<&str> =
            en_msgs.iter().map(String::as_str).collect();
        assert_eq!(unique.len(), cases.len(), "EN messages should all differ");
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
        // ZeroTier name prefix is now folded into Tun (mesh-VPN-ish virtual iface)
        assert_eq!(
            classify_iface("ztpp6kuvag", &Ipv4Addr::new(10, 24, 0, 11)),
            NicKind::Tun
        );
        assert_eq!(
            classify_iface("zerotier0", &Ipv4Addr::new(192, 168, 1, 5)),
            NicKind::Tun
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
    fn parse_docker_state_maps_status_strings() {
        // `running` is the only "live" state we care about; everything else is
        // some flavour of "not running".
        assert_eq!(parse_docker_state("running"), DockerState::Running);
        for s in ["created", "exited", "paused", "restarting", "dead", "removing"] {
            assert_eq!(parse_docker_state(s), DockerState::Stopped, "{}", s);
        }
        // Empty / weird → unknown so we don't lie about the launcher's state.
        assert_eq!(parse_docker_state(""), DockerState::Unknown);
        // Whitespace-trimmed (docker inspect output has trailing newline).
        assert_eq!(parse_docker_state("running\n"), DockerState::Running);
        assert_eq!(parse_docker_state("  running  "), DockerState::Running);
    }

    #[test]
    fn nic_kind_priority_orders_lan_first() {
        assert!(nic_kind_priority(NicKind::Lan) < nic_kind_priority(NicKind::Public));
        assert!(nic_kind_priority(NicKind::Public) < nic_kind_priority(NicKind::Tun));
        assert!(nic_kind_priority(NicKind::Tun) < nic_kind_priority(NicKind::Docker));
        assert!(nic_kind_priority(NicKind::Docker) < nic_kind_priority(NicKind::Loopback));
    }

    #[test]
    fn nic_kind_label_localized() {
        for k in [
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

    #[test]
    fn scans_uuid_of_player_lines() {
        let lines = vec![
            "[00:05:59] [User Authenticator #3/INFO]: UUID of player Urisaki is 43b6b1f9-4219-3f2c-a702-036847c8b8cc".to_string(),
            "garbage line".to_string(),
            "[00:06:00] [User Authenticator #4/INFO]: UUID of player AlphaGuy is aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
        ];
        let mut acc = LogScanResult::default();
        scan_lines(&lines, 1_000_000, &mut acc);
        assert_eq!(acc.uuid_by_name.get("Urisaki").map(|s| s.as_str()), Some("43b6b1f9-4219-3f2c-a702-036847c8b8cc"));
        assert_eq!(acc.uuid_by_name.get("AlphaGuy").map(|s| s.as_str()), Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"));
    }

    #[test]
    fn scans_denied_login_lines() {
        let lines = vec![
            "[23:50:15] [Server thread/INFO]: Disconnecting Urisaki (/10.24.0.123:12463): You are not whitelisted on this server!".to_string(),
            "[23:51:02] [Server thread/INFO]: Added Urisaki to the whitelist".to_string(),
        ];
        let mut acc = LogScanResult::default();
        scan_lines(&lines, 1_700_000_000, &mut acc);
        assert_eq!(acc.last_denied_by_name.get("Urisaki").copied(), Some(1_700_000_000));
    }

    #[test]
    fn denied_logins_keep_latest_date() {
        let lines = vec![
            "[20:00:00] [Server thread/INFO]: Disconnecting Urisaki (/x): You are not whitelisted on this server!".to_string(),
        ];
        let mut acc = LogScanResult::default();
        scan_lines(&lines, 1_700_000_000, &mut acc);
        scan_lines(&lines, 1_700_086_400, &mut acc); // newer date
        scan_lines(&lines, 1_699_900_000, &mut acc); // older date — must NOT overwrite
        assert_eq!(acc.last_denied_by_name.get("Urisaki").copied(), Some(1_700_086_400));
    }

    #[test]
    fn scan_log_corpus_handles_gz() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let dir = tempdir();
        let logs = dir.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        // Plain latest.log
        std::fs::write(
            logs.join("latest.log"),
            b"[00:00:00] [Server thread/INFO]: Disconnecting Bob (/1.2.3.4:5): You are not whitelisted on this server!\n",
        ).unwrap();
        // Rotated YYYY-MM-DD-N.log.gz
        let raw = b"[00:00:00] [User Authenticator #1/INFO]: UUID of player Carol is 11111111-2222-3333-4444-555555555555\n";
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(raw).unwrap();
        let gz = enc.finish().unwrap();
        std::fs::write(logs.join("2026-04-30-1.log.gz"), gz).unwrap();
        // server.properties so dir looks like a server-dir
        std::fs::write(dir.join("server.properties"), b"").unwrap();

        let res = scan_log_corpus(&dir);
        assert!(res.last_denied_by_name.contains_key("Bob"));
        assert_eq!(
            res.uuid_by_name.get("Carol").map(|s| s.as_str()),
            Some("11111111-2222-3333-4444-555555555555")
        );
    }

    #[test]
    fn scan_players_merges_and_sorts() {
        let dir = tempdir();
        std::fs::write(dir.join("server.properties"), b"").unwrap();
        let wl = vec![WhitelistEntry { uuid: "u1".into(), name: "Alice".into() }];
        let ops = vec![OpEntry { uuid: "u2".into(), name: "Bob".into(), level: 4, bypasses_player_limit: false }];
        // No log dir → no log entries.
        let players = scan_players(&dir, "world", &wl, &ops);
        // Expect: Bob (op, bucket 1) before Alice (whitelist-only, bucket 2)
        let names: Vec<_> = players.iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, vec!["Bob".to_string(), "Alice".to_string()]);
        let bob = &players[0];
        assert_eq!(bob.op_level, Some(4));
        assert!(!bob.in_whitelist);
        let alice = &players[1];
        assert!(alice.in_whitelist);
        assert!(alice.op_level.is_none());
    }

    #[test]
    fn natfrp_token_roundtrip_with_0600_perms() {
        let dir = tempdir();
        // Pin XDG_CONFIG_HOME so write/read use a sandboxed path under our tempdir.
        // SAFETY: Rust 2024 set_var requires unsafe — single-threaded test, no
        // concurrent env access here.
        let key = "XDG_CONFIG_HOME";
        let prev = std::env::var(key).ok();
        unsafe { std::env::set_var(key, &dir); }

        write_natfrp_token("hello-token-123").unwrap();
        let path = natfrp_token_path();
        assert!(path.starts_with(&dir));
        assert_eq!(read_natfrp_token().as_deref(), Some("hello-token-123"));

        // 0600
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "want 0600, got {:o}", mode);
        }

        // empty file → None
        fs::write(&path, "   ").unwrap();
        assert!(read_natfrp_token().is_none());

        // restore env
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}
