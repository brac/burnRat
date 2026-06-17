# burnRat — Design Doc

> A transparent desktop pet that reacts to your Claude Code token burn rate. The harder you're hammering Claude, the more the rat panics (and eventually combusts). Lightweight, always-on-top, click-through, cross-platform.

---

## 1. Concept

burnRat is an **ambient desktop companion**, not a dashboard. It renders as a single transparent-background graphic floating over the desktop — no window chrome, no taskbar entry, just the rat sitting on top of whatever you're working in.

Its one job: translate your live Claude Code token consumption into a **creature state**. Low burn → calm rat. High burn → stressed/sweating rat. Sustained heavy burn → on fire. The reactive graphic is the whole point; the numbers are secondary and optional.

### Why this exists / market gap

The Claude Code usage-tracking space on GitHub is crowded but **uniformly analytical** — every existing tool is a terminal panel, menu-bar dropdown, browser-toolbar widget, or web dashboard. Nothing occupies the *ambient-companion* niche. burnRat is derivative on the plumbing (everyone reads the same local data) and novel on the presentation (a reactive pet instead of a chart).

### Non-goals (v1)

- Not a historical analytics dashboard. ccusage already does that better.
- Not a billing/cost-optimization advisor.
- Not a team tool. Single-user, single-machine.
- No account, no login, no network calls for core functionality.

---

## 2. Data layer

### Source of truth

Claude Code writes one JSONL file per session to `~/.claude/projects/<project>/<conversation-id>.jsonl`. Every assistant turn carries a `usage` block with exact input / output / cache-creation / cache-read token counts straight from the API. **This is true regardless of plan** (Pro, Max, or API), and it's all local — no Anthropic call needed to get *consumed* tokens.

### Approach: shell out to ccusage (primary), tail JSONL (fallback)

We use **ccusage as the parsing engine** rather than reimplementing JSONL parsing. ccusage already handles the file discovery, the 5-hour billing-window grouping, cache-token accounting, and pricing.

**Critical version note:** `ccusage blocks --live` was **removed in v18.0.0**. The maintainer's recommended path for real-time use is now the `statusline` command. So burnRat must **not** depend on `--live`. Instead we run our own poll loop against the one-shot JSON command:

```bash
ccusage blocks --json
```

We own the interval, which is more robust than depending on ccusage's live renderer anyway. Poll cadence: **default 2s**, configurable 1–10s. (1–2.5s is the established norm across the ecosystem.)

### The JSON shape we consume

`ccusage blocks --json` returns a `blocks[]` array of 5-hour windows. We care about the one where `isActive: true`:

```json
{
  "blocks": [
    {
      "id": "2026-05-16T09:00:00.000Z",
      "startTime": "2026-05-16T09:00:00.000Z",
      "endTime": "2026-05-16T14:00:00.000Z",
      "actualEndTime": "2026-05-16T11:15:00.000Z",
      "isActive": true,
      "tokenCounts": {
        "inputTokens": 4512,
        "outputTokens": 285846,
        "cacheCreationInputTokens": 512,
        "cacheReadInputTokens": 1024
      },
      "costUSD": 156.4,
      "burnRate": 0,
      "projectedTotal": 0,
      "models": ["opus-4-1", "sonnet-4-5"]
    }
  ]
}
```

Fields that matter to us:

| Field | Use |
|---|---|
| `isActive` | Find the current window. If none active → rat is idle/sleeping. |
| `tokenCounts.*` | Sum for total-this-window; feeds the usage bar. |
| `burnRate` | **Tokens per minute** for the active block. This is the primary input to the creature state. |
| `projectedTotal` | Projected tokens if current rate holds — drives "danger" prediction. |
| `timeRemaining` | Human-readable time left in the 5-hour window (active blocks only). |
| `costUSD` | Optional readout. |

### Per-minute granularity (and finer)

ccusage hands us `burnRate` in **tokens/min** directly for the active block. For finer/smoother granularity we compute our own short-window rate by diffing `tokenCounts` totals between polls:

```
deltaTokens = total(now) - total(prev)
deltaSeconds = pollInterval
instantRate = deltaTokens / deltaSeconds   // tokens/sec
```

We keep a rolling buffer (e.g. last 60s) and expose both:
- **Smoothed rate** (rolling avg) → drives the steady creature state, avoids flicker.
- **Instant spikes** → can trigger one-shot animations (rat flinch on a big turn).

### The "usage bar" question

There are two different bars, and they have very different feasibility:

1. **Consumed-this-window bar** — fully doable, zero Anthropic calls. `sum(tokenCounts)` against a reference ceiling. For the ceiling we use ccusage's `-t max` convention (highest previous block) or a user-set limit.

2. **True remaining-quota bar (Pro/Max)** — **no clean public consumer API exists.** Tools approximate it from the known 5-hour rolling window plus reverse-engineered plan caps (community numbers: Pro ~44k, Max5 ~88k, Max20 ~220k tokens per window). burnRat v1 ships the *approximation* bar with a clear "estimated" label, configurable to the user's plan.

3. **Official API bars** — only the **Anthropic Admin API** gives authoritative usage/cost with real progress bars, and that's **org/API-key billing data, not subscription seats.** Out of scope for v1; revisit only if there's demand and the user has an Admin key.

---

## 3. Creature state machine

Burn rate (smoothed tokens/min) maps to a small set of states. Exact thresholds are **magic numbers in `data/`**, tuned during the vertical slice — do not hardcode in logic.

