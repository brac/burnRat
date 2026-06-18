//! Burn-rate computation from the monotonic cumulative work-token counter.
//!
//! We sample `DataMonitor::cumulative_work` each poll and expose two rates:
//! - **smoothed** drives the steady creature state and avoids flicker. It is a
//!   time-aware exponential moving average (EMA) of the per-poll rate, so the
//!   signal rises and decays *continuously* — a burst no longer holds a flat
//!   plateau for the whole window and then craters in one tick.
//! - **instant** (last poll interval) feeds one-shot spike animations and must
//!   stay spiky, so it is the raw per-interval rate, never the EMA.

use chrono::{DateTime, Utc};

use crate::config::DisplayCfg;

/// Which unit the rate readout is currently shown in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateUnit {
    PerSecond,
    PerMinute,
}

impl RateUnit {
    pub fn as_str(&self) -> &'static str {
        match self {
            RateUnit::PerSecond => "sec",
            RateUnit::PerMinute => "min",
        }
    }
}

/// Picks the readout unit from the smoothed rate with hysteresis: prefer tok/sec
/// (granular), drop to tok/min only at low rates, and require crossing the *other*
/// cutoff to switch back so the unit doesn't flip-flop in the boundary band.
pub struct UnitSelector {
    cfg: DisplayCfg,
    unit: RateUnit,
}

impl UnitSelector {
    pub fn new(cfg: DisplayCfg) -> Self {
        UnitSelector {
            cfg,
            unit: RateUnit::PerSecond,
        }
    }

    pub fn select(&mut self, smoothed_tpm: f64) -> RateUnit {
        match self.unit {
            RateUnit::PerSecond if smoothed_tpm < self.cfg.per_min_below_tpm => {
                self.unit = RateUnit::PerMinute;
            }
            RateUnit::PerMinute if smoothed_tpm >= self.cfg.per_sec_above_tpm => {
                self.unit = RateUnit::PerSecond;
            }
            _ => {}
        }
        self.unit
    }
}

/// Time-aware EMA of the per-poll burn rate.
///
/// Each poll we turn the change in the cumulative counter into a per-interval
/// rate (`instant`) and fold it into the EMA with a weight derived from the
/// *elapsed time* since the last poll: `alpha = 1 - exp(-dt/tau)`. This makes
/// the response depend on wall-clock time rather than tick count, which is
/// correct under the loop's variable `dt` (1–10 s), and it decays smoothly even
/// when only the idle tick fires (delta = 0 ⇒ EMA glides toward 0).
///
/// `tau_secs` keeps the peak magnitude comparable to the old 15 s window: a
/// burst of `D` tokens peaks the EMA at `≈ 60·D / tau`, matching the window's
/// `≈ 4·D` at `tau = 15`, so the thresholds in `data/thresholds.json` need no
/// recalibration — only the flat-then-cliff shape changes.
pub struct RateTracker {
    tau_secs: f64,
    /// Previous (timestamp, cumulative work tokens), or `None` before the first
    /// sample.
    prev: Option<(DateTime<Utc>, u64)>,
    /// The smoothed EMA value (tokens/min).
    ema: f64,
    /// The most recent per-interval rate (tokens/min), un-smoothed.
    last_instant: f64,
}

impl RateTracker {
    pub fn new(tau_secs: i64) -> Self {
        RateTracker {
            tau_secs: tau_secs.max(1) as f64,
            prev: None,
            ema: 0.0,
            last_instant: 0.0,
        }
    }

    pub fn sample(&mut self, now: DateTime<Utc>, cumulative_work: u64) {
        if let Some((t_prev, c_prev)) = self.prev {
            let dt_secs = (now - t_prev).num_milliseconds() as f64 / 1000.0;
            if dt_secs > 0.0 {
                let instant = cumulative_work.saturating_sub(c_prev) as f64 / (dt_secs / 60.0);
                let alpha = 1.0 - (-dt_secs / self.tau_secs).exp();
                self.ema = alpha * instant + (1.0 - alpha) * self.ema;
                self.last_instant = instant;
            }
        }
        self.prev = Some((now, cumulative_work));
    }

    /// Smoothed tokens/min (time-aware EMA).
    pub fn smoothed_tpm(&self) -> f64 {
        self.ema
    }

