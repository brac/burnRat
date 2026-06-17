//! Loads tunable magic numbers from `data/*.json`.
//!
//! Defaults are embedded at compile time so the app always has valid config,
//! regardless of where the binary runs. During development we additionally try
//! to read the live files from the repo's `data/` dir (resolved via
//! `CARGO_MANIFEST_DIR`) so thresholds can be tuned without a rebuild. In a
//! shipped binary that path does not exist, so the embedded defaults are used.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

const DEFAULT_SETTINGS: &str = include_str!("../../data/settings.default.json");
const DEFAULT_THRESHOLDS: &str = include_str!("../../data/thresholds.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub poll_interval_seconds: u64,
    pub poll_interval_min_seconds: u64,
    pub poll_interval_max_seconds: u64,
    pub rate_window_seconds: i64,
    /// How much cache tokens count toward the burn signal: 0.0 = work only
    /// (input+output), 1.0 = full cache included. The state thresholds are
    /// calibrated for the configured weight — change one, retune the other.
    pub rate_cache_weight: f64,
    pub block_window_hours: i64,
    pub plan: String,
    pub opacity: f64,
    pub click_through: bool,
    pub display: DisplayCfg,
    /// Optional manual tokens-per-window cap (input+output+cache) keyed by plan
    /// name. A nonzero entry for the active `plan` overrides the self-calibrating
    /// ceiling; 0/absent means use the auto-adapted ceiling instead.
    pub plan_limits: HashMap<String, u64>,
    /// How far back (days) the startup scan looks for the largest completed block
    /// when auto-calibrating the usage ceiling.
    pub limit_history_days: i64,
    /// A learned ceiling below this many tokens is treated as not-yet-credible
    /// (too little history) and suppresses the approaching-limit warnings.
    pub limit_min_credible_tokens: u64,
}

/// Rate-readout auto-scale cutoffs (tokens/min). The readout prefers tok/sec and
/// drops to tok/min only at low rates; the gap between the two cutoffs is
/// hysteresis so the displayed unit doesn't flip-flop on a noisy signal.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayCfg {
    /// At or above this smoothed rate (tok/min), show tok/sec.
    pub per_sec_above_tpm: f64,
    /// Below this smoothed rate (tok/min), show tok/min.
    pub per_min_below_tpm: f64,
}

impl Settings {
    /// Poll interval clamped to the configured min/max.
    pub fn poll_interval(&self) -> std::time::Duration {
        let secs = self
            .poll_interval_seconds
            .clamp(self.poll_interval_min_seconds, self.poll_interval_max_seconds);
        std::time::Duration::from_secs(secs)
    }

    /// Estimated token cap for the active plan, if one is configured (> 0).
    /// Drives the approaching-limit warnings; `None` disables them.
    pub fn plan_limit(&self) -> Option<u64> {
        self.plan_limits.get(&self.plan).copied().filter(|&c| c > 0)
    }
}

/// Up/down burn-rate cutoffs (tokens/min) for one state — separate values give
/// hysteresis so a noisy signal doesn't strobe between states.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct StateCut {
    pub up: f64,
    pub down: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateThresholds {
    pub working: StateCut,
    pub stressed: StateCut,
    pub onfire: StateCut,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpentCfg {
    /// Smoothed rate (tokens/min) below which, post-onfire, the rat is spent.
    pub rate_threshold: f64,
    /// How recently onfire must have happened to trigger the crash.
    pub after_onfire_seconds: i64,
    /// How long the spent state lasts before relaxing to calm.
    pub duration_seconds: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnfireCfg {
    pub sustained_seconds: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeCfg {
    pub instant_tpm_flinch: f64,
}

/// Remaining-quota fractions (1 - consumed/limit) at/under which each escalating
/// approaching-limit warning becomes eligible. E.g. warn10 = 0.10 → within 10%.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproachingCfg {
    pub warn10: f64,
    pub warn5: f64,
    pub warn1: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thresholds {
    pub states: StateThresholds,
    pub spent: SpentCfg,
    pub onfire: OnfireCfg,
    pub spike: SpikeCfg,
    pub approaching: ApproachingCfg,
    /// A token written within this many seconds makes the rat at least
    /// `working` immediately, without waiting for the smoothed rate to climb.
    pub activity_floor_seconds: i64,
    /// Idle grace (seconds without new tokens) before the rat naps to sleep.
    pub idle_timeout_seconds: i64,
    /// How long to hold the `done` pose after a finished turn before napping.
    pub done_hold_seconds: i64,
    /// How long to hold the idle pose after the user sends a message (awaiting
    /// Claude) before napping — longer than `idle_timeout_seconds` so we don't
    /// nap through the "dead air" before Claude starts responding.
    pub sent_hold_seconds: i64,
    /// How long to hold the `refreshed` pose after the 5h window rolls over
    /// (fresh quota) before letting the rat nap.
    pub refreshed_hold_seconds: i64,
    /// Active-block age (seconds) beyond which a session counts as long-running.
    /// TODO: this is a first cut — revisit the exact semantics/visual.
    pub long_running_seconds: i64,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub settings: Settings,
    pub thresholds: Thresholds,
}

impl Config {
    pub fn load() -> Self {
        let settings: Settings =
            parse_with_override("settings.default.json", DEFAULT_SETTINGS);
        let thresholds: Thresholds =
            parse_with_override("thresholds.json", DEFAULT_THRESHOLDS);
        Config {
            settings,
            thresholds,
        }
    }
}

/// Path to the repo's `data/` dir as known at compile time (dev only).
fn dev_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("data")
}

/// Parse the live file from `data/` if present, otherwise the embedded default.
fn parse_with_override<T: for<'de> Deserialize<'de>>(file: &str, embedded: &str) -> T {
    let live = dev_data_dir().join(file);
    if let Ok(text) = std::fs::read_to_string(&live) {
        if let Ok(parsed) = serde_json::from_str::<T>(&text) {
            return parsed;
        }
    }
    serde_json::from_str(embedded).expect("embedded default config must be valid")
}
