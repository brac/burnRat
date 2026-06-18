# Plan: EMA smoothing for the burn-rate signal

## Goal

Make the creature ramp **continuously** between states. Today the rate signal
holds flat at a burst's level for the whole window and then drops off a cliff,
which makes both the climb and the descent step in chunks instead of gliding.
Replace the window-difference smoothing with a **time-aware exponential moving
average (EMA)** so the rate rises and decays smoothly.

## Background — why the current signal is chunky

`RateTracker::smoothed_tpm` (`src-tauri/src/rate.rs:82`) computes
`(last - first) / window_minutes` over a 15 s sample deque. Because a whole
turn's tokens land on a single JSONL line, the cumulative counter is bursty, so:

- **Plateau-then-cliff descent.** After a burst the pre-burst sample stays as
  `first`, so the rate reads a near-constant `~4 × delta` for ~15 s, then craters
  to ~0 in a single tick when the burst sample rolls out of the window
  (`sample`, `rate.rs:71-79`). The level machine then drops multiple bands in one
  tick.
- **Chunky climb.** A single large turn can push the smoothed rate from ~0 into
  the stressed band in one tick (`advance_level`, `src-tauri/src/state.rs:271`),
  skipping a visible `working` dwell.

The state machine's gates (`onfire` 12 s sustain, the `burning`/`spent` descent
guards in `src-tauri/src/lib.rs:259-270`) already prevent the catastrophic jumps
(`→onfire`, `→sleeping`). The remaining roughness is in the **rate signal
itself**, so that's where the fix belongs.

## Approach — time-aware EMA

Track a single smoothed value updated each poll from the per-interval rate:

```
instant = (cumulative_now - cumulative_prev) / dt_minutes      // per-poll rate
alpha   = 1 - exp(-dt_seconds / tau_seconds)                   // time-aware weight
ema     = alpha * instant + (1 - alpha) * ema_prev
```

`smoothed_tpm()` returns `ema`; `instant_tpm()` keeps returning the last
per-interval rate (it still feeds the spike/flinch detector and must stay spiky).

### Why time-aware (not a fixed alpha)

The poll loop is event-driven with a 1–10 s idle fallback (`lib.rs` poll loop),
so `dt` varies. A fixed alpha would smooth differently depending on cadence.
`alpha = 1 - exp(-dt/tau)` makes the response depend on elapsed *time*, not tick
count — correct under variable `dt`, and it decays smoothly even when only the
10 s idle tick fires (delta = 0 ⇒ EMA glides toward 0).

### Why magnitude stays comparable (minimal threshold retuning)

For a burst of `D` tokens captured at a poll of length `dt`, the EMA's peak jump
is `alpha · (D·60/dt)`. In the small-`dt` limit this is `≈ 60·D / tau`. With
`tau = 15 s` that's `≈ 4·D` — **the same peak the 15 s window produced**. So
keeping `tau = 15` preserves the magnitude scale the thresholds in
`data/thresholds.json` are calibrated for, but replaces the flat-then-cliff
shape with a smooth exponential decay. This is the key property that lets us
ship without a full threshold recalibration.

## Implementation

### 1. Rewrite `RateTracker` (`src-tauri/src/rate.rs`)

Drop the sample deque; keep only the previous sample plus the EMA accumulator.

```rust
pub struct RateTracker {
    tau_secs: f64,
    prev: Option<(DateTime<Utc>, u64)>,
    ema: f64,
    last_instant: f64,
}

impl RateTracker {
    pub fn new(tau_secs: i64) -> Self {
        RateTracker { tau_secs: (tau_secs.max(1)) as f64, prev: None, ema: 0.0, last_instant: 0.0 }
    }

    pub fn sample(&mut self, now: DateTime<Utc>, cumulative: u64) {
        if let Some((t_prev, c_prev)) = self.prev {
            let dt_secs = (now - t_prev).num_milliseconds() as f64 / 1000.0;
            if dt_secs > 0.0 {
                let instant = (cumulative.saturating_sub(c_prev)) as f64 / (dt_secs / 60.0);
                let alpha = 1.0 - (-dt_secs / self.tau_secs).exp();
                self.ema = alpha * instant + (1.0 - alpha) * self.ema;
                self.last_instant = instant;
            }
        }
        self.prev = Some((now, cumulative));
    }

    pub fn smoothed_tpm(&self) -> f64 { self.ema }
    pub fn instant_tpm(&self) -> f64 { self.last_instant }
}
```

