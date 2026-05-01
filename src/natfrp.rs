//! SakuraFrp REST API client (v4). All calls are blocking — mc-tui has no
//! async runtime. Each call is a single HTTP round-trip; we do not stream or
//! poll. Caller is expected to call only on user-initiated refresh, never on
//! every render frame.
//!
//! Schema below is verified against live `api.natfrp.com/v4` responses on
//! 2026-05-01 — fields are what the server actually returns, not OpenAPI guesses.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde::Deserialize;

const API_BASE: &str = "https://api.natfrp.com/v4";

/// Typed error so the caller can translate to user-facing copy. `Display` is the
/// English fallback for logs / debug — the UI layer is expected to pattern-match
/// and produce a localized string.
#[derive(Debug, Clone)]
pub enum NatfrpError {
    /// 401 — token is wrong / revoked / cleared by the user on the server side.
    Unauthorized,
    /// 403 — token authenticated but lacks the permission bit for this endpoint.
    Forbidden,
    /// 5xx from `api.natfrp.com` — server-side outage / overload.
    ServerError(u16),
    /// Other non-2xx HTTP statuses (e.g. 404, 429, 4xx outside the above).
    HttpError(u16),
    /// DNS / TCP / TLS / timeout — couldn't talk to the API at all.
    Network(String),
    /// JSON body didn't match the expected schema.
    Parse(String),
}

impl fmt::Display for NatfrpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NatfrpError::Unauthorized => write!(f, "401 Unauthorized"),
            NatfrpError::Forbidden => write!(f, "403 Forbidden"),
            NatfrpError::ServerError(code) => write!(f, "{} server error", code),
            NatfrpError::HttpError(code) => write!(f, "HTTP {}", code),
            NatfrpError::Network(detail) => write!(f, "network: {}", detail),
            NatfrpError::Parse(detail) => write!(f, "parse: {}", detail),
        }
    }
}

impl std::error::Error for NatfrpError {}

pub type ApiResult<T> = Result<T, NatfrpError>;

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

    fn get_text(&self, path: &str) -> ApiResult<String> {
        let url = format!("{}{}", API_BASE, path);
        let resp = self
            .agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(classify_ureq_error)?;
        resp.into_string()
            .map_err(|e| NatfrpError::Network(e.to_string()))
    }

    pub fn user_info(&self) -> ApiResult<UserInfo> {
        let body = self.get_text("/user/info")?;
        parse_user_info(&body)
    }

    pub fn tunnels(&self) -> ApiResult<Vec<Tunnel>> {
        let body = self.get_text("/tunnels")?;
        parse_tunnels(&body)
    }

    pub fn nodes(&self) -> ApiResult<HashMap<u64, Node>> {
        let body = self.get_text("/nodes")?;
        parse_nodes(&body)
    }

    /// Map of unix-epoch-seconds → bytes used in that bucket. Caller sums or
    /// picks the latest depending on what they want to display.
    pub fn tunnel_traffic(&self, id: u64) -> ApiResult<HashMap<u64, u64>> {
        let body = self.get_text(&format!("/tunnel/traffic?id={}", id))?;
        parse_tunnel_traffic(&body)
    }

    // ---------- v0.13 write operations ----------
    //
    // SakuraFrp v4 expects `application/x-www-form-urlencoded` on writes (NOT
    // JSON). Empty/optional fields are omitted entirely so the server's
    // defaults kick in (most importantly `remote=""` → server-allocated public
    // port). The post_form helper centralizes the auth header + error mapping
    // so each verb stays a one-liner.
    //
    // ⚠ These have NOT been smoke-tested against the live API on this
    // machine yet (the user's only existing tunnel is production). When you
    // first invoke them in a real session, watch the response carefully —
    // SakuraFrp's POST replies are not always shaped like the GET responses,
    // and serde deserialization may need tweaking.

    fn post_form(&self, path: &str, params: &[(&str, &str)]) -> ApiResult<String> {
        let url = format!("{}{}", API_BASE, path);
        let resp = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_form(params)
            .map_err(classify_ureq_error)?;
        resp.into_string()
            .map_err(|e| NatfrpError::Network(e.to_string()))
    }

    /// Create a new tcp tunnel. Returns the new tunnel's id when the API
    /// gives one back; otherwise `None` and the caller should `tunnels()`
    /// to find the freshly-added entry.
    pub fn create_tunnel(
        &self,
        name: &str,
        node: u64,
        local_port: u16,
    ) -> ApiResult<Option<u64>> {
        let node_str = node.to_string();
        let port_str = local_port.to_string();
        let params: &[(&str, &str)] = &[
            ("name", name),
            ("type", "tcp"),
            ("node", &node_str),
            ("local_ip", "127.0.0.1"),
            ("local_port", &port_str),
            // `remote` deliberately omitted → SakuraFrp auto-assigns a public port.
        ];
        let body = self.post_form("/tunnels", params)?;
        Ok(parse_create_tunnel_id(&body))
    }

    /// Move an existing tunnel onto a new node. Public address changes after
    /// migrate (the host follows the node), so the caller should refresh
    /// `tunnels()` before reading the address.
    pub fn migrate_tunnel(&self, id: u64, node: u64) -> ApiResult<()> {
        let id_str = id.to_string();
        let node_str = node.to_string();
        let params: &[(&str, &str)] = &[("id", &id_str), ("node", &node_str)];
        self.post_form("/tunnel/migrate", params)?;
        Ok(())
    }

    /// Delete one or more tunnels. SakuraFrp accepts up to 10 ids in one call,
    /// comma-separated. Caller is expected to confirm with the user before
    /// invoking — there's no undo.
    pub fn delete_tunnels(&self, ids: &[u64]) -> ApiResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let joined = ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let params: &[(&str, &str)] = &[("ids", &joined)];
        self.post_form("/tunnel/delete", params)?;
        Ok(())
    }
}

