//! Creature state machine.
//!
//! Maps the smoothed burn rate (tokens/min) to a creature state using
//! hysteresis: a higher state is entered only when the rate crosses its `up`
//! cutoff, and exited only when it falls below the *current* state's `down`
//! cutoff. `OnFire` additionally requires the rate to stay high for a sustained
//! period. `Spent` is the crash *after* burning hot: when the rate collapses
//! shortly after being onfire, the rat slumps as spent for a while, then
//! relaxes back to calm.

use chrono::{DateTime, Duration, Utc};

use crate::config::Thresholds;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatureState {
    Sleeping,
    Done,
    Waiting,
    Calm,
    Working,
    Stressed,
    OnFire,
    Spent,
    Approaching10,
    Approaching5,
    Approaching1,
}

impl CreatureState {
    pub fn as_str(&self) -> &'static str {
        match self {
            CreatureState::Sleeping => "sleeping",
            CreatureState::Done => "done",
            CreatureState::Waiting => "waiting",
            CreatureState::Calm => "calm",
            CreatureState::Working => "working",
            CreatureState::Stressed => "stressed",
            CreatureState::OnFire => "onfire",
            CreatureState::Spent => "spent",
            CreatureState::Approaching10 => "approaching10",
            CreatureState::Approaching5 => "approaching5",
            CreatureState::Approaching1 => "approaching1",
        }
    }
}

/// Layer an approaching-limit warning over the resolved creature state.
///
/// `level` is the most-severe band the user is in: 0 none, 1 = within 10%,
/// 2 = within 5%, 3 = within 1%. The warning only replaces certain base states
/// (louder as it escalates): 10% shows over idle only; 5% over idle + working;
/// 1% over everything *except* the resting/awaiting poses (sleeping, waiting,
/// done) — those are never overridden at any level.
pub fn apply_approaching(base: CreatureState, level: u8) -> CreatureState {
    use CreatureState::*;
    if matches!(base, Sleeping | Waiting | Done) {
        return base;
    }
    match level {
        3 => Approaching1,
        2 => match base {
            Calm | Working => Approaching5,
            _ => base,
        },
        1 => match base {
            Calm => Approaching10,
            _ => base,
        },
        _ => base,
    }
}

