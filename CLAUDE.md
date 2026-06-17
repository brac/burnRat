# CLAUDE.md

Guidance for Claude Code (and other agents) working in this repo.

## What this is

burnRat is a **Tauri 2** desktop app: a transparent, always-on-top, draggable "pet" that reacts to the user's live Claude Code token burn rate. Rust core + vanilla-TypeScript/CSS frontend. See `DESIGN.md` for the original design doc and `README.md` for user-facing docs.

## Architecture

Rust polls the data, computes everything, and pushes a single `GameState` struct to the frontend via a Tauri event each tick. **The frontend is a dumb clear-and-redraw view** — it maps `state` → sprite and renders the readout. Keep business logic in Rust; keep tunable numbers in `data/`.

```
Rust poll loop (1s)                          Frontend (event listener)
  tail JSONL ──► rate ──► blocks ──► state ──► emit "game-state" ──► swap sprite/readout
```

### Key files

| File | Responsibility |
|---|---|
| `src-tauri/src/lib.rs` | App setup, window/tray/global-shortcut, the poll loop, `GameState`, event emit |
| `src-tauri/src/data.rs` | Discover + incrementally tail `~/.claude/projects/**/*.jsonl`; dedup; cache file list (re-scan every 10s); classify the latest conversational line into `Awaiting` (Done / Asking / **Sent** = user-just-messaged, vs. tool-result `user` lines) + track last-activity time. **Unit-tested.** |
| `src-tauri/src/blocks.rs` | 5-hour billing-window grouping (ccusage-equivalent); active block, consumed, projected |
| `src-tauri/src/rate.rs` | Rolling smoothed + instant tokens/min from a monotonic work-token counter; `UnitSelector` picks the readout unit (tok/sec ↔ tok/min) with hysteresis. **Unit-tested.** |
| `src-tauri/src/state.rs` | Creature state machine (hysteresis, onfire sustain, post-onfire `spent` crash). **Unit-tested.** |
| `src-tauri/src/config.rs` | Loads `data/*.json` (embedded defaults + live dev override) |
| `src-tauri/src/userconfig.rs` | Persists user overrides (opacity) to the OS app-config dir |
| `src/main.ts` | Listens for `game-state`; sprite/animation; working frame loop; surprised pop; eased + auto-scaling rate readout (`requestAnimationFrame` glide over the chunky per-turn signal) |
| `src/styles.css` | Per-state styling; sprites are images animated via JS frame swaps |
| `data/` | All tunable magic numbers — **no logic depends on hardcoded numbers elsewhere** |
| `src/sprites/` | Per-state PNGs, auto-discovered by filename: `<state>.png` + optional `<state>_1.png`, `<state>_2.png`, … grouped into that state's loop via `import.meta.glob` in `main.ts` |

## Conventions

- **Tunables go in `data/`**, never hardcoded in logic. `config.rs` embeds the defaults via `include_str!` *and* re-reads the live files from the repo `data/` dir in dev (resolved via `CARGO_MANIFEST_DIR`), so thresholds can be tuned without a rebuild.
- **The burn signal is `input + output` tokens only** — cache read/creation tokens are excluded (they're ~100× larger and would peg the rat permanently hot).
- Art is auto-discovered from `src/sprites/` by filename convention (`main.ts`); add frames by dropping files in, no code changes. `STATE_BASE` in `main.ts` maps a state to a differently-named base (e.g. `calm` → `idle`).
- Frontend has no business logic. If you're tempted to add a threshold or rule in TS, it belongs in Rust + `data/`. (The exception is pure *animation/presentation* timing — e.g. `FRAME_MS`, `RATE_EASE_ALPHA` — which lives in `main.ts` as view constants. The readout's *unit* choice is a rule, so it's decided in Rust; the *glide* is presentation, so it's eased in TS.)

## Build / test / run

```bash
npm install
npm run tauri dev        # run the app (vite + cargo)
npm run tauri build      # release binary

cd src-tauri && cargo test   # state-machine, rate-unit, and message-classification unit tests
```

- **Rust toolchain (`rustup`) + platform C/C++ build tools are required** (MSVC on Windows, Xcode CLT on macOS).
- **PATH gotcha:** a terminal opened *before* Rust was installed won't have `cargo` on PATH. Open a new shell, or prepend `~/.cargo/bin` (`$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"` on Windows).

## Cross-platform notes

- Transparent windows on macOS require `"macOSPrivateApi": true` in `tauri.conf.json` (already set).
- Resolve the home dir via `dirs::home_dir()` — never hardcode `~` or path separators.
- `set_ignore_cursor_events` (pass-through) + always-on-top + transparent is the finickiest combo on Windows — test there.

## Verifying data correctness

The vendored parser is meant to match ccusage exactly. To sanity-check, compare the active-window `input+output` and `totalTokens` against:

```bash
npx ccusage@latest blocks --json
```

(ccusage's `tokensPerMinute` includes cache and will be much larger than burnRat's work-only rate — that's expected.)
