//! Phase 0 — the hook bridge: a loopback-only HTTP listener that Claude Code
//! lifecycle hooks POST to, plus the `burnrat hook <Event>` subcommand client
//! that does the POSTing.
//!
//! This is **opt-in and off by default** (see `localServer.enabled`): when
//! disabled no socket is ever opened, preserving burnRat's no-network default.
//! It is deliberately a tiny hand-rolled HTTP/1.1 server over `std::net` rather
//! than a full async stack — the payloads are a handful of tiny JSON POSTs, the
//! dependency surface stays auditable, and owning the raw socket makes the
//! held-open-connection trick that the permission feature (#2) will need trivial.
//!
//! Phase 0 only records the latest event into a shared [`HookState`] (a debug
//! signal). Mapping events to poses (#1) and the blocking `/permission` endpoint
//! (#2) build on this foundation.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::data::Awaiting;
use crate::permission::{Decision, PermissionRegistry};

/// UI callback the bridge invokes to drive the permission bubble: `(event_name,
/// payload)`. The lib supplies a closure that emits the Tauri event + shows/hides
/// the bubble window; tests pass a no-op. Boxed so this module stays Tauri-free.
pub type Notifier = Arc<dyn Fn(&str, Value) + Send + Sync + 'static>;

/// Read timeout for the blocking `burnrat permission` client. A backstop only —
/// the server always answers first (at `permissionTimeoutSeconds` → no-decision).
const PERMISSION_READ_TIMEOUT: Duration = Duration::from_secs(600);

/// Identifying response header so the client can confirm it reached *our*
/// listener and not some other service squatting on the port.
const SERVER_HEADER: &str = "x-burnrat-server";
const SERVER_ID: &str = "burnrat";

/// Fallback candidate ports the client probes when the runtime file is missing.
/// Mirrors `localServer.ports` in `data/settings.default.json`.
const DEFAULT_PORTS: [u16; 5] = [23333, 23334, 23335, 23336, 23337];

/// Cap on the bytes we read for a single request — loopback only, but bounded
/// so a malformed/hostile client can't make us allocate without limit.
const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Short timeout for the client's fire-and-forget POST. A hook must never block
/// Claude Code, so we give up fast and exit cleanly if the app isn't listening.
const CLIENT_TIMEOUT: Duration = Duration::from_millis(300);

/// The latest lifecycle event the bridge has received. Phase 0 keeps just enough
/// to prove the round-trip and give #1 something to read; richer fusion with the
/// JSONL signal comes with #1.
#[derive(Debug, Default)]
pub struct HookState {
    /// The most recent event name (e.g. "Stop", "PreToolUse").
    pub last_event: Option<String>,
    /// The raw payload Claude Code passed on stdin, if any.
    pub last_payload: Option<Value>,
    /// When we received it.
    pub last_at: Option<DateTime<Utc>>,
    /// Total events received this run (debug counter).
    pub count: u64,
}

/// A running hook-bridge listener. Holds the bound port; the shared [`HookState`]
/// it writes lives in `Shared` so the poll loop reads it directly. Dropping this
/// does not stop the accept thread (the listener lives in that thread for the
/// process's lifetime); "Disconnect" uninstalls the hooks so nothing posts.
pub struct HookServer {
    // Kept for the tray to know a listener is up (and report the port); the poll
    // loop reads the shared state via `Shared`, not through here.
    #[allow(dead_code)]
    pub port: u16,
}

impl HookServer {
    /// Bind the first free port in `ports`, write the runtime file so the hook
    /// client can find us, and spawn the accept loop writing into `state`.
    /// Returns `None` if no port could be bound.
    pub fn start(
        ports: &[u16],
        state: Arc<Mutex<HookState>>,
        registry: Arc<PermissionRegistry>,
        notify: Notifier,
        perm_timeout: Duration,
    ) -> Option<HookServer> {
        let candidates: &[u16] = if ports.is_empty() {
            &DEFAULT_PORTS
        } else {
            ports
        };

        // Bind the first candidate that's free. Read the actual port back from
        // the socket (`local_addr`) rather than trusting the requested value —
        // correct in general and lets a `0` request take an ephemeral port.
        let listener = candidates.iter().find_map(|&p| {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, p));
            TcpListener::bind(addr).ok()
        })?;
        let port = listener.local_addr().ok()?.port();

        write_runtime_file(port);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let st = state.clone();
                let reg = registry.clone();
                let nf = notify.clone();
                // One short-lived thread per connection; a /permission handler
                // blocks this thread until the user decides (or it times out).
                std::thread::spawn(move || handle_connection(stream, st, reg, nf, perm_timeout));
            }
        });

        eprintln!("burnRat: hook bridge listening on 127.0.0.1:{port}");
        Some(HookServer { port })
    }
}

