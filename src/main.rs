//! shulker — a TUI manager for a local Minecraft Paper/Purpur server.

mod cli;
mod console;
mod data;
mod i18n;
mod natfrp;
mod scheduler;
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
    time::{Duration, Instant},
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
use ratatui::{
    layout::Rect,
    prelude::*,
    widgets::ListState,
    Terminal,
};


// (i18n / data / sys / cli moved to their own files — see mod declarations above.)

// ---------- App state ----------

/// v0.16 — restructured from 8 verb-categories to 5 verbs. See CLAUDE.md
/// roadmap. Old tabs (Logs/YAML/Backups/Server/Config/SakuraFrp) are absorbed:
///   Logs       → fullscreen overlay (key `L`)
///   YAML       → Settings file picker
///   Backups    → Worlds detail panel
///   Server     → command palette (key `:`) + frpc lifecycle on Network
///   Config     → renamed Settings
///   SakuraFrp  → renamed Network
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabId {
    Overview,
    Players,
    Worlds,
    Settings,
    Network,
}

const TABS: &[(TabId, &str)] = &[
    (TabId::Overview, "Overview"),
    (TabId::Players, "Players"),
    (TabId::Worlds, "Worlds"),
    (TabId::Settings, "Settings"),
    (TabId::Network, "Network"),
];

/// Command-palette actions (v0.16, formerly Server-tab actions). The original
/// 12 entries were trimmed: frpc lifecycle (start/stop/restart/show-logs) is
/// now implicit via Network-tab `e`/`x` (toggling a tunnel restarts frpc),
/// and the manual SakuraFrp address override moved to Network-tab `A`. The
/// remaining 7 surface in the `:` command palette overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerAction {
    RestartNow,
    BackupNow,
    ScheduleDailyRestart,
    ScheduleDailyBackup,
    PreGenChunks,
    OpenSystemdStatus,
    ShowAttachCommand,
}

/// v0.16 — fullscreen overlays that take precedence over tab content. Key
/// events route to the overlay instead of the underlying tab while one is up.
/// `LogsOverlay` (Phase 4) gets its own variant; the others fit a single
/// `ListState` so they share `OverlayList`.
#[derive(Debug, Default)]
pub enum Overlay {
    #[default]
    None,
    /// `?` — symbol legend + tab-grouped key list. Read-only; Esc closes.
    Help,
    /// `:` — command palette: advanced operations (schedule restart/backup,
    /// pre-gen chunks, tmux attach, systemd unit paths) that used to live in
    /// the now-deleted Server tab. ↑↓ select, Enter execute, Esc close.
    Palette { state: ListState },
    /// `L` — fullscreen scrollable log viewer with level filter. Phase 4
    /// fleshes this out; for now an open variant just renders a placeholder.
    Logs(LogsOverlay),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum LogsLevelFilter {
    #[default]
    All,
    Info,
    Warn,
    Error,
}

/// State for the `L` fullscreen log overlay. `scroll_back` is "how many
/// lines back from the tail to show" — 0 means autotail (always show the
/// last screen), increments scroll up. Saturated to `total - height` by the
/// renderer; pressing `End` resets to 0.
#[derive(Debug, Clone, Default)]
pub struct LogsOverlay {
    pub source: LogsView,
    pub filter: LogsLevelFilter,
    /// Lines back from the tail. 0 = autotail. usize::MAX is the "scroll all
    /// the way to top" sentinel — the renderer clamps it on each frame.
    pub scroll_back: usize,
}

impl LogsOverlay {
    pub fn is_autotail(&self) -> bool {
        self.scroll_back == 0
    }
}

/// Command-palette entries. Reuses `ServerAction` so the existing handler
/// in main.rs (`handle_server_action`) keeps working — the palette is just a
/// new entry-point for the same operations.
const PALETTE_COMMANDS: &[ServerAction] = &[
    ServerAction::RestartNow,
    ServerAction::BackupNow,
    ServerAction::ShowAttachCommand,
    ServerAction::ScheduleDailyRestart,
    ServerAction::ScheduleDailyBackup,
    ServerAction::PreGenChunks,
    ServerAction::OpenSystemdStatus,
];

/// v0.16 — toast (transient status). Replaces the always-on status line that
/// used to live in the footer. A toast appears in the bottom-right of the
/// content area and auto-fades after a kind-dependent timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Green check — 3s. "Did the thing" / "saved".
    Ok,
    /// Cyan info — 3s. Informational, non-actionable.
    Info,
    /// Yellow warn — 6s. Heads-up; user may want to act.
    Warn,
    /// Red x — 8s. Operation failed; user should read.
    Err,
}

impl ToastKind {
    pub fn duration(self) -> Duration {
        match self {
            ToastKind::Ok | ToastKind::Info => Duration::from_secs(3),
            ToastKind::Warn => Duration::from_secs(6),
            ToastKind::Err => Duration::from_secs(8),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub expires_at: Instant,
}

/// Classify a status string by its leading glyph. Lets the bulk of legacy
/// status sites push to the toast system without spelling a kind out at every
/// callsite — they keep their existing prefix conventions and the kind is
/// inferred. Anything that doesn't match a glyph defaults to Info.
pub fn classify_status(msg: &str) -> ToastKind {
    let trimmed = msg.trim_start();
    if trimmed.starts_with('\u{2713}') {
        // ✓
        ToastKind::Ok
    } else if trimmed.starts_with('\u{2717}') {
        // ✗
        ToastKind::Err
    } else if trimmed.starts_with('\u{26A0}') {
        // ⚠
        ToastKind::Warn
    } else {
        ToastKind::Info
    }
}

/// Settings tab view (v0.16 — formerly the YAML tab's `YamlView`, extended to
/// also cover `server.properties` editing now that Settings absorbed the old
/// Config tab). Three states:
///   * Files — file picker (server.properties as virtual entry 0, then yaml
///     files). User picks → transitions to Properties or Yaml.
///   * Properties — the property editor (server.properties).
///   * Yaml { file_idx } — the YAML row editor for `yaml_files[file_idx]`.
/// Esc from Properties / Yaml returns to Files.
#[derive(Debug, Clone)]
pub enum YamlView {
    Files,
    Properties,
    Editing { file_idx: usize },
}

/// Logs view: MC server log (default) or frpc tmux capture-pane output.
/// In v0.16 the Logs *tab* is gone; this drives the fullscreen Logs overlay
/// (key `L`) and the Phase-5 Overview tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogsView {
    #[default]
    Server,
    Frpc,
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
    /// v0.10 — input the SakuraFrp API token. Persisted to natfrp.token (0600).
    SetNatfrpToken,
    // v0.13 — three-step tunnel-create flow. Each step pre-fills sensible
    // defaults; on confirm, the next step opens automatically. Cancelling any
    // step aborts the whole flow without touching the API.
    CreateTunnelName,
    CreateTunnelPort {
        name: String,
        node: u64,
    },
    /// v0.13 — destructive ops require typing the tunnel name as confirmation.
    /// We don't accept a generic "yes" because the user has multiple tabs
    /// where `d` does something irreversible (Players' purge); typing the
    /// actual name forces them to look at what they're about to break.
    ConfirmDeleteTunnel {
        id: u64,
        name: String,
    },
    /// v0.15.1 — confirm the setup-wizard plan. User types `yes` to proceed.
    /// We don't reuse a name (no obvious noun here) so a typed "yes" is
    /// fine; the prompt title shows the full plan.
    ConfirmSetupWizard {
        plan: SetupPlan,
    },
}

#[derive(Debug, Clone)]
struct SetupPlan {
    /// `Some` when frpc isn't already on disk; `None` to skip the download
    /// step (user already has a copy).
    download: Option<SetupDownload>,
    /// Tunnel ids that will be added to `frpc_enabled_ids` when the wizard
    /// commits. Empty means "user has no tunnels" — wizard refuses earlier.
    tunnel_ids: Vec<u64>,
}

#[derive(Debug, Clone)]
struct SetupDownload {
    url: String,
    md5: String,
    target: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct CreateTunnelDraft {
    pub name: String,
    pub node: Option<u64>,
}

/// v0.13 — the node picker is a full-screen overlay, not its own tab. It's
/// `Some` while the picker is up; main.rs's key handler routes Up/Down/Enter
/// to it instead of the underlying tab. Esc closes without selecting.
#[derive(Debug, Clone)]
struct NodePickerState {
    pub purpose: NodePickerPurpose,
    pub list_state: ListState,
    /// Cached, sorted list of (node_id, label) pairs. Computed once when the
    /// picker opens so navigation is stable even if the underlying nodes map
    /// is re-fetched.
    pub entries: Vec<NodePickerEntry>,
}

#[derive(Debug, Clone)]
struct NodePickerEntry {
    pub node_id: u64,
    pub name: String,
    pub description: String,
    pub vip: u32,
    pub is_game: bool,
    pub host_present: bool,
}

#[derive(Debug, Clone)]
enum NodePickerPurpose {
    /// User is on step 2 of the create flow; we already have the name and the
    /// next prompt is the port.
    CreateTunnel { name: String },
    /// User is migrating an existing tunnel onto a new node.
    MigrateTunnel { tunnel_id: u64, tunnel_name: String },
}

struct App {
    pub server_dir: PathBuf,
    pub properties: Vec<(String, String)>,
    pub worlds: Vec<WorldEntry>,
    pub whitelist: Vec<WhitelistEntry>,
    pub ops: Vec<OpEntry>,
    /// True if the on-disk whitelist.json failed to parse last refresh.
    /// While set, shulker refuses to write to whitelist.json (would clobber the
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
    /// Logs tab subview: server log (default) or frpc tmux capture-pane.
    pub logs_view: LogsView,
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

    // v0.10 — SakuraFrp API integration. Token is read from
    // ~/.config/shulker/natfrp.token (0600); other fields are populated by
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

