// burnRat frontend — a dumb clear-and-redraw view.
// Sprite rendering runs on its own frame loop (for the working animation and
// the brief surprised pop); the game-state event only updates the data.

import { listen } from "@tauri-apps/api/event";

interface GameState {
  smoothedTpm: number;
  instantTpm: number;
  consumed: number;
  consumedWithCache: number;
  projected: number;
  timeRemainingMin: number;
  isActive: boolean;
  opacity: number;
  state: string;
  event: string | null;
  rateUnit: string; // "sec" | "min" — which unit to render the readout in
  model: string; // model family ("opus"/"sonnet"/…/"none") → hat
}

const FRAME_MS = 280; // animation cadence; also the "1 frame" surprised duration
// Readout easing: each animation frame the shown rate approaches the latest
// value by this fraction. Smaller = smoother, lazier glide. Pure presentation —
// it "fakes" a smooth signal over the chunky, per-turn token writes; the data
// smoothing itself lives in Rust (rateWindowSeconds).
const RATE_EASE_ALPHA = 0.15;

// Auto-discover sprite frames from src/sprites/. Drop in `<state>.png` plus
// optional `<state>_1.png`, `<state>_2.png`, … and they're grouped into that
// state's animation loop automatically: 1 frame = static, 2 = alternate,
// 3+ = smooth ping-pong. Adding a file is picked up on the next dev reload /
// build — no code changes needed.
const modules = import.meta.glob("./sprites/*.png", {
  eager: true,
  query: "?url",
  import: "default",
}) as Record<string, string>;

// Group file URLs by state base name, ordered by frame index.
// "onfire.png" -> base "onfire" idx 0; "onfire_1.png" -> base "onfire" idx 1.
const FRAMES: Record<string, string[]> = {};
{
  const tmp: Record<string, Array<{ idx: number; url: string }>> = {};
  for (const [path, url] of Object.entries(modules)) {
    const name = path.split("/").pop()!.replace(/\.png$/i, "");
    const m = name.match(/^(.+?)_(\d+)$/);
    const base = m ? m[1] : name;
    const idx = m ? parseInt(m[2], 10) : 0;
    (tmp[base] ??= []).push({ idx, url });
  }
  for (const [base, arr] of Object.entries(tmp)) {
    FRAMES[base] = arr.sort((a, b) => a.idx - b.idx).map((x) => x.url);
  }
}

// Map a creature state to its frame base name (states whose art uses a
// different filename go here). Everything else uses the state name directly.
const STATE_BASE: Record<string, string> = { calm: "idle" };

function framesFor(state: string): string[] {
  return FRAMES[STATE_BASE[state] ?? state] ?? FRAMES["idle"] ?? [];
}

// Per-model hats, auto-discovered from src/hats/ by model family
// (`opus.png`, `sonnet.png`, …). TODO: no hat art exists yet — drop files in
// and they light up automatically; until then the overlay stays hidden.
const hatModules = import.meta.glob("./hats/*.png", {
  eager: true,
  query: "?url",
  import: "default",
}) as Record<string, string>;
const HATS: Record<string, string> = {};
for (const [path, url] of Object.entries(hatModules)) {
  HATS[path.split("/").pop()!.replace(/\.png$/i, "")] = url;
}

// Preload all frames so transitions don't flash a blank.
for (const url of Object.values(FRAMES).flat()) {
  const img = new Image();
  img.src = url;
}

// Auto-scaling rate formatter. The unit ("sec"/"min") is chosen in Rust (with
// hysteresis); here we just render the eased value in it. Prefer integers so the
// last digit doesn't flicker — the easing carries the visible motion.
function formatRate(tpm: number, unit: string): string {
  if (unit === "sec") {
    const v = tpm / 60;
    return v < 10 ? `${v.toFixed(1)}/s` : `${Math.round(v)}/s`;
  }
  return tpm >= 1000 ? `${(tpm / 1000).toFixed(1)}k/min` : `${Math.round(tpm)}/min`;
}

