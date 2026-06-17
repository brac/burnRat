# burnRat 🐀

A transparent desktop pet that reacts to your live **Claude Code token burn rate**. The harder you hammer Claude, the more the rat panics — calm → working → stressed → on fire — and when you burn out hot then stop, it slumps spent. Lightweight, always-on-top, draggable, cross-platform (macOS + Windows).

burnRat is an *ambient companion*, not a dashboard. It floats over whatever you're working in and translates your token consumption into a creature state. The reactive pet is the point; the numbers are secondary. Unlike the usual desktop pets that wander around at random, burnRat is wired to real telemetry — it only reacts to what you're actually doing to Claude.

---

## How it works

Claude Code writes one JSONL file per session to `~/.claude/projects/<project>/<conversation-id>.jsonl`, and every assistant turn records exact token usage. burnRat tails those files directly in Rust (no external dependency), computes a smoothed burn rate (tokens per minute), and maps it to a creature state. By default the signal includes cache tokens (`rateCacheWeight`, see [Tuning](#tuning)) for big, lively numbers; set the weight to `0` to watch only work (input + output) tokens.

| State | When |
|---|---|
| `sleeping` | No new tokens for a while (idle nap) |
| `calm` | Active window, low/no burn |
| `working` | Moderate burn (animated loop) |
| `stressed` | High burn |
| `onfire` | Sustained very high burn |
| `spent` | The crash *after* burning onfire — rate collapses and the rat slumps |
| `waiting` | Claude is asking you something (`AskUserQuestion` / plan approval) |
| `done` | Claude finished a turn — task complete, awaiting your next instruction |
| `error` | Claude Code hit an API error — concerned, holds like `waiting` |
| `refreshed` | Your 5-hour quota window just rolled over (fresh quota) — holds, then naps |
| `approaching10`/`5`/`1` | Within 10% / 5% / 1% of your (auto-calibrated) usage limit — escalating glow |
| `atlimit` | At your usage limit — shows a countdown to the window refresh |
| `longrun` | A long-running session (shown over idle) |

A brief **surprised** pop plays when the rat perks up from rest into work. Thresholds use hysteresis so it doesn't strobe on a noisy signal.

**Napping is smart about your messages.** The nap clock runs from the last conversational line (yours *or* Claude's), so sending a message resets it — no jarring `done → message → nap`. Right after you send a message the rat holds the idle pose longer (`sentHoldSeconds`, default 3 min) so it doesn't nap through the dead air before Claude starts responding, then naps if nothing happens. The rat also won't nap while the burn rate is still elevated — it always winds down through its lower states (e.g. `onfire → stressed → working → calm → sleeping`) rather than snapping straight from a high state into a nap.

### Sprites

Frames live in [`src/sprites/`](src/sprites/) and are auto-discovered by filename: drop in `<state>.png` plus optional `<state>_1.png`, `<state>_2.png`, … and they're grouped into that state's loop automatically (1 frame = static, 2 = alternate, 3+ = smooth ping-pong). No code changes needed — new files are picked up on the next dev reload / build.

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
- **Tray menu:** toggle pass-through, set **Opacity**, and **Quit**.

---

## Tuning

All the magic numbers live in [`data/`](data/) and are read live in `dev` (no rebuild needed):

- **`data/thresholds.json`** — burn-rate cutoffs per state (with up/down hysteresis), the onfire sustain time, the post-onfire `spent` crash, and the nap/hold timers: `idleTimeoutSeconds` (idle grace before the rat sleeps), `doneHoldSeconds` (how long the `done` pose holds after a finished turn), and `sentHoldSeconds` (how long the rat holds the idle pose after you send a message — longer, so it doesn't nap through the "dead air" before Claude responds).
- **`data/settings.default.json`** — poll interval, rate smoothing window, 5-hour block window, default opacity, whether it starts interactive or pass-through, `display` (the tok/sec↔tok/min auto-scale cutoffs for the readout), `rateCacheWeight` (how much cache counts toward the burn signal — `1.0` = full cache/bigger numbers, `0.0` = work only; **retune `thresholds.json` if you change it**), and the usage-limit settings (`limitHistoryDays` / `limitMinCredibleTokens` for the auto-calibrated approaching-limit ceiling, plus `planLimits` as an optional manual override).

The **approaching-limit warnings** (10%/5%/1% glows) calibrate the ceiling automatically: on startup burnRat scans your recent history for the largest *completed* 5-hour block (tokens incl. cache) and uses that as the limit estimate, rather than guessing a cap. It reads conservatively until you have history, and since your past peak is a lower bound on your true limit it can warn a little early — set a `planLimits` entry if you'd rather pin an exact cap.

The rate readout under the rat **auto-scales** between tokens/sec and tokens/min (with hysteresis so the unit doesn't flip-flop) and is **display-eased** on an animation-frame loop so the number glides smoothly over Claude's chunky, per-turn token writes — the underlying data smoothing is still `rateWindowSeconds`.

User-changed settings (opacity) persist to your OS app-config dir; defaults are bundled into the binary.

---

## Tech

- **[Tauri 2](https://tauri.app)** — Rust core + web frontend, tiny binary, first-class transparent/always-on-top/click-through windows.
- **Rust** core: tails the JSONL incrementally (per-file byte cursors, dedup), 5-hour billing-window grouping equivalent to [ccusage](https://github.com/ryoppippi/ccusage), rolling burn-rate, and the creature state machine.
- **Vanilla TypeScript + CSS** frontend: a dumb clear-and-redraw view that maps state → sprite. No business logic.

## License

- **Code:** [MIT](LICENSE).
- **Art:** the sprite and hat artwork in [`src/sprites/`](src/sprites/) and [`src/hats/`](src/hats/) is **not** covered by the MIT license — © the burnRat authors, all rights reserved. Please don't redistribute or reuse the art without permission.