    // v0.13 — full-screen node picker overlay. `Some` while the user is in the
    // create-tunnel-step-2 or migrate-tunnel flows; key events route to the
    // overlay instead of the underlying tab. None means no picker active.
    pub node_picker: Option<NodePickerState>,
    /// In-progress create draft. Built up across the three create steps so we
    /// can roll back cleanly if the user cancels mid-flow.
    pub create_tunnel_draft: Option<CreateTunnelDraft>,

    // v0.14.1 — per-tunnel enable/disable state read from the launcher's
    // /run/config.json::auto_start_tunnels. Map<tunnel_id, enabled>. Empty
    // when the launcher container isn't running — UI then renders ? markers.
    pub natfrp_tunnel_enabled: std::collections::HashMap<u64, bool>,

    // v0.15 — direct frpc subprocess management. shulker now runs frpc itself
    // (via tmux) instead of going through the SakuraFrp launcher container.
    // Source-of-truth for "which tunnels are enabled" is `frpc_enabled_ids`,
    // persisted in state.toml so it survives shulker restarts. `frpc_pid` is
    // probed each refresh from the process table, sysinfo-style.
    pub frpc_binary: Option<PathBuf>,
    pub frpc_pid: Option<u32>,
    pub frpc_enabled_ids: Vec<u64>,
    /// Cached download manifest from `/v4/system/clients`. Populated lazily
    /// the first time the user is told they need to fetch a binary.
    pub clients_manifest: Option<natfrp::ClientsManifest>,

    // v0.17 — Console abstraction. Linux/macOS = tmux ref (stateless),
    // Windows = ConPTY state. Re-created when the user switches `--server-dir`.
    pub server_console: console::Console,
    pub frpc_console: console::Console,

    pub status: String,
    /// v0.16 — toast (transient status). UI renders in content-area corner;
    /// auto-clears after kind-dependent timeout. Set via `set_status` (which
    /// classifies by leading glyph) or `toast` for explicit kind. The legacy
    /// `status` field stays around for the (rare) callsites that want a
    /// non-fading message; Phase 2 cleanup migrated almost everything.
    pub toast: Option<Toast>,
    /// v0.16 — fullscreen overlay (help / command palette / logs). When
    /// non-`None`, the tab content is still rendered underneath but key
    /// events route to the overlay handler.
    pub overlay: Overlay,
    /// v0.16 — Network tab toggle: when true, the (folded by default) NIC
    /// list expands at the bottom for the rare case the user wants to share a
    /// non-primary interface (ZeroTier, secondary VPN, …). Press `n`.
    pub network_show_nics: bool,
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
        let server_console = console::Console::new(console::ConsoleRole::Server, &server_dir);
        let frpc_console = console::Console::new(console::ConsoleRole::Frpc, &server_dir);
        let mut app = App {
            server_dir,
            properties,
            worlds: Vec::new(),
            whitelist: Vec::new(),
            ops: Vec::new(),
            whitelist_corrupt: false,
            ops_corrupt: false,
            pid: None,
            tab: TabId::Overview,
            worlds_state: ListState::default(),
            players: Vec::new(),
            players_state: ListState::default(),
            whitelist_enabled: false,
            config_state: ListState::default(),
            yaml_files: Vec::new(),
            yaml_files_state: ListState::default(),
            yaml_view: YamlView::Files,
            logs_view: LogsView::Server,
            yaml_root: None,
            yaml_rows: Vec::new(),
            yaml_rows_state: ListState::default(),
            backups: Vec::new(),
            backups_state: ListState::default(),
            server_state: ListState::default(),
            // Pull persisted SakuraFrp address so the join-bar shows the
            // public entry from the start of the session.
            sakurafrp_address: read_persisted_state().sakurafrp_address,
            natfrp_token: read_natfrp_token(),
            natfrp_user: None,
            natfrp_tunnels: Vec::new(),
            natfrp_nodes: HashMap::new(),
            natfrp_last_error: None,
            natfrp_state: ListState::default(),
            natfrp_loaded: false,
            node_picker: None,
            create_tunnel_draft: None,
            natfrp_tunnel_enabled: HashMap::new(),
            frpc_binary: find_frpc_binary(),
            frpc_pid: None,
            frpc_enabled_ids: read_persisted_state().frpc_enabled_ids,
            clients_manifest: None,
            server_console,
            frpc_console,
            status: String::new(),
            toast: None,
            overlay: Overlay::None,
            network_show_nics: false,
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
        // v0.15 — re-discover the frpc binary every refresh so the user can
        // drop one in mid-session and not have to restart shulker. Cheap
        // (PATH walk + one stat).
        self.frpc_binary = find_frpc_binary();
        // Only count frpc that shulker actually launched: gate on the
        // platform console session being alive. Otherwise sysinfo would see
        // frpc instances running inside someone else's container
        // (e.g. natfrp-service) and falsely report Active.
        self.frpc_pid = if self.frpc_console.is_alive() {
            detect_frpc_pid(self.frpc_pid)
        } else {
            None
        };
        self.whitelist_enabled = get_property(&self.properties, "white-list")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        self.players = scan_players(
            &self.server_dir,
            &cur,
            &self.whitelist,
            &self.ops,
            self.pid.is_some(),
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
            // Overview has no list to navigate (Phase 5 may add one); reuse
            // worlds_state as a harmless dummy so move_selection is a no-op.
            TabId::Overview => &mut self.worlds_state,
            TabId::Worlds => &mut self.worlds_state,
            TabId::Players => &mut self.players_state,
            TabId::Settings => match self.yaml_view {
                // File picker reuses yaml_files_state — selection survives
                // entering / leaving a file.
                YamlView::Files => &mut self.yaml_files_state,
                YamlView::Properties => &mut self.config_state,
                YamlView::Editing { .. } => &mut self.yaml_rows_state,
            },
            TabId::Network => &mut self.natfrp_state,
        }
    }

    fn list_len_for(&self, tab: TabId) -> usize {
        match tab {
            TabId::Overview => 0,
            TabId::Worlds => self.worlds.len(),
            TabId::Players => self.players.len(),
            TabId::Settings => match self.yaml_view {
                // +1 for the virtual `server.properties` entry that always
                // sits at the top of the Settings file picker.
                YamlView::Files => 1 + self.yaml_files.len(),
                YamlView::Properties => self.properties.len(),
                YamlView::Editing { .. } => self.yaml_rows.len(),
            },
            TabId::Network => self.natfrp_tunnels.len(),
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
        // Lazy-load Network/SakuraFrp data the first time the user opens that
        // tab. Subsequent refreshes are user-driven via 'r' to keep the
        // network off the hot redraw path.
        if tab == TabId::Network && !self.natfrp_loaded && self.natfrp_token.is_some() {
            self.refresh_natfrp();
        }
    }

    /// Set a transient toast classified by the leading glyph of the message.
    /// Use this for the bulk-migration of the old `self.status = "..."` sites
    /// — keep the existing prefix conventions (✓ / ✗ / ⚠) and the kind is
    /// inferred. For non-prefixed messages, defaults to Info.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        let m: String = msg.into();
        let kind = classify_status(&m);
        self.toast(m, kind);
    }

    /// Set a transient toast with explicit kind. Auto-expires per
    /// `ToastKind::duration`.
    pub fn toast(&mut self, msg: impl Into<String>, kind: ToastKind) {
        self.toast = Some(Toast {
            message: msg.into(),
            kind,
            expires_at: Instant::now() + kind.duration(),
        });
    }

    /// Active toast (if any) — only returns Some while not yet expired. UI
    /// calls this each frame; an expired toast is effectively cleared.
    pub fn active_toast(&self) -> Option<&Toast> {
        self.toast
            .as_ref()
            .filter(|t| t.expires_at > Instant::now())
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

        // v0.14 — opportunistically refresh the launcher's per-tunnel
        // enable/disable state. Errors are silent: the markers fall back to
        // "?" and the user can fix it (start the launcher, fix the password)
        // without seeing a noisy error every refresh.
        self.refresh_launcher_state();
    }

    /// v0.15 — populate `natfrp_tunnel_enabled` from `App.frpc_enabled_ids`.
    /// Marker rules: tunnel id ∈ enabled list AND frpc running → ▶, in list
    /// but frpc dead → ■ (configured but not active), otherwise ?. Caller
    /// usually runs this from `refresh_natfrp` plus after each toggle.
    fn refresh_launcher_state(&mut self) {
        self.natfrp_tunnel_enabled.clear();
        let enabled_set: std::collections::HashSet<u64> =
            self.frpc_enabled_ids.iter().copied().collect();
        let frpc_alive = self.frpc_pid.is_some();
        for t in &self.natfrp_tunnels {
            // ▶ requires both: configured AND frpc actually serving traffic.
            // ■ when configured but frpc is down (tells the user "press S to start").
            let enabled = enabled_set.contains(&t.id) && frpc_alive;
            self.natfrp_tunnel_enabled.insert(t.id, enabled);
        }
    }

    /// `e` pressed — add the selected tunnel id to the auto-enable list,
    /// persist, then bounce frpc so the change takes effect.
    fn enable_selected_tunnel(&mut self) {
        self.toggle_selected_tunnel(true);
    }

    /// `x` pressed — remove the selected tunnel id and bounce frpc.
    fn disable_selected_tunnel(&mut self) {
        self.toggle_selected_tunnel(false);
    }

