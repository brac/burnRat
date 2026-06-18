//! Layer 1 — the base-state machine.
//!
//! Maps the smoothed burn rate (tokens/min) to one of seven base poses using
//! hysteresis: a higher state is entered only when the rate crosses its `up`
//! cutoff, and exited only when it falls below the *current* state's `down`
//! cutoff. `OnFire` additionally requires the rate to stay high for a sustained
//! period. `Spent` is the crash *after* burning hot: when the rate collapses
//! shortly after being onfire, the rat slumps as spent for a while, then
//! relaxes back to thinking.
//!
//! Quota proximity (Layer 2) and transient events (Layer 3) are computed
//! elsewhere (the poll loop and `events.rs`) and composed by the view — this
//! module only resolves the base pose.

use chrono::{DateTime, Duration, Utc};

use crate::config::Thresholds;

/// The seven base poses every character must supply. This is the fixed contract
/// the character manifests are validated against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseState {
    Sleeping,
    Thinking,
    Working,
    Frantic,
    OnFire,
    Spent,
    Done,
}

impl BaseState {
    pub fn as_str(&self) -> &'static str {
        match self {
            BaseState::Sleeping => "sleeping",
            BaseState::Thinking => "thinking",
            BaseState::Working => "working",
            BaseState::Frantic => "frantic",
            BaseState::OnFire => "onfire",
            BaseState::Spent => "spent",
            BaseState::Done => "done",
        }
    }
}

/// Rate-driven tiers, ordered thinking < working < frantic < onfire.
const CALM: u8 = 0;
const WORKING: u8 = 1;
const STRESSED: u8 = 2;
const ONFIRE: u8 = 3;

pub struct StateMachine {
    thresholds: Thresholds,
    level: u8,
    /// When the rate first crossed the onfire `up` cutoff (for sustain check).
    onfire_since: Option<DateTime<Utc>>,
    /// Last time the rat was actually OnFire (for the post-onfire crash).
    last_onfire: Option<DateTime<Utc>>,
    /// While set and in the future, the rat is crashing (spent).
    spent_until: Option<DateTime<Utc>>,
    last_instant: f64,
    /// Whether the previous tick resolved to `Sleeping`. The `flinch` startle
    /// only fires on the *wake* tick (asleep → active), not on every spike, so
    /// the rat doesn't keep startling as the burn ramps working → onfire.
    was_sleeping: bool,
}

impl StateMachine {
    pub fn new(thresholds: Thresholds) -> Self {
        StateMachine {
            thresholds,
            level: CALM,
            onfire_since: None,
            last_onfire: None,
            spent_until: None,
            last_instant: 0.0,
            // Start "asleep" so a launch straight into an active burst startles
            // once (the rat waking up), but not repeatedly thereafter.
            was_sleeping: true,
        }
    }

