# CLAUDE.md

Guidance for Claude Code (and other agents) working in this repo.

## What this is

burnRat is a **Tauri 2** desktop app: a transparent, always-on-top, draggable "pet" that reacts to the user's live Claude Code token burn rate. Rust core + vanilla-TypeScript/CSS frontend. See `README.md` for user-facing docs and `CLAWD_INGEST_PLAN.md` for the Claude-Code-integration roadmap (Phases 0–2 shipped, #3 next).

## Project status & next steps (handoff)

Beyond the original burn-rate pet, these have shipped (all verified, gates green):

- **Hook bridge** (`hookbridge.rs` + `hookinstall.rs`) — **opt-in, default ON**. A loopback-only HTTP listener (`127.0.0.1`, first free port in `localServer.ports`) plus burnRat's own Claude Code hooks installed into `~/.claude/settings.json`. Tray **"Connect to Claude Code"** toggles it. Lifecycle events (`SessionStart`/`Stop`/`PreToolUse`/…) reach the app via the `burnrat hook <Event>` subcommand and **sharpen the discrete states in real time** (fused with the JSONL tail in the poll loop — "more recent source wins", `hookSignalTtlSeconds` backstop). [clawd #1]
- **Permission bubble** (`permission.rs` + the dedicated `permission` window) — when Claude requests a tool permission, decide **Allow/Deny from a floating bubble** (anchored over the pet) or global **Ctrl/Cmd+Shift+Y/N** (Cmd on macOS), instead of the terminal. The blocking `burnrat permission` command hook holds the request on the bridge until you decide; on timeout/dismissal it **defers to Claude's own prompt** (never silently allows/blocks). [clawd #2]
- **`idle` + `asking` base poses** — `idle` = awake but quiet (a lull, or you composing); `asking` = the agent is asking *you* (an interactive question **or** a pending permission), distinct from `done` (a finished turn). Both **optional** poses (fall back to `thinking`/`done`).