| State | Trigger (illustrative) | Visual |
|---|---|---|
| `sleeping` | No active block | Curled up, Zzz |
| `calm` | Low burn | Idle breathing |
| `working` | Moderate burn | Alert, tapping |
| `stressed` | High burn | Sweating, wide eyes |
| `onfire` | Sustained very high burn | Literally combusting |
| `spent` | Window quota near exhaustion (est.) | Slumped / smoking |

Transitions use hysteresis (separate up/down thresholds) so the rat doesn't strobe between states on a noisy signal. Instant spikes can layer a transient animation on top of the steady state.

---

## 4. Tech stack — Tauri (committed)

**Tauri** (Rust core + web frontend), chosen over Electron.

Rationale:
- **Tiny binary**, low memory — matches the "lightweight" requirement. Electron ships a full Chromium per app.
- **Transparent / always-on-top / click-through windows are first-class** in Tauri's window config.
- **Rust core can poll/spawn `ccusage` and tail the JSONL directly** — no Python sidecar needed (unlike the browser-extension approach, which needs Native Messaging).
- **Cross-platform Mac + Windows** from one codebase.

**Reference implementation:** `LyndonWangWork/Claude-Code-Usage-Tracker` is already Tauri with a compact always-on-top floating mode and 5-hour-window tracking. Read its `src-tauri/tauri.conf.json` for the proven transparent-window setup before scaffolding.

### Window configuration (the floating-pet trick)

```jsonc
{
  "transparent": true,      // see-through background
  "decorations": false,     // no title bar / chrome
  "alwaysOnTop": true,      // floats over other apps
  "skipTaskbar": true,      // no taskbar/dock entry
  "resizable": false,
  "shadow": false
}
```

Plus click-through (mouse events pass to the app underneath) toggled via Tauri's `set_ignore_cursor_events`, so the rat doesn't block clicks on your editor. A modifier-hold or tray toggle re-enables interaction for dragging/repositioning.

### Architecture (mirrors established PixiJS-game conventions where they apply)

- **Rust side:** poll loop spawning `ccusage blocks --json`, parse, compute smoothed/instant rates, push a single `GameState`-style struct to the frontend via Tauri events.
- **Frontend:** dumb clear-and-redraw view. Reads state, picks creature sprite/animation. No business logic in the view.
- **`data/`:** all thresholds, poll interval, plan-cap presets, sprite mappings as magic numbers — tunable without touching logic.
- **Deferred:** audio (rat squeaks on state change) is post-vertical-slice.

---

## 5. Build phases

### Phase 0 — Scaffold
Tauri app with a transparent, decoration-less, always-on-top, click-through window rendering a single static placeholder sprite. Proves the floating-pet shell works on both Mac and Windows. (Reference LyndonWang's `tauri.conf.json`.)

### Phase 1 — Data spine
Rust poll loop → `ccusage blocks --json` → parse active block → compute smoothed + instant tokens/min → emit to frontend. Frontend prints raw numbers (no art yet). Prove the data is live and correct against a real Claude Code session.

### Phase 2 — Creature state machine
Wire burn rate → states with hysteresis. Swap placeholder for real sprite states. **This is the vertical slice that proves the fun** — tune thresholds in `data/` against real sessions until the rat's reactions feel right.

### Phase 3 — Usage bar + polish
Add the estimated consumed/remaining bar (plan-cap presets, "estimated" label), drag-to-reposition, tray menu, settings (poll interval, plan, opacity). Deferred audio lands here.

### Phase 4 — (optional) Admin API
Only if wanted: opt-in Anthropic Admin API integration for authoritative bars, gated behind a user-supplied Admin key. Org-billing data, not subscription — clearly scoped as a power-user extra.

---

## 6. Open decisions

- **Exact burn-rate thresholds** per state — tuned in Phase 2, not guessable up front.
- **Art direction / sprite pipeline** — could lean on the bracSprite flat-color/heavy-outline aesthetic; rat states are a natural fit for that pipeline.
- **ccusage as runtime dependency vs. vendored parser** — start by shelling out to the user's installed ccusage (or bundled via `npx`/`bunx`); revisit vendoring the parse logic in Rust if startup latency or the dependency bothers you.
- **Plan-cap accuracy** — community caps drift as Anthropic adjusts limits; keep them in `data/` and easy to update.

---

## 7. Reference tools surveyed

| Tool | Approach | Relevance to burnRat |
|---|---|---|
| **ccusage** (ryoppippi) | CLI, parses local JSONL, `blocks --json` | **Our data engine.** Note `--live` removed in v18. |
| **better-ccusage** | ccusage fork, multi-provider | Reference if non-Anthropic providers ever matter. |
| **Claude-Code-Usage-Monitor** (Maciek) | Python/Rich terminal, ML burn predictions, plan caps | Source for plan-cap numbers + prediction ideas. |
| **LyndonWang Usage-Tracker** | **Tauri**, floating always-on-top compact mode | **Window-config reference implementation.** |
| **claude-code-counter** | Chrome ext + Python Native Messaging, 2.5s poll | Confirms poll cadence; architecture we're avoiding. |
| **ClaudeWatch / menu-bar tier** | SwiftUI + Python, Mac-only | Presentation tier we're deliberately not in. |
| **pepperonas / phuryn / ccflare** | Heavy web dashboards; pepperonas uses Admin API | Admin-API precedent for the optional Phase 4. |