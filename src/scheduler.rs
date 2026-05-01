//! Cross-platform "run this command daily at HH:MM" scheduling.
//!
//! Three backends keyed off the host OS:
//!
//! - **Linux:** writes a `systemd --user` `.service` + `.timer` pair to
//!   `~/.config/systemd/user/`. The user runs `systemctl --user daemon-reload
//!   && systemctl --user enable --now <unit>.timer` to activate. v1 leaves
//!   activation manual so we don't need to fork systemctl.
//!
//! - **macOS:** writes a launchd `.plist` to `~/Library/LaunchAgents/` with
//!   `StartCalendarInterval` for the daily slot. We attempt
//!   `launchctl bootstrap gui/<uid> <path>` to load it; if that fails we
//!   surface the manual command.
//!
//! - **Windows:** invokes `schtasks /Create /SC DAILY /ST HH:MM /TN ... /F`.
//!   No file written; Task Scheduler keeps its own state. Forced create
//!   (`/F`) overwrites a same-named task so re-scheduling is idempotent.
//!
//! All three backends return a [`ScheduleOutcome`] with the exact follow-up
//! command (or "already activated" notice) to surface in the status bar,
//! matching the v0.6 UX where shulker prints next-step instructions instead
//! of trying to be clever.
//!
//! Unschedule is intentionally not implemented — the v1 UX is to print the
//! exact remove command for the user to copy.

use std::path::Path;

use anyhow::{Context, Result};

use crate::sys::server_dir_slug;

#[derive(Debug, Clone, Copy)]
pub enum JobKind {
    Restart,
    Backup,
}

impl JobKind {
    fn slug_prefix(self) -> &'static str {
        match self {
            JobKind::Restart => "shulker-restart",
            JobKind::Backup => "shulker-backup",
        }
    }

    /// Human-readable description embedded in unit/plist metadata.
    /// Only the Unix backends use this; Windows' schtasks does not take a
    /// description through the CLI invocation we use.
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    fn description(self) -> &'static str {
        match self {
            JobKind::Restart => "shulker daily restart",
            JobKind::Backup => "shulker daily backup",
        }
    }
}

/// What the user sees after scheduling. Two flavors: backends that just
/// register the job (Windows) report `Activated`; backends that need a
/// manual follow-up (Linux, macOS) report `WroteFile` with the activation
/// command. `unschedule_command` is preserved on each variant so a future
/// `App::unschedule_daily` UX can read it back; v1 only renders it for the
/// user to copy.
#[allow(dead_code)]
#[derive(Debug)]
pub enum ScheduleOutcome {
    /// Job is live in the OS scheduler. `unschedule_command` is what the
    /// user copies to remove it later.
    Activated { unschedule_command: String },
    /// Backend wrote a file and now needs the user to run an activation
    /// command. (Linux: systemctl --user enable --now; macOS: launchctl bootstrap.)
    WroteFile {
        path: String,
        activate_command: String,
        unschedule_command: String,
    },
}

/// Schedule `kind` for `server_dir` to fire daily at `hh:mm` (local time).
pub fn schedule_daily(
    kind: JobKind,
    server_dir: &Path,
    hour: u8,
    minute: u8,
) -> Result<ScheduleOutcome> {
    let slug = server_dir_slug(server_dir);
    let job_name = format!("{}-{}", kind.slug_prefix(), slug);

    #[cfg(target_os = "linux")]
    {
        return linux::schedule(kind, server_dir, hour, minute, &job_name);
    }
    #[cfg(target_os = "macos")]
    {
        return macos::schedule(kind, server_dir, hour, minute, &job_name);
    }
    #[cfg(target_os = "windows")]
    {
        return windows::schedule(kind, server_dir, hour, minute, &job_name);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (kind, server_dir, hour, minute, job_name);
        anyhow::bail!("scheduling is not supported on this platform")
    }
}

// ---------- Linux: systemd --user ----------

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs;

    pub fn schedule(
        kind: JobKind,
        server_dir: &Path,
        hour: u8,
        minute: u8,
        job_name: &str,
    ) -> Result<ScheduleOutcome> {
        // We deliberately keep the Linux ExecStart shape unchanged from the
        // pre-v0.17 implementation — same restart fallback (stop.sh → pkill),
        // same setsid wrapper. shulker itself launches via tmux, but the
        // systemd job runs *outside* shulker's process tree so the older
        // recipe (which doesn't depend on tmux being on the user's PATH at
        // unit-load time) is the right tool here.
        let cwd = format!("{:?}", server_dir);
        let exec_start = match kind {
            JobKind::Restart => format!(
                "/usr/bin/env bash -c 'cd {0} && (test -x ./stop.sh && ./stop.sh || pkill -TERM -f \"java.*paper\\|purpur\"; sleep 30; setsid bash {0}/start.sh)'",
                cwd
            ),
            JobKind::Backup => format!("/usr/bin/env bash {}/backup.sh", cwd),
        };

        let unit_dir = crate::sys::config_dir()
            .parent()
            .unwrap_or(Path::new("."))
            .join("systemd")
            .join("user");
        fs::create_dir_all(&unit_dir)
            .with_context(|| format!("create {}", unit_dir.display()))?;
        let service = format!(
            "[Unit]\nDescription={desc}\n\n[Service]\nType=oneshot\nWorkingDirectory={cwd}\nExecStart={cmd}\n",
            desc = kind.description(),
            cwd = cwd,
            cmd = exec_start
        );
        let timer = format!(
            "[Unit]\nDescription={desc} timer\n\n[Timer]\nOnCalendar=*-*-* {h:02}:{m:02}:00\nPersistent=true\nUnit={name}.service\n\n[Install]\nWantedBy=timers.target\n",
            desc = kind.description(),
            h = hour,
            m = minute,
            name = job_name
        );
        let svc_path = unit_dir.join(format!("{}.service", job_name));
        let tim_path = unit_dir.join(format!("{}.timer", job_name));
        fs::write(&svc_path, service).context("write .service")?;
        fs::write(&tim_path, timer).context("write .timer")?;
        Ok(ScheduleOutcome::WroteFile {
            path: svc_path.display().to_string(),
            activate_command: format!(
                "systemctl --user daemon-reload && systemctl --user enable --now {}.timer",
                job_name
            ),
            unschedule_command: format!(
                "systemctl --user disable --now {0}.timer && rm {1} {2}",
                job_name,
                svc_path.display(),
                tim_path.display()
            ),
        })
    }
}