/// Rate-driven tiers, ordered calm < working < stressed < onfire.
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
        }
    }

    /// Advance one tick. Returns the steady state plus an optional transient
    /// event ("flinch") layered on top for one frame.
    pub fn update(
        &mut self,
        is_active: bool,
        done: bool,
        asking: bool,
        recent_activity: bool,
        smoothed_tpm: f64,
        instant_tpm: f64,
        now: DateTime<Utc>,
    ) -> (CreatureState, Option<&'static str>) {
        let event = self.detect_spike(instant_tpm);

        // No active billing window → the rat sleeps; reset everything.
        if !is_active {
            self.level = CALM;
            self.onfire_since = None;
            self.last_onfire = None;
            self.spent_until = None;
            return (CreatureState::Sleeping, event);
        }

        // Awaiting the user takes precedence over the rate-driven states (and
        // the post-onfire crash). Asking a question (interactive tool) →
        // Waiting; a finished turn → Done.
        if asking || done {
            self.level = CALM;
            self.onfire_since = None;
            self.spent_until = None;
            let state = if asking {
                CreatureState::Waiting
            } else {
                CreatureState::Done
            };
            return (state, event);
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
            CALM => CreatureState::Calm,
            WORKING => CreatureState::Working,
            STRESSED => CreatureState::Stressed,
            _ => self.resolve_onfire(now),
        };
        if base == CreatureState::OnFire {
            self.last_onfire = Some(now);
        }

        (self.apply_spent(base, smoothed_tpm, now), event)
    }

    /// The post-onfire crash: collapse to spent, hold, then relax to calm.
    fn apply_spent(
        &mut self,
        base: CreatureState,
        smoothed_tpm: f64,
        now: DateTime<Utc>,
    ) -> CreatureState {
        let cfg = &self.thresholds.spent;

        // Already crashing: stay spent until the timer ends or work resumes.
        if let Some(until) = self.spent_until {
            if self.level >= WORKING {
                self.spent_until = None; // recovered — back to work
            } else if now < until {
                return CreatureState::Spent;
            } else {
                self.spent_until = None; // crash over — relax to calm
            }
        }

        // Enter the crash: rate collapsed soon after burning onfire.
        let collapsed = base == CreatureState::Calm && smoothed_tpm < cfg.rate_threshold;
        let recently_onfire = self
            .last_onfire
            .map(|t| now - t <= Duration::seconds(cfg.after_onfire_seconds))
            .unwrap_or(false);
        if collapsed && recently_onfire {
            self.spent_until = Some(now + Duration::seconds(cfg.duration_seconds));
            self.last_onfire = None; // consume, so it doesn't retrigger after
            return CreatureState::Spent;
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

    fn resolve_onfire(&mut self, now: DateTime<Utc>) -> CreatureState {
        let since = *self.onfire_since.get_or_insert(now);
        let sustained = now - since >= Duration::seconds(self.thresholds.onfire.sustained_seconds);
        if sustained {
            CreatureState::OnFire
        } else {
            CreatureState::Stressed
        }
    }

    fn detect_spike(&mut self, instant_tpm: f64) -> Option<&'static str> {
        let jump = instant_tpm - self.last_instant;
        self.last_instant = instant_tpm;
        if jump >= self.thresholds.spike.instant_tpm_flinch {
            Some("flinch")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApproachingCfg, OnfireCfg, SpentCfg, SpikeCfg, StateCut, StateThresholds, Thresholds,
    };

    fn thresholds() -> Thresholds {
        Thresholds {
            states: StateThresholds {
                working: StateCut { up: 1500.0, down: 800.0 },
                stressed: StateCut { up: 8000.0, down: 5500.0 },
                onfire: StateCut { up: 25000.0, down: 18000.0 },
            },
            approaching: ApproachingCfg { warn10: 0.10, warn5: 0.05, warn1: 0.01 },
            spent: SpentCfg {
                rate_threshold: 1500.0,
                after_onfire_seconds: 90,
                duration_seconds: 20,
            },
            onfire: OnfireCfg { sustained_seconds: 12 },
            spike: SpikeCfg { instant_tpm_flinch: 80000.0 },
            activity_floor_seconds: 15,
            idle_timeout_seconds: 90,
            done_hold_seconds: 120,
            sent_hold_seconds: 180,
        }
    }

    fn t0() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(0, 0).unwrap()
    }

    #[test]
    fn sleeps_when_inactive() {
        let mut m = StateMachine::new(thresholds());
        assert_eq!(m.update(false, false, false, false, 99_999.0, 0.0, t0()).0, CreatureState::Sleeping);
    }

    #[test]
    fn rises_through_tiers() {
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        assert_eq!(m.update(true, false, false, false,500.0, 0.0, t).0, CreatureState::Calm);
        assert_eq!(m.update(true, false, false, false,3_000.0, 0.0, t).0, CreatureState::Working);
        assert_eq!(m.update(true, false, false, false,10_000.0, 0.0, t).0, CreatureState::Stressed);
    }

    #[test]
    fn hysteresis_holds_state() {
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        m.update(true, false, false, false,10_000.0, 0.0, t); // -> stressed
        // Between stressed.down (5500) and stressed.up (8000): stays stressed.
        assert_eq!(m.update(true, false, false, false,6_500.0, 0.0, t).0, CreatureState::Stressed);
        // Below stressed.down: drop to working.
        assert_eq!(m.update(true, false, false, false,4_000.0, 0.0, t).0, CreatureState::Working);
    }

    #[test]
    fn onfire_requires_sustain() {
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        assert_eq!(m.update(true, false, false, false,30_000.0, 0.0, t).0, CreatureState::Stressed);
        let later = t + Duration::seconds(15);
        assert_eq!(m.update(true, false, false, false,30_000.0, 0.0, later).0, CreatureState::OnFire);
    }

    #[test]
    fn spent_is_post_onfire_crash() {
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        // Burn onfire (sustained)...
        m.update(true, false, false, false,30_000.0, 0.0, t);
        let hot = t + Duration::seconds(15);
        assert_eq!(m.update(true, false, false, false,30_000.0, 0.0, hot).0, CreatureState::OnFire);
        // ...then the rate collapses → spent crash.
        let crash = hot + Duration::seconds(10);
        assert_eq!(m.update(true, false, false, false,100.0, 0.0, crash).0, CreatureState::Spent);
        // Still spent during the crash window.
        assert_eq!(
            m.update(true, false, false, false,100.0, 0.0, crash + Duration::seconds(5)).0,
            CreatureState::Spent
        );
        // After the crash window, relaxes to calm.
        assert_eq!(
            m.update(true, false, false, false,100.0, 0.0, crash + Duration::seconds(25)).0,
            CreatureState::Calm
        );
    }

    #[test]
    fn no_spent_without_prior_onfire() {
        let mut m = StateMachine::new(thresholds());
        let t = t0();
        // Low rate with no onfire history → calm, never spent.
        assert_eq!(m.update(true, false, false, false,100.0, 0.0, t).0, CreatureState::Calm);
    }

    #[test]
    fn spike_emits_flinch() {
        let mut m = StateMachine::new(thresholds());
        let (_, e) = m.update(true, false, false, false, 0.0, 90_000.0, t0());
        assert_eq!(e, Some("flinch"));
    }

    #[test]
    fn recent_activity_floors_to_working() {
        let mut m = StateMachine::new(thresholds());
        // A fresh token write (recent_activity) perks up to working immediately,
        // even though the smoothed rate is still ~0.
        assert_eq!(m.update(true, false, false, true, 0.0, 0.0, t0()).0, CreatureState::Working);
    }

    #[test]
    fn done_overrides_rate() {
        let mut m = StateMachine::new(thresholds());
        // A finished turn (done) wins over a high rate / recent activity.
        assert_eq!(
            m.update(true, true, false, true, 30_000.0, 0.0, t0()).0,
            CreatureState::Done
        );
    }

    #[test]
    fn asking_overrides_rate() {
        let mut m = StateMachine::new(thresholds());
        // An interactive question (asking) wins over the rate → Waiting.
        assert_eq!(
            m.update(true, false, true, true, 30_000.0, 0.0, t0()).0,
            CreatureState::Waiting
        );
    }

    use CreatureState::*;

    #[test]
    fn approaching10_shows_over_idle_only() {
        assert_eq!(apply_approaching(Calm, 1), Approaching10);
        // 10% is subtle: it does not interrupt visible work.
        assert_eq!(apply_approaching(Working, 1), Working);
        assert_eq!(apply_approaching(Stressed, 1), Stressed);
    }

    #[test]
    fn approaching5_shows_over_idle_and_working() {
        assert_eq!(apply_approaching(Calm, 2), Approaching5);
        assert_eq!(apply_approaching(Working, 2), Approaching5);
        // ...but not louder states.
        assert_eq!(apply_approaching(Stressed, 2), Stressed);
        assert_eq!(apply_approaching(OnFire, 2), OnFire);
    }

    #[test]
    fn approaching1_shows_over_everything_active() {
        assert_eq!(apply_approaching(Calm, 3), Approaching1);
        assert_eq!(apply_approaching(Working, 3), Approaching1);
        assert_eq!(apply_approaching(Stressed, 3), Approaching1);
        assert_eq!(apply_approaching(OnFire, 3), Approaching1);
        assert_eq!(apply_approaching(Spent, 3), Approaching1);
    }

    #[test]
    fn approaching_never_overrides_resting_poses() {
        // Sleeping / waiting / done are excluded at every level.
        for level in 1..=3 {
            assert_eq!(apply_approaching(Sleeping, level), Sleeping);
            assert_eq!(apply_approaching(Waiting, level), Waiting);
            assert_eq!(apply_approaching(Done, level), Done);
        }
    }

    #[test]
    fn no_warning_passes_state_through() {
        assert_eq!(apply_approaching(Working, 0), Working);
        assert_eq!(apply_approaching(Calm, 0), Calm);
    }
}