/// An immutable read of the latest hook edge, taken by the poll loop each tick to
/// fuse with the JSONL-inferred state. `None` (no events yet) leaves the JSONL
/// behavior untouched — which is exactly the disabled-bridge case.
#[derive(Debug, Clone)]
pub struct HookSnapshot {
    pub event: String,
    pub payload: Option<Value>,
    pub at: DateTime<Utc>,
}

impl HookState {
    /// Snapshot the latest event, if any has arrived.
    pub fn snapshot(&self) -> Option<HookSnapshot> {
        match (&self.last_event, self.last_at) {
            (Some(event), Some(at)) => Some(HookSnapshot {
                event: event.clone(),
                payload: self.last_payload.clone(),
                at,
            }),
            _ => None,
        }
    }
}

/// The discrete awaiting-signal a lifecycle hook implies, if any. `UserPromptSubmit`
/// = the user just sent a message (awaiting Claude); `Stop` = turn finished
/// (awaiting user); a `PreToolUse` for an interactive tool = Claude is asking.
/// A normal tool / other event implies no awaiting state (Claude is mid-work),
/// so returns `None`.
pub fn hook_awaiting(event: &str, payload: Option<&Value>) -> Option<Awaiting> {
    match event {
        "UserPromptSubmit" => Some(Awaiting::Sent),
        "Stop" => Some(Awaiting::Done),
        "PreToolUse" => {
            let tool = payload
                .and_then(|p| p.get("tool_name"))
                .and_then(|t| t.as_str());
            matches!(tool, Some("AskUserQuestion" | "ExitPlanMode")).then_some(Awaiting::Asking)
        }
        _ => None,
    }
}

/// Whether a hook event means Claude is actively working *right now* (so the rat
/// should perk to at least `working` without waiting for the smoothed rate). A
/// user prompt is deliberately excluded — that's the user typing, not Claude.
pub fn hook_is_activity(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PostToolUse" | "SubagentStop")
}

/// Fuse the JSONL-inferred awaiting signal with a hook-derived one: the more
/// recent source wins, with `ttl_secs` as a backstop so a stale hook can never
/// override JSONL indefinitely. A hook that's newest but implies no awaiting
/// (e.g. a normal tool running) yields `Awaiting::None` — trusting "Claude is
/// working" over an older JSONL "done/asking".
pub fn fuse_awaiting(
    jsonl_kind: Awaiting,
    jsonl_at: Option<DateTime<Utc>>,
    hook: Option<&HookSnapshot>,
    ttl_secs: i64,
    now: DateTime<Utc>,
) -> Awaiting {
    let Some(h) = hook else {
        return jsonl_kind;
    };
    if (now - h.at).num_seconds() > ttl_secs.max(0) {
        return jsonl_kind;
    }
    // JSONL strictly newer than the hook → trust JSONL.
    if jsonl_at.is_some_and(|j| j > h.at) {
        return jsonl_kind;
    }
    hook_awaiting(&h.event, h.payload.as_ref()).unwrap_or(Awaiting::None)
}

/// Path to the runtime info file (`~/.burnrat/runtime.json`). Shared between the
/// server (writer) and the hook subcommand (reader) so neither needs Tauri path
/// resolution. `BURNRAT_RUNTIME_FILE` overrides it (used by tests so they never
/// clobber a running app's real runtime file).
pub fn runtime_file_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BURNRAT_RUNTIME_FILE") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".burnrat").join("runtime.json"))
}

fn write_runtime_file(port: u16) {
    let Some(path) = runtime_file_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = serde_json::json!({ "app": SERVER_ID, "port": port });
    if let Ok(text) = serde_json::to_string_pretty(&body) {
        let _ = std::fs::write(&path, text);
    }
}

fn read_runtime_port() -> Option<u16> {
    let path = runtime_file_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("port").and_then(|p| p.as_u64()).map(|p| p as u16)
}

// ---------------------------------------------------------------------------
// Server side: parse a tiny HTTP/1.1 request and route it.
// ---------------------------------------------------------------------------

