//! Phase 2 — the permission control surface's backend.
//!
//! When Claude Code requests a tool permission, the `burnrat permission`
//! subcommand POSTs it to the bridge's blocking `/permission` endpoint. That
//! handler parks the request here (a one-shot channel keyed by id), shows the
//! Allow/Deny bubble, and blocks on the channel until the user decides — via the
//! bubble buttons (the `resolve_permission` command) or the global hotkeys — or
//! until it times out. The decision travels back over the held HTTP connection.
//!
//! This registry is plain, UI-agnostic Rust (no Tauri types) so it's unit-
//! testable; the bridge supplies a `Notifier` closure to drive the actual bubble.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use serde::Serialize;

/// The user's verdict on a pending tool-permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Let the tool run.
    Allow,
    /// Block the tool, with an optional reason shown to Claude.
    Deny(String),
    /// No verdict (timed out / bubble dismissed) — defer to Claude Code's own
    /// terminal permission prompt. burnRat never silently allows or blocks.
    Defer,
}

impl Decision {
    /// Map the bridge's HTTP request-body verb to a decision. Unknown / "none"
    /// → `Defer` (safe fallback).
    pub fn from_behavior(behavior: &str, message: Option<String>) -> Decision {
        match behavior {
            "allow" => Decision::Allow,
            "deny" => Decision::Deny(message.unwrap_or_default()),
            _ => Decision::Defer,
        }
    }
}

struct Pending {
    tx: Sender<Decision>,
    tool: String,
    detail: String,
}

/// What the bubble shows for the current request — fetched (not just pushed) so
/// a freshly-shown window reliably gets the id even if it missed the emit.
#[derive(Debug, Clone, Serialize)]
pub struct PermissionInfo {
    pub id: u64,
    pub tool: String,
    pub detail: String,
}

/// Registry of in-flight permission requests awaiting a user decision.
#[derive(Default)]
pub struct PermissionRegistry {
    seq: AtomicU64,
    pending: Mutex<HashMap<u64, Pending>>,
    /// Arrival order, so a hotkey/the bubble can resolve "the current request".
    order: Mutex<Vec<u64>>,
}

impl PermissionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new pending request. Returns its id and the receiver the
    /// caller blocks on for the decision.
    pub fn register(&self, tool: String, detail: String) -> (u64, Receiver<Decision>) {
        let id = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = channel();
        self.pending
            .lock()
            .unwrap()
            .insert(id, Pending { tx, tool, detail });
        self.order.lock().unwrap().push(id);
        (id, rx)
    }

    /// The most recent still-pending request's display info (for the bubble to
    /// pull on show). `None` when nothing is pending.
    pub fn current(&self) -> Option<PermissionInfo> {
        let id = *self.order.lock().unwrap().last()?;
        let pending = self.pending.lock().unwrap();
        let p = pending.get(&id)?;
        Some(PermissionInfo {
            id,
            tool: p.tool.clone(),
            detail: p.detail.clone(),
        })
    }

    /// Resolve a specific request. Returns true if it was still pending (so a
    /// duplicate Allow+hotkey race resolves only once).
    pub fn resolve(&self, id: u64, decision: Decision) -> bool {
        self.order.lock().unwrap().retain(|&x| x != id);
        match self.pending.lock().unwrap().remove(&id) {
            Some(p) => {
                // The receiver may already be gone (handler timed out) — ignore.
                let _ = p.tx.send(decision);
                true
            }
            None => false,
        }
    }

    /// Stop tracking a request without sending (the handler timed out / returned).
    pub fn forget(&self, id: u64) {
        self.order.lock().unwrap().retain(|&x| x != id);
        self.pending.lock().unwrap().remove(&id);
    }

    /// The most recent still-pending request id (what a global hotkey acts on).
    pub fn latest(&self) -> Option<u64> {
        self.order.lock().unwrap().last().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn from_behavior_maps_verbs() {
        assert_eq!(Decision::from_behavior("allow", None), Decision::Allow);
        assert_eq!(
            Decision::from_behavior("deny", Some("nope".into())),
            Decision::Deny("nope".into())
        );
        assert_eq!(Decision::from_behavior("none", None), Decision::Defer);
        assert_eq!(Decision::from_behavior("weird", None), Decision::Defer);
    }

    #[test]
    fn register_then_resolve_delivers_decision() {
        let reg = PermissionRegistry::new();
        let (id, rx) = reg.register("Bash".into(), "ls".into());
        assert_eq!(reg.latest(), Some(id));
        assert!(reg.resolve(id, Decision::Allow));
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(Decision::Allow));
        // It's gone now.
        assert_eq!(reg.latest(), None);
        assert!(!reg.resolve(id, Decision::Allow));
    }

    #[test]
    fn latest_tracks_arrival_order() {
        let reg = PermissionRegistry::new();
        let (a, _ra) = reg.register("Bash".into(), "a".into());
        let (b, _rb) = reg.register("Edit".into(), "b".into());
        assert_eq!(reg.latest(), Some(b));
        reg.resolve(b, Decision::Deny("x".into()));
        // Falls back to the older pending request.
        assert_eq!(reg.latest(), Some(a));
    }

    #[test]
    fn current_returns_latest_pending_info() {
        let reg = PermissionRegistry::new();
        assert!(reg.current().is_none());
        let (_a, _ra) = reg.register("Bash".into(), "ls -la".into());
        let (b, _rb) = reg.register("Write".into(), "/tmp/x".into());
        let info = reg.current().unwrap();
        assert_eq!(info.id, b);
        assert_eq!(info.tool, "Write");
        assert_eq!(info.detail, "/tmp/x");
    }

    #[test]
    fn forget_drops_without_sending() {
        let reg = PermissionRegistry::new();
        let (id, rx) = reg.register("Bash".into(), "ls".into());
        reg.forget(id);
        assert_eq!(reg.latest(), None);
        // Sender dropped → receiver errors rather than blocking forever.
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    }
}
