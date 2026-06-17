# burnRat 🐀

A transparent desktop pet that reacts to your live **Claude Code token burn rate**. The harder you hammer Claude, the more the rat panics — calm → working → stressed → on fire — and when you burn out hot then stop, it slumps spent. Lightweight, always-on-top, draggable, cross-platform (macOS + Windows).

burnRat is an *ambient companion*, not a dashboard. It floats over whatever you're working in and translates your token consumption into a creature state. The reactive pet is the point; the numbers are secondary.

---

## How it works

Claude Code writes one JSONL file per session to `~/.claude/projects/<project>/<conversation-id>.jsonl`, and every assistant turn records exact token usage. burnRat tails those files directly in Rust (no external dependency), computes a smoothed **work** burn rate (input + output tokens per minute — cache reads are excluded since they dwarf real work), and maps it to a creature state.

| State | When |
|---|---|
| `sleeping` | No new tokens for a while (idle nap) |
| `calm` | Active window, low/no burn |
| `working` | Moderate burn (animated 3-frame loop) |
| `stressed` | High burn |
| `onfire` | Sustained very high burn |
| `spent` | The crash *after* burning onfire — rate collapses and the rat slumps |

A brief **surprised** pop plays when the rat perks up from rest into work. Thresholds use hysteresis so it doesn't strobe on a noisy signal.

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

- **`data/thresholds.json`** — burn-rate cutoffs per state (with up/down hysteresis), the onfire sustain time, the post-onfire `spent` crash, and `idleTimeoutSeconds` (how long with no tokens before the rat sleeps).
- **`data/settings.default.json`** — poll interval, rate smoothing window, 5-hour block window, default opacity, and whether it starts interactive or pass-through.

User-changed settings (opacity) persist to your OS app-config dir; defaults are bundled into the binary.

---

## Tech

- **[Tauri 2](https://tauri.app)** — Rust core + web frontend, tiny binary, first-class transparent/always-on-top/click-through windows.
- **Rust** core: tails the JSONL incrementally (per-file byte cursors, dedup), 5-hour billing-window grouping equivalent to [ccusage](https://github.com/ryoppippi/ccusage), rolling burn-rate, and the creature state machine.
- **Vanilla TypeScript + CSS** frontend: a dumb clear-and-redraw view that maps state → sprite. No business logic.

## License

[MIT](LICENSE)