fn handle_connection(
    mut stream: TcpStream,
    state: Arc<Mutex<HookState>>,
    registry: Arc<PermissionRegistry>,
    notify: Notifier,
    perm_timeout: Duration,
) {
    // /state is fire-and-forget (short read); /permission blocks for the user,
    // so don't impose a short read timeout on the whole connection. The request
    // line + headers + small body arrive promptly; the wait is after parsing.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let Some(req) = read_request(&mut stream) else {
        let _ = write_response(&mut stream, 400, "{\"error\":\"bad request\"}");
        return;
    };

    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/state") => {
            let payload = serde_json::from_slice::<Value>(&req.body).ok();
            record_event(&state, payload);
            let _ = write_response(&mut stream, 200, "{\"ok\":true}");
        }
        // Blocking: park the request, show the bubble, wait for the decision.
        ("POST", "/permission") => {
            let payload = serde_json::from_slice::<Value>(&req.body).unwrap_or(Value::Null);
            let body = handle_permission(payload, &registry, &notify, perm_timeout);
            let _ = write_response(&mut stream, 200, &body);
        }
        // A cheap liveness probe (used by the client to confirm it's us).
        ("GET", "/health") => {
            let _ = write_response(&mut stream, 200, "{\"ok\":true}");
        }
        _ => {
            let _ = write_response(&mut stream, 404, "{\"error\":\"not found\"}");
        }
    }
}

/// Park a permission request, drive the bubble via `notify`, and block until the
/// user decides or `timeout` elapses. Returns the response body the client maps
/// to Claude's stdout. Loopback only, so the request is trusted.
fn handle_permission(
    payload: Value,
    registry: &PermissionRegistry,
    notify: &Notifier,
    timeout: Duration,
) -> String {
    let tool = payload
        .get("tool_name")
        .and_then(|t| t.as_str())
        .unwrap_or("a tool")
        .to_string();
    let detail = summarize_tool_input(&payload);
    let (id, rx) = registry.register(tool.clone(), detail.clone());
    notify(
        "permission-request",
        serde_json::json!({ "id": id, "tool": tool, "detail": detail }),
    );

    let decision = rx.recv_timeout(timeout).unwrap_or(Decision::Defer);
    registry.forget(id);
    notify("permission-resolved", serde_json::json!({ "id": id }));

    decision_to_body(&decision)
}

/// The bridge's generic decision wire-format (the client maps it to Claude's
/// hook stdout). `none` = no decision → Claude falls back to its own prompt.
fn decision_to_body(d: &Decision) -> String {
    match d {
        Decision::Allow => serde_json::json!({ "behavior": "allow" }),
        Decision::Deny(msg) => serde_json::json!({ "behavior": "deny", "message": msg }),
        Decision::Defer => serde_json::json!({ "behavior": "none" }),
    }
    .to_string()
}

