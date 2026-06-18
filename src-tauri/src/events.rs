//! Layer 3 — transient one-shot events.
//!
//! `refreshed` (the 5h quota window rolled over) and `error` (an API error)
//! used to be *held* poses. They are now brief one-shot events that play over
//! whatever base pose is showing (including `sleeping`) and then hand control
//! back. `flinch` (a rate spike) is the third event. The `EventResolver`
//! centralizes priority + per-event debounce so a retryable hiccup can't spam
//! `error`, and the frontend plays the chosen event for a fixed duration.

use chrono::{DateTime, Duration, Utc};

use crate::config::EventsCfg;

/// Watches the 5-hour-window boundary and fires a **rising edge** exactly once
/// when a window we saw go active rolls over (fresh quota) while the user is
/// idle. Unlike the old tracker it does not *hold* — the event player owns the
/// display duration. A stale window already past its end at first sight is
/// suppressed (no celebration on startup).
pub struct RefreshTracker {
    /// End of the latest used window we're tracking.
    cur_end: Option<DateTime<Utc>>,
    /// Whether we observed that window *before* it expired.
    saw_active: bool,
}

impl Default for RefreshTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RefreshTracker {
    pub fn new() -> Self {
        RefreshTracker { cur_end: None, saw_active: false }
    }

    /// Advance one tick; returns `true` only on the single tick the observed
    /// window rolls over while idle.
    pub fn update(
        &mut self,
        latest_used_end: Option<DateTime<Utc>>,
        recent_activity: bool,
        now: DateTime<Utc>,
    ) -> bool {
        if latest_used_end != self.cur_end {
            self.cur_end = latest_used_end;
            self.saw_active = latest_used_end.is_some_and(|e| now < e);
        }
        if self.saw_active {
            if let Some(end) = self.cur_end {
                if now >= end {
                    self.saw_active = false;
                    // Rising edge — but not if work is actively resuming.
                    return !recent_activity;
                }
            }
        }
        false
    }
}

/// Resolves the single Layer-3 event to emit this tick from the candidate
/// signals, honoring a configured priority order and a per-event cooldown.
pub struct EventResolver {
    cfg: EventsCfg,
    last_error: Option<DateTime<Utc>>,
    last_refreshed: Option<DateTime<Utc>>,
}

impl EventResolver {
    pub fn new(cfg: EventsCfg) -> Self {
        EventResolver { cfg, last_error: None, last_refreshed: None }
    }

    /// Pick the highest-priority *eligible* event (or `None`). `error_now` is the
    /// current API-error signal, `refreshed_edge` the one-tick quota-refresh
    /// edge, `flinch` the spike event from the state machine.
    pub fn resolve(
        &mut self,
        refreshed_edge: bool,
        error_now: bool,
        flinch: Option<&'static str>,
        now: DateTime<Utc>,
    ) -> Option<&'static str> {
        for name in &self.cfg.priority {
            match name.as_str() {
                "error" if error_now && self.ready(self.last_error, self.cfg.error_debounce_seconds, now) => {
                    self.last_error = Some(now);
                    return Some("error");
                }
                "refreshed"
                    if refreshed_edge
                        && self.ready(self.last_refreshed, self.cfg.refreshed_cooldown_seconds, now) =>
                {
                    self.last_refreshed = Some(now);
                    return Some("refreshed");
                }
                // The spike detector already gates flinch, so no extra cooldown.
                "flinch" if flinch.is_some() => return flinch,
                _ => {}
            }
        }
        None
    }

    /// Whether enough time has passed since `last` for an event to fire again.
    fn ready(&self, last: Option<DateTime<Utc>>, cooldown_secs: i64, now: DateTime<Utc>) -> bool {
        match last {
            None => true,
            Some(t) => now - t > Duration::seconds(cooldown_secs.max(0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(n, 0).unwrap()
    }

    fn resolver() -> EventResolver {
        EventResolver::new(EventsCfg {
            priority: vec!["error".into(), "refreshed".into(), "flinch".into()],
            error_debounce_seconds: 30,
            refreshed_cooldown_seconds: 60,
        })
    }

    #[test]
    fn refresh_fires_once_on_edge() {
        let mut r = RefreshTracker::new();
        let end = secs(1000);
        // Observe the window while it's still active (now < end): no edge yet.
        assert!(!r.update(Some(end), false, secs(900)));
        // Boundary passes while idle → fires exactly once.
        assert!(r.update(Some(end), false, secs(1001)));
        // Does NOT hold on subsequent ticks (the player owns duration).
        assert!(!r.update(Some(end), false, secs(1002)));
        assert!(!r.update(Some(end), false, secs(1200)));
    }

    #[test]
    fn refresh_suppressed_if_window_already_expired_at_first_sight() {
        let mut r = RefreshTracker::new();
        let end = secs(1000);
        assert!(!r.update(Some(end), false, secs(1001)));
        assert!(!r.update(Some(end), false, secs(1002)));
    }

    #[test]
    fn refresh_suppressed_when_work_resumes_on_the_edge() {
        let mut r = RefreshTracker::new();
        let end = secs(1000);
        assert!(!r.update(Some(end), false, secs(900)));
        // The boundary passes but a token just landed → no celebration.
        assert!(!r.update(Some(end), true, secs(1001)));
    }

    #[test]
    fn priority_error_beats_refreshed_and_flinch() {
        let mut r = resolver();
        assert_eq!(r.resolve(true, true, Some("flinch"), secs(0)), Some("error"));
    }

    #[test]
    fn refreshed_beats_flinch() {
        let mut r = resolver();
        assert_eq!(r.resolve(true, false, Some("flinch"), secs(0)), Some("refreshed"));
    }

    #[test]
    fn flinch_when_alone() {
        let mut r = resolver();
        assert_eq!(r.resolve(false, false, Some("flinch"), secs(0)), Some("flinch"));
        assert_eq!(r.resolve(false, false, None, secs(0)), None);
    }

    #[test]
    fn error_debounce_suppresses_rapid_refire() {
        let mut r = resolver();
        assert_eq!(r.resolve(false, true, None, secs(0)), Some("error"));
        // Within the 30s debounce: suppressed.
        assert_eq!(r.resolve(false, true, None, secs(10)), None);
        // After the debounce: fires again.
        assert_eq!(r.resolve(false, true, None, secs(40)), Some("error"));
    }
}
