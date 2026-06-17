//! Burn-rate computation from the monotonic cumulative work-token counter.
//!
//! We sample `DataMonitor::cumulative_work` each poll and expose two rates:
//! - **smoothed** (over the whole rolling window) drives the steady creature
//!   state and avoids flicker.
//! - **instant** (last two samples) feeds one-shot spike animations.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};

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