    /// Advance one tick. Returns the base pose plus an optional transient
    /// `"flinch"` event (a single-frame startle bounce) for the event resolver.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        is_active: bool,
        done: bool,
        asking: bool,
        sent: bool,
        recent_activity: bool,
        smoothed_tpm: f64,
        instant_tpm: f64,
        now: DateTime<Utc>,
    ) -> (BaseState, Option<&'static str>) {
        let spike = self.detect_spike(instant_tpm);
        let base = self.resolve_base(
            is_active,
            done,
            asking,
            sent,
            recent_activity,
            smoothed_tpm,
            now,
        );

        // Startle only on waking: a rate spike on the first active tick after the
        // rat was asleep. Spikes while it's already awake are just the burn
        // ramping (working → frantic → onfire), so they don't re-trigger it.
        let event = if spike && self.was_sleeping && base != BaseState::Sleeping {
            Some("flinch")
        } else {
            None
        };
        self.was_sleeping = base == BaseState::Sleeping;

        (base, event)
    }

    /// Resolve just the Layer-1 base pose (no event). Split out so `update` can
    /// decide the wake-startle from the resulting pose.
    #[allow(clippy::too_many_arguments)]
    fn resolve_base(
        &mut self,
        is_active: bool,
        done: bool,
        asking: bool,
        sent: bool,
        recent_activity: bool,
        smoothed_tpm: f64,
        now: DateTime<Utc>,
    ) -> BaseState {
        // No active billing window → the rat sleeps; reset everything.
        if !is_active {
            self.level = CALM;
            self.onfire_since = None;
            self.last_onfire = None;
            self.spent_until = None;
            return BaseState::Sleeping;
        }

        // Awaiting the user takes precedence over the rate-driven states (and
        // the post-onfire crash). A finished turn *and* an interactive question
        // both read as `Done` (the rat is waiting on you).
        if asking || done {
            self.level = CALM;
            self.onfire_since = None;
            self.spent_until = None;
            return BaseState::Done;
        }

        // A fresh user message (awaiting Claude, no tokens flowing yet) → the
        // latency-gap "thinking" pose, so the rat ponders rather than napping or
        // sitting calm through the dead air before Claude responds.
        if sent {
            self.level = CALM;
            self.onfire_since = None;
            self.spent_until = None;
            return BaseState::Thinking;
        }

        self.advance_level(smoothed_tpm);
        // Fast attack: a token was just written, so perk up to at least working
        // immediately instead of waiting for the smoothed rate to ramp.
        if recent_activity && self.level < WORKING {
            self.level = WORKING;
        }
        if self.level < ONFIRE {
            self.onfire_since = None;
        }

        let base = match self.level {
            CALM => BaseState::Thinking,
            WORKING => BaseState::Working,
            STRESSED => BaseState::Frantic,
            _ => self.resolve_onfire(now),
        };
        if base == BaseState::OnFire {
            self.last_onfire = Some(now);
        }

        self.apply_spent(base, smoothed_tpm, now)
    }

    /// The post-onfire crash: collapse to spent, hold, then relax to thinking.
    fn apply_spent(&mut self, base: BaseState, smoothed_tpm: f64, now: DateTime<Utc>) -> BaseState {
        let cfg = &self.thresholds.spent;

        // Already crashing: stay spent until the timer ends or work resumes.
        if let Some(until) = self.spent_until {
            if self.level >= WORKING {
                self.spent_until = None; // recovered — back to work
            } else if now < until {
                return BaseState::Spent;
            } else {
                self.spent_until = None; // crash over — relax to thinking
            }
        }

        // Enter the crash: rate collapsed soon after burning onfire.
        let collapsed = base == BaseState::Thinking && smoothed_tpm < cfg.rate_threshold;
        let recently_onfire = self
            .last_onfire
            .map(|t| now - t <= Duration::seconds(cfg.after_onfire_seconds))
            .unwrap_or(false);
        if collapsed && recently_onfire {
            self.spent_until = Some(now + Duration::seconds(cfg.duration_seconds));
            self.last_onfire = None; // consume, so it doesn't retrigger after
            return BaseState::Spent;
        }

        base
    }

    fn advance_level(&mut self, r: f64) {
        let s = &self.thresholds.states;
        let ups = [0.0, s.working.up, s.stressed.up, s.onfire.up];
        let downs = [0.0, s.working.down, s.stressed.down, s.onfire.down];

        while self.level < ONFIRE && r >= ups[(self.level + 1) as usize] {
            self.level += 1;
        }
        while self.level > CALM && r < downs[self.level as usize] {
            self.level -= 1;
        }
    }

    fn resolve_onfire(&mut self, now: DateTime<Utc>) -> BaseState {
        let since = *self.onfire_since.get_or_insert(now);
        let sustained = now - since >= Duration::seconds(self.thresholds.onfire.sustained_seconds);
        if sustained {
            BaseState::OnFire
        } else {
            BaseState::Frantic
        }
    }

    /// Whether the instant rate jumped by at least the spike threshold this tick.
    /// Always advances `last_instant`; the caller decides whether the spike
    /// actually startles (only on waking — see `update`).
    fn detect_spike(&mut self, instant_tpm: f64) -> bool {
        let jump = instant_tpm - self.last_instant;
        self.last_instant = instant_tpm;
        jump >= self.thresholds.spike.instant_tpm_flinch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        EventsCfg, OnfireCfg, QuotaCfg, SpentCfg, SpikeCfg, StateCut, StateThresholds, Thresholds,
    };

    fn thresholds() -> Thresholds {
        Thresholds {
            states: StateThresholds {
                working: StateCut {
                    up: 1500.0,
                    down: 800.0,
                },
                stressed: StateCut {
                    up: 8000.0,
                    down: 5500.0,
                },
                onfire: StateCut {
                    up: 25000.0,
                    down: 18000.0,
                },
            },
            quota: QuotaCfg {
                start_percent: 0.90,
                full_percent: 0.99,
            },
            events: EventsCfg {
                priority: vec!["error".into(), "refreshed".into(), "flinch".into()],
                error_debounce_seconds: 30,
                refreshed_cooldown_seconds: 60,
            },
            spent: SpentCfg {
                rate_threshold: 1500.0,
                after_onfire_seconds: 90,
                duration_seconds: 20,
            },
            onfire: OnfireCfg {
                sustained_seconds: 12,
            },
            spike: SpikeCfg {
                instant_tpm_flinch: 80000.0,
            },
            activity_floor_seconds: 15,
            idle_timeout_seconds: 90,
            done_hold_seconds: 120,
            hook_signal_ttl_seconds: 120,
        }
    }

    fn t0() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(0, 0).unwrap()
    }

    // Convenience: (is_active, done, asking, sent, recent, smoothed, instant).
    fn step(m: &mut StateMachine, smoothed: f64, now: DateTime<Utc>) -> BaseState {
        m.update(true, false, false, false, false, smoothed, 0.0, now)
            .0
    }

    #[test]
    fn sleeps_when_inactive() {
        let mut m = StateMachine::new(thresholds());
        assert_eq!(
            m.update(false, false, false, false, false, 99_999.0, 0.0, t0())
                .0,
            BaseState::Sleeping
        );
    }

    #[test]
    fn rises_through_tiers() {
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        assert_eq!(step(&mut m, 500.0, t), BaseState::Thinking);
        assert_eq!(step(&mut m, 3_000.0, t), BaseState::Working);
        assert_eq!(step(&mut m, 10_000.0, t), BaseState::Frantic);
    }

    #[test]
    fn hysteresis_holds_state() {
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        step(&mut m, 10_000.0, t); // -> frantic
                                   // Between stressed.down (5500) and stressed.up (8000): stays frantic.
        assert_eq!(step(&mut m, 6_500.0, t), BaseState::Frantic);
        // Below stressed.down: drop to working.
        assert_eq!(step(&mut m, 4_000.0, t), BaseState::Working);
    }

    #[test]
    fn onfire_requires_sustain() {
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        assert_eq!(step(&mut m, 30_000.0, t), BaseState::Frantic);
        let later = t + Duration::seconds(15);
        assert_eq!(step(&mut m, 30_000.0, later), BaseState::OnFire);
    }

    #[test]
    fn spent_is_post_onfire_crash() {
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        // Burn onfire (sustained)...
        step(&mut m, 30_000.0, t);
        let hot = t + Duration::seconds(15);
        assert_eq!(step(&mut m, 30_000.0, hot), BaseState::OnFire);
        // ...then the rate collapses → spent crash.
        let crash = hot + Duration::seconds(10);
        assert_eq!(step(&mut m, 100.0, crash), BaseState::Spent);
        // Still spent during the crash window.
        assert_eq!(
            step(&mut m, 100.0, crash + Duration::seconds(5)),
            BaseState::Spent
        );
        // After the crash window, relaxes to thinking.
        assert_eq!(
            step(&mut m, 100.0, crash + Duration::seconds(25)),
            BaseState::Thinking
        );
    }

    #[test]
    fn no_spent_without_prior_onfire() {
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        // Low rate with no onfire history → thinking, never spent.
        assert_eq!(step(&mut m, 100.0, t), BaseState::Thinking);
    }

    #[test]
    fn spike_emits_flinch_on_wake() {
        // A fresh machine starts "asleep", so the first active tick with a spike
        // startles (the rat waking up).
        let mut m = StateMachine::new(thresholds());
        let (_, e) = m.update(true, false, false, false, false, 0.0, 90_000.0, t0());
        assert_eq!(e, Some("flinch"));
    }

    #[test]
    fn no_flinch_while_already_awake() {
        // Wake with a spike (startles), then a second, even bigger spike while
        // already active must NOT re-startle — that was the "startle bug" where
        // the rat flinched repeatedly as the burn ramped.
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        let (_, e1) = m.update(true, false, false, false, false, 0.0, 90_000.0, t);
        assert_eq!(e1, Some("flinch"));
        let (_, e2) = m.update(true, false, false, false, false, 5_000.0, 300_000.0, t);
        assert_eq!(e2, None);
    }

    #[test]
    fn flinch_again_after_sleeping() {
        // Once the rat sleeps and wakes again, a spike startles anew.
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        // Wake + startle, then ride the burn (no further flinches).
        m.update(true, false, false, false, false, 0.0, 90_000.0, t);
        m.update(true, false, false, false, false, 5_000.0, 90_000.0, t);
        // Go to sleep (inactive).
        let (b, _) = m.update(false, false, false, false, false, 0.0, 0.0, t);
        assert_eq!(b, BaseState::Sleeping);
        // Wake again with a burst → startles again.
        let (_, e) = m.update(true, false, false, false, true, 0.0, 90_000.0, t);
        assert_eq!(e, Some("flinch"));
    }

    #[test]
    fn recent_activity_floors_to_working() {
        let mut m = StateMachine::new(thresholds());
        // A fresh token write (recent_activity) perks up to working immediately,
        // even though the smoothed rate is still ~0.
        assert_eq!(
            m.update(true, false, false, false, true, 0.0, 0.0, t0()).0,
            BaseState::Working
        );
    }

    #[test]
    fn done_maps_to_done() {
        let mut m = StateMachine::new(thresholds());
        // A finished turn wins over a high rate / recent activity.
        assert_eq!(
            m.update(true, true, false, false, true, 30_000.0, 0.0, t0())
                .0,
            BaseState::Done
        );
    }

    #[test]
    fn asking_maps_to_done() {
        let mut m = StateMachine::new(thresholds());
        // An interactive question now also reads as Done (waiting on the user).
        assert_eq!(
            m.update(true, false, true, false, true, 30_000.0, 0.0, t0())
                .0,
            BaseState::Done
        );
    }

    #[test]
    fn sent_maps_to_thinking() {
        let mut m = StateMachine::new(thresholds());
        // A just-sent user message → thinking, even over a stale high rate.
        assert_eq!(
            m.update(true, false, false, true, false, 30_000.0, 0.0, t0())
                .0,
            BaseState::Thinking
        );
    }
}
