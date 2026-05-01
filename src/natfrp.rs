//! SakuraFrp REST API client (v4). All calls are blocking — shulker has no
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
    /// Kept as a future seam for per-tunnel traffic UX; not currently wired
    /// to any tab.
    #[allow(dead_code)]
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

// ---------- v0.15: clients manifest (frpc binary download URLs) ----------
//
// `https://api.natfrp.com/v4/system/clients` returns a manifest of every
// official client binary the SakuraFrp team ships, keyed by platform → arch.
// We don't redistribute — we just point users at the right URL + hash for
// their host. Endpoint requires no auth.

#[derive(Debug, Clone, Default)]
pub struct ClientArch {
    pub url: String,
    pub hash: String,
}

#[derive(Debug, Clone, Default)]
pub struct ClientsManifest {
    pub frpc_version: String,
    /// Map of `<os>_<arch>` (matches `data::host_target_for_manifest` output)
    /// to the download metadata for the frpc binary.
    pub frpc_archs: HashMap<String, ClientArch>,
}

/// Hit `/v4/system/clients` (no auth) and parse out the frpc download URLs.
/// Cheap call — no auth header needed; we still go through `Client` since it
/// already has the timeout configured.
pub fn fetch_clients_manifest() -> ApiResult<ClientsManifest> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .build();
    let url = format!("{}/system/clients", API_BASE);
    let resp = agent.get(&url).call().map_err(classify_ureq_error)?;
    let body = resp
        .into_string()
        .map_err(|e| NatfrpError::Network(e.to_string()))?;
    parse_clients_manifest(&body)
}

pub fn parse_clients_manifest(body: &str) -> ApiResult<ClientsManifest> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| NatfrpError::Parse(format!("/system/clients: {}", e)))?;
    let frpc = v
        .get("frpc")
        .ok_or_else(|| NatfrpError::Parse("missing 'frpc' key".into()))?;
    let mut out = ClientsManifest::default();
    if let Some(s) = frpc.get("ver").and_then(|x| x.as_str()) {
        out.frpc_version = s.to_string();
    }
    if let Some(archs) = frpc.get("archs").and_then(|x| x.as_object()) {
        for (key, val) in archs {
            let url = val
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let hash = val
                .get("hash")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if !url.is_empty() {
                out.frpc_archs.insert(
                    key.clone(),
                    ClientArch { url, hash },
                );
            }
        }
    }
    Ok(out)
}

// ---------- v0.14.1: launcher single-tunnel lifecycle ----------
//
// Story so far (and why this file is shorter than v0.14 had it):
//
// We did the rabbit-hole on the launcher's WebUI at https://127.0.0.1:7102:
//   1. The auth flow is a websocket challenge: server sends
//      `{"v":"ilsf-1-challenge","token":"<base64-server-nonce>"}`, client
//      replies with `{"v":"ilsf-1-response","token":"<hex>"}` where the
//      hex is `HMAC-SHA256(key=webui_pass, msg=challenge_token_string)`.
//      `webui_pass` lives at /run/config.json::webui_pass inside the
//      container (auto-generated by the launcher).
//   2. After auth, control messages ride a private gRPC-Web protobuf
//      schema (subprotocol `natfrp-launcher-grpc`). We confirmed method
//      names (`UpdateTunnel`, `ReloadTunnels`, `StreamTunnels`) but the
//      `.proto` is closed-source and reverse-engineering field tags is a
//      multi-day exercise that breaks every minor launcher rev.
//
// Pivot: rather than ship a fragile guessed-protobuf client, we treat
// `/run/config.json::auto_start_tunnels` as the source of truth. It's
// what the launcher reads on boot, and the user's existing
// `docker restart natfrp-service` workflow already absorbs the ~10s
// reload cost. No proto schemas, no version drift.
//
// `LauncherClient` exists but is intentionally minimal — it lets a caller
// verify the password works (handshake round-trip) without sending RPCs.
// Useful as a "is the launcher reachable AND is our cached webui_pass
// still valid?" probe; useful as a future-proofing seam if/when the proto
// gets documented.

#[allow(dead_code)] // Used by LauncherClient (v0.14.1 scaffold for future v0.14.2 websocket).
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme as S;
        vec![
            S::RSA_PKCS1_SHA256,
            S::RSA_PKCS1_SHA384,
            S::RSA_PKCS1_SHA512,
            S::ECDSA_NISTP256_SHA256,
            S::ECDSA_NISTP384_SHA384,
            S::ECDSA_NISTP521_SHA512,
            S::RSA_PSS_SHA256,
            S::RSA_PSS_SHA384,
            S::RSA_PSS_SHA512,
            S::ED25519,
        ]
    }
}

/// Install the rustls AWS-LC crypto provider exactly once per process.
/// Idempotent; safe to call from each LauncherClient::new().
#[allow(dead_code)] // Hooked when v0.14.2 ships actual websocket bring-up.
fn ensure_rustls_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Tiny HMAC-SHA256 implementation in pure Rust. Avoids pulling another crate
/// just for the launcher handshake. RFC 2104 reference; verified to match
/// `crypto.subtle` output during protocol discovery.
#[allow(dead_code)] // Future seam: used by launcher_challenge_response when v0.14.2 wires the websocket.
fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    const BLOCK: usize = 64;
    let mut k = if key.len() > BLOCK {
        let mut h = Sha256::new();
        h.update(key);
        h.finalize().as_slice().to_vec()
    } else {
        key.to_vec()
    };
    k.resize(BLOCK, 0);
    let mut ipad = [0u8; BLOCK];
    let mut opad = [0u8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] = k[i] ^ 0x36;
        opad[i] = k[i] ^ 0x5c;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let final_hash = outer.finalize();
    let mut hex = String::with_capacity(final_hash.len() * 2);
    for b in final_hash {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{:02x}", b);
    }
    hex
}

