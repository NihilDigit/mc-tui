//! System / OS helpers: state.toml persistence, tmux session helpers,
//! shell-escape, path expansion, directory helpers.
//!
//! No business logic — just functions that wrap OS / process / filesystem
//! conventions for reuse from the rest of the crate.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

// ---------- Persistent state (state.toml) ----------

/// OS-native per-user config dir for shulker. Maps to:
///   - Linux:   `$XDG_CONFIG_HOME/shulker` (or `~/.config/shulker`)
///   - macOS:   `~/Library/Application Support/shulker`
///   - Windows: `%APPDATA%\shulker`
///
/// Backed by the `dirs` crate, which is the standard `BaseDirs::config_dir`
/// implementation. Falls back to `.shulker` in CWD if `dirs` can't resolve a
/// home (containers without `$HOME`, broken `passwd` lookups, etc.).
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .map(|p| p.join("shulker"))
        .unwrap_or_else(|| PathBuf::from(".shulker"))
}

pub fn state_path() -> PathBuf {
    config_dir().join("state.toml")
}

// ---------- SakuraFrp API token ----------
//
// Stored separate from state.toml because it's a user credential bound to the
// account, not to the server-dir, and should be readable only by the owner.

pub fn natfrp_token_path() -> PathBuf {
    config_dir().join("natfrp.token")
}

pub fn read_natfrp_token() -> Option<String> {
    let raw = fs::read_to_string(natfrp_token_path()).ok()?;
    let s = raw.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

pub fn write_natfrp_token(token: &str) -> Result<()> {
    let path = natfrp_token_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&path, token.trim())
        .with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms)?;
    }
    Ok(())
}

#[derive(Debug, Default, Clone)]
pub struct PersistedState {
    pub server_dir: Option<PathBuf>,
    pub lang: Option<String>,
    /// SakuraFrp tunnel public address (e.g. `frp-way.com:36192`). User-set
    /// via the Server tab prompt; rendered in join-info, click-to-copy.
    pub sakurafrp_address: Option<String>,
    /// SakuraFrp launcher Docker container name. v0.9-v0.14.1 used this to
    /// manage the launcher container; v0.15 runs frpc directly. Field kept
    /// so existing state.toml files don't error on read.
    pub sakurafrp_container: Option<String>,
    /// v0.15 — comma-separated tunnel ids passed to `frpc -f`. The list of
    /// tunnels shulker auto-starts when the user runs frpc; toggled by `e`/`x`.
    pub frpc_enabled_ids: Vec<u64>,
}

pub fn read_persisted_state() -> PersistedState {
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
                "sakurafrp_address" => state.sakurafrp_address = Some(v),
                "sakurafrp_container" => state.sakurafrp_container = Some(v),
                "frpc_enabled_ids" => {
                    // Comma-separated u64 list, e.g. "27014725,27014726".
                    // Silently drops malformed entries — better than failing
                    // the whole state load over a single typo.
                    state.frpc_enabled_ids = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .filter_map(|s| s.parse().ok())
                        .collect();
                }
                _ => {}
            }
        }
    }
    state
}

pub fn write_persisted_state(state: &PersistedState) -> Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let mut s = String::from("# shulker state — auto-managed, hand-edit at your own risk.\n");
    if let Some(dir) = &state.server_dir {
        s.push_str(&format!("server_dir = \"{}\"\n", dir.display()));
    }
    if let Some(lang) = &state.lang {
        s.push_str(&format!("lang = \"{}\"\n", lang));
    }
    if let Some(addr) = &state.sakurafrp_address {
        s.push_str(&format!("sakurafrp_address = \"{}\"\n", addr));
    }
    if let Some(c) = &state.sakurafrp_container {
        s.push_str(&format!("sakurafrp_container = \"{}\"\n", c));
    }
    if !state.frpc_enabled_ids.is_empty() {
        let joined: Vec<String> = state.frpc_enabled_ids.iter().map(u64::to_string).collect();
        s.push_str(&format!("frpc_enabled_ids = \"{}\"\n", joined.join(",")));
    }
    fs::write(&path, s).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

// ---------- Path / shell / tmux helpers ----------

pub fn parse_hh_mm(s: &str) -> Option<(u8, u8)> {
    let s = s.trim();
    let mut parts = s.splitn(2, ':');
    let h: u8 = parts.next()?.parse().ok()?;
    let m: u8 = parts.next()?.parse().ok()?;
    if h >= 24 || m >= 60 {
        return None;
    }
    Some((h, m))
}

/// POSIX-shell-safe single-quote of `s`. tmux `new-session [shell-command]`
/// passes its command string to `/bin/sh -c`, so any path containing whitespace,
/// quotes, `$`, backticks, etc. would otherwise break. Unix-only: the Windows
/// console backend talks to ConPTY directly with an argv array and never
/// needs to round-trip through a shell.
#[cfg(unix)]
pub fn shell_quote_sh(s: &str) -> String {
    // `'` inside a single-quoted string is closed with `'`, escaped as `\'`,
    // then re-opened with `'`. Empty input → `''`.
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | ','));
    if safe {
        return s.to_string();
    }
    let mut out = String::from("'");
    for c in s.chars() {
        if c == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// True if tmux reports a session by this name. Used by the Unix console
/// backend; Windows ConPTY tracks its own child handle so this isn't
/// referenced there.
#[cfg(unix)]
pub fn tmux_session_alive(name: &str) -> bool {
    use std::process::{Command, Stdio};
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn server_dir_slug(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("server")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect()
}

// ---------- Clipboard / browser helpers ----------
//
// Cross-platform replacements for the v0.15-era `wl-copy` / `xdg-open`
// shell-outs. arboard handles Linux (X11 + Wayland), macOS (NSPasteboard),
// and Windows (OpenClipboard); webbrowser handles xdg-open / open / ShellExecute.
// Both fail cleanly on headless / SSH-only hosts — callers must surface the
// `Err` to the user instead of unwrapping.

/// Copy `text` to the system clipboard. Returns `Ok(())` on success; `Err`
/// when no clipboard is available (typical SSH session, no X11/Wayland, etc.).
/// Callers turn the error into a "copy failed — copy manually" status hint.
pub fn clipboard_copy(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new()
        .with_context(|| "open system clipboard")?;
    clipboard
        .set_text(text.to_string())
        .with_context(|| "write to clipboard")?;
    Ok(())
}

/// Open `url` in the user's default browser. Returns `Ok(())` on success;
/// `Err` when no browser is available (headless, missing xdg-open, etc.).
pub fn open_url(url: &str) -> Result<()> {
    webbrowser::open(url).with_context(|| format!("open {} in browser", url))?;
    Ok(())
}

pub fn expand_tilde(p: &str) -> PathBuf {
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
