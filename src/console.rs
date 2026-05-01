//! Cross-platform "console session" abstraction for the Minecraft server
//! and frpc subprocesses.
//!
//! ## Why this exists
//!
//! Pre-v0.17, shulker shelled out to `tmux` for both processes. That gave us:
//!
//! 1. The JVM survives shulker exit (tmux holds the session)
//! 2. We can write `stop` to the JVM's stdin via `tmux send-keys` — the only
//!    reliable Minecraft shutdown path (SIGTERM races with startup)
//! 3. The user can `tmux attach` from a second terminal to interact directly
//!
//! On Windows, none of those primitives exist out of the box. There's no
//! tmux, no `send-keys`, and no detached console with reattach. ConPTY (the
//! modern Windows pseudo-tty) gives us (2) cleanly via `portable-pty`, and
//! we accept losing (1) and (3) on Windows in exchange for shipping at all
//! — closing shulker on Windows ends the JVM, and there's no attach.
//! Surfaced clearly in the status bar so users aren't surprised.
//!
//! ## Backends
//!
//! - **Unix (Linux/macOS):** thin wrapper around `tmux` exactly like
//!   pre-v0.17. `Console` holds nothing but the session name and cwd; every
//!   operation re-spawns tmux. `tmux` must be on `$PATH` (macOS users:
//!   `brew install tmux`).
//!
//! - **Windows:** `portable-pty::native_pty_system()` opens a ConPTY pair.
//!   The child runs under the master; a background thread drains stdout
//!   into a ring buffer (`MAX_BUFFER` bytes) so the Logs tab can paint it.
//!   `send_line` writes to the master writer; `stop` sends "stop\r\n" and
//!   waits up to 30s for graceful exit before force-killing.

use std::path::{Path, PathBuf};

use anyhow::Result;

#[cfg(windows)]
use std::sync::{Arc, Mutex};

/// Max bytes retained in the per-console scrollback buffer (Windows only;
/// Unix backend reads from tmux's own pane history). 256 KiB ≈ 2-3 thousand
/// log lines, plenty for the Logs tab.
#[cfg(windows)]
const MAX_BUFFER: usize = 256 * 1024;

// ---------- Common type ----------

pub struct Console {
    /// e.g. "shulker-myserver" (server) or "shulker-frpc-myserver" (frpc).
    /// Used as the tmux session name on Unix and as a stable identifier on
    /// Windows. Derived from the server-dir slug + role prefix.
    pub session_name: String,
    pub cwd: PathBuf,
    #[cfg(windows)]
    state: Option<WindowsState>,
}