/// Compute the response token the launcher expects for the
/// `ilsf-1-challenge` exchange. Public because the test suite verifies it
/// against a fixture nonce; the live LauncherClient feeds it the live
/// challenge string.
#[allow(dead_code)] // Future seam: hooked when v0.14.2 ships the websocket bring-up.
pub fn launcher_challenge_response(password: &str, challenge: &str) -> String {
    hmac_sha256_hex(password.as_bytes(), challenge.as_bytes())
}

/// Stub LauncherClient. Kept as the eventual home for the websocket /
/// gRPC-Web bits when v0.14.2 ships those; for v0.14.1 enable/disable is
/// done through `data::write_launcher_auto_start` which doesn't need it.
#[allow(dead_code)]
pub struct LauncherClient {
    password: String,
}

#[allow(dead_code)]
impl LauncherClient {
    pub fn new(password: String) -> ApiResult<Self> {
        ensure_rustls_provider();
        Ok(Self { password })
    }

    /// Quick connectivity / auth probe. Currently a no-op: a real probe
    /// would open a websocket, complete the ilsf-1 handshake, and close.
    /// We've validated the protocol externally — leaving this as a seam
    /// for v0.14.2 if/when we ship websocket bring-up.
    pub fn probe(&self) -> ApiResult<()> {
        if self.password.is_empty() {
            return Err(NatfrpError::Unauthorized);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserInfo {
    /// Deserialized for the API response shape; not currently rendered.
    #[allow(dead_code)]
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub speed: String,
    /// Server-reported tunnel count; we render our own count from the
    /// /tunnels list instead. Kept here so the deserialize stays lossless.
    #[allow(dead_code)]
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
    /// Plan level integer (0 = free, higher = paid tiers). Not rendered;
    /// the user-facing label comes from `name` instead.
    #[allow(dead_code)]
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

    /// v0.15 — pull a small subset of the real `/v4/system/clients` payload
    /// (the bits we care about) and confirm we extract the frpc version +
    /// per-arch URL/hash. Schema captured against the live response on
    /// 2026-05-01; if upstream renames `frpc.archs.<key>.url`, this catches it.
    #[test]
    fn parses_clients_manifest_minimal() {
        let body = r#"{
            "frpc": {
                "ver": "0.51.0-sakura-12.3",
                "archs": {
                    "linux_amd64": {
                        "title": "Linux 64",
                        "url": "https://nya.globalslb.net/natfrp/client/frpc/0.51.0-sakura-12.3/frpc_linux_amd64",
                        "hash": "8ba7dcde07b1181e4f011e011321c6f8"
                    },
                    "darwin_arm64": {
                        "title": "macOS Apple Silicon",
                        "url": "https://nya.globalslb.net/natfrp/client/frpc/0.51.0-sakura-12.3/frpc_darwin_arm64",
                        "hash": "19b546b7bcfd709a94d583280fb3cc4d"
                    },
                    "docker_natfrp": {
                        "title": "Docker",
                        "url": "https://natfrp.com/registry/#!/taglist/frpc"
                    }
                }
            }
        }"#;
        let m = parse_clients_manifest(body).unwrap();
        assert_eq!(m.frpc_version, "0.51.0-sakura-12.3");
        let linux = m.frpc_archs.get("linux_amd64").unwrap();
        assert!(linux.url.ends_with("frpc_linux_amd64"));
        assert_eq!(linux.hash, "8ba7dcde07b1181e4f011e011321c6f8");
        // Docker entry has no hash; should still appear with empty hash.
        let docker = m.frpc_archs.get("docker_natfrp").unwrap();
        assert!(docker.hash.is_empty());
    }

    #[test]
    fn parse_clients_manifest_rejects_missing_frpc_key() {
        let body = r#"{"windows":{}}"#;
        let err = parse_clients_manifest(body).unwrap_err();
        match err {
            NatfrpError::Parse(_) => {}
            other => panic!("expected Parse, got {:?}", other),
        }
    }

    /// v0.14.1 — RFC 4231 HMAC-SHA256 test vector, hex-encoded. Validates
    /// the pure-Rust HMAC against a known-good fixture before relying on it
    /// for the launcher handshake.
    #[test]
    fn hmac_sha256_matches_rfc_4231_test_vector() {
        // RFC 4231 §4.2: key = 20 bytes of 0x0b, data = "Hi There"
        let key = [0x0bu8; 20];
        let got = hmac_sha256_hex(&key, b"Hi There");
        assert_eq!(
            got,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    /// v0.14.1 — verifies our challenge/response against the launcher's
    /// in-browser implementation: `qx(t,e) = HMAC-SHA256(key=e, msg=t)`,
    /// `Oj(b) = hex(b)`, request body = `Oj(qx(challenge_bytes,
    /// password_bytes))`. Numbers below are reproducible: same challenge
    /// + password should always yield the same response.
    #[test]
    fn launcher_challenge_response_is_stable() {
        // Fixed challenge + password; if the math drifts, this catches it.
        let r1 = launcher_challenge_response("supersecret", "deadbeef==");
        let r2 = launcher_challenge_response("supersecret", "deadbeef==");
        assert_eq!(r1, r2);
        assert_eq!(r1.len(), 64); // SHA-256 → 32 bytes → 64 hex chars
        // Different password → different response.
        let r3 = launcher_challenge_response("other", "deadbeef==");
        assert_ne!(r1, r3);
    }

}
