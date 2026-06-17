//! 5-hour billing-window grouping, equivalent to ccusage `blocks`.
//!
//! Entries are grouped into windows that start at the top of the hour of the
//! first entry and span `block_window_hours`. A gap larger than the window
//! between consecutive entries also starts a new block. The block containing
//! "now" (within the window of both its start and its last activity) is active.

use chrono::{DateTime, Duration, Timelike, Utc};

use crate::data::UsageEntry;

#[derive(Debug, Clone, Default)]
pub struct Block {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub actual_end: DateTime<Utc>,
    pub is_active: bool,
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
}

impl Block {
    /// Real-work tokens (input + output) consumed in this window.
    pub fn work(&self) -> u64 {
        self.input + self.output
    }

    /// Total tokens including cache (optional readout only).
    pub fn total_with_cache(&self) -> u64 {
        self.input + self.output + self.cache_create + self.cache_read
    }
}

fn floor_to_hour(ts: DateTime<Utc>) -> DateTime<Utc> {
    ts.with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(ts)
}

/// Group sorted entries into 5-hour blocks and return them in time order.
pub fn group(entries: &[UsageEntry], window_hours: i64, now: DateTime<Utc>) -> Vec<Block> {
    let window = Duration::hours(window_hours);
    let mut blocks: Vec<Block> = Vec::new();
    let mut last_ts: Option<DateTime<Utc>> = None;

    for e in entries {
        let new_block = match (blocks.last(), last_ts) {
            (None, _) => true,
            (Some(b), Some(prev)) => e.ts - b.start >= window || e.ts - prev >= window,
            (Some(b), None) => e.ts - b.start >= window,
        };

        if new_block {
            let start = floor_to_hour(e.ts);
            blocks.push(Block {
                start,
                end: start + window,
                actual_end: e.ts,
                ..Default::default()
            });
        }

        let b = blocks.last_mut().unwrap();
        b.input += e.input;
        b.output += e.output;
        b.cache_create += e.cache_create;
        b.cache_read += e.cache_read;
        b.actual_end = e.ts;
        last_ts = Some(e.ts);
    }

    for b in &mut blocks {
        b.is_active = now < b.end && (now - b.actual_end) < window;
    }

    blocks
}

/// The currently active block, if any.
pub fn active(blocks: &[Block]) -> Option<&Block> {
    blocks.iter().find(|b| b.is_active)
}

/// Minutes remaining in the active window.
pub fn time_remaining_min(block: &Block, now: DateTime<Utc>) -> i64 {
    (block.end - now).num_minutes().max(0)
}

/// Projected work tokens if the smoothed rate holds for the rest of the window.
pub fn projected_work(block: &Block, smoothed_tpm: f64, now: DateTime<Utc>) -> u64 {
    let remaining = time_remaining_min(block, now) as f64;
    block.work() + (smoothed_tpm * remaining).round() as u64
}