    fn toggle_selected_tunnel(&mut self, enable: bool) {
        let Some(idx) = self.natfrp_state.selected() else {
            self.status = self.lang.s().sf_no_selected_tunnel.to_string();
            return;
        };
        let Some(t) = self.natfrp_tunnels.get(idx) else {
            self.status = self.lang.s().sf_no_selected_tunnel.to_string();
            return;
        };
        let id = t.id;
        let name = t.name.clone();
        let already = self.frpc_enabled_ids.contains(&id);
        let frpc_alive = self.frpc_pid.is_some();

        // True no-op fast paths:
        //   * disabling something that isn't enabled
        //   * enabling something that *is* already enabled AND the daemon
        //     is alive — nothing to do.
        // The recovery case — `enable && already && !frpc_alive` — falls
        // through so pressing `e` on an enabled tunnel after a daemon crash
        // (or a fresh shulker session where state.toml carries enabled ids
        // but no process is running yet) restarts the forwarder. Without
        // this, the old behavior left the user no UI path to restart the
        // forwarder short of `x` then `e`.
        if enable && already && frpc_alive {
            self.status = match self.lang {
                Lang::En => format!("→ Tunnel '{}' is already enabled.", name),
                Lang::Zh => format!("→ 隧道 '{}' 已是启用状态。", name),
            };
            return;
        }
        if !enable && !already {
            self.status = match self.lang {
                Lang::En => format!("→ Tunnel '{}' is already disabled.", name),
                Lang::Zh => format!("→ 隧道 '{}' 已是停用状态。", name),
            };
            return;
        }

        // Mutate the persisted enabled-id list. push+dedup is idempotent so
        // the recovery case doesn't double-add.
        if enable {
            self.frpc_enabled_ids.push(id);
            self.frpc_enabled_ids.sort_unstable();
            self.frpc_enabled_ids.dedup();
        } else {
            self.frpc_enabled_ids.retain(|x| *x != id);
        }
        if let Err(e) = self.persist_state() {
            self.status = match self.lang {
                Lang::En => format!("✗ Failed to persist state: {}", e),
                Lang::Zh => format!("✗ 写入状态失败：{}", e),
            };
            return;
        }

        // Bounce frpc so the new enabled-list takes effect. If frpc isn't
        // up yet, `restart_frpc` falls through to `start_frpc`.
        let recovery = enable && already && !frpc_alive;
        match self.restart_frpc() {
            Ok(()) => {
                self.status = if recovery {
                    match self.lang {
                        Lang::En => format!(
                            "✓ Forwarder restarted (tunnel '{}' was already enabled).",
                            name
                        ),
                        Lang::Zh => format!("✓ 转发已重启（隧道 '{}' 此前已启用）。", name),
                    }
                } else if enable {
                    match self.lang {
                        Lang::En => {
                            format!("✓ Enabled tunnel '{}' (id {}). Forwarder restarted.", name, id)
                        }
                        Lang::Zh => {
                            format!("✓ 已启用隧道 '{}' (id {})。转发已重启。", name, id)
                        }
                    }
                } else {
                    match self.lang {
                        Lang::En => format!(
                            "✓ Disabled tunnel '{}' (id {}). Forwarder restarted.",
                            name, id
                        ),
                        Lang::Zh => {
                            format!("✓ 已停用隧道 '{}' (id {})。转发已重启。", name, id)
                        }
                    }
                };
            }
            Err(e) => {
                self.status = match self.lang {
                    Lang::En => format!(
                        "✗ Tunnel state updated but forwarder failed to (re)start: {}",
                        e
                    ),
                    Lang::Zh => format!("✗ 隧道状态已写入，但转发启动失败：{}", e),
                };
            }
        }
        self.refresh_launcher_state();
    }

    // ---------- v0.15: frpc subprocess lifecycle ----------

    // ---------- v0.15.1: setup wizard ----------
    //
    // Single key (`i`) drives the whole onboarding flow: download the right
    // frpc binary, verify md5, enable every tunnel the user has on the
    // SakuraFrp account, start frpc. Each step produces a status update so
    // a stuck wizard tells the user where it stuck.

