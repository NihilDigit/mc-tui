//! `mc-tui new <dir>` subcommand: scaffold a fresh Paper/Purpur server.
//!
//! Detects Java version, fetches the latest jar via the upstream API, writes
//! `eula.txt` + `start.sh` (with Aikar's flags + RAM-aware heap), and
//! optionally first-boots to generate `server.properties`.
//!
//! `Cli` / `Cmd` / `ServerType` live here because they're the CLI surface.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;

use crate::fmt_bytes;

#[derive(Parser, Debug)]
#[command(name = "mc-tui", about, version)]
pub struct Cli {
    /// Path to the Minecraft server directory (must contain server.properties).
    /// If omitted, falls back to the value remembered in $XDG_CONFIG_HOME/mc-tui/state.toml.
    #[arg(short = 'd', long, env = "MC_SERVER_DIR", global = true)]
    pub server_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(clap::Subcommand, Debug)]
pub enum Cmd {
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
        /// Tab to render: worlds | whitelist | ops | config | logs | yaml | backups | server.
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
        /// 0-based index of the row to highlight (for detail-panel QA).
        #[arg(long, default_value_t = 0)]
        select: usize,
        /// v0.13 — open the node picker overlay before rendering. Values:
        /// `create` (CreateTunnel purpose) | `migrate` (MigrateTunnel purpose).
        /// Useful for QA-ing the picker without building a write-side flow.
        #[arg(long)]
        picker: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ServerType {
    Paper,
    Purpur,
}

impl ServerType {
    pub fn name(self) -> &'static str {
        match self {
            ServerType::Paper => "paper",
            ServerType::Purpur => "purpur",
        }
    }
}


// ---------- v0.4: server scaffolder (`mc-tui new <dir>`) ----------

pub fn scaffold_new(
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


// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;

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
    fn build_download_url_for_purpur_is_simple() {
        let url = build_download_url(ServerType::Purpur, "1.21.4").unwrap();
        assert_eq!(url, "https://api.purpurmc.org/v2/purpur/1.21.4/latest/download");
    }
}
