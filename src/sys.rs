//! System / OS helpers: state.toml persistence, tmux session helpers,
//! shell-escape, path expansion, directory helpers.
//!
//! No business logic — just functions that wrap OS / process / filesystem
//! conventions for reuse from the rest of the crate.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

// ---------- Persistent state (state.toml) ----------

pub fn config_dir() -> PathBuf {
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
    /// SakuraFrp launcher Docker container name. Defaults to `natfrp-service`
    /// (matches the official launcher image's typical name). Used by Server
    /// tab actions that probe / start / stop / restart the container.
    pub sakurafrp_container: Option<String>,
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
    let mut s = String::from("# mc-tui state — auto-managed, hand-edit at your own risk.\n");
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

/// Stable tmux session name keyed off the server-dir basename.
/// Same dir → same session every time, so `start` / `stop` find the same place.
pub fn tmux_session_name(server_dir: &Path) -> String {
    format!("mc-tui-{}", server_dir_slug(server_dir))
}

/// POSIX-shell-safe single-quote of `s`. tmux `new-session [shell-command]`
/// passes its command string to `/bin/sh -c`, so any path containing whitespace,
/// quotes, `$`, backticks, etc. would otherwise break.
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

pub fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(prog);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
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