// Countdown to the 5h window refresh, shown under the rat when at the limit.
function formatCountdown(min: number): string {
  if (min <= 0) return "0m";
  const h = Math.floor(min / 60);
  const m = min % 60;
  return h > 0 ? `${h}h${m}m` : `${m}m`;
}

window.addEventListener("DOMContentLoaded", () => {
  const pet = document.querySelector<HTMLElement>("#pet");
  const sprite = document.querySelector<HTMLImageElement>("#sprite");
  const hat = document.querySelector<HTMLImageElement>("#hat");
  const readout = document.querySelector<HTMLElement>("#readout");

  let liveState = "sleeping";
  let prevState = "sleeping";
  let surprisedUntil = 0; // show the surprised sprite until this timestamp
  let step = 0;
  let countdownMin = 0; // minutes until window refresh (shown when at-limit)
  let liveModel = "none";

  // Readout easing state, driven on its own animation-frame loop so the number
  // glides smoothly between the sparse, event-driven backend updates.
  let targetTpm = 0; // latest smoothed rate from Rust
  let displayTpm = 0; // eased value actually shown
  let rateUnit = "min";
  let rateActive = false;
  let lastReadout = ""; // avoid redundant DOM writes when nothing changed

  function easeReadout() {
    if (readout) {
      let text = "";
      if (liveState === "atlimit") {
        // At the limit, show the countdown to the quota refresh instead of rate.
        text = `${formatCountdown(countdownMin)} ⏳`;
      } else if (rateActive) {
        displayTpm += (targetTpm - displayTpm) * RATE_EASE_ALPHA;
        if (Math.abs(targetTpm - displayTpm) < 0.5) displayTpm = targetTpm; // snap when settled
        text = formatRate(displayTpm, rateUnit);
      } else {
        displayTpm = 0; // reset so the next wake doesn't glide down from a stale value
      }
      if (text !== lastReadout) {
        readout.textContent = text;
        lastReadout = text;
      }
    }
    requestAnimationFrame(easeReadout);
  }
  requestAnimationFrame(easeReadout);

  // Sprite render loop — independent of the data poll. Ping-pongs through
  // however many frames the current state has (0→1→2→1→0…); 1 frame is static,
  // 2 frames alternate, 3+ make a smooth back-and-forth.
  setInterval(() => {
    if (!sprite) return;
    const frames =
      Date.now() < surprisedUntil ? framesFor("surprised") : framesFor(liveState);
    const n = frames.length;
    if (n === 0) return;
    const period = n <= 1 ? 1 : 2 * (n - 1);
    step = (step + 1) % period;
    const i = n <= 1 ? 0 : step < n ? step : period - step;
    sprite.src = frames[i];
  }, FRAME_MS);

  listen<GameState>("game-state", (event) => {
    const s = event.payload;

    // Surprised "perk-up": resting (sleeping/done/waiting/calm/refreshed/error/
    // longrun) -> busy.
    const RESTING = new Set([
      "sleeping", "done", "waiting", "calm", "refreshed", "error", "longrun",
    ]);
    const BUSY = new Set(["working", "stressed", "onfire"]);
    if (RESTING.has(prevState) && BUSY.has(s.state)) {
      surprisedUntil = Date.now() + FRAME_MS;
    }
    prevState = s.state;
    liveState = s.state;
    countdownMin = s.timeRemainingMin;

    if (pet) {
      pet.className = `pet state-${s.state} model-${s.model}`;
      pet.style.opacity = String(s.opacity);
    }

    // Per-model hat overlay (hidden when there's no art for this model).
    if (hat && s.model !== liveModel) {
      liveModel = s.model;
      const url = HATS[s.model];
      if (url) {
        hat.src = url;
        hat.hidden = false;
      } else {
        hat.removeAttribute("src");
        hat.hidden = true;
      }
    }

    // Feed the eased readout loop; it renders on its own frame cadence.
    rateActive = s.isActive;
    targetTpm = s.smoothedTpm;
    rateUnit = s.rateUnit;
  });
});