/// Map a ureq::Error to our typed enum. ureq splits errors into Status (HTTP
/// non-2xx) and Transport (everything else: DNS, TCP, TLS, timeout, ...).
pub fn classify_ureq_error(e: ureq::Error) -> NatfrpError {
    match e {
        ureq::Error::Status(code, _resp) => match code {
            401 => NatfrpError::Unauthorized,
            403 => NatfrpError::Forbidden,
            500..=599 => NatfrpError::ServerError(code),
            other => NatfrpError::HttpError(other),
        },
        ureq::Error::Transport(t) => NatfrpError::Network(t.to_string()),
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
    /// VIP tier required to use this node. 0 = open to everyone. v0.13's node
    /// picker uses this as the secondary sort key (so users see nodes they
    /// can actually pick before locked-out higher-tier ones).
    #[serde(default)]
    pub vip: u32,
}

pub fn parse_user_info(body: &str) -> ApiResult<UserInfo> {
    serde_json::from_str(body).map_err(|e| NatfrpError::Parse(format!("/user/info: {}", e)))
}

pub fn parse_tunnels(body: &str) -> ApiResult<Vec<Tunnel>> {
    serde_json::from_str(body).map_err(|e| NatfrpError::Parse(format!("/tunnels: {}", e)))
}

pub fn parse_nodes(body: &str) -> ApiResult<HashMap<u64, Node>> {
    let raw: HashMap<String, Node> = serde_json::from_str(body)
        .map_err(|e| NatfrpError::Parse(format!("/nodes: {}", e)))?;
    let mut out = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        let id: u64 = k
            .parse()
            .map_err(|_| NatfrpError::Parse(format!("non-numeric node id: {}", k)))?;
        out.insert(id, v);
    }
    Ok(out)
}

#[allow(dead_code)] // exposed via Client::tunnel_traffic for v0.10 MTD usage; kept for v0.11
pub fn parse_tunnel_traffic(body: &str) -> ApiResult<HashMap<u64, u64>> {
    let raw: HashMap<String, u64> = serde_json::from_str(body)
        .map_err(|e| NatfrpError::Parse(format!("/tunnel/traffic: {}", e)))?;
    let mut out = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        let ts: u64 = k
            .parse()
            .map_err(|_| NatfrpError::Parse(format!("non-numeric ts: {}", k)))?;
        out.insert(ts, v);
    }
    Ok(out)
}

/// Best-effort id extractor for the `POST /tunnels` response body. The shape
/// isn't documented and may differ between SakuraFrp versions — we look for a
/// numeric `id` field in either a top-level object or a top-level
/// `{ "data": { "id": ... } }` envelope. On miss we return `None` so the
/// caller can fall back to a `tunnels()` refresh + name lookup.
pub fn parse_create_tunnel_id(body: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if let Some(id) = v.get("id").and_then(|x| x.as_u64()) {
        return Some(id);
    }
    if let Some(id) = v.pointer("/data/id").and_then(|x| x.as_u64()) {
        return Some(id);
    }
    None
}