    /// `i` pressed on SakuraFrp tab → build a plan, confirm, execute.
    fn start_setup_wizard(&mut self) {
        // Prereqs: token must be set; tunnels must be loaded; we must have
        // resolved the host's target arch.
        if self.natfrp_token.is_none() {
            self.status = match self.lang {
                Lang::En => "✗ Set a SakuraFrp token first (press t).".into(),
                Lang::Zh => "✗ 请先设置 SakuraFrp token（按 t）。".into(),
            };
            return;
        }
        if !self.natfrp_loaded {
            self.status = match self.lang {
                Lang::En => "✗ Press r to load tunnels first.".into(),
                Lang::Zh => "✗ 请先按 r 加载隧道。".into(),
            };
            return;
        }
        if self.natfrp_tunnels.is_empty() {
            self.status = match self.lang {
                Lang::En => "✗ No tunnels on this account — create one first (press c).".into(),
                Lang::Zh => "✗ 账户没有隧道 — 请先按 c 创建。".into(),
            };
            return;
        }

        // Make sure we have the manifest. One-shot fetch.
        if self.clients_manifest.is_none() {
            match natfrp::fetch_clients_manifest() {
                Ok(m) => self.clients_manifest = Some(m),
                Err(e) => {
                    self.status = translate_natfrp_error(self.lang, &e);
                    return;
                }
            }
        }
        let manifest = self.clients_manifest.as_ref().unwrap();
        let Some((os, arch)) = data::host_target_for_manifest() else {
            self.status = match self.lang {
                Lang::En => format!(
                    "✗ Host platform {}/{} not in SakuraFrp's binary list.",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
                Lang::Zh => format!(
                    "✗ 当前平台 {}/{} 不在 SakuraFrp 官方下载清单里。",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
            };
            return;
        };
        let key = format!("{}_{}", os, arch);
        let Some(arch_entry) = manifest.frpc_archs.get(&key) else {
            self.status = match self.lang {
                Lang::En => format!("✗ Manifest has no entry for {}.", key),
                Lang::Zh => format!("✗ 清单里没有 {} 的下载项。", key),
            };
            return;
        };

        let install_path = sys::config_dir().join("frpc");
        let download = if self.frpc_binary.is_some() {
            // Already on disk — skip the network round-trip.
            None
        } else {
            Some(SetupDownload {
                url: arch_entry.url.clone(),
                md5: arch_entry.hash.clone(),
                target: install_path.clone(),
            })
        };

        let tunnel_ids: Vec<u64> = self.natfrp_tunnels.iter().map(|t| t.id).collect();
        let plan = SetupPlan {
            download,
            tunnel_ids,
        };

        // Build a human-readable summary for the prompt title.
        let summary = self.summarize_setup_plan(&plan);

        self.prompt = Some(InputPrompt {
            title: summary,
            label: match self.lang {
                Lang::En => "type yes".into(),
                Lang::Zh => "输入 yes".into(),
            },
            buffer: String::new(),
            action: PromptAction::ConfirmSetupWizard { plan },
        });
    }

    fn summarize_setup_plan(&self, plan: &SetupPlan) -> String {
        let mut steps: Vec<String> = Vec::new();
        if let Some(d) = &plan.download {
            steps.push(match self.lang {
                Lang::En => format!("download frpc → {}", d.target.display()),
                Lang::Zh => format!("下载 frpc → {}", d.target.display()),
            });
        }
        let names: Vec<String> = plan
            .tunnel_ids
            .iter()
            .filter_map(|id| {
                self.natfrp_tunnels
                    .iter()
                    .find(|t| t.id == *id)
                    .map(|t| t.name.clone())
            })
            .collect();
        steps.push(match self.lang {
            Lang::En => format!("enable {}", names.join(", ")),
            Lang::Zh => format!("启用 {}", names.join("、")),
        });
        steps.push(match self.lang {
            Lang::En => "start frpc (tmux)".into(),
            Lang::Zh => "启动 frpc (tmux)".into(),
        });
        match self.lang {
            Lang::En => format!("Setup wizard: {}.", steps.join(" → ")),
            Lang::Zh => format!("一键配置：{}。", steps.join(" → ")),
        }
    }

    /// Confirmation accepted — run the plan. Status updates show progress
    /// step by step so a hung wizard reveals the stuck step.
    fn execute_setup_wizard(&mut self, plan: SetupPlan) {
        // Step 1: download (if planned).
        if let Some(d) = &plan.download {
            self.status = match self.lang {
                Lang::En => format!("→ Downloading frpc from {}…", d.url),
                Lang::Zh => format!("→ 正在下载 frpc：{}…", d.url),
            };
            // The status bar won't actually repaint mid-call (we're on the
            // event-loop thread), but the message lands once we return.
            // ureq blocking ~5–10 s on a fast link.
            if let Some(parent) = d.target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = data::download_frpc(&d.url, &d.target) {
                self.status = match self.lang {
                    Lang::En => format!("✗ Download failed: {}", e),
                    Lang::Zh => format!("✗ 下载失败：{}", e),
                };
                return;
            }
            // Step 1b: verify md5.
            match data::verify_md5(&d.target, &d.md5) {
                Ok(true) => {}
                Ok(false) => {
                    let _ = std::fs::remove_file(&d.target);
                    self.status = match self.lang {
                        Lang::En => "✗ Downloaded frpc md5 mismatch — file removed. Try again.".into(),
                        Lang::Zh => "✗ 下载的 frpc md5 不匹配 — 已删除，请重试。".into(),
                    };
                    return;
                }
                Err(e) => {
                    self.status = match self.lang {
                        Lang::En => format!("✗ md5 check failed: {}", e),
                        Lang::Zh => format!("✗ md5 校验失败：{}", e),
                    };
                    return;
                }
            }
            self.frpc_binary = data::find_frpc_binary();
        }

        // Step 2: enable the requested tunnel ids (in addition to whatever
        // was already enabled — preserves manual prior toggles).
        for id in &plan.tunnel_ids {
            if !self.frpc_enabled_ids.contains(id) {
                self.frpc_enabled_ids.push(*id);
            }
        }
        self.frpc_enabled_ids.sort_unstable();
        self.frpc_enabled_ids.dedup();
        if let Err(e) = self.persist_state() {
            self.status = match self.lang {
                Lang::En => format!("✗ Failed to persist state: {}", e),
                Lang::Zh => format!("✗ 保存状态失败：{}", e),
            };
            return;
        }

        // Step 3: bounce frpc so the new enabled list takes effect.
        match self.restart_frpc() {
            Ok(()) => {
                self.status = match self.lang {
                    Lang::En => format!(
                        "✓ Setup complete: {} tunnel(s) enabled, frpc running.",
                        plan.tunnel_ids.len()
                    ),
                    Lang::Zh => format!(
                        "✓ 一键配置完成：已启用 {} 条隧道，frpc 已运行。",
                        plan.tunnel_ids.len()
                    ),
                };
                self.refresh_launcher_state();
            }
            Err(e) => {
                self.status = match self.lang {
                    Lang::En => format!(
                        "✓ Tunnels enabled, but frpc start failed: {}",
                        e
                    ),
                    Lang::Zh => format!(
                        "✓ 隧道已启用，但 frpc 启动失败：{}",
                        e
                    ),
                };
            }
        }
    }

    /// Start frpc inside the platform's console abstraction. No-op if already
    /// running. Returns Err with a translated reason if any prerequisite is
    /// missing (binary, token, no tunnels enabled).
    fn start_frpc(&mut self) -> Result<()> {
        if self.frpc_console.is_alive() {
            return Ok(());
        }
        let Some(binary) = self.frpc_binary.clone() else {
            anyhow::bail!(self.frpc_missing_message());
        };
        let Some(token) = self.natfrp_token.clone() else {
            anyhow::bail!(match self.lang {
                Lang::En => "no SakuraFrp token configured (press t on tab 8)",
                Lang::Zh => "未配置 SakuraFrp token（在 SakuraFrp tab 按 t）",
            });
        };
        if self.frpc_enabled_ids.is_empty() {
            anyhow::bail!(match self.lang {
                Lang::En => "no tunnels enabled (press e on a tunnel row first)",
                Lang::Zh => "没有启用任何隧道（先在隧道列表里按 e 启用）",
            });
        }
        let ids_joined: Vec<String> =
            self.frpc_enabled_ids.iter().map(u64::to_string).collect();
        // frpc -f "<token>:<id1>,<id2>" -n   (no version-check on startup —
        // frpc updates are out-of-band on the user's machine)
        let fetch_arg = format!("{}:{}", token, ids_joined.join(","));
        let argv = vec![
            binary.display().to_string(),
            "-f".to_string(),
            fetch_arg,
            "-n".to_string(),
        ];
        self.frpc_console
            .start(&argv)
            .with_context(|| "start frpc console")?;
        // Give the proc table a moment so the next refresh sees the pid.
        std::thread::sleep(std::time::Duration::from_millis(200));
        self.frpc_pid = data::detect_frpc_pid(self.frpc_pid);
        Ok(())
    }

    /// Stop frpc by killing its console session. frpc handles SIGINT cleanly.
    fn stop_frpc(&mut self) -> Result<()> {
        if !self.frpc_console.is_alive() {
            return Ok(());
        }
        self.frpc_console
            .kill_session()
            .with_context(|| "kill frpc console")?;
        // Poll briefly for pid to vanish — frpc handles SIGINT in <100 ms.
        for _ in 0..30 {
            if data::detect_frpc_pid(None).is_none() {
                self.frpc_pid = None;
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // Best-effort: even if we still see something, return Ok — the
        // session is dead, sysinfo will catch up.
        self.frpc_pid = None;
        Ok(())
    }

    /// Stop + start. Used after toggling an enabled tunnel.
    fn restart_frpc(&mut self) -> Result<()> {
        self.stop_frpc().ok();
        self.start_frpc()
    }

    /// Persist server-dir / lang / sakurafrp_address / frpc_enabled_ids.
    fn persist_state(&self) -> Result<()> {
        let mut state = read_persisted_state();
        state.server_dir = Some(self.server_dir.clone());
        state.lang = Some(self.lang.code().to_string());
        state.sakurafrp_address = self.sakurafrp_address.clone();
        // Keep container name field for backward-compat read; don't rewrite.
        state.frpc_enabled_ids = self.frpc_enabled_ids.clone();
        write_persisted_state(&state)?;
        Ok(())
    }

    /// Resolve "the user needs to fetch a frpc binary" UX. Returns a status
    /// string; populates the clipboard with the official URL when possible.
    fn frpc_missing_message(&mut self) -> String {
        // Make sure we have a manifest cached. One-shot fetch — if it fails
        // we fall back to a generic message.
        if self.clients_manifest.is_none() {
            if let Ok(m) = natfrp::fetch_clients_manifest() {
                self.clients_manifest = Some(m);
            }
        }
        let target = data::host_target_for_manifest();
        let install_dir = sys::config_dir();
        let install_path = install_dir.join("frpc");

        let url_opt = self.clients_manifest.as_ref().and_then(|m| {
            target.and_then(|(os, arch)| {
                m.frpc_archs
                    .get(&format!("{}_{}", os, arch))
                    .map(|a| a.url.clone())
            })
        });

        if let Some(url) = url_opt.as_deref() {
            // Best-effort copy to clipboard. Failure here is silent — the
            // URL is still in the status message either way.
            let _ = sys::clipboard_copy(url);
            match self.lang {
                Lang::En => format!(
                    "✗ frpc not found. Download {} → save to {} (chmod +x). URL copied to clipboard.",
                    url,
                    install_path.display()
                ),
                Lang::Zh => format!(
                    "✗ 没找到 frpc。下载 {} → 保存到 {}（chmod +x）。URL 已复制到剪贴板。",
                    url,
                    install_path.display()
                ),
            }
        } else {
            match self.lang {
                Lang::En => format!(
                    "✗ frpc not found. See https://www.natfrp.com/tunnel/launcher → save to {} (chmod +x).",
                    install_path.display()
                ),
                Lang::Zh => format!(
                    "✗ 没找到 frpc。参考 https://www.natfrp.com/tunnel/launcher → 保存到 {}（chmod +x）。",
                    install_path.display()
                ),
            }
        }
    }

    /// v0.12 — open the SakuraFrp access-key page in the user's default
    /// browser. Falls back to copying the URL to the clipboard when no
    /// browser is reachable (headless / SSH / no portal). The deep link
    /// lands on the access-key tab.
    fn open_natfrp_dashboard(&mut self) {
        const URL: &str = "https://www.natfrp.com/user/edit/auth#info-key";
        if sys::open_url(URL).is_ok() {
            self.status = match self.lang {
                Lang::En => format!("✓ Opened {} in your browser.", URL),
                Lang::Zh => format!("✓ 已在浏览器打开 {}。", URL),
            };
            return;
        }
        // Best-effort clipboard fallback. Tell the user even when copy
        // succeeds — they may have expected a browser to pop.
        let copied = sys::clipboard_copy(URL).is_ok();
        self.status = match (self.lang, copied) {
            (Lang::En, true) => {
                format!("ℹ Browser unavailable; copied {} to clipboard.", URL)
            }
            (Lang::En, false) => {
                format!("ℹ Open this URL manually: {}", URL)
            }
            (Lang::Zh, true) => {
                format!("ℹ 浏览器不可用；已把 {} 复制到剪贴板。", URL)
            }
            (Lang::Zh, false) => {
                format!("ℹ 请手动打开此 URL：{}", URL)
            }
        };
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

    /// Y on Overview — copy the multi-line share text to the system
    /// clipboard. Picks frp address if configured (preferred for non-LAN
    /// friends); otherwise primary LAN interface.
    pub fn copy_share_text(&mut self) {
        let nics = detect_interfaces();
        let port: u16 = get_property(&self.properties, "server-port")
            .and_then(|v| v.parse().ok())
            .unwrap_or(25565);
        let primary = nics.iter().find(|n| {
            !matches!(n.kind, NicKind::Loopback | NicKind::Docker | NicKind::Tun)
        });
        let frp_addr = self.effective_sakurafrp_address();
        let lan = primary.map(|n| (n.ip.to_string(), port));
        let text = crate::ui::build_share_text(self, lan, frp_addr.as_deref());
        let copied = sys::clipboard_copy(&text).is_ok();
        let msg: String = match (self.lang, copied) {
            (Lang::En, true) => "✓ Share text copied to clipboard.".into(),
            (Lang::En, false) => "ℹ Clipboard unavailable — copy from the preview manually.".into(),
            (Lang::Zh, true) => "✓ 分享文本已复制到剪贴板。".into(),
            (Lang::Zh, false) => "ℹ 剪贴板不可用 — 请手动复制预览。".into(),
        };
        self.set_status(msg);
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
        let copied = sys::clipboard_copy(&addr).is_ok();
        self.status = match (self.lang, copied) {
            (Lang::En, true) => format!("✓ Copied {} to clipboard", addr),
            (Lang::En, false) => format!("ℹ {} (clipboard unavailable)", addr),
            (Lang::Zh, true) => format!("✓ 已复制 {} 到剪贴板", addr),
            (Lang::Zh, false) => format!("ℹ {}（剪贴板不可用）", addr),
        };
    }

    // ---------- v0.13: tunnel write operations ----------

    /// Build the default `mc_<slug>` tunnel name for the current server-dir.
    /// `server_dir_slug` uses `-` for non-alnum chars but SakuraFrp rejects
    /// hyphens, so we re-normalize to `_` here.
    fn default_tunnel_name(&self) -> String {
        let slug = sys::server_dir_slug(&self.server_dir).replace('-', "_");
        format!("mc_{}", slug)
    }

    /// Step 1 of create flow: prompt for the tunnel name.
    fn start_create_tunnel(&mut self) {
        let s = self.lang.s();
        self.create_tunnel_draft = Some(CreateTunnelDraft::default());
        self.prompt = Some(InputPrompt {
            title: s.sf_prompt_create_name_title.into(),
            label: s.sf_prompt_create_name_label.into(),
            buffer: self.default_tunnel_name(),
            action: PromptAction::CreateTunnelName,
        });
    }

    /// Step 1 confirmed → validate, store name in draft, open node picker.
    fn handle_create_tunnel_name(&mut self, raw_name: &str) -> Result<()> {
        let name = raw_name.trim().to_string();
        if !natfrp::validate_tunnel_name(&name) {
            self.status = match self.lang {
                Lang::En => format!(
                    "✗ Invalid tunnel name '{}' — only A-Z a-z 0-9 _ allowed (≤32 chars).",
                    raw_name
                ),
                Lang::Zh => format!(
                    "✗ 隧道名 '{}' 不合法 — 只允许字母数字下划线（≤32 字符）。",
                    raw_name
                ),
            };
            self.create_tunnel_draft = None;
            return Ok(());
        }
        if let Some(d) = self.create_tunnel_draft.as_mut() {
            d.name = name.clone();
        }
        self.open_node_picker(NodePickerPurpose::CreateTunnel { name });
        Ok(())
    }

    /// Step 2 picker selected → store node id, prompt for port.
    fn handle_create_tunnel_node(&mut self, node: u64, name: String) {
        let s = self.lang.s();
        if let Some(d) = self.create_tunnel_draft.as_mut() {
            d.node = Some(node);
        }
        self.prompt = Some(InputPrompt {
            title: s.sf_prompt_create_port_title.into(),
            label: s.sf_prompt_create_port_label.into(),
            buffer: "25565".to_string(),
            action: PromptAction::CreateTunnelPort { name, node },
        });
    }

    /// Step 3 confirmed → validate port, fire POST, refresh, select new tunnel.
    fn handle_create_tunnel_port(&mut self, raw: &str, name: &str, node: u64) {
        let port: u16 = match raw.trim().parse() {
            Ok(n) if n != 0 => n,
            _ => {
                self.status = match self.lang {
                    Lang::En => format!("✗ Invalid port '{}'", raw),
                    Lang::Zh => format!("✗ 端口 '{}' 不合法", raw),
                };
                return;
            }
        };
        let Some(token) = self.natfrp_token.clone() else {
            self.status = self.lang.s().sf_user_no_token.to_string();
            return;
        };
        let client = natfrp::Client::new(token);
        match client.create_tunnel(name, node, port) {
            Ok(_id) => {
                self.status = match self.lang {
                    Lang::En => format!("✓ Created tunnel {} on node #{}.", name, node),
                    Lang::Zh => format!("✓ 已创建隧道 {}，节点 #{}。", name, node),
                };
                self.create_tunnel_draft = None;
                // Refresh tunnels list and try to select the freshly added one.
                self.natfrp_loaded = false;
                self.refresh_natfrp();
                if let Some(i) = self
                    .natfrp_tunnels
                    .iter()
                    .position(|t| t.name == name)
                {
                    self.natfrp_state.select(Some(i));
                }
            }
            Err(e) => {
                self.status = translate_natfrp_error(self.lang, &e);
                self.create_tunnel_draft = None;
            }
        }
    }

    /// `m` pressed on a tunnel row → open node picker for migration.
    fn start_migrate_tunnel(&mut self) {
        let Some(idx) = self.natfrp_state.selected() else {
            self.status = self.lang.s().sf_no_selected_tunnel.to_string();
            return;
        };
        let Some(t) = self.natfrp_tunnels.get(idx) else {
            self.status = self.lang.s().sf_no_selected_tunnel.to_string();
            return;
        };
        let purpose = NodePickerPurpose::MigrateTunnel {
            tunnel_id: t.id,
            tunnel_name: t.name.clone(),
        };
        self.open_node_picker(purpose);
    }

    /// Migration node selected → fire POST, refresh.
    fn handle_migrate_node(&mut self, tunnel_id: u64, tunnel_name: &str, new_node: u64) {
        let Some(token) = self.natfrp_token.clone() else {
            self.status = self.lang.s().sf_user_no_token.to_string();
            return;
        };
        let client = natfrp::Client::new(token);
        match client.migrate_tunnel(tunnel_id, new_node) {
            Ok(()) => {
                self.status = match self.lang {
                    Lang::En => format!(
                        "✓ Migrated {} to node #{}. If clients can't connect, restart the launcher container from the Server tab.",
                        tunnel_name, new_node
                    ),
                    Lang::Zh => format!(
                        "✓ 已将 {} 迁移到节点 #{}。如果客户端连不上，请到运维 tab 重启 SakuraFrp 容器。",
                        tunnel_name, new_node
                    ),
                };
                self.natfrp_loaded = false;
                self.refresh_natfrp();
            }
            Err(e) => {
                self.status = translate_natfrp_error(self.lang, &e);
            }
        }
    }

    /// `d` pressed on a tunnel row → confirmation prompt; user must type the
    /// tunnel name to proceed.
    fn start_delete_tunnel(&mut self) {
        let Some(idx) = self.natfrp_state.selected() else {
            self.status = self.lang.s().sf_no_selected_tunnel.to_string();
            return;
        };
        let Some(t) = self.natfrp_tunnels.get(idx) else {
            self.status = self.lang.s().sf_no_selected_tunnel.to_string();
            return;
        };
        let title = match self.lang {
            Lang::En => format!("DELETE tunnel '{}' (id {}) — type the name to confirm", t.name, t.id),
            Lang::Zh => format!("删除隧道 '{}' (id {}) — 输入名称以确认", t.name, t.id),
        };
        self.prompt = Some(InputPrompt {
            title,
            label: self.lang.s().sf_prompt_create_name_label.into(),
            buffer: String::new(),
            action: PromptAction::ConfirmDeleteTunnel {
                id: t.id,
                name: t.name.clone(),
            },
        });
    }

    /// Confirmation prompt resolved → if input matches name, fire POST.
    fn handle_confirm_delete_tunnel(&mut self, raw: &str, id: u64, name: &str) {
        if raw.trim() != name {
            self.status = match self.lang {
                Lang::En => format!("✗ Confirmation mismatch — expected '{}'. Aborted.", name),
                Lang::Zh => format!("✗ 确认输入不匹配 — 期望 '{}'。已取消。", name),
            };
            return;
        }
        let Some(token) = self.natfrp_token.clone() else {
            self.status = self.lang.s().sf_user_no_token.to_string();
            return;
        };
        let client = natfrp::Client::new(token);
        match client.delete_tunnels(&[id]) {
            Ok(()) => {
                self.status = match self.lang {
                    Lang::En => format!("✓ Deleted tunnel '{}' (id {}).", name, id),
                    Lang::Zh => format!("✓ 已删除隧道 '{}' (id {})。", name, id),
                };
                self.natfrp_loaded = false;
                self.refresh_natfrp();
                if self.natfrp_state.selected().is_some() && self.natfrp_tunnels.is_empty() {
                    self.natfrp_state.select(None);
                }
            }
            Err(e) => {
                self.status = translate_natfrp_error(self.lang, &e);
            }
        }
    }

    /// Build a sorted node-picker entries list from the current `/nodes`
    /// cache. Game-friendly nodes go first, then by VIP tier ascending
    /// (lowest = available to most users), then by id for stable ordering.
    fn open_node_picker(&mut self, purpose: NodePickerPurpose) {
        let mut entries: Vec<NodePickerEntry> = self
            .natfrp_nodes
            .iter()
            .map(|(id, n)| NodePickerEntry {
                node_id: *id,
                name: n.name.clone(),
                description: n.description.clone(),
                vip: n.vip,
                is_game: natfrp::is_game_node(n),
                host_present: !n.host.is_empty(),
            })
            .collect();
        entries.sort_by(|a, b| {
            // Game-friendly first (true > false → reverse boolean compare)
            b.is_game
                .cmp(&a.is_game)
                .then(a.vip.cmp(&b.vip))
                .then(a.node_id.cmp(&b.node_id))
        });
        let mut list_state = ListState::default();
        if !entries.is_empty() {
            list_state.select(Some(0));
        }
        self.node_picker = Some(NodePickerState {
            purpose,
            list_state,
            entries,
        });
    }

    /// Picker confirmed (Enter pressed inside overlay).
    fn handle_node_picker_select(&mut self) {
        let Some(picker) = self.node_picker.take() else {
            return;
        };
        let Some(idx) = picker.list_state.selected() else {
            return;
        };
        let Some(entry) = picker.entries.get(idx) else {
            return;
        };
        let node_id = entry.node_id;
        // Plan §6: nag once when the user picks a non-game-friendly node, since
        // that's the original "宁波 30秒掉线" failure mode. We don't block the
        // pick — they may have a reason (regional latency, paid plan, etc.).
        let warn_non_game = !entry.is_game;
        match picker.purpose {
            NodePickerPurpose::CreateTunnel { name } => {
                self.handle_create_tunnel_node(node_id, name);
            }
            NodePickerPurpose::MigrateTunnel {
                tunnel_id,
                tunnel_name,
            } => {
                self.handle_migrate_node(tunnel_id, &tunnel_name, node_id);
            }
        }
        if warn_non_game {
            // Append rather than replace — the success/failure status the
            // handler set is more important; the warning is supplemental.
            let warn = self.lang.s().sf_picker_warn_non_game;
            self.status = format!("{}  {}", self.status, warn);
        }
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
        let job_kind = match kind {
            ServerAction::ScheduleDailyRestart => scheduler::JobKind::Restart,
            ServerAction::ScheduleDailyBackup => scheduler::JobKind::Backup,
            _ => return Ok(()),
        };
        let outcome = match scheduler::schedule_daily(job_kind, &self.server_dir, hour, minute) {
            Ok(o) => o,
            Err(e) => {
                self.status = match self.lang {
                    Lang::En => format!("✗ schedule failed: {}", e),
                    Lang::Zh => format!("✗ 计划任务创建失败：{}", e),
                };
                return Ok(());
            }
        };
        self.status = match (self.lang, &outcome) {
            (Lang::En, scheduler::ScheduleOutcome::Activated { unschedule_command }) => {
                format!(
                    "✓ Scheduled. To unschedule: {}",
                    unschedule_command
                )
            }
            (Lang::Zh, scheduler::ScheduleOutcome::Activated { unschedule_command }) => {
                format!(
                    "✓ 已注册定时任务。取消命令：{}",
                    unschedule_command
                )
            }
            (
                Lang::En,
                scheduler::ScheduleOutcome::WroteFile { path, activate_command, .. },
            ) => format!(
                "✓ Wrote {}. Activate: {}",
                path, activate_command
            ),
            (
                Lang::Zh,
                scheduler::ScheduleOutcome::WroteFile { path, activate_command, .. },
            ) => format!(
                "✓ 已写入 {}。激活：{}",
                path, activate_command
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
        if !self.server_console.is_alive() {
            self.status = match self.lang {
                Lang::En => "✗ Server console isn't running — start the server with S first.".into(),
                Lang::Zh => "✗ 服务器控制台未运行 — 请先按 S 启动服务器。".into(),
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
        for c in &cmds {
            if let Err(e) = self.server_console.send_line(c) {
                self.status = match self.lang {
                    Lang::En => format!("✗ console send-line failed: {}", e),
                    Lang::Zh => format!("✗ 控制台发送失败：{}", e),
                };
                return Ok(());
            }
        }
        let attach = self.server_console.attach_command();
        self.status = match (self.lang, attach.as_deref()) {
            (Lang::En, Some(a)) => format!(
                "✓ Pre-gen sent (radius {}). Attach with `{}` to watch.",
                radius, a
            ),
            (Lang::En, None) => format!(
                "✓ Pre-gen sent (radius {}). Watch progress in the Logs tab (L).",
                radius
            ),
            (Lang::Zh, Some(a)) => format!(
                "✓ 已发送区块预加载（半径 {}）。`{}` 查看进度。",
                radius, a
            ),
            (Lang::Zh, None) => format!(
                "✓ 已发送区块预加载（半径 {}）。在 Logs tab (L) 查看进度。",
                radius
            ),
        };
        Ok(())
    }

    fn show_attach_command(&mut self) {
        let Some(cmd) = self.server_console.attach_command() else {
            // Windows: no attach. Point users at the Logs tab instead.
            self.status = match self.lang {
                Lang::En => "ℹ No attach on this platform — watch the server in the Logs tab (L).".into(),
                Lang::Zh => "ℹ 此平台不支持 attach — 请在 Logs tab (L) 查看服务器输出。".into(),
            };
            return;
        };
        let alive = self.server_console.is_alive();
        // Best-effort copy; ignore failures (e.g. headless / no clipboard).
        let _ = sys::clipboard_copy(&cmd);
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
        // Pick the OS-native launch script. `shulker new` writes start.sh on
        // Unix and start.bat on Windows. We don't try to fall back from one
        // to the other — bash on Windows would need WSL, and cmd on Unix is
        // pointless.
        let script_name = if cfg!(windows) { "start.bat" } else { "start.sh" };
        let script = self.server_dir.join(script_name);
        if !script.exists() {
            self.status = fmt_start_script_missing(self.lang, &script);
            return Ok(());
        }

        // Re-attach situation: if the (Unix) tmux session is already up, it's
        // our previous server. Tell the user how to attach instead of double-spawning.
        if self.server_console.is_alive() {
            let attach = self.server_console.attach_command()
                .unwrap_or_else(|| self.server_console.session_name.clone());
            self.status = match self.lang {
                Lang::En => format!("→ Server console already running. Attach: {}", attach),
                Lang::Zh => format!("→ 服务器控制台已在运行。接管：{}", attach),
            };
            return Ok(());
        }

        // argv form: Unix runs `bash start.sh`, Windows runs `start.bat` directly.
        // (Windows .bat files are spawnable as a "command" via ConPTY through
        // CommandBuilder; cmd.exe handles the parsing.)
        let argv: Vec<String> = if cfg!(windows) {
            vec![script.display().to_string()]
        } else {
            vec!["bash".to_string(), script.display().to_string()]
        };

        match self.server_console.start(&argv) {
            Ok(()) => {
                let attach_hint = self.server_console.attach_command();
                self.status = match (self.lang, attach_hint.as_deref()) {
                    (Lang::En, Some(cmd)) => {
                        format!("✓ Server console started. Attach: {}", cmd)
                    }
                    (Lang::En, None) => {
                        "✓ Server started. Watch output in the Logs tab (L); closing shulker will stop the server.".into()
                    }
                    (Lang::Zh, Some(cmd)) => {
                        format!("✓ 服务器控制台已启动。接管：{}", cmd)
                    }
                    (Lang::Zh, None) => {
                        "✓ 服务器已启动。在 Logs tab (L) 查看输出；关闭 shulker 会停服。".into()
                    }
                };
            }
            Err(e) => self.status = fmt_spawn_failed(self.lang, &e.to_string()),
        }
        Ok(())
    }

    fn stop_server(&mut self) -> Result<()> {
        if self.pid.is_none() && !self.server_console.is_alive() {
            self.status = self.lang.s().err_not_running.into();
            return Ok(());
        }

        // Send the literal text "stop" + Enter to the console — Minecraft's
        // synchronous shutdown handler runs on the main thread, which is the
        // only reliable shutdown path. SIGTERM races with startup and can
        // leave a half-dead JVM (port closed, world ticking, no progress).
        match self.server_console.stop_graceful("stop") {
            Ok(()) => {
                self.status = match self.lang {
                    Lang::En => "→ Sent `stop` to server console. Watching for exit…".into(),
                    Lang::Zh => "→ 已向服务器控制台发送 `stop`，等待退出…".into(),
                };
            }
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

        // Re-bind the console handles to the new server-dir so the tmux
        // session names / ConPTY working dirs follow the switch.
        self.server_console = console::Console::new(console::ConsoleRole::Server, &self.server_dir);
        self.frpc_console = console::Console::new(console::ConsoleRole::Frpc, &self.server_dir);

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
                        PromptAction::SetNatfrpToken => app.set_natfrp_token(&value)?,
                        PromptAction::CreateTunnelName => {
                            app.handle_create_tunnel_name(&value)?
                        }
                        PromptAction::CreateTunnelPort { name, node } => {
                            app.handle_create_tunnel_port(&value, &name, node)
                        }
                        PromptAction::ConfirmDeleteTunnel { id, name } => {
                            app.handle_confirm_delete_tunnel(&value, id, &name)
                        }
                        PromptAction::ConfirmSetupWizard { plan } => {
                            if value.trim() == "yes" {
                                app.execute_setup_wizard(plan);
                            } else {
                                app.status = match app.lang {
                                    Lang::En => "→ Setup cancelled.".into(),
                                    Lang::Zh => "→ 已取消一键配置。".into(),
                                };
                            }
                        }
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

        // v0.13 — node picker overlay intercepts input. Only nav + confirm +
        // cancel go through; everything else (including tab-switch / quit
        // shortcuts) is silently dropped so the user can't accidentally
        // navigate away mid-create. Esc closes the overlay without selecting.
        if app.node_picker.is_some() {
            match key.code {
                KeyCode::Esc => {
                    app.node_picker = None;
                    app.create_tunnel_draft = None;
                    app.status = app.lang.s().cancelled.into();
                }
                KeyCode::Up => {
                    if let Some(p) = app.node_picker.as_mut() {
                        let n = p.entries.len();
                        if n > 0 {
                            let cur = p.list_state.selected().unwrap_or(0) as isize;
                            let new = (cur - 1).rem_euclid(n as isize) as usize;
                            p.list_state.select(Some(new));
                        }
                    }
                }
                KeyCode::Down => {
                    if let Some(p) = app.node_picker.as_mut() {
                        let n = p.entries.len();
                        if n > 0 {
                            let cur = p.list_state.selected().unwrap_or(0) as isize;
                            let new = (cur + 1).rem_euclid(n as isize) as usize;
                            p.list_state.select(Some(new));
                        }
                    }
                }
                KeyCode::Enter => app.handle_node_picker_select(),
                _ => {}
            }
            continue;
        }

        // v0.16 — overlay (help / palette / logs) intercepts input. Esc closes
        // it. Help is read-only; palette has its own up/down/enter; logs
        // overlay (Phase 4) handles its own scroll + filter keys.
        if !matches!(app.overlay, Overlay::None) {
            handle_overlay_key(app, key.code)?;
            continue;
        }

        match key.code {
            KeyCode::Char('q') => return Ok(()),
            KeyCode::Esc => {
                // Settings → Properties / YAML editing: Esc returns to file
                // picker instead of quitting.
                if app.tab == TabId::Settings {
                    match app.yaml_view {
                        YamlView::Properties | YamlView::Editing { .. } => {
                            app.yaml_close();
                            continue;
                        }
                        YamlView::Files => {}
                    }
                }
                return Ok(());
            }
            KeyCode::Char('1') => app.switch_tab(TabId::Overview),
            KeyCode::Char('2') => app.switch_tab(TabId::Players),
            KeyCode::Char('3') => app.switch_tab(TabId::Worlds),
            KeyCode::Char('4') => app.switch_tab(TabId::Settings),
            KeyCode::Char('5') => app.switch_tab(TabId::Network),
            KeyCode::Tab => app.cycle_tab(1),
            KeyCode::BackTab => app.cycle_tab(-1),
            KeyCode::Char('r') => {
                app.refresh_all();
                if app.tab == TabId::Network {
                    app.refresh_natfrp();
                } else {
                    app.set_status(app.lang.s().refreshed);
                }
            }
            KeyCode::Up => app.move_selection(-1),
            KeyCode::Down => app.move_selection(1),
            KeyCode::Enter => match app.tab {
                TabId::Worlds => app.switch_world()?,
                TabId::Settings => match app.yaml_view.clone() {
                    YamlView::Files => {
                        // File picker — entry 0 is server.properties (open
                        // Properties view), entry 1+ are yaml files.
                        if let Some(idx) = app.yaml_files_state.selected() {
                            if idx == 0 {
                                app.yaml_view = YamlView::Properties;
                                if !app.properties.is_empty() {
                                    app.config_state.select(Some(0));
                                }
                            } else {
                                let yaml_idx = idx - 1;
                                if let Err(e) = app.yaml_open(yaml_idx) {
                                    app.set_status(format!("✗ {}", e));
                                }
                            }
                        }
                    }
                    YamlView::Properties => {
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
                TabId::Network => app.copy_selected_tunnel_address(),
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
            KeyCode::Char('d') => match app.tab {
                TabId::Players => app.remove_selected_player()?,
                TabId::Network => app.start_delete_tunnel(),
                _ => {}
            },
            KeyCode::Char('c') if app.tab == TabId::Network => {
                app.start_create_tunnel();
            }
            KeyCode::Char('m') if app.tab == TabId::Network => {
                app.start_migrate_tunnel();
            }
            KeyCode::Char('e') if app.tab == TabId::Network => {
                app.enable_selected_tunnel();
            }
            KeyCode::Char('x') if app.tab == TabId::Network => {
                app.disable_selected_tunnel();
            }
            KeyCode::Char('i') if app.tab == TabId::Network => {
                app.start_setup_wizard();
            }
            KeyCode::Char('o') => match app.tab {
                TabId::Players => app.toggle_op_for_selected()?,
                TabId::Network => app.open_natfrp_dashboard(),
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
            // Global lifecycle keys (work on any tab).
            KeyCode::Char('S') => app.start_server()?,
            KeyCode::Char('X') => app.stop_server()?,
            // v0.16 — uppercase R = restart now (top-level shortcut), B =
            // backup now. They re-use the same code paths as the (now-deleted)
            // Server tab actions, so behavior is identical to picking those
            // entries from the command palette.
            KeyCode::Char('R') => {
                if let Err(e) = app.restart_now() {
                    app.set_status(format!("✗ {}", e));
                }
            }
            KeyCode::Char('B') => {
                if let Err(e) = app.backup_now() {
                    app.set_status(format!("✗ {}", e));
                }
            }
            // Y = copy the share text (the multi-line "MC server / address /
            // send me your username" message host pastes into chat).
            KeyCode::Char('Y') => app.copy_share_text(),
            // Overlays.
            KeyCode::Char('?') => app.overlay = Overlay::Help,
            KeyCode::Char(':') => {
                let mut state = ListState::default();
                state.select(Some(0));
                app.overlay = Overlay::Palette { state };
            }
            KeyCode::Char('L') => {
                app.overlay = Overlay::Logs(LogsOverlay {
                    source: app.logs_view,
                    filter: LogsLevelFilter::All,
                    scroll_back: 0,
                });
            }
            // Language toggle moved from L → T to free L for the logs overlay.
            KeyCode::Char('T') => app.toggle_lang(),
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
            KeyCode::Char('t') if app.tab == TabId::Network => {
                let s = app.lang.s();
                app.prompt = Some(InputPrompt {
                    title: s.sf_prompt_token_title.into(),
                    label: s.sf_prompt_token_label.into(),
                    buffer: String::new(),
                    action: PromptAction::SetNatfrpToken,
                });
            }
            // A — manual SakuraFrp address fallback (used when the API/launcher
            // path isn't available but you have a tunnel address from the
            // SakuraFrp dashboard). Replaces the old Server-tab action of the
            // same name.
            KeyCode::Char('A') if app.tab == TabId::Network => {
                let s = app.lang.s();
                app.prompt = Some(InputPrompt {
                    title: s.frp_prompt_address_title.into(),
                    label: s.frp_prompt_address_label.into(),
                    buffer: app.sakurafrp_address.clone().unwrap_or_default(),
                    action: PromptAction::SetSakuraFrpAddress,
                });
            }
            // n — toggle the (collapsed by default) NIC list at the bottom of
            // the Network tab. ZeroTier / secondary VPN / Docker bridges are
            // there for the rare case the user wants a non-primary address.
            KeyCode::Char('n') if app.tab == TabId::Network => {
                app.network_show_nics = !app.network_show_nics;
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
    }
}

/// v0.16 — overlay key dispatcher. Called when an overlay is up; routes Esc /
/// nav / confirm / overlay-specific filters (logs source toggle, level
/// filter). Returns Ok unconditionally — errors from action execution are
/// surfaced through `set_status` rather than bubbled.
fn handle_overlay_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.overlay = Overlay::None;
            return Ok(());
        }
        KeyCode::Char('?') => {
            // ? toggles Help open/closed for parity with vim-style help keys.
            app.overlay = match std::mem::take(&mut app.overlay) {
                Overlay::Help => Overlay::None,
                _ => Overlay::Help,
            };
            return Ok(());
        }
        _ => {}
    }

    match &mut app.overlay {
        Overlay::None | Overlay::Help => {
            // Help has no extra interaction; everything else falls through.
        }
        Overlay::Palette { state } => {
            let len = PALETTE_COMMANDS.len();
            match code {
                KeyCode::Up => {
                    let cur = state.selected().unwrap_or(0) as isize;
                    let new = (cur - 1).rem_euclid(len as isize) as usize;
                    state.select(Some(new));
                }
                KeyCode::Down => {
                    let cur = state.selected().unwrap_or(0) as isize;
                    let new = (cur + 1).rem_euclid(len as isize) as usize;
                    state.select(Some(new));
                }
                KeyCode::Enter => {
                    if let Some(idx) = state.selected() {
                        if let Some(cmd) = PALETTE_COMMANDS.get(idx).copied() {
                            app.overlay = Overlay::None;
                            if let Err(e) = handle_server_action(app, cmd) {
                                app.set_status(format!("✗ {}", e));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Overlay::Logs(state) => {
            // Conservative page size; the renderer clamps to total when the
            // buffer is shorter than this.
            const PAGE: usize = 24;

            match code {
                KeyCode::Char('f') => {
                    state.source = match state.source {
                        LogsView::Server => LogsView::Frpc,
                        LogsView::Frpc => LogsView::Server,
                    };
                    // Source change → re-attach to tail so the user sees
                    // current activity rather than the same offset into a
                    // different log.
                    state.scroll_back = 0;
                }
                KeyCode::Char('1') => {
                    state.filter = LogsLevelFilter::All;
                    state.scroll_back = 0;
                }
                KeyCode::Char('2') => {
                    state.filter = LogsLevelFilter::Info;
                    state.scroll_back = 0;
                }
                KeyCode::Char('3') => {
                    state.filter = LogsLevelFilter::Warn;
                    state.scroll_back = 0;
                }
                KeyCode::Char('4') => {
                    state.filter = LogsLevelFilter::Error;
                    state.scroll_back = 0;
                }
                KeyCode::Up => state.scroll_back = state.scroll_back.saturating_add(1),
                KeyCode::Down => state.scroll_back = state.scroll_back.saturating_sub(1),
                KeyCode::PageUp => state.scroll_back = state.scroll_back.saturating_add(PAGE),
                KeyCode::PageDown => state.scroll_back = state.scroll_back.saturating_sub(PAGE),
                // Home → top of buffer. usize::MAX is a sentinel; renderer
                // clamps to actual `total - height` on the next frame.
                KeyCode::Home => state.scroll_back = usize::MAX,
                // End → re-attach to the tail.
                KeyCode::End => state.scroll_back = 0,
                _ => {}
            }
        }
    }
    Ok(())
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
            let copied = sys::clipboard_copy(&payload).is_ok();
            app.status = match (app.lang, copied) {
                (Lang::En, true) => format!("✓ Copied {} to clipboard", payload),
                (Lang::En, false) => format!("ℹ {} (clipboard unavailable)", payload),
                (Lang::Zh, true) => format!("✓ 已复制 {} 到剪贴板", payload),
                (Lang::Zh, false) => format!("ℹ {}（剪贴板不可用）", payload),
            };
            return;
        }
    }

    if rect_contains(app.tabs_rect, col, row) {
        // Tab title format must match `draw_tabs` exactly so click-zones line
        // up. v0.16: Tabs are rendered with a single-space divider and the
        // localized name (`tab_name(lang, id)`), and the title is " {idx} {name} ".
        // Use unicode-width to count display columns — CJK names render
        // 2 cols per char, so .chars().count() over-counts on EN, under-counts
        // on ZH.
        use unicode_width::UnicodeWidthStr;
        let inner_x = app.tabs_rect.x;
        if col < inner_x {
            return;
        }
        let dx = col - inner_x;
        let mut x: u16 = 0;
        let divider_w: u16 = 1; // matches `.divider(" ")` in draw_tabs
        for (i, (id, _en)) in TABS.iter().enumerate() {
            let title = format!(" {} {} ", i + 1, tab_name(app.lang, *id));
            let title_w = UnicodeWidthStr::width(title.as_str()) as u16;
            if dx >= x && dx < x + title_w {
                // Route through switch_tab so lazy-load (Network → SakuraFrp
                // API) fires the same way `1`..`5` keyboard shortcuts do.
                app.switch_tab(*id);
                return;
            }
            x = x + title_w + if i + 1 < TABS.len() { divider_w } else { 0 };
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
            picker,
            overlay,
        }) => {
            let server_dir = resolve_server_dir(cli.server_dir.clone())?;
            return render_screenshot(
                &server_dir,
                &tab,
                width,
                height,
                &lang,
                select,
                picker.as_deref(),
                overlay.as_deref(),
            );
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
    picker: Option<&str>,
    overlay: Option<&str>,
) -> Result<()> {
    use ratatui::backend::TestBackend;
    let lang = Lang::from_code(lang);
    let mut app = App::new_with_lang(server_dir.to_path_buf(), lang)?;
    app.tab = match tab.to_ascii_lowercase().as_str() {
        "overview" | "home" => TabId::Overview,
        "players" | "whitelist" | "ops" => TabId::Players,
        "worlds" => TabId::Worlds,
        // Aliases for old tab names — keep so existing screenshot scripts and
        // CI snapshots don't break across the v0.16 restructure.
        "settings" | "config" | "yaml" => TabId::Settings,
        "network" | "sakurafrp" | "frp" | "natfrp" => TabId::Network,
        // The old standalone tabs all roll into the four above; map them to
        // the most useful destination so QA scripts still work.
        "logs" | "backups" | "server" => TabId::Overview,
        other => anyhow::bail!("unknown tab: {}", other),
    };
    // Network tab fetches from the network on first visit. The screenshot
    // path bypasses switch_tab(), so trigger refresh_natfrp() explicitly here
    // — gives QA the same content the user would see after entering the tab.
    if app.tab == TabId::Network && app.natfrp_token.is_some() {
        app.refresh_natfrp();
    }
    // Allow QA to highlight a specific row to inspect its detail panel.
    let len = app.list_len_for(app.tab);
    if len > 0 {
        let idx = select.min(len - 1);
        let t = app.tab;
        app.list_state_for(t).select(Some(idx));
    }
    // v0.13 picker QA: open the node picker before rendering so its layout
    // can be inspected without firing destructive ops.
    if let Some(kind) = picker {
        let purpose = match kind.to_ascii_lowercase().as_str() {
            "create" => NodePickerPurpose::CreateTunnel {
                name: app.default_tunnel_name(),
            },
            "migrate" => {
                let (id, name) = app
                    .natfrp_tunnels
                    .first()
                    .map(|t| (t.id, t.name.clone()))
                    .unwrap_or((0, "—".to_string()));
                NodePickerPurpose::MigrateTunnel {
                    tunnel_id: id,
                    tunnel_name: name,
                }
            }
            other => anyhow::bail!("unknown picker kind: {}", other),
        };
        app.open_node_picker(purpose);
    }
    if let Some(kind) = overlay {
        app.overlay = match kind.to_ascii_lowercase().as_str() {
            "help" => Overlay::Help,
            "palette" => {
                let mut state = ListState::default();
                state.select(Some(0));
                Overlay::Palette { state }
            }
            "logs" => Overlay::Logs(LogsOverlay {
                source: LogsView::Server,
                filter: LogsLevelFilter::All,
                scroll_back: 0,
            }),
            other => anyhow::bail!("unknown overlay kind: {}", other),
        };
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
    fn classify_status_uses_leading_glyph() {
        assert_eq!(classify_status("✓ saved"), ToastKind::Ok);
        assert_eq!(classify_status("  ✓ saved"), ToastKind::Ok);
        assert_eq!(classify_status("✗ failed"), ToastKind::Err);
        assert_eq!(classify_status("⚠ heads up"), ToastKind::Warn);
        assert_eq!(classify_status("→ in progress"), ToastKind::Info);
        assert_eq!(classify_status("Refreshed."), ToastKind::Info);
        assert_eq!(classify_status(""), ToastKind::Info);
    }

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
            "shulker-test-{}",
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

    /// v0.15 — frpc_enabled_ids serializes as comma-separated u64 in state.toml.
    /// We can't safely mutate XDG_CONFIG_HOME from a parallel test (races with
    /// the natfrp_token test), so this just exercises the same parser shape
    /// the existing `persisted_state_roundtrip` test uses — string-only.
    #[test]
    fn frpc_enabled_ids_parser_drops_malformed_entries() {
        // Mirror the inline parser shape used by sys::read_persisted_state.
        let raw = "frpc_enabled_ids = \"27014725,not-a-number,27014726, ,9\"\n";
        let mut ids: Vec<u64> = Vec::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(eq) = line.find('=') {
                let k = line[..eq].trim();
                let v = line[eq + 1..].trim().trim_matches('"');
                if k == "frpc_enabled_ids" {
                    ids = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .filter_map(|s| s.parse().ok())
                        .collect();
                }
            }
        }
        assert_eq!(ids, vec![27014725, 27014726, 9]);
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
    fn property_zh_unknown_key_returns_none() {
        // Fallback path: unknown keys must return None so the UI shows the raw
        // key, not an empty annotation.
        assert!(property_zh("not-a-real-key").is_none());
    }

    /// v0.13 — node picker sort: game-friendly first, then VIP ascending,
    /// then id ascending. Important: a user with `vip=0` should see free
    /// nodes before paid ones, otherwise they pick a node they can't use.
    #[test]
    fn node_picker_sorts_game_then_vip_then_id() {
        use natfrp::Node;
        let mk = |id: u64, vip: u32, desc: &str| Node {
            name: format!("n{}", id),
            host: "h".into(),
            description: desc.into(),
            flag: 0,
            vip,
        };
        // Build a faux nodes map and sort it the way `open_node_picker` does.
        let nodes: HashMap<u64, Node> = HashMap::from([
            (10, mk(10, 1, "")),                  // non-game, vip 1
            (20, mk(20, 0, "")),                  // non-game, vip 0
            (30, mk(30, 0, "游戏专用")),          // game, vip 0
            (40, mk(40, 2, "Minecraft optimized")), // game, vip 2
            (50, mk(50, 0, "Game server")),       // game, vip 0 (later id)
        ]);
        let mut entries: Vec<(u64, bool, u32)> = nodes
            .iter()
            .map(|(id, n)| (*id, natfrp::is_game_node(n), n.vip))
            .collect();
        entries.sort_by(|a, b| {
            b.1.cmp(&a.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0))
        });
        let order: Vec<u64> = entries.into_iter().map(|(id, _, _)| id).collect();
        // Expected: [30 (game,0), 50 (game,0), 40 (game,2), 20 (non,0), 10 (non,1)]
        assert_eq!(order, vec![30, 50, 40, 20, 10]);
    }

    /// Default tunnel name normalizes hyphens (server_dir_slug uses `-` for
    /// non-alnum chars, but SakuraFrp rejects hyphens). The result must always
    /// pass validate_tunnel_name.
    #[test]
    fn default_tunnel_name_passes_validation() {
        use natfrp::validate_tunnel_name;
        // Simulate: server-dir basename → slug → mc_<slug-with-hyphens-replaced>
        let slug = sys::server_dir_slug(std::path::Path::new("/srv/My Cool Server"));
        let normalized = format!("mc_{}", slug.replace('-', "_"));
        assert!(
            validate_tunnel_name(&normalized),
            "default name '{}' must pass server validation",
            normalized
        );
        // Plain alnum slug should also work.
        let slug = sys::server_dir_slug(std::path::Path::new("/srv/mcserver"));
        assert!(validate_tunnel_name(&format!("mc_{}", slug.replace('-', "_"))));
    }

    /// v0.12 — every NatfrpError variant maps to a distinct, language-appropriate
    /// message, and the action hint (press t / disable proxy / wait + r) is
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
        // Network: should mention proxy as the most common workaround. The
        // exact phrasing is human-friendly — no specific tool names; the user
        // doesn't need to know which proxy is running to act on it.
        assert!(en_msgs[4].to_lowercase().contains("proxy"));
        assert!(zh_msgs[4].contains("代理"));
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
    fn console_session_name_stable_and_safe() {
        // Same dir → same name (so start and stop agree across calls).
        let a = console::Console::new(console::ConsoleRole::Server, Path::new("/mnt/data/mc-server"));
        let b = console::Console::new(console::ConsoleRole::Server, Path::new("/mnt/data/mc-server"));
        assert_eq!(a.session_name, b.session_name);
        // No characters tmux would choke on.
        let c = console::Console::new(console::ConsoleRole::Server, Path::new("/srv/MyServer 2024!"));
        for ch in c.session_name.chars() {
            assert!(ch.is_ascii_alphanumeric() || ch == '-', "bad char {:?} in {}", ch, c.session_name);
        }
        assert!(c.session_name.starts_with("shulker-"));
        // Server vs frpc share the same slug but have distinct prefixes —
        // they must never collide in tmux's session table.
        let frpc = console::Console::new(console::ConsoleRole::Frpc, Path::new("/mnt/data/mc-server"));
        assert_ne!(a.session_name, frpc.session_name);
        assert!(frpc.session_name.starts_with("shulker-frpc-"));
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
    fn property_metadata_unknown_returns_none() {
        assert!(property_metadata("not-a-real-key").is_none());
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
        scan_lines(&lines, 1_000_000, false, &mut acc);
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
        scan_lines(&lines, 1_700_000_000, false, &mut acc);
        assert_eq!(acc.last_denied_by_name.get("Urisaki").copied(), Some(1_700_000_000));
    }

    #[test]
    fn denied_logins_keep_latest_date() {
        let lines = vec![
            "[20:00:00] [Server thread/INFO]: Disconnecting Urisaki (/x): You are not whitelisted on this server!".to_string(),
        ];
        let mut acc = LogScanResult::default();
        scan_lines(&lines, 1_700_000_000, false, &mut acc);
        scan_lines(&lines, 1_700_086_400, false, &mut acc); // newer date
        scan_lines(&lines, 1_699_900_000, false, &mut acc); // older date — must NOT overwrite
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
        let players = scan_players(&dir, "world", &wl, &ops, false);
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
