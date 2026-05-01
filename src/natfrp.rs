//! SakuraFrp REST API client (v4). All calls are blocking — mc-tui has no
//! async runtime. Each call is a single HTTP round-trip; we do not stream or
//! poll. Caller is expected to call only on user-initiated refresh, never on
//! every render frame.
//!
//! Schema below is verified against live `api.natfrp.com/v4` responses on
//! 2026-05-01 — fields are what the server actually returns, not OpenAPI guesses.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

const API_BASE: &str = "https://api.natfrp.com/v4";

pub struct Client {
    token: String,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(token: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(8))
            .build();
        Self { token, agent }
    }

    fn get_text(&self, path: &str) -> Result<String> {
        let url = format!("{}{}", API_BASE, path);
        let resp = self
            .agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .call()
            .with_context(|| format!("GET {}", path))?;
        resp.into_string().with_context(|| format!("read body {}", path))
    }

    pub fn user_info(&self) -> Result<UserInfo> {
        let body = self.get_text("/user/info")?;
        parse_user_info(&body)
    }

    pub fn tunnels(&self) -> Result<Vec<Tunnel>> {
        let body = self.get_text("/tunnels")?;
        parse_tunnels(&body)
    }

    pub fn nodes(&self) -> Result<HashMap<u64, Node>> {
        let body = self.get_text("/nodes")?;
        parse_nodes(&body)
    }

    /// Map of unix-epoch-seconds → bytes used in that bucket. Caller sums or
    /// picks the latest depending on what they want to display.
    pub fn tunnel_traffic(&self, id: u64) -> Result<HashMap<u64, u64>> {
        let body = self.get_text(&format!("/tunnel/traffic?id={}", id))?;
        parse_tunnel_traffic(&body)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserInfo {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub speed: String,
    #[serde(default)]
    pub tunnels: u32,
    #[serde(default)]
    pub group: UserGroup,
    /// `[used_bytes, total_bytes]` for the user's traffic plan.
    #[serde(default)]
    pub traffic: Vec<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserGroup {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub level: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // note/local_ip/local_port/etc surfaced in v0.11+ tunnel-edit UI
pub struct Tunnel {
    pub id: u64,
    pub name: String,
    pub node: u64,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub local_ip: String,
    #[serde(default)]
    pub local_port: u16,
    #[serde(default)]
    pub remote: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // description/flag surfaced in v0.11 node picker
pub struct Node {
    pub name: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub description: String,
    /// Bitmask. We don't yet know every meaning; we just surface "game-friendly"
    /// nodes by looking at the description string for now.
    #[serde(default)]
    pub flag: u32,
}

pub fn parse_user_info(body: &str) -> Result<UserInfo> {
    serde_json::from_str(body).with_context(|| "parse /user/info")
}

pub fn parse_tunnels(body: &str) -> Result<Vec<Tunnel>> {
    serde_json::from_str(body).with_context(|| "parse /tunnels")
}

pub fn parse_nodes(body: &str) -> Result<HashMap<u64, Node>> {
    let raw: HashMap<String, Node> =
        serde_json::from_str(body).with_context(|| "parse /nodes")?;
    let mut out = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        let id: u64 = k.parse().map_err(|_| anyhow!("non-numeric node id: {}", k))?;
        out.insert(id, v);
    }
    Ok(out)
}

#[allow(dead_code)] // exposed via Client::tunnel_traffic for v0.10 MTD usage; kept for v0.11
pub fn parse_tunnel_traffic(body: &str) -> Result<HashMap<u64, u64>> {
    let raw: HashMap<String, u64> =
        serde_json::from_str(body).with_context(|| "parse /tunnel/traffic")?;
    let mut out = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        let ts: u64 = k.parse().map_err(|_| anyhow!("non-numeric ts: {}", k))?;
        out.insert(ts, v);
    }
    Ok(out)
}

/// Public address for a tunnel, suitable for the join bar / clipboard.
/// Returns `None` when we can't compose one (missing host or remote port).
pub fn public_address(t: &Tunnel, nodes: &HashMap<u64, Node>) -> Option<String> {
    let node = nodes.get(&t.node)?;
    if node.host.is_empty() || t.remote.is_empty() {
        return None;
    }
    Some(format!("{}:{}", node.host, t.remote))
}

/// Pretty label for a node — `"#218 镇江多线PLUS-扩容1"`. Falls back to the bare id
/// when the nodes map doesn't have it (cache miss).
pub fn node_label(node_id: u64, nodes: &HashMap<u64, Node>) -> String {
    match nodes.get(&node_id) {
        Some(n) => format!("#{} {}", node_id, n.name),
        None => format!("#{}", node_id),
    }
}

/// Human-readable byte count: `"1.2 GB"` / `"512 MB"` / `"42 KB"` / `"7 B"`.
pub fn fmt_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;
    if n >= TB {
        format!("{:.2} TB", n as f64 / TB as f64)
    } else if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{} KB", n / KB)
    } else {
        format!("{} B", n)
    }
}