    /// Tokens/min over the most recent poll interval (raw, un-smoothed).
    pub fn instant_tpm(&self) -> f64 {
        self.last_instant
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tracker and feed a cumulative counter that grows by `delta` every
    /// `dt_secs` for `ticks` polls, starting from a fixed epoch. Returns the
    /// tracker after the last sample.
    fn run_steady(tau: i64, delta: u64, dt_secs: i64, ticks: u64) -> RateTracker {
        let mut t = RateTracker::new(tau);
        let start = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let mut cumulative = 0u64;
        for i in 0..ticks {
            let now = start + chrono::Duration::seconds(dt_secs * i as i64);
            t.sample(now, cumulative);
            cumulative += delta;
        }
        t
    }

    #[test]
    fn cold_start_is_zero() {
        let mut t = RateTracker::new(15);
        assert_eq!(t.smoothed_tpm(), 0.0);
        assert_eq!(t.instant_tpm(), 0.0);
        // A single sample (no prior) still yields no rate.
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        t.sample(now, 500);
        assert_eq!(t.smoothed_tpm(), 0.0);
        assert_eq!(t.instant_tpm(), 0.0);
    }

    #[test]
    fn steady_inflow_converges_to_true_rate() {
        // 100 tokens/sec = 6000 tok/min, held long enough (≫ tau) to converge.
        let t = run_steady(15, 100, 1, 600);
        let true_tpm = 6000.0;
        assert!(
            (t.smoothed_tpm() - true_tpm).abs() / true_tpm < 0.01,
            "EMA {} should converge to {}",
            t.smoothed_tpm(),
            true_tpm
        );
    }

    #[test]
    fn instant_tracks_last_interval() {
        // Last interval delivered 200 tokens over 2 s = 6000 tok/min, regardless
        // of the smoothed history.
        let mut t = RateTracker::new(15);
        let start = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        t.sample(start, 0);
        t.sample(start + chrono::Duration::seconds(2), 200);
        assert!((t.instant_tpm() - 6000.0).abs() < 1e-6);
    }

    #[test]
    fn climbs_monotonically() {
        // Equal deltas each tick from cold ⇒ EMA strictly increasing, never
        // overshooting the steady-state rate it is approaching.
        let mut t = RateTracker::new(15);
        let start = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let mut cumulative = 0u64;
        let mut prev = 0.0;
        // 60 tokens/sec = 3600 tok/min steady state.
        for i in 0..120 {
            let now = start + chrono::Duration::seconds(i);
            t.sample(now, cumulative);
            cumulative += 60;
            let cur = t.smoothed_tpm();
            assert!(cur >= prev, "EMA dipped: {} -> {}", prev, cur);
            assert!(cur <= 3600.0 + 1e-6, "EMA overshot steady state: {}", cur);
            prev = cur;
        }
    }

    #[test]
    fn decays_smoothly_after_burst() {
        // One burst, then zero-delta idle ticks: the EMA must strictly decrease
        // every tick (no flat plateau, no one-tick cliff to zero).
        let mut t = RateTracker::new(15);
        let start = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        t.sample(start, 0);
        t.sample(start + chrono::Duration::seconds(1), 100_000); // burst
        let mut prev = t.smoothed_tpm();
        assert!(prev > 0.0);
        for i in 2..20 {
            let now = start + chrono::Duration::seconds(i);
            t.sample(now, 100_000); // no new tokens
            let cur = t.smoothed_tpm();
            assert!(
                cur < prev,
                "EMA did not decay at tick {}: {} -> {}",
                i,
                prev,
                cur
            );
            assert!(cur > 0.0, "EMA cratered to zero in one step at tick {}", i);
            prev = cur;
        }
    }

    #[test]
    fn time_aware_under_variable_dt() {
        // Same total tokens over the same wall-clock span, delivered at 1 s vs
        // 5 s cadence, should land the EMA in nearly the same place — the weight
        // tracks elapsed time, not tick count.
        let fine = run_steady(15, 100, 1, 300); // 100 tok/s for ~300 s
        let coarse = run_steady(15, 500, 5, 60); // 500 tok per 5 s = 100 tok/s for ~300 s
        let diff = (fine.smoothed_tpm() - coarse.smoothed_tpm()).abs();
        assert!(
            diff / fine.smoothed_tpm() < 0.02,
            "cadence changed the EMA: fine {} vs coarse {}",
            fine.smoothed_tpm(),
            coarse.smoothed_tpm()
        );
    }

    fn selector() -> UnitSelector {
        UnitSelector::new(DisplayCfg {
            per_sec_above_tpm: 90.0,
            per_min_below_tpm: 50.0,
        })
    }

    #[test]
    fn high_rate_shows_per_second() {
        let mut s = selector();
        assert_eq!(s.select(8_000.0), RateUnit::PerSecond);
    }

    #[test]
    fn low_rate_drops_to_per_minute() {
        let mut s = selector();
        assert_eq!(s.select(20.0), RateUnit::PerMinute);
    }

    #[test]
    fn hysteresis_holds_unit_in_the_band() {
        let mut s = selector();
        // Start per-second, then a value inside the band (50..90) holds it.
        assert_eq!(s.select(200.0), RateUnit::PerSecond);
        assert_eq!(s.select(70.0), RateUnit::PerSecond);
        // Drop below the low cutoff -> per-minute.
        assert_eq!(s.select(40.0), RateUnit::PerMinute);
        // A value back inside the band keeps per-minute (no flip-flop).
        assert_eq!(s.select(70.0), RateUnit::PerMinute);
        // Only crossing the high cutoff returns to per-second.
        assert_eq!(s.select(95.0), RateUnit::PerSecond);
    }
}