- `smoothed_tpm` / `instant_tpm` keep the same signature, so **`lib.rs`,
  `state.rs`, `blocks::projected_work`, and `UnitSelector` need no changes.**
- Cold start (`prev = None`) ⇒ `ema = 0`, matching today.
- Counter reset / decrease ⇒ `saturating_sub` ⇒ 0, matching today.
- Long idle gap ⇒ `alpha → 1` ⇒ EMA snaps to the (small) new rate; harmless
  because bursts arrive on ~1 s file-change events, not on the 10 s idle tick.

### 2. Config (`data/settings.default.json` + `src-tauri/src/config.rs`)

Reuse the existing knob to avoid migrating user `settings.json` overrides:
keep the field but reinterpret `rateWindowSeconds` (default `15`) as the EMA
time constant `tau`. Update its `$comment` to say it is now a smoothing time
constant (larger = smoother/slower, ~`tau` seconds to forget a burst), not a
hard window.

Optional cleanup (separate, breaking): rename to `rateSmoothingTauSeconds` with a
backward-compatible read of the old key. Defer unless we touch config anyway.

### 3. Tests (`src-tauri/src/rate.rs` `#[cfg(test)]`)

- `steady_inflow_converges_to_true_rate` — constant delta per second ⇒ EMA
  converges to that tok/min within tolerance.
- `decays_smoothly_after_burst` — one burst then zero-delta ticks ⇒ EMA is
  strictly **decreasing** every tick (no plateau, no one-tick cliff).
- `climbs_monotonically` — successive equal deltas ⇒ EMA strictly increasing,
  no overshoot above steady state.
- `time_aware_under_variable_dt` — same total tokens over the same wall-clock
  span at 1 s vs 5 s cadence ⇒ EMA ends within tolerance (cadence-independent).
- `cold_start_is_zero` and `instant_tracks_last_interval` — preserve existing
  contract; keep the spike/flinch path working.

The `state.rs` tests feed `smoothed` directly and are unaffected.

## Validation / calibration

1. `cd src-tauri && cargo test` — rate + state + message-classification suites.
2. `npm run tauri dev`; drive a heavy turn and watch the climb, then go idle and
   watch the decay. Confirm: `calm → working → stressed` glides up and
   `stressed → working → calm → sleep` glides down (no band-skips at the end of
   activity).
3. Cross-check magnitude against `npx ccusage@latest blocks --json` (work-only
   rate is smaller than ccusage's cache-inclusive number — expected).
4. **Re-verify `onfire` reachability.** With EMA, a *single* mega-turn peaks for
   one tick then decays, so it may no longer hold the onfire band for the 12 s
   sustain — only genuinely sustained heavy burning will. Decide whether that's
   the desired semantics (arguably yes). If we want single big turns to still
   reach fire, lower `onfire.up` or `onfire.sustainedSeconds` in
   `data/thresholds.json`, or raise `tau`.

## Risks & tradeoffs

- **onfire now means *sustained* burn**, not one big turn (see step 4). Most
  likely an improvement, but a behavior change to confirm.
- **Peak-magnitude match is exact only in the small-`dt` limit**; at larger `dt`
  the burst peak reads slightly lower. Negligible at 1 s polls during activity.
- **`instant` must stay un-smoothed** for the flinch detector — preserved.

## Scope / non-goals

- No change to the state machine, the `awake`/`burning`/`spent` gates, or the
  frontend glide (`RATE_EASE_ALPHA` in `src/main.ts` stays a presentation
  concern).
- No new dependencies; `f64::exp` is std.

## Rollout

Single isolated change to `rate.rs` (+ config note + tests). Trivial to revert
(restore the deque). No migration needed if we keep the `rateWindowSeconds` key.
A `rateSmoothing: "ema" | "window"` toggle is possible for A/B but not worth the
complexity (YAGNI) — recommend shipping EMA outright.
