//! Burn-rate computation from the monotonic cumulative work-token counter.
//!
//! We sample `DataMonitor::cumulative_work` each poll and expose two rates:
//! - **smoothed** (over the whole rolling window) drives the steady creature
//!   state and avoids flicker.
//! - **instant** (last two samples) feeds one-shot spike animations.

use std::collections::VecDeque;

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
        UnitSelector { cfg, unit: RateUnit::PerSecond }
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

pub struct RateTracker {
    window_secs: i64,
    /// (timestamp, cumulative work tokens) samples within the window.
    samples: VecDeque<(DateTime<Utc>, u64)>,
}

impl RateTracker {
    pub fn new(window_secs: i64) -> Self {
        RateTracker {
            window_secs: window_secs.max(1),
            samples: VecDeque::new(),
        }
    }

    pub fn sample(&mut self, now: DateTime<Utc>, cumulative_work: u64) {
        self.samples.push_back((now, cumulative_work));
        let cutoff = now - chrono::Duration::seconds(self.window_secs);
        // Keep one sample older than the cutoff so the window spans the full
        // duration when computing the rate.
        while self.samples.len() > 2 && self.samples[1].0 < cutoff {
            self.samples.pop_front();
        }
    }

    /// Tokens/min averaged across the rolling window.
    pub fn smoothed_tpm(&self) -> f64 {
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return 0.0;
        };
        tpm(first, last)
    }

    /// Tokens/min over the most recent poll interval.
    pub fn instant_tpm(&self) -> f64 {
        let n = self.samples.len();
        if n < 2 {
            return 0.0;
        }
        tpm(&self.samples[n - 2], &self.samples[n - 1])
    }
}

fn tpm(a: &(DateTime<Utc>, u64), b: &(DateTime<Utc>, u64)) -> f64 {
    let minutes = (b.0 - a.0).num_milliseconds() as f64 / 60_000.0;
    if minutes <= 0.0 {
        return 0.0;
    }
    let delta = b.1.saturating_sub(a.1) as f64;
    delta / minutes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selector() -> UnitSelector {
        UnitSelector::new(DisplayCfg { per_sec_above_tpm: 90.0, per_min_below_tpm: 50.0 })
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