/// A short, human-readable summary of a tool call for the bubble — the most
/// informative field (command / path / url / pattern), else compact JSON.
fn summarize_tool_input(payload: &Value) -> String {
    let Some(input) = payload.get("tool_input") else {
        return String::new();
    };
    for key in ["command", "file_path", "path", "url", "pattern", "query"] {
        if let Some(v) = input.get(key).and_then(|x| x.as_str()) {
            return truncate(v, 240);
        }
    }
    truncate(&input.to_string(), 240)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

/// Fold an incoming `/state` payload into the shared state.
fn record_event(state: &Arc<Mutex<HookState>>, payload: Option<Value>) {
    // The client wraps the stdin JSON as `{ "event": "<Name>", "payload": ... }`.
    let event = payload
        .as_ref()
        .and_then(|v| v.get("event"))
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());
    let inner_payload = payload
        .as_ref()
        .and_then(|v| v.get("payload"))
        .cloned()
        .filter(|p| !p.is_null());

    if let Ok(mut s) = state.lock() {
        s.last_event = event.clone();
        s.last_payload = inner_payload;
        s.last_at = Some(Utc::now());
        s.count += 1;
        if cfg!(debug_assertions) {
            eprintln!(
                "burnRat: hook /state #{} event={}",
                s.count,
                event.as_deref().unwrap_or("<none>")
            );
        }
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

/// Read and parse a minimal HTTP/1.1 request: request line, headers (we only
/// care about Content-Length), then exactly that many body bytes.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];

    // Read until we have the full header block (\r\n\r\n) or hit the cap.
    let header_end = loop {
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > MAX_REQUEST_BYTES {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None; // connection closed before headers completed
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let header_text = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let content_length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_REQUEST_BYTES);

    // Body bytes already buffered, plus any still on the wire.
    let mut body: Vec<u8> = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Some(Request { method, path, body })
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         {SERVER_HEADER}: {SERVER_ID}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// First index of `needle` within `haystack`, or `None`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// Client side: the `burnrat hook <Event>` subcommand.
// ---------------------------------------------------------------------------

/// Entry point for `burnrat hook <Event>`. Reads the event JSON Claude Code
/// pipes on stdin, finds the running server's port, and POSTs `/state`. Always
/// returns 0 (exit code 0) — a hook must never block or fail Claude Code, so any
/// error (app not running, port closed) is swallowed silently.
pub fn run_hook_client(event: &str) -> i32 {
    // Read whatever Claude Code piped on stdin (may be empty for some events).
    let mut stdin_raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin_raw);
    let payload: Value = serde_json::from_str(stdin_raw.trim()).unwrap_or(Value::Null);

    let body = serde_json::json!({ "event": event, "payload": payload });
    let body_text = body.to_string();

    // Prefer the port the running server advertised; fall back to the range.
    let mut candidates: Vec<u16> = Vec::new();
    if let Some(p) = read_runtime_port() {
        candidates.push(p);
    }
    for p in DEFAULT_PORTS {
        if !candidates.contains(&p) {
            candidates.push(p);
        }
    }

    for port in candidates {
        if post_state(port, &body_text) {
            return 0;
        }
    }
    0
}

/// Entry point for `burnrat permission`. Reads the `PermissionRequest` payload
/// Claude Code pipes on stdin, forwards it to the running bridge's blocking
/// `/permission` endpoint, and waits for the decision — then prints the
/// `hookSpecificOutput` JSON Claude expects on stdout (Allow/Deny), or nothing
/// (no decision → Claude falls back to its own prompt). Always exits 0; if the
/// app isn't running we simply emit nothing and Claude prompts as usual.
pub fn run_permission_client() -> i32 {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let payload: Value = serde_json::from_str(raw.trim()).unwrap_or(Value::Null);
    let body_text = payload.to_string();

    let mut candidates: Vec<u16> = Vec::new();
    if let Some(p) = read_runtime_port() {
        candidates.push(p);
    }
    for p in DEFAULT_PORTS {
        if !candidates.contains(&p) {
            candidates.push(p);
        }
    }

    for port in candidates {
        if let Some(resp) = post_permission(port, &body_text) {
            if let Some(out) = format_permission_stdout(&resp) {
                println!("{out}");
            }
            return 0;
        }
    }
    0
}

/// POST the request to `/permission` and block (long read timeout) for the
/// decision body. Returns the response body, or `None` if we didn't reach a
/// confirmed burnRat server.
fn post_permission(port: u16, body: &str) -> Option<String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, CLIENT_TIMEOUT).ok()?;
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_read_timeout(Some(PERMISSION_READ_TIMEOUT));

    let request = format!(
        "POST /permission HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).ok()?;
    let _ = stream.flush();

    let mut resp = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                resp.extend_from_slice(&chunk[..n]);
                if resp.len() > MAX_REQUEST_BYTES {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&resp);
    if !text
        .to_lowercase()
        .contains(&format!("{SERVER_HEADER}: {SERVER_ID}"))
    {
        return None;
    }
    let body_start = text.find("\r\n\r\n")? + 4;
    Some(text[body_start..].to_string())
}

/// Map the bridge's `{behavior}` response body to the `PermissionRequest` hook
/// stdout Claude Code expects. Returns `None` for "none"/unknown → print nothing
/// (no decision → Claude's own prompt).
fn format_permission_stdout(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body.trim()).ok()?;
    match v.get("behavior").and_then(|b| b.as_str())? {
        "allow" => Some(
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "allow" }
                }
            })
            .to_string(),
        ),
        "deny" => {
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("Denied via burnRat");
            Some(
                serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PermissionRequest",
                        "decision": { "behavior": "deny", "message": msg }
                    }
                })
                .to_string(),
            )
        }
        _ => None,
    }
}