**Next steps** (detail in `CLAWD_INGEST_PLAN.md` #3 and `NEXT.md`):
- **Art:** `asking.png` for `rat` + `skull` (falls back to `done` today). Optional distinct `nearlimit`/`refreshed`/`error` art.
- **clawd #3 "operational maturity":** ship a 2nd agent adapter via the `Source` trait sketch in `docs/other-agents.md` (Gemini CLI is lowest-friction), a multi-session HUD, auto-update, i18n, sound.
- **Animated poses:** APNG (zero-change, keeps `idle.png`) or animated WebP — see the "Movies" answer in `NEXT.md`.
- **Open question:** `thinking` now only shows in the brief post-send window (Sent→Thinking); keep, or repurpose.
- **GitHub launch TODOs** (need artifacts): GIF in README, Releases page + SmartScreen note, repo topics — see `NEXT.md` "FOR ENGINEER".

## Architecture

Rust polls the data, computes everything, and pushes a single `GameState` struct to the frontend via a Tauri event each tick. **The frontend is a dumb clear-and-redraw view** — it maps `state` → sprite and renders the readout. Keep business logic in Rust; keep tunable numbers in `data/`.

```
Rust poll loop (1s)                          Frontend (event listener)
  tail JSONL ──► rate ──► blocks ──► state ──► emit "game-state" ──► swap sprite/readout
```

### Key files

| File | Responsibility |
|---|---|
| `src-tauri/src/lib.rs` | App setup, two windows (pet + permission bubble), tray, global shortcuts, the poll loop, `GameState`, event emit; the hook-bridge connect/notifier wiring + `resolve_permission`/`current_permission` commands |
| `src-tauri/src/data.rs` | Discover + incrementally tail `~/.claude/projects/**/*.jsonl`; dedup; cache file list (re-scan every 10s); classify the latest conversational line into `Awaiting` (Done / Asking / **Sent** = user-just-messaged, vs. tool-result `user` lines) + track last-activity time; `historical_peak_block` one-shot scan that auto-calibrates the usage-limit ceiling (largest completed block in recent history). **Unit-tested.** |
| `src-tauri/src/hookbridge.rs` | **Opt-in loopback hook bridge.** Hand-rolled `std::net` HTTP/1.1 server (binds `localServer.ports`, writes `~/.burnrat/runtime.json`): `POST /state` records lifecycle edges, blocking `POST /permission` parks a request until decided. Plus the `burnrat hook <Event>` + `burnrat permission` subcommand clients, and `fuse_awaiting` (hook⊕JSONL). **Unit-tested.** |
| `src-tauri/src/hookinstall.rs` | Idempotent merge/remove of burnRat's command hooks in `~/.claude/settings.json` (canonical matcher-group form, one-time backup, removes only ours by marker, preserves foreign hooks). **Unit-tested.** |
| `src-tauri/src/permission.rs` | `PermissionRegistry` (register→id+receiver, resolve, latest, current) carrying each request's tool/detail; `Decision` (Allow/Deny/Defer). UI-agnostic. **Unit-tested.** |
| `src-tauri/src/blocks.rs` | 5-hour billing-window grouping (ccusage-equivalent); active block, consumed, projected |
| `src-tauri/src/rate.rs` | Rolling smoothed + instant tokens/min from a monotonic work-token counter; `UnitSelector` picks the readout unit (tok/sec ↔ tok/min) with hysteresis. **Unit-tested.** |
| `src-tauri/src/state.rs` | Creature state machine (hysteresis, onfire sustain, post-onfire `spent` crash). Base poses: `sleeping`/`idle`/`thinking`/`working`/`frantic`/`onfire`/`spent`/`done`/`asking`. **Unit-tested.** |
| `src-tauri/src/character.rs` | Runtime character loader: discover `characters/<id>/` folders (dev repo + bundled resources + user drop-in), validate against the fixed contract, resolve the active one to base64 data-URL assets for the `active_character` command. **Unit-tested.** |
| `src-tauri/src/config.rs` | Loads `data/*.json` (embedded defaults + live dev override) |
| `src-tauri/src/userconfig.rs` | Persists user overrides (opacity, selected character) to the OS app-config dir |
| `src/main.ts` | The pet window. Listens for `game-state`; on startup (and `character-changed`) `invoke("active_character")` to load the active character's data-URL frames; base-pose ping-pong loop (`POSE_FALLBACK` maps optional `idle`→`thinking`, `asking`→`done`); one-shot event player; near-limit overlay; eased + auto-scaling rate readout |
| `src/permission.ts` + `permission.html` | The permission-bubble window (a 2nd Vite entry). Pulls the active request via `current_permission` on focus (robust to a missed emit), resolves via `resolve_permission`; Esc defers. |
| `src/styles.css` | Per-state styling; sprites/overlay/hat are stacked images animated via JS frame swaps |
| `src-tauri/capabilities/` | Tauri v2 IPC capabilities — `default.json` (pet window) + `permission.json` (bubble window). **A window with no capability has no IPC**: that bit us once (bubble clicks silently failed). |
| `data/` | All tunable magic numbers — **no logic depends on hardcoded numbers elsewhere** |
| `characters/<id>/` | A character = a folder of ~10 PNGs + a `character.json` manifest, discovered at **runtime** by `character.rs`. Filenames are the contract (`sleeping.png`, `working.png`, `nearlimit.png`, …); extra ping-pong frames are declared per-entry in the manifest. The `rat` is the reference character. |

## Conventions

- **Tunables go in `data/`**, never hardcoded in logic. `config.rs` embeds the defaults via `include_str!` *and* re-reads the live files from the repo `data/` dir in dev (resolved via `CARGO_MANIFEST_DIR`), so thresholds can be tuned without a rebuild.
- **The burn signal mixes work + cache**, weighted by `rateCacheWeight` (default `1.0` = full cache; `0.0` = work-only). Cache tokens run ~70× larger than work, so the state thresholds in `data/thresholds.json` are calibrated for the cache-inclusive scale — **change the weight and you must retune the thresholds together** (the `$comment` there gives the work-only divisor). `consumed` (work) and `consumedWithCache` are still reported separately for the readout/limit math.
- **Characters are runtime-loaded folders**, not a build-time glob. `character.rs` discovers `characters/<id>/`, validates the contract (7 **required** base states `sleeping/thinking/working/frantic/onfire/spent/done` + the `quotaProximity` modifier + `refreshed`/`error` events), and resolves the active one to data-URL assets; the frontend maps `base_state`/`event` → asset by name. Two **optional** base poses are recognized: `idle` (falls back to `thinking`) and `asking` (falls back to `done`) — supply `idle.png`/`asking.png` to use them, else the view falls back via `POSE_FALLBACK` in `main.ts`. Add/replace art by dropping a PNG in over the contract filename — **zero code changes**. Per-model **hats** are still a build-time glob from `src/hats/` (filename = model family), shared across characters.
- **The hook bridge is opt-in (default ON) and loopback-only.** When connected it installs burnRat's hooks into `~/.claude/settings.json` and binds a `127.0.0.1` port — never a non-loopback socket; when off it opens nothing. Keep it that way (the project's positioning is "trivially auditable, tiny trust surface"). The bridge is a deliberately hand-rolled `std::net` server (no async stack) so it stays auditable and owning the raw socket makes the held-open `/permission` connection trivial. **All hooks route through subcommands of this same binary** (`burnrat hook <Event>`, `burnrat permission`) dispatched in `main.rs` *before* Tauri starts — they talk to the running app over the bridge and exit; they must never spin up a window or block Claude (the `hook` client always exits 0; the `permission` client defers to Claude's prompt if the app's down).
- **The permission bubble is a control surface — treat changes with care.** It's burnRat's only feature that can *act* (approve/deny tool calls), so: loopback-only, opt-in, and **default-to-defer** on any timeout/dismissal/error (Claude falls back to its own prompt — never silently allow/deny). It's a 2nd Tauri window; remember **every window needs a capability** (`capabilities/permission.json`) or its `invoke`/event-`listen` silently fail.
- **Lifecycle hooks sharpen, they don't replace.** The JSONL tail (`data.rs`) stays the source of truth for rate/blocks/quota/model and is the fallback when the bridge is off. Hook edges only *refine* the discrete `Awaiting`/activity state in the poll loop (`hookbridge::fuse_awaiting`, more-recent-source-wins). Don't move rate/block logic onto hooks.
- **Frames auto-discover from the folder** (`character.rs` `frame_files`): an entry's `asset` is the base pose, and any `<stem>_1.png`, `<stem>_2.png`, … next to it join that pose's ping-pong loop in index order — drop a frame file in or remove one, no manifest edit. An explicit `frames: [...]` on an entry overrides discovery (and then every listed file must exist). In **dev**, `spawn_character_watcher` watches the characters dirs and re-emits `character-changed` on any change, so art edits hot-reload live (no-op in release; the running app otherwise caches the resolved data-URLs until restart or a tray character-switch).
- **Sprite sizing:** the rat renders at **150×150 CSS px**, so source PNGs should be **300×300** (2× for HiDPI) and optimized to **well under ~100 KB** each. Run **`npm run optimize-art`** (`scripts/optimize-art.mjs`, uses `sharp`) to resize/compress in place — lossless first, palette-quantize only if over budget, preserves alpha.
- The `rat`'s `nearlimit.png`/`refreshed.png`/`error.png` are still **placeholders** reusing other poses (see `characters/rat/ART_NEEDS.md`) — `nearlimit` especially must become an *overlay* accent, not a full second rat. They render today (engine is wired); only the art is pending.
- Frontend has no business logic. If you're tempted to add a threshold or rule in TS, it belongs in Rust + `data/`. (The exception is pure *animation/presentation* timing — e.g. `FRAME_MS`, `RATE_EASE_ALPHA` — which lives in `main.ts` as view constants. The readout's *unit* choice is a rule, so it's decided in Rust; the *glide* is presentation, so it's eased in TS.)

## Build / test / run

```bash
npm install
npm run tauri dev        # run the app (vite + cargo)
npm run tauri build      # release binary
npm run optimize-art     # resize/compress character PNGs in place

cd src-tauri && cargo test   # state-machine, rate-unit, message-classification, hook-bridge, permission, settings-merge
```

- The frontend is **two Vite entries** (`index.html` → the pet, `permission.html` → the bubble); see `vite.config.ts` `rollupOptions.input` and the two windows in `tauri.conf.json`.
- The binary doubles as its own hook client: `burnrat hook <Event>` and `burnrat permission` are dispatched in `main.rs` before Tauri (used by the installed Claude Code hooks; harmless to run manually — they just POST to the bridge or exit).
- Gates to keep green before committing: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `npm run build`.
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
