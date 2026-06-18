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
| `src-tauri/src/data.rs` | Discover + incrementally tail `~/.claude/projects/**/*.jsonl`; dedup; cache file list (re-scan every 10s); classify the latest conversational line into `Awaiting` (Done / Asking / **Sent** = user-just-messaged, vs. tool-result `user` lines) + track last-activity time; `historical_peak_block` one-shot scan that auto-calibrates the usage-limit ceiling (largest completed block in recent history). **Unit-tested.** |
| `src-tauri/src/blocks.rs` | 5-hour billing-window grouping (ccusage-equivalent); active block, consumed, projected |
| `src-tauri/src/rate.rs` | Rolling smoothed + instant tokens/min from a monotonic work-token counter; `UnitSelector` picks the readout unit (tok/sec ↔ tok/min) with hysteresis. **Unit-tested.** |
| `src-tauri/src/state.rs` | Creature state machine (hysteresis, onfire sustain, post-onfire `spent` crash). **Unit-tested.** |
| `src-tauri/src/character.rs` | Runtime character loader: discover `characters/<id>/` folders (dev repo + bundled resources + user drop-in), validate against the fixed contract, resolve the active one to base64 data-URL assets for the `active_character` command. **Unit-tested.** |
| `src-tauri/src/config.rs` | Loads `data/*.json` (embedded defaults + live dev override) |
| `src-tauri/src/userconfig.rs` | Persists user overrides (opacity, selected character) to the OS app-config dir |
| `src/main.ts` | Listens for `game-state`; on startup (and `character-changed`) `invoke("active_character")` to load the active character's data-URL frames; base-pose ping-pong loop; one-shot event player; near-limit overlay; eased + auto-scaling rate readout (`requestAnimationFrame` glide over the chunky per-turn signal) |
| `src/styles.css` | Per-state styling; sprites/overlay/hat are stacked images animated via JS frame swaps |
| `data/` | All tunable magic numbers — **no logic depends on hardcoded numbers elsewhere** |
| `characters/<id>/` | A character = a folder of ~10 PNGs + a `character.json` manifest, discovered at **runtime** by `character.rs`. Filenames are the contract (`sleeping.png`, `working.png`, `nearlimit.png`, …); extra ping-pong frames are declared per-entry in the manifest. The `rat` is the reference character. |

## Conventions

- **Tunables go in `data/`**, never hardcoded in logic. `config.rs` embeds the defaults via `include_str!` *and* re-reads the live files from the repo `data/` dir in dev (resolved via `CARGO_MANIFEST_DIR`), so thresholds can be tuned without a rebuild.
- **The burn signal mixes work + cache**, weighted by `rateCacheWeight` (default `1.0` = full cache; `0.0` = work-only). Cache tokens run ~70× larger than work, so the state thresholds in `data/thresholds.json` are calibrated for the cache-inclusive scale — **change the weight and you must retune the thresholds together** (the `$comment` there gives the work-only divisor). `consumed` (work) and `consumedWithCache` are still reported separately for the readout/limit math.
- **Characters are runtime-loaded folders**, not a build-time glob. `character.rs` discovers `characters/<id>/`, validates the contract (7 base states `sleeping/thinking/working/frantic/onfire/spent/done` + the `quotaProximity` modifier + `refreshed`/`error` events), and resolves the active one to data-URL assets; the frontend maps `base_state`/`event` → asset by name. Add/replace art by dropping a PNG in over the contract filename — **zero code changes**. Per-model **hats** are still a build-time glob from `src/hats/` (filename = model family), shared across characters.
- **Frames auto-discover from the folder** (`character.rs` `frame_files`): an entry's `asset` is the base pose, and any `<stem>_1.png`, `<stem>_2.png`, … next to it join that pose's ping-pong loop in index order — drop a frame file in or remove one, no manifest edit. An explicit `frames: [...]` on an entry overrides discovery (and then every listed file must exist). In **dev**, `spawn_character_watcher` watches the characters dirs and re-emits `character-changed` on any change, so art edits hot-reload live (no-op in release; the running app otherwise caches the resolved data-URLs until restart or a tray character-switch).
- **Sprite sizing:** the rat renders at **150×150 CSS px**, so source PNGs should be **300×300** (2× for HiDPI) and optimized to **well under ~100 KB** each. Run **`npm run optimize-art`** (`scripts/optimize-art.mjs`, uses `sharp`) to resize/compress in place — lossless first, palette-quantize only if over budget, preserves alpha.
- The `rat`'s `nearlimit.png`/`refreshed.png`/`error.png` are still **placeholders** reusing other poses (see `characters/rat/ART_NEEDS.md`) — `nearlimit` especially must become an *overlay* accent, not a full second rat. They render today (engine is wired); only the art is pending.
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