/// SakuraFrp tunnel names are constrained server-side to ASCII alphanumerics +
/// underscore (no dashes!). Pre-validate so the user gets immediate feedback
/// instead of a delayed API rejection.
pub fn validate_tunnel_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// "Is this node tagged as game-friendly?" — drives the v0.13 picker's primary
/// sort. The signal is whatever the SakuraFrp operator wrote into the node's
/// description; matching is intentionally loose (CN/EN markers + the bare
/// substring "MC") because the upstream doesn't expose a typed flag.
pub fn is_game_node(node: &Node) -> bool {
    let d = node.description.to_ascii_lowercase();
    node.description.contains("游戏专用")
        || node.description.contains("游戏")
        || d.contains("game")
        || d.contains("minecraft")
        || d.contains(" mc ")
        || d.starts_with("mc ")
        || d.ends_with(" mc")
        || d == "mc"
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

    /// Parse failures bubble up as NatfrpError::Parse so the UI can show a
    /// distinct "schema drifted" message rather than hiding it inside a generic
    /// network error.
    #[test]
    fn parse_returns_parse_variant_on_bad_json() {
        let err = parse_user_info("{not json}").unwrap_err();
        match err {
            NatfrpError::Parse(_) => {}
            other => panic!("expected Parse, got {:?}", other),
        }
    }

    #[test]
    fn validate_tunnel_name_accepts_alnum_underscore_only() {
        assert!(validate_tunnel_name("mc_fuchenling"));
        assert!(validate_tunnel_name("server1"));
        assert!(validate_tunnel_name("a"));
        assert!(validate_tunnel_name("ABC_123"));
    }

    #[test]
    fn validate_tunnel_name_rejects_invalid_input() {
        assert!(!validate_tunnel_name("")); // empty
        assert!(!validate_tunnel_name("mc-fuchenling")); // hyphen — server rejects
        assert!(!validate_tunnel_name("server name")); // space
        assert!(!validate_tunnel_name("中文")); // non-ascii
        assert!(!validate_tunnel_name(&"a".repeat(33))); // overlong
    }

    #[test]
    fn is_game_node_picks_up_common_markers() {
        let mk = |desc: &str| Node {
            name: "n".into(),
            host: "h".into(),
            description: desc.into(),
            flag: 0,
            vip: 0,
        };
        assert!(is_game_node(&mk("游戏专用")));
        assert!(is_game_node(&mk("CN-华北 游戏专用 BGP")));
        assert!(is_game_node(&mk("Minecraft optimized")));
        assert!(is_game_node(&mk("GAME node")));
        assert!(is_game_node(&mk("mc")));
        assert!(!is_game_node(&mk("普通节点 BGP")));
        assert!(!is_game_node(&mk(""))); // empty desc → not game
    }

    #[test]
    fn parse_create_tunnel_id_handles_envelope_shapes() {
        // Top-level id
        assert_eq!(parse_create_tunnel_id(r#"{"id":42}"#), Some(42));
        // Wrapped in data
        assert_eq!(
            parse_create_tunnel_id(r#"{"code":0,"data":{"id":99}}"#),
            Some(99)
        );
        // Missing → None (caller falls back to tunnels())
        assert_eq!(parse_create_tunnel_id(r#"{"ok":true}"#), None);
        // Garbage → None, no panic
        assert_eq!(parse_create_tunnel_id("not json"), None);
    }

    /// Sanity check: a node payload with `vip` populated round-trips through
    /// serde without losing the field. v0.13 picker sorts by this and
    /// silently broke would surface as "wrong order" rather than a parse error.
    #[test]
    fn parses_node_with_vip_field() {
        let body = r#"{"218":{"name":"test","host":"h","description":"游戏专用","vip":3,"flag":44}}"#;
        let ns = parse_nodes(body).unwrap();
        assert_eq!(ns.get(&218).unwrap().vip, 3);
    }

    #[test]
    fn natfrp_error_display_is_specific_per_variant() {
        // Display strings double as a debug log when the UI doesn't translate;
        // make sure each variant says something distinguishable.
        assert!(format!("{}", NatfrpError::Unauthorized).contains("401"));
        assert!(format!("{}", NatfrpError::Forbidden).contains("403"));
        assert!(format!("{}", NatfrpError::ServerError(503)).contains("503"));
        assert!(format!("{}", NatfrpError::HttpError(404)).contains("404"));
        let net = format!("{}", NatfrpError::Network("dns failed".into()));
        assert!(net.contains("dns failed"));
        let parse = format!("{}", NatfrpError::Parse("bad json".into()));
        assert!(parse.contains("bad json"));
    }
}
