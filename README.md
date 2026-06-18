# burnRat 🐀

A transparent desktop pet that reacts to your live **Claude Code token burn rate**. The harder you hammer Claude, the more the rat panics — calm → working → stressed → on fire — and when you burn out hot then stop, it slumps spent. Lightweight, always-on-top, draggable, cross-platform (macOS + Windows).

burnRat is an *ambient companion*, not a dashboard. It floats over whatever you're working in and translates your token consumption into a creature state. The reactive pet is the point; the numbers are secondary. Unlike the usual desktop pets that wander around at random, burnRat is wired to real telemetry — it only reacts to what you're actually doing to Claude.

---

## How it works

Claude Code writes one JSONL file per session to `~/.claude/projects/<project>/<conversation-id>.jsonl`, and every assistant turn records exact token usage. burnRat tails those files directly in Rust (no external dependency), computes a smoothed burn rate (tokens per minute), and maps it to a creature state. By default the signal includes cache tokens (`rateCacheWeight`, see [Tuning](#tuning)) for big, lively numbers; set the weight to `0` to watch only work (input + output) tokens.

The state is resolved in Rust as **three independent layers** the view composes:

**Layer 1 — base pose** (7 required — the fixed contract every character supplies — plus 2 optional, `idle`/`asking`, which fall back if a character lacks the art):

| Pose | When |
|---|---|
| `sleeping` | No active window / idle nap |
| `idle` | Awake but quiet — a lull between turns, or you composing a message (then naps). *Optional; falls back to `thinking`.* |
| `thinking` | You just sent a message and Claude is about to respond ("Claude is thinking") |
| `working` | Moderate burn (animated loop) |
| `frantic` | High burn |
| `onfire` | Sustained very high burn |
| `spent` | The crash *after* burning onfire — the rate collapses and the rat slumps |
| `done` | Claude finished a turn — your move |
| `asking` | The agent is asking *you* something — an interactive question, or a pending tool-permission request (see below). *Optional; falls back to `done`.* |

**Layer 2 — near-limit overlay.** As you approach your (auto-calibrated) usage ceiling, a warning overlay fades in over whatever pose is showing, and the readout switches to a `%`, then a countdown to the window refresh once you're at the limit.

**Layer 3 — transient events.** Brief one-shots that play over the base pose and hand control back: `refreshed` (your 5-hour quota window just rolled over — a ~5-minute celebration), `error` (Claude Code hit an API error), and `flinch` (a startle when the rat wakes from sleep into a sudden burst of work).

Thresholds use hysteresis so the pose doesn't strobe on a noisy signal.

### Claude Code integration (optional, on by default)

Beyond tailing the logs, burnRat can connect to Claude Code's **lifecycle hooks** for instant, precise reactions. It runs a tiny **loopback-only** listener (`127.0.0.1`) and installs its own hook entries into `~/.claude/settings.json`. Toggle it from the tray (**"Connect to Claude Code"**); when off, burnRat opens no socket and touches no settings.

- **Sharper, instant states.** Turn-start, turn-end, and tool-use edges arrive the moment they happen — so the rat reacts without waiting for the next poll. The log tail stays the source of truth (and the fallback when disconnected); the hooks only sharpen the discrete poses.
- **Approve tool permissions from the pet.** When Claude asks to run a tool, a small **Allow / Deny bubble** pops up over the rat (which shifts to its `asking` pose). Decide with the mouse or global **Ctrl/Cmd+Shift+Y** (allow) / **Ctrl/Cmd+Shift+N** (deny) — no need to switch to the terminal. If you ignore it, or burnRat isn't running, it quietly **defers to Claude's normal terminal prompt** — burnRat never silently allows or blocks anything.

Everything here is loopback-only and opt-in; the listener and the hooks exist only while connected.

**Napping is smart about your messages.** The nap clock runs from the last conversational line (yours *or* Claude's), so sending a message resets it — no jarring `done → message → nap`. While you're **waiting on Claude's reply** the rat holds the `thinking` pose and *won't* nap — the wait ends the instant Claude responds. When nothing's happening at all (a lull, or you composing) it sits in `idle` until the nap timer fires. The rat also won't nap while the burn rate is still elevated — it always winds down through its lower states (e.g. `onfire → frantic → working → idle → sleeping`) rather than snapping straight from a high state into a nap.

### Characters

The pet is a **character** — a folder under [`characters/`](characters/) holding a `character.json` manifest plus ~10 PNGs, one per base pose (`sleeping`, `working`, `frantic`, …) plus the near-limit overlay and the transient event poses. Characters are discovered and loaded at **runtime**: drop a new folder in (bundled with the app, or in the per-user characters dir) and it appears in the tray **Character** submenu to swap live — no rebuild. burnRat ships two: the [`rat`](characters/rat/) (reference) and a [flaming `skull`](characters/skull/).

The **filename is the contract** and extra ping-pong frames **auto-discover** from the folder: drop `working_1.png`, `working_2.png`, … next to `working.png` and they join that pose's loop in order (1 frame = static, 2 = alternate, 3+ = smooth ping-pong) — no manifest edit. While developing (`tauri dev`), a file-watcher hot-reloads art the moment you change it.

Art should be **300×300** (2× the 150×150 display) and well under ~100 KB. Run **`npm run optimize-art`** to resize/compress every PNG in place — lossless where it can, alpha preserved.

---

## Prerequisites

- **Node.js** 18+ and npm
- **Rust** toolchain (`rustup`) — https://rustup.rs
- Platform build tools for Tauri 2:
  - **Windows:** Visual Studio C++ Build Tools (MSVC); WebView2 ships with Windows 11.
  - **macOS:** Xcode Command Line Tools (`xcode-select --install`).

See the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for details.

## Run

```bash
npm install
npm run tauri dev
```

> **Windows note:** if you just installed Rust, open a fresh terminal so `~/.cargo/bin` is on your PATH (or run `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"` first).

## Build a release binary

```bash
npm run tauri build
```

Produces a `.msi`/`.exe` on Windows and a `.app`/`.dmg` on macOS.

---

## Using it

- **Move it:** click and drag the rat anywhere on screen. Its position is remembered across restarts.
- **Pass-through:** if the rat is ever in the way of a click, press **Ctrl/Cmd+Shift+M** (or use the tray menu) to let clicks fall through to the app underneath. Press again to make it grabbable.
- **Permission bubble:** when connected to Claude Code, **Ctrl/Cmd+Shift+Y** allows / **Ctrl/Cmd+Shift+N** denies a pending tool-permission request (or click the bubble).
- **Tray menu:** toggle pass-through, set **Opacity**, pick a **Character** (swaps live), **Connect to Claude Code** (the hook integration above), and **Quit**.

---

## Tuning

All the magic numbers live in [`data/`](data/) and are read live in `dev` (no rebuild needed):

- **`data/thresholds.json`** — burn-rate cutoffs per state (with up/down hysteresis), the onfire sustain time, the post-onfire `spent` crash, the `quota` near-limit ramp (`startPercent`/`fullPercent`), the Layer-3 `events` config (priority order + `errorDebounceSeconds`/`refreshedCooldownSeconds`), `hookSignalTtlSeconds` (how long a lifecycle-hook edge can override the JSONL-inferred state when the hook bridge is connected), and the nap/hold timers: `idleTimeoutSeconds` (idle grace before the rat sleeps) and `doneHoldSeconds` (how long the `done` pose holds after a finished turn). While awaiting Claude's reply the rat holds `thinking` indefinitely (no nap, no knob).
- **`data/settings.default.json`** — poll interval, rate smoothing window, 5-hour block window, default opacity, whether it starts interactive or pass-through, `display` (the tok/sec↔tok/min auto-scale cutoffs for the readout), `rateCacheWeight` (how much cache counts toward the burn signal — `1.0` = full cache/bigger numbers, `0.0` = work only; **retune `thresholds.json` if you change it**), the usage-limit settings (`limitHistoryDays` / `limitMinCredibleTokens` for the auto-calibrated approaching-limit ceiling, plus `planLimits` as an optional manual override), and `localServer` (the Claude Code hook integration — `enabled` default on, the candidate `ports`, and `permissionTimeoutSeconds` before a permission request defers to the terminal).

The **near-limit overlay** (Layer 2) calibrates its ceiling automatically: on startup burnRat scans your recent history for the largest *completed* 5-hour block (tokens incl. cache) and uses that as the limit estimate, rather than guessing a cap. The overlay opacity ramps in between `quota.startPercent` and `quota.fullPercent` of that ceiling. It reads conservatively until you have history, and since your past peak is a lower bound on your true limit it can warn a little early — set a `planLimits` entry if you'd rather pin an exact cap.

The rate readout under the rat **auto-scales** between tokens/sec and tokens/min (with hysteresis so the unit doesn't flip-flop) and is **display-eased** on an animation-frame loop so the number glides smoothly over Claude's chunky, per-turn token writes — the underlying data smoothing is still `rateWindowSeconds`.

User-changed settings (opacity, selected character) persist to your OS app-config dir; defaults are bundled into the binary.

---

## Tech

- **[Tauri 2](https://tauri.app)** — Rust core + web frontend, tiny binary, first-class transparent/always-on-top/click-through windows.
- **Rust** core: tails the JSONL incrementally (per-file byte cursors, dedup), 5-hour billing-window grouping equivalent to [ccusage](https://github.com/ryoppippi/ccusage), rolling burn-rate, and the creature state machine.
- **Optional Claude Code hook bridge:** a hand-rolled loopback HTTP server (no async stack, no extra deps) plus burnRat's own hooks in `~/.claude/settings.json` — feeds real-time lifecycle edges and powers the in-pet permission bubble. Opt-in, loopback-only.
- **Vanilla TypeScript + CSS** frontend: a dumb clear-and-redraw view that maps state → sprite. No business logic.

## License

- **Code:** [MIT](LICENSE).
- **Art:** the character and hat artwork in [`characters/`](characters/) and [`src/hats/`](src/hats/) is **not** covered by the MIT license — © the burnRat authors, all rights reserved. Please don't redistribute or reuse the art without permission.