// ---------- macOS: launchd ----------

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::fs;
    use std::process::Command;

    pub fn schedule(
        kind: JobKind,
        server_dir: &Path,
        hour: u8,
        minute: u8,
        job_name: &str,
    ) -> Result<ScheduleOutcome> {
        let label = format!("com.{}", job_name);
        let agents_dir = dirs::home_dir()
            .context("resolve $HOME")?
            .join("Library")
            .join("LaunchAgents");
        fs::create_dir_all(&agents_dir)
            .with_context(|| format!("create {}", agents_dir.display()))?;
        let plist_path = agents_dir.join(format!("{}.plist", label));

        let cwd = server_dir.display().to_string();
        // launchd runs `ProgramArguments` directly (no shell), so we wrap
        // pipelines in `/bin/bash -c` exactly like the Linux ExecStart does.
        let bash_command = match kind {
            JobKind::Restart => format!(
                "cd {0} && (test -x ./stop.sh && ./stop.sh || pkill -TERM -f \"java.*paper\\|purpur\"; sleep 30; bash {0}/start.sh)",
                cwd
            ),
            JobKind::Backup => format!("bash {}/backup.sh", cwd),
        };
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>-c</string>
        <string>{cmd}</string>
    </array>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key><integer>{hour}</integer>
        <key>Minute</key><integer>{minute}</integer>
    </dict>
    <key>WorkingDirectory</key><string>{cwd}</string>
    <key>RunAtLoad</key><false/>
</dict>
</plist>
"#,
            label = label,
            cmd = xml_escape(&bash_command),
            hour = hour,
            minute = minute,
            cwd = xml_escape(&cwd),
        );
        fs::write(&plist_path, plist)
            .with_context(|| format!("write {}", plist_path.display()))?;

        // Best-effort `launchctl bootstrap`. If it fails (already loaded,
        // missing tool, sandboxing), surface the activation command.
        let uid = unsafe { libc_getuid() };
        let domain = format!("gui/{}", uid);
        let bootstrap = Command::new("launchctl")
            .args(["bootstrap", &domain])
            .arg(&plist_path)
            .status();
        let activated = matches!(bootstrap, Ok(s) if s.success());
        if activated {
            Ok(ScheduleOutcome::Activated {
                unschedule_command: format!(
                    "launchctl bootout {0}/{1} && rm {2}",
                    domain,
                    label,
                    plist_path.display()
                ),
            })
        } else {
            Ok(ScheduleOutcome::WroteFile {
                path: plist_path.display().to_string(),
                activate_command: format!(
                    "launchctl bootstrap {0} {1}",
                    domain,
                    plist_path.display()
                ),
                unschedule_command: format!(
                    "launchctl bootout {0}/{1} && rm {2}",
                    domain,
                    label,
                    plist_path.display()
                ),
            })
        }
    }

    /// Tiny XML escape — the bash command and cwd are the only places we
    /// inject untrusted strings. `&`/`<`/`>` are the realistic pitfalls
    /// (server paths almost never carry quotes, but cover those too).
    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    /// libc::getuid() — pulling the libc crate just for this is overkill;
    /// extern via std's fallback link group instead.
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe fn libc_getuid() -> u32 { getuid() }
}

// ---------- Windows: schtasks ----------

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use std::process::Command;

    pub fn schedule(
        kind: JobKind,
        server_dir: &Path,
        hour: u8,
        minute: u8,
        job_name: &str,
    ) -> Result<ScheduleOutcome> {
        let cwd = server_dir.display().to_string();
        // schtasks /TR takes a single command line — wrap in cmd.exe /C so
        // we can chain a `cd /d`. For backup, just point at backup.bat.
        let task_run = match kind {
            JobKind::Restart => format!(
                "cmd.exe /C \"cd /d \"{0}\" && start.bat\"",
                cwd
            ),
            JobKind::Backup => format!(
                "cmd.exe /C \"cd /d \"{0}\" && backup.bat\"",
                cwd
            ),
        };
        let st = format!("{:02}:{:02}", hour, minute);
        let status = Command::new("schtasks")
            .args(&[
                "/Create",
                "/SC",
                "DAILY",
                "/ST",
                &st,
                "/TN",
                job_name,
                "/TR",
                &task_run,
                "/F",
            ])
            .status()
            .with_context(|| "spawn schtasks /Create")?;
        if !status.success() {
            anyhow::bail!("schtasks /Create exited with {:?}", status.code());
        }
        Ok(ScheduleOutcome::Activated {
            unschedule_command: format!("schtasks /Delete /TN \"{}\" /F", job_name),
        })
    }
}