#[cfg(windows)]
struct WindowsState {
    /// Owns the master end of the ConPTY pair. Drop = close child stdio.
    /// We never read the master directly (the reader thread cloned a reader
    /// off it), but we hold it so the master stays open as long as the
    /// state does.
    #[allow(dead_code)]
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// Live child process handle (None after wait/kill returns).
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Master writer, kept separate so `send_line` doesn't borrow `master`.
    writer: Box<dyn std::io::Write + Send>,
    /// Rolling stdout buffer fed by the reader thread. Trimmed to MAX_BUFFER.
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl Console {
    pub fn new(role: ConsoleRole, server_dir: &Path) -> Self {
        let slug = crate::sys::server_dir_slug(server_dir);
        let session_name = match role {
            ConsoleRole::Server => format!("shulker-{}", slug),
            ConsoleRole::Frpc => format!("shulker-frpc-{}", slug),
        };
        Self {
            session_name,
            cwd: server_dir.to_path_buf(),
            #[cfg(windows)]
            state: None,
        }
    }

    /// Linux/macOS: run `tmux attach -t <session>` to interact directly.
    /// Windows: no attach (closed PTY can't be reattached); returns `None`.
    pub fn attach_command(&self) -> Option<String> {
        if cfg!(unix) {
            Some(format!("tmux attach -t {}", self.session_name))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConsoleRole {
    Server,
    Frpc,
}

// ---------- Unix backend (tmux) ----------

#[cfg(unix)]
impl Console {
    /// True if the underlying tmux session exists. Takes `&mut self` so the
    /// signature matches the Windows backend (which has to poll `try_wait`
    /// against owned state).
    pub fn is_alive(&mut self) -> bool {
        crate::sys::tmux_session_alive(&self.session_name)
    }

    /// Spawn `argv` in a new detached tmux session. tmux is given the joined
    /// `argv` (each token shell-quoted) as the session shell-command, which
    /// it passes to `/bin/sh -c`. Caller is responsible for refusing
    /// double-start; we no-op if alive.
    pub fn start(&mut self, argv: &[String]) -> Result<()> {
        if self.is_alive() {
            return Ok(());
        }
        let shell_cmd: String = argv
            .iter()
            .map(|a| crate::sys::shell_quote_sh(a))
            .collect::<Vec<_>>()
            .join(" ");
        use std::process::{Command, Stdio};
        let status = Command::new("tmux")
            .arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(&self.session_name)
            .arg("-c")
            .arg(&self.cwd)
            .arg(&shell_cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux new-session failed for {}", self.session_name);
        }
        Ok(())
    }

    /// Send `text` followed by Enter to the session's pane.
    pub fn send_line(&mut self, text: &str) -> Result<()> {
        use std::process::{Command, Stdio};
        let status = Command::new("tmux")
            .args(["send-keys", "-t", &self.session_name, text, "Enter"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux send-keys failed");
        }
        Ok(())
    }

    /// Last `lines` rows from the pane's scrollback. Mirrors the v0.16
    /// `data::tmux_capture_pane` helper. Borrowed shared (`&self`) so the
    /// UI render pass can call this from a `&App` context.
    pub fn capture_recent(&self, lines: u32) -> Result<String> {
        use std::process::Command;
        let start = format!("-{}", lines);
        let out = Command::new("tmux")
            .args(["capture-pane", "-t", &self.session_name, "-p", "-S", &start])
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("tmux capture-pane failed: {}", stderr.trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// Send a graceful shutdown command and forget. Caller is expected to
    /// poll `is_alive` until false (or its sibling pid detector). For the MC
    /// server this is `send_line("stop")`; for frpc, just `kill_session()`.
    pub fn stop_graceful(&mut self, command: &str) -> Result<()> {
        if !self.is_alive() {
            return Ok(());
        }
        self.send_line(command)
    }

    /// Hard-kill the session. Used as a backstop when `stop_graceful` times
    /// out, and as the only path for frpc (no in-process "stop" command).
    pub fn kill_session(&mut self) -> Result<()> {
        if !self.is_alive() {
            return Ok(());
        }
        use std::process::{Command, Stdio};
        let status = Command::new("tmux")
            .args(["kill-session", "-t", &self.session_name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux kill-session failed");
        }
        Ok(())
    }
}

// ---------- Windows backend (ConPTY via portable-pty) ----------

#[cfg(windows)]
impl Console {
    pub fn is_alive(&mut self) -> bool {
        let Some(state) = self.state.as_mut() else { return false };
        match state.child.try_wait() {
            Ok(None) => true,           // still running
            Ok(Some(_)) | Err(_) => {   // exited or handle gone
                self.state = None;
                false
            }
        }
    }

    /// Spawn `argv[0]` with the rest of `argv` as arguments. Unlike the Unix
    /// path we don't go through a shell — Windows users don't have one in a
    /// portable form. Wrap in `cmd.exe /C ...` upstream if you need shell
    /// expansion.
    pub fn start(&mut self, argv: &[String]) -> Result<()> {
        if self.state.is_some() {
            return Ok(());
        }
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(portable_pty::PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow::anyhow!("openpty: {}", e))?;
        let mut cmd = portable_pty::CommandBuilder::new(&argv[0]);
        for arg in &argv[1..] {
            cmd.arg(arg);
        }
        cmd.cwd(&self.cwd);
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow::anyhow!("spawn_command: {}", e))?;
        // The slave handle isn't needed once the child is spawned; dropping
        // it lets the master's read side hit EOF when the child exits.
        drop(pair.slave);
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| anyhow::anyhow!("take_writer: {}", e))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| anyhow::anyhow!("try_clone_reader: {}", e))?;
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(MAX_BUFFER)));
        let buf_for_thread = Arc::clone(&buffer);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut buf = buf_for_thread.lock().unwrap();
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > MAX_BUFFER {
                            let drop = buf.len() - MAX_BUFFER;
                            buf.drain(..drop);
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        self.state = Some(WindowsState {
            master: pair.master,
            child,
            writer,
            buffer,
        });
        Ok(())
    }

    pub fn send_line(&mut self, text: &str) -> Result<()> {
        let Some(state) = self.state.as_mut() else {
            anyhow::bail!("console not running");
        };
        use std::io::Write;
        // Minecraft's console reads CRLF on Windows; UNIX-style "\n" works on
        // ConPTY too but CRLF is the safe default for parity with cmd.exe.
        state.writer.write_all(text.as_bytes())?;
        state.writer.write_all(b"\r\n")?;
        state.writer.flush()?;
        Ok(())
    }

    pub fn capture_recent(&self, _lines: u32) -> Result<String> {
        let Some(state) = self.state.as_ref() else {
            return Ok(String::new());
        };
        let buf = state.buffer.lock().unwrap();
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    pub fn stop_graceful(&mut self, command: &str) -> Result<()> {
        if self.state.is_none() {
            return Ok(());
        }
        self.send_line(command)
    }

    pub fn kill_session(&mut self) -> Result<()> {
        if let Some(mut state) = self.state.take() {
            // Best-effort SIGKILL equivalent. Drop closes master/writer.
            let _ = state.child.kill();
            let _ = state.child.wait();
        }
        Ok(())
    }
}