/// First 4 chars of the token followed by `****`. For UI display only — never
/// log the full token.
pub fn redact_token(token: &str) -> String {
    let prefix: String = token.chars().take(4).collect();
    format!("{}****", prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_USER: &str = r#"{"id":12345,"name":"sample-user","avatar":"https://x","token":"FAKE_TOKEN_FOR_TESTS","speed":"10 Mbps","tunnels":2,"realname":2,"group":{"name":"普通用户","level":0,"expires":0},"traffic":[8449054,14057568840],"sign":{"config":[1,4],"signed":true,"last":"2026-05-01","days":5,"traffic":14.8},"bandwidth":null}"#;

    const SAMPLE_TUNNELS: &str = r#"[{"id":27014725,"name":"mc_fuchenling","node":218,"type":"tcp","online":true,"status":0,"status_reason":null,"note":"hi","extra":"","remote":"36192","local_ip":"127.0.0.1","local_port":25565,"export":false}]"#;

    const SAMPLE_NODES: &str = r#"{"218":{"name":"镇江多线PLUS-扩容1","host":"frp-way.com","description":"游戏专用","vip":0,"flag":44,"band":""},"2":{"name":"天津联通PLUS1","host":"frp-act.com","description":"","vip":0,"flag":44,"band":""}}"#;

    const SAMPLE_TRAFFIC: &str = r#"{"1777615200":8449054,"1774937200":1234567}"#;

    #[test]
    fn parses_user_info() {
        let u = parse_user_info(SAMPLE_USER).unwrap();
        assert_eq!(u.id, 12345);
        assert_eq!(u.name, "sample-user");
        assert_eq!(u.tunnels, 2);
        assert_eq!(u.group.name, "普通用户");
        assert_eq!(u.traffic, vec![8449054_u64, 14057568840_u64]);
    }

    #[test]
    fn parses_tunnels() {
        let ts = parse_tunnels(SAMPLE_TUNNELS).unwrap();
        assert_eq!(ts.len(), 1);
        let t = &ts[0];
        assert_eq!(t.id, 27014725);
        assert_eq!(t.name, "mc_fuchenling");
        assert_eq!(t.node, 218);
        assert_eq!(t.kind, "tcp");
        assert_eq!(t.local_port, 25565);
        assert_eq!(t.remote, "36192");
        assert!(t.online);
    }

    #[test]
    fn parses_nodes() {
        let ns = parse_nodes(SAMPLE_NODES).unwrap();
        assert_eq!(ns.len(), 2);
        assert_eq!(ns.get(&218).unwrap().host, "frp-way.com");
        assert_eq!(ns.get(&2).unwrap().name, "天津联通PLUS1");
    }

    #[test]
    fn parses_tunnel_traffic() {
        let m = parse_tunnel_traffic(SAMPLE_TRAFFIC).unwrap();
        assert_eq!(m.get(&1777615200).copied(), Some(8449054));
        assert_eq!(m.get(&1774937200).copied(), Some(1234567));
    }

    #[test]
    fn composes_public_address() {
        let ts = parse_tunnels(SAMPLE_TUNNELS).unwrap();
        let ns = parse_nodes(SAMPLE_NODES).unwrap();
        assert_eq!(public_address(&ts[0], &ns).as_deref(), Some("frp-way.com:36192"));
    }

    #[test]
    fn public_address_none_when_node_missing() {
        let ts = parse_tunnels(SAMPLE_TUNNELS).unwrap();
        let ns: HashMap<u64, Node> = HashMap::new();
        assert!(public_address(&ts[0], &ns).is_none());
    }

    #[test]
    fn node_label_falls_back_to_id() {
        let ns = parse_nodes(SAMPLE_NODES).unwrap();
        assert_eq!(node_label(218, &ns), "#218 镇江多线PLUS-扩容1");
        assert_eq!(node_label(99999, &ns), "#99999");
    }

    #[test]
    fn formats_bytes() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(500), "500 B");
        assert_eq!(fmt_bytes(2048), "2 KB");
        assert_eq!(fmt_bytes(1_500_000), "1.4 MB");
        assert_eq!(fmt_bytes(2_500_000_000), "2.33 GB");
    }

    #[test]
    fn redacts_token() {
        assert_eq!(redact_token("abcd1234efgh5678"), "abcd****");
        assert_eq!(redact_token("ab"), "ab****");
    }

    #[test]
    fn public_address_skips_when_remote_empty() {
        let mut ts = parse_tunnels(SAMPLE_TUNNELS).unwrap();
        ts[0].remote.clear();
        let ns = parse_nodes(SAMPLE_NODES).unwrap();
        assert!(public_address(&ts[0], &ns).is_none());
    }
}