/// POST the body to `127.0.0.1:<port>/state`. Returns `true` only if we reached
/// a confirmed burnRat server (identifying header present).
fn post_state(port: u16, body: &str) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, CLIENT_TIMEOUT) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));

    let request = format!(
        "POST /state HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let _ = stream.flush();

    // Read the response head and confirm the identifying header.
    let mut resp = Vec::new();
    let mut chunk = [0u8; 1024];
    while resp.len() < 4096 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => resp.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let head = String::from_utf8_lossy(&resp).to_lowercase();
    head.contains(&format!("{SERVER_HEADER}: {SERVER_ID}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests that point `BURNRAT_RUNTIME_FILE` at a temp path, so
    /// concurrent test threads don't race on the shared process env / file.
    static RUNTIME_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn temp_runtime_file(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("burnrat-rt-{}-{tag}.json", std::process::id()))
    }

    #[test]
    fn find_subsequence_locates_header_end() {
        let buf = b"POST /state HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}";
        let pos = find_subsequence(buf, b"\r\n\r\n").unwrap();
        assert_eq!(&buf[pos..pos + 4], b"\r\n\r\n");
    }

    #[test]
    fn record_event_unwraps_client_envelope() {
        let state = Arc::new(Mutex::new(HookState::default()));
        let payload = serde_json::json!({
            "event": "Stop",
            "payload": { "session_id": "abc", "hook_event_name": "Stop" }
        });
        record_event(&state, Some(payload));
        let s = state.lock().unwrap();
        assert_eq!(s.last_event.as_deref(), Some("Stop"));
        assert_eq!(s.count, 1);
        assert_eq!(
            s.last_payload
                .as_ref()
                .and_then(|p| p.get("session_id"))
                .and_then(|v| v.as_str()),
            Some("abc")
        );
    }

    #[test]
    fn record_event_tolerates_missing_payload() {
        let state = Arc::new(Mutex::new(HookState::default()));
        record_event(&state, Some(serde_json::json!({ "event": "SessionStart" })));
        let s = state.lock().unwrap();
        assert_eq!(s.last_event.as_deref(), Some("SessionStart"));
        assert!(s.last_payload.is_none());
    }

    /// The runtime file written by the server is read back identically by the
    /// client's port lookup (the cross-process handshake, in one process).
    #[test]
    fn runtime_file_round_trips() {
        let _g = RUNTIME_ENV_LOCK.lock().unwrap();
        let tmp = temp_runtime_file("roundtrip");
        std::env::set_var("BURNRAT_RUNTIME_FILE", &tmp);
        write_runtime_file(45_678);
        assert_eq!(read_runtime_port(), Some(45_678));
        std::env::remove_var("BURNRAT_RUNTIME_FILE");
        let _ = std::fs::remove_file(&tmp);
    }

    /// Full loopback round-trip: start the server, POST /state with the client
    /// envelope, and confirm the shared state recorded it.
    #[test]
    fn server_records_posted_event() {
        let _g = RUNTIME_ENV_LOCK.lock().unwrap();
        // Redirect the runtime file so start() can't clobber a running app's.
        let tmp = temp_runtime_file("server");
        std::env::set_var("BURNRAT_RUNTIME_FILE", &tmp);

        let state = Arc::new(Mutex::new(HookState::default()));
        let registry = Arc::new(PermissionRegistry::new());
        let noop: Notifier = Arc::new(|_, _| {});
        let server = HookServer::start(&[0], state.clone(), registry, noop, Duration::from_secs(1))
            .expect("bind ephemeral port");
        let body = serde_json::json!({ "event": "PreToolUse", "payload": { "tool_name": "Bash" } })
            .to_string();
        assert!(post_state(server.port, &body));
        // Give the connection thread a beat to record.
        std::thread::sleep(Duration::from_millis(50));
        let s = state.lock().unwrap();
        assert_eq!(s.last_event.as_deref(), Some("PreToolUse"));

        std::env::remove_var("BURNRAT_RUNTIME_FILE");
        let _ = std::fs::remove_file(&tmp);
    }

    fn secs(n: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(n, 0).unwrap()
    }

    fn snap(event: &str, at: i64, payload: Option<Value>) -> HookSnapshot {
        HookSnapshot {
            event: event.to_string(),
            payload,
            at: secs(at),
        }
    }

    #[test]
    fn hook_awaiting_maps_known_events() {
        assert_eq!(
            hook_awaiting("UserPromptSubmit", None),
            Some(Awaiting::Sent)
        );
        assert_eq!(hook_awaiting("Stop", None), Some(Awaiting::Done));
        assert_eq!(hook_awaiting("PostToolUse", None), None);
        // A normal tool is "working", not awaiting.
        let bash = serde_json::json!({ "tool_name": "Bash" });
        assert_eq!(hook_awaiting("PreToolUse", Some(&bash)), None);
        // An interactive tool blocks on the user → asking.
        let ask = serde_json::json!({ "tool_name": "AskUserQuestion" });
        assert_eq!(
            hook_awaiting("PreToolUse", Some(&ask)),
            Some(Awaiting::Asking)
        );
    }

    #[test]
    fn fuse_prefers_fresher_hook_over_older_jsonl() {
        // Hook (Stop@100) is newer than the JSONL line (@90) → hook wins.
        let h = snap("Stop", 100, None);
        let fused = fuse_awaiting(Awaiting::None, Some(secs(90)), Some(&h), 120, secs(105));
        assert_eq!(fused, Awaiting::Done);
    }

    #[test]
    fn fuse_prefers_newer_jsonl_over_older_hook() {
        // JSONL line (@100) is newer than the hook (@90) → JSONL wins.
        let h = snap("Stop", 90, None);
        let fused = fuse_awaiting(Awaiting::Asking, Some(secs(100)), Some(&h), 120, secs(105));
        assert_eq!(fused, Awaiting::Asking);
    }

    #[test]
    fn fuse_ignores_stale_hook_past_ttl() {
        // Hook is newest but older than the TTL backstop → fall back to JSONL.
        let h = snap("Stop", 10, None);
        let fused = fuse_awaiting(Awaiting::Sent, Some(secs(5)), Some(&h), 120, secs(200));
        assert_eq!(fused, Awaiting::Sent);
    }

    #[test]
    fn fuse_newest_nonawaiting_hook_yields_none() {
        // A normal tool just started (newest) → not awaiting anyone, even if the
        // older JSONL line said "done".
        let h = snap(
            "PreToolUse",
            100,
            Some(serde_json::json!({ "tool_name": "Edit" })),
        );
        let fused = fuse_awaiting(Awaiting::Done, Some(secs(90)), Some(&h), 120, secs(101));
        assert_eq!(fused, Awaiting::None);
    }

    #[test]
    fn fuse_no_hook_is_passthrough() {
        assert_eq!(
            fuse_awaiting(Awaiting::Done, Some(secs(90)), None, 120, secs(100)),
            Awaiting::Done
        );
    }

    #[test]
    fn summarize_prefers_informative_field() {
        let bash = serde_json::json!({ "tool_input": { "command": "rm -rf build" } });
        assert_eq!(summarize_tool_input(&bash), "rm -rf build");
        let edit = serde_json::json!({ "tool_input": { "file_path": "/tmp/x.rs", "old": "a" } });
        assert_eq!(summarize_tool_input(&edit), "/tmp/x.rs");
        // No tool_input → empty.
        assert_eq!(summarize_tool_input(&serde_json::json!({})), "");
    }

    #[test]
    fn format_stdout_maps_behaviors() {
        let allow = format_permission_stdout(r#"{"behavior":"allow"}"#).unwrap();
        assert!(allow.contains("\"behavior\":\"allow\"") && allow.contains("PermissionRequest"));
        let deny = format_permission_stdout(r#"{"behavior":"deny","message":"no"}"#).unwrap();
        assert!(deny.contains("\"behavior\":\"deny\"") && deny.contains("\"no\""));
        // No decision → nothing to print.
        assert!(format_permission_stdout(r#"{"behavior":"none"}"#).is_none());
    }

    #[test]
    fn handle_permission_allow_round_trip() {
        let reg = Arc::new(PermissionRegistry::new());
        let reg2 = reg.clone();
        // The notifier fires synchronously when the request is parked; resolve it
        // Allow from a background thread so the blocked handler wakes.
        let notify: Notifier = Arc::new(move |name: &str, payload: Value| {
            if name == "permission-request" {
                let id = payload.get("id").and_then(|i| i.as_u64()).unwrap();
                let r = reg2.clone();
                std::thread::spawn(move || {
                    r.resolve(id, Decision::Allow);
                });
            }
        });
        let payload = serde_json::json!({"tool_name":"Bash","tool_input":{"command":"ls"}});
        let body = handle_permission(payload, &reg, &notify, Duration::from_secs(5));
        assert!(body.contains("\"behavior\":\"allow\""), "got: {body}");
    }

    #[test]
    fn handle_permission_times_out_to_no_decision() {
        let reg = Arc::new(PermissionRegistry::new());
        let noop: Notifier = Arc::new(|_, _| {});
        let payload = serde_json::json!({"tool_name":"Bash"});
        let body = handle_permission(payload, &reg, &noop, Duration::from_millis(50));
        assert!(body.contains("\"behavior\":\"none\""), "got: {body}");
    }
}
