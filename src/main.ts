// burnRat frontend — a dumb clear-and-redraw view.
// Sprite rendering runs on its own frame loop (for the working animation and
// the one-shot event poses); the game-state event only updates the data. The
// backend resolves three layers — base pose, near-limit overlay, and transient
// event — and the view composes them.

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
  baseState: string; // Layer 1 — sleeping/thinking/working/frantic/onfire/spent/done
  nearLimitOpacity: number; // Layer 2 — overlay opacity 0..1 (presentation-ready)
  quotaPercent: number; // Layer 2 — consumed/ceiling (0 if no ceiling); drives the % readout
  event: string | null; // Layer 3 — transient: "refreshed"/"error"/"flinch"
  rateUnit: string; // "sec" | "min" — which unit to render the readout in
  model: string; // model family ("opus"/"sonnet"/…/"none") → hat
  character: string; // active character id (single character for now)
}

const FRAME_MS = 280; // sprite animation cadence
// How long a one-shot event pose (flinch/refreshed/error) plays before handing
// back to the base pose. Pure presentation — the *decision* to fire (priority +
// debounce) is made in Rust; this is just the on-screen dwell.
const EVENT_MS = 900;
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

// Map a base-state / event name to its frame base name (where the existing art
// uses a different filename). Everything else uses the name directly; anything
// missing falls back to the idle pose. This bridge exists only while Stage 1
// still renders from the build-time src/sprites glob — Stage 2 replaces the
// whole glob with per-character assets named exactly by state.
const STATE_BASE: Record<string, string> = {
  thinking: "idle", // Layer-1 thinking pose uses the old idle art
  frantic: "stressed", // renamed from the old stressed pose
  flinch: "surprised", // the flinch event uses the old surprised art
  // refreshed / error have no art yet → fall back to idle.
};

function framesFor(state: string): string[] {
  return FRAMES[STATE_BASE[state] ?? state] ?? FRAMES["idle"] ?? [];
}

// Dev-only: every pose the in-window picker can force — the 7 base states plus
// the transient events. Keep in sync with BaseState::as_str() in
// src-tauri/src/state.rs. Only used when import.meta.env.DEV is true.
const DEV_STATES = [
  "sleeping", "thinking", "working", "frantic", "onfire", "spent", "done",
  "refreshed", "error", "flinch",
];
// Vite sets this true under `tauri dev`, false in a production build, so the
// picker never ships. Read defensively so tsconfig need not include vite types.
const DEV = Boolean((import.meta as { env?: { DEV?: boolean } }).env?.DEV);

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

  let liveState = "sleeping"; // Layer 1 base pose from the backend
  let step = 0;
  let countdownMin = 0; // minutes until window refresh (shown at/over quota)
  let liveModel = "none";
  let quotaPct = 0; // Layer 2 — consumed/ceiling
  let nearLimit = 0; // Layer 2 — overlay opacity 0..1

  // Layer-3 one-shot event player: while now < eventUntil, render activeEvent's
  // frames, then fall back to the base pose.
  let activeEvent: string | null = null;
  let eventUntil = 0;

  // Dev-only forced pose (set via the in-window picker); null = follow live
  // state. When set, it pins the sprite + the pet class.
  let devForced: string | null = null;
  const shownState = () => devForced ?? liveState;

  // Paint the pet container class from the effective state + model. Called from
  // the game-state listener and whenever the dev override changes.
  function paintClass() {
    if (pet) pet.className = `pet state-${shownState()} model-${liveModel}`;
  }

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
      if (quotaPct >= 1.0) {
        // At/over the quota ceiling: show the countdown to the window refresh.
        text = `${formatCountdown(countdownMin)} ⏳`;
      } else if (nearLimit > 0) {
        // In the near-limit band: show how close to the ceiling.
        text = `${Math.round(quotaPct * 100)}%`;
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
    // Pose priority: a dev-forced pose wins outright; otherwise a live one-shot
    // event plays for its dwell, then the base pose.
    let pose: string;
    if (devForced !== null) {
      pose = devForced;
    } else if (activeEvent !== null && Date.now() < eventUntil) {
      pose = activeEvent;
    } else {
      pose = liveState;
    }
    const frames = framesFor(pose);
    const n = frames.length;
    if (n === 0) return;
    const period = n <= 1 ? 1 : 2 * (n - 1);
    step = (step + 1) % period;
    const i = n <= 1 ? 0 : step < n ? step : period - step;
    sprite.src = frames[i];
  }, FRAME_MS);

  listen<GameState>("game-state", (event) => {
    const s = event.payload;

    liveState = s.baseState;
    countdownMin = s.timeRemainingMin;
    quotaPct = s.quotaPercent;
    nearLimit = s.nearLimitOpacity;

    // Per-model hat overlay (hidden when there's no art for this model). Runs
    // before paintClass so liveModel is current for the class string.
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

    // A dev-forced pose pins the class; otherwise track the live state.
    paintClass();
    if (pet) pet.style.opacity = String(s.opacity);

    // Layer 3 — start a one-shot event pose (the sprite loop plays it for its
    // dwell). The backend already debounced/prioritized which one to send.
    if (s.event) {
      activeEvent = s.event;
      eventUntil = Date.now() + EVENT_MS;
    }

    // Feed the eased readout loop; it renders on its own frame cadence.
    rateActive = s.isActive;
    targetTpm = s.smoothedTpm;
    rateUnit = s.rateUnit;
  });

  // ---- Dev-only in-window state picker (never built in a release) ----
  // A single <select> (one clearly-selected pose, "live" to clear) plus a Cycle
  // toggle that sweeps every state. Replaces the old tray submenu whose stuck
  // checkmarks were confusing.
  if (DEV) {
    let cycleTimer: number | null = null;

    const panel = document.createElement("div");
    panel.id = "dev-panel";

    const select = document.createElement("select");
    const live = document.createElement("option");
    live.value = "";
    live.textContent = "— live —";
    select.appendChild(live);
    for (const name of DEV_STATES) {
      const opt = document.createElement("option");
      opt.value = name;
      opt.textContent = name;
      select.appendChild(opt);
    }

    const cycleBtn = document.createElement("button");
    cycleBtn.textContent = "cycle";

    // Set the forced pose, sync the dropdown, and repaint immediately (no need
    // to wait for the next backend tick).
    const setForced = (name: string | null) => {
      devForced = name;
      select.value = name ?? "";
      paintClass();
    };
    const stopCycle = () => {
      if (cycleTimer !== null) {
        clearInterval(cycleTimer);
        cycleTimer = null;
        cycleBtn.classList.remove("on");
      }
    };

    select.addEventListener("change", () => {
      stopCycle();
      setForced(select.value || null);
    });
    cycleBtn.addEventListener("click", () => {
      if (cycleTimer !== null) {
        stopCycle();
        setForced(null);
        return;
      }
      cycleBtn.classList.add("on");
      let i = 0;
      setForced(DEV_STATES[i]);
      cycleTimer = window.setInterval(() => {
        i = (i + 1) % DEV_STATES.length;
        setForced(DEV_STATES[i]);
      }, 1200);
    });

    panel.appendChild(select);
    panel.appendChild(cycleBtn);
    document.body.appendChild(panel);
  }
});
