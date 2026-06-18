# burnRat — Character System & Three-Layer State Refactor

## Context

burnRat today is a **single hardcoded character** (the rat) with a **flat 15-variant state enum** (`CreatureState` in `src-tauri/src/state.rs`) that collapses three unrelated concepts into peer states: burn intensity (`Calm/Working/Stressed/OnFire/Spent`), quota proximity (`Approaching10/5/1/AtLimit`), and one-off moments (`Refreshed/Error`). The poll loop in `lib.rs` (lines 319–357) flattens all of this into a single `state: &'static str` via a priority cascade, and the frontend (`src/main.ts`) maps that one string to a sprite discovered at **build time** via `import.meta.glob("./sprites/*.png")`.

This blocks two goals:
1. **Swappable characters** — a character should be a *folder* of art + a manifest, discovered at runtime, with no code path that is character-specific.
2. **Cheap character creation** — every character costs the same fixed ~10-asset set, no matter how many situations the engine represents.

The fix is to (a) split state into **three explicit layers** (base / modifier / event) carried explicitly in `GameState`, and (b) make characters **runtime-loaded folders** behind a `character.json` manifest. The engine resolves layers; the active character supplies the matching asset. The rat becomes the reference character; a second character (a **flaming skull**) proves the contract.

### Locked decisions (from clarifying questions)
- **Pose mapping:** the old `Calm` (active, low burn) → **`thinking`**; the old `Waiting` (Claude asking an interactive question) → **`done`**. The fixed base contract is exactly: `sleeping, thinking, working, frantic, onfire, spent, done`.
- **Character loading:** **runtime filesystem** — scan a bundled resource dir *and* an external user dir at startup; end users drop a folder in and restart, no rebuild.
- **`longrun`:** **dropped** (no dedicated asset, overlaps `spent`, default-off). Removed from the resolution cascade and config.
- **Art:** **wire now with placeholders** — build the full 10-file rat folder reusing existing PNGs for the new assets; ship a precise art-needs list; real art drops in later with zero code changes.

---

## Target architecture

```
Rust poll loop (1s)                                  Frontend (dumb view)
  tail JSONL → rate → blocks                          on init: invoke("active_character")
    │                                                   → ResolvedCharacter (data-URL assets)
    ├─ Layer 1 base_state  (hysteresis) ─┐
    ├─ Layer 2 near_limit_opacity+quota% ─┼─ emit "game-state" → map base_state→asset,
    └─ Layer 3 event (refreshed/error/    ┘                       overlay nearlimit @ opacity,
              flinch, debounced)                                  play event one-shot, ease readout
  tray "Character" submenu → persist → emit "character-changed" → re-invoke active_character
```

Business logic stays in Rust + `data/`. The frontend only eases the readout, ping-pongs frames, plays one-shot events, and looks assets up in the active manifest.

---

## Part A — Three-layer state model (Rust)

### A1. New base-state enum — `src-tauri/src/state.rs`

Replace the 15-variant `CreatureState` with a 7-variant **`BaseState`** (Layer 1 only):

```rust
pub enum BaseState { Sleeping, Thinking, Working, Frantic, OnFire, Spent, Done }
// as_str: "sleeping" "thinking" "working" "frantic" "onfire" "spent" "done"
```

Delete `Calm`, `Waiting`, `Stressed` (renamed), `Approaching10/5/1`, `AtLimit`, `Refreshed`, `Error`, `LongRun`, and the whole `apply_approaching()` function (lines 56–82) — those concepts move to Layers 2 and 3.

### A2. `StateMachine::update` — rate tier → base pose

Keep the existing hysteresis ladder (`advance_level`), onfire-sustain (`resolve_onfire`), and post-onfire crash (`apply_spent`) **unchanged in mechanism**; only remap the produced poses and add `sent`:

- signature adds a `sent: bool` arg (new) — `update(is_active, done, asking, sent, recent_activity, smoothed_tpm, instant_tpm, now)`.
- `!is_active` → `Sleeping`.
- `done || asking` → **`Done`** (both map to Done now; was `Done`/`Waiting`).
- `sent` (and not done/asking) → **`Thinking`** (NEW — today `Sent` only extends the nap hold; now it gets the latency-gap pose).
- rate tiers: `CALM → Thinking` (was Calm), `WORKING → Working`, `STRESSED → Frantic` (rename), `ONFIRE → OnFire` (sustained) else `Frantic`.
- `apply_spent`: the `collapsed` check `base == Calm` becomes `base == Thinking`; everything else unchanged.

`detect_spike` still returns the `"flinch"` event; `update` returns `(BaseState, Option<&'static str> /* flinch */)`.

### A3. Layer 2 — quota proximity (collapse 4 sprites → 1 + a number)

Computed in the **poll loop** (`lib.rs`), reusing the existing ceiling math (lines 280–317). Replace the 0–4 `warn_level` band with:

- `quota_percent: f64` = `consumed_with_cache / ceiling` (0.0 when `ceiling == 0`, i.e. no credible learned cap yet).
- `near_limit_opacity: f64` = ramp of `quota_percent` between `startPercent` and `fullPercent` from `data/` (0 below start, 1 at/after full). **Computed in Rust** because "when to start showing concern" is a rule (project convention: thresholds live in Rust + `data/`, not TS).
- at-limit is simply `quota_percent >= 1.0` — the frontend swaps the readout to the refresh countdown there; no separate state.

### A4. Layer 3 — transient events — new `src-tauri/src/events.rs`

`refreshed` and `error` become **one-shot debounced events** (today they are *held* poses — `refreshed` holds 5 min, `error` holds like a question). An `EventResolver` centralizes Layer 3:

- inputs each tick: `refreshed_edge` (rising edge from `RefreshTracker`), `error_now`, `flinch` (from the state machine).
- applies **priority** (`error > refreshed > flinch`) and **debounce/cooldown** per event from `data/` so a retryable API hiccup doesn't spam `error`.
- returns `Option<&'static str>` = the single event to emit this tick (or `None`).

`RefreshTracker` (state.rs lines 89–143) is adapted to return a **rising edge** (fires once when the watched window rolls over) instead of a sustained hold bool. Its window-observation logic is preserved; only the return contract changes.

Because `refreshed`/`error` no longer hold the rat awake, **remove them from the `awake` gate** in `lib.rs` (the `awaiting_user` term keeps only `done || asking`; `sent` still drives the longer `idle_hold`). Events play over whatever the base state is (including `sleeping`) and hand control back.

### A5. New `GameState` shape — `src-tauri/src/lib.rs`

Replace the single `state` field with the three explicit layers; keep all readout/data fields:

```rust
struct GameState {
    // data / readout (unchanged)
    smoothed_tpm, instant_tpm, consumed, consumed_with_cache, projected,
    time_remaining_min, is_active, opacity, rate_unit, model,
    // three layers (NEW shape)
    base_state: &'static str,        // Layer 1
    near_limit_opacity: f64,         // Layer 2 — overlay opacity 0..1 (presentation-ready)
    quota_percent: f64,              // Layer 2 — for the numeric readout (0 if no ceiling)
    event: Option<&'static str>,     // Layer 3 — "refreshed"|"error"|"flinch", transient
    character: &'static str,         // active character id (lets the view guard swaps)
}
```

The priority cascade at lib.rs:319–357 collapses to: `let (base, flinch) = machine.update(...)`, compute `quota_percent`/`near_limit_opacity`, `let event = event_resolver.resolve(refreshed_edge, error_now, flinch, now)`, emit. No more `apply_approaching`, no `longrun`, no held `Refreshed`/`Error`.

### A6. Tests
- Update `state.rs` tests: `Calm→Thinking`, `Stressed→Frantic`, `asking→Done`, add `sent→Thinking`, keep onfire-sustain + spent-crash assertions (rename expectations). Delete the `apply_approaching`/`approaching*` test block (lines ~441–492) — that logic is gone.
- New `events.rs` tests: priority ordering, error debounce suppresses rapid re-fire, refreshed edge fires once.
- Keep `RefreshTracker` tests, adapted to the edge contract.

---

## Part B — Character system (Rust) — new `src-tauri/src/character.rs`

### B1. Folder + manifest contract

```
characters/<id>/
  character.json
  sleeping.png thinking.png working.png frantic.png onfire.png spent.png done.png
  nearlimit.png refreshed.png error.png        # ~10 required, fixed cost
```

`character.json` (mirrors the spec): `id`, `name`, `renderer` (default `"sprite"`), `canvas {width,height}`, `anchor {x,y}`, and `states`/`modifiers`/`events` maps of name → `{ asset, anchor?, canvas?, frames? }`. Optional `frames: [...]` per entry declares extra ping-pong frames (preserves the rat's current 2–3-frame `sleeping`/`working`/`frantic` loops) without inflating the *required* set.

### B2. Loader + validation

```rust
struct CharacterManifest { id, name, renderer, canvas, anchor, states, modifiers, events }
struct AssetEntry { asset: String, anchor: Option<Anchor>, canvas: Option<CanvasBox>, frames: Option<Vec<String>> }
```

- **Discover** subfolders of every characters dir at startup (see B3).
- **Validate** against the contract: all 7 base states present, `quotaProximity` modifier present, both `refreshed`+`error` events present, every referenced asset exists on disk, `renderer == "sprite"` (warn + skip on `"mesh"`). On any failure: `eprintln!` (matches the codebase's existing logging) and **exclude that character** from the valid set — never silently render blank.
- Hold the valid set as `Vec<(id, CharacterManifest, base_dir)>`.

### B3. Where characters live (runtime dirs, scanned in order)
- **Dev:** repo `characters/` via `CARGO_MANIFEST_DIR/../characters` — mirrors the existing `config::dev_data_dir()` pattern so dev edits need no rebuild.
- **Bundled defaults (prod):** ship via `tauri.conf.json` → `bundle.resources` and read `app.path().resource_dir()/characters`.
- **User drop-in:** `app.path().app_data_dir()/characters` — adding a folder here + restart makes it appear (the true drop-in path). Later dirs override earlier by `id`.

### B4. Assets to the frontend — **base64 data URLs**

The `active_character` Tauri command resolves the selected character to a frontend-ready struct, reading each PNG and encoding it as a `data:image/png;base64,…` URL:

```rust
#[derive(Serialize)]
struct ResolvedCharacter { id, name, renderer, canvas, anchor, assets: HashMap<String, ResolvedAsset> }
struct ResolvedAsset { urls: Vec<String>, anchor: Anchor, canvas: CanvasBox }  // urls = frames
// assets keyed by: base-state names + "quotaProximity" + event names
```

**Why data URLs, not the asset protocol:** ~10 PNGs <100 KB each → encoding cost is negligible (once at startup, again only on character switch), and it sidesteps Tauri asset-protocol scoping + CSP entirely (`tauri.conf.json` already has `"csp": null`). If art ever grows large, switch this one function to `convertFileSrc` + `assetProtocol.scope` without touching the frontend contract. (Trade-off noted: data URLs re-encode on swap; fine for this size.)

### B5. Commands + hot-swap state
- `#[tauri::command] active_character(state) -> ResolvedCharacter` — reads the selected id from `Shared` and resolves it.
- `Shared` gains the loaded character list + selected id (behind the existing `Mutex`/atomics). Register the command via `.invoke_handler(tauri::generate_handler![active_character])`.

---

## Part C — Frontend refactor (`src/main.ts`, `index.html`, `src/styles.css`)

- **Drop** `import.meta.glob("./sprites/*.png")`, the `FRAMES` build-time map, and `STATE_BASE`. On `DOMContentLoaded`, `invoke("active_character")` (from `@tauri-apps/api/core`) → build a `frames: Record<string,string[]>` from `resolved.assets[name].urls`. Re-invoke on a new `"character-changed"` event.
- **GameState interface** updated to the new shape (base_state, near_limit_opacity, quota_percent, event, character).
- **DOM** (`index.html`): add a stacked overlay image inside `.stack`: `#sprite` (base), `#overlay` (nearlimit), `#hat` (model). Overlay `opacity = near_limit_opacity`, source = `assets["quotaProximity"]`.
- **Render loop:** ping-pong the base-state frames (existing math at main.ts:149–159, unchanged) over `frames[base_state]`. Generalize the current "surprised pop" latch (main.ts:164–172, `surprisedUntil`) into a one-shot **event player**: on `event != null`, set `activeEvent`/`eventUntil = now + EVENT_MS`; while playing, render `frames[activeEvent]`; then return to base. `EVENT_MS` is a view constant (presentation); the *debounce/cooldown* that decides whether to fire lives in Rust.
- **Readout (`easeReadout`):** if `quota_percent >= 1.0` → refresh countdown (existing `atlimit` branch, re-keyed); else if `near_limit_opacity > 0` → show `${Math.round(quota_percent*100)}%`; else the eased rate. Easing/glide unchanged.
- **Positioning:** size the `.stack` box from the manifest `canvas` and align via `anchor` (CSS transform), so swapping a 256×256 rat for a differently-shaped skull doesn't jump. Keep the 150×150 display footprint.
- **`styles.css`:** remove `.state-approaching10/5/1` and `.state-atlimit` glows (replaced by the opacity overlay). Keep a couple of character-agnostic accents (`onfire` glow, at-limit pulse) as pure presentation. Hats stay global/build-time (not per-character).
- **Renderer pluggability:** route base/overlay/event drawing through a tiny `renderer` object selected on `resolved.renderer` (only `"sprite"` implemented). A future `"mesh"` renderer is an added branch, not a rewrite — **do not implement mesh now**.

---

## Part D — Config, tray, data

### `data/` changes
- `data/thresholds.json`: replace the `approaching {warn10,warn5,warn1}` block with `quota { startPercent: 0.90, fullPercent: 0.99 }`; **remove** `longRunningSeconds` and `refreshedHoldSeconds`; add `events { priority: ["error","refreshed","flinch"], errorDebounceSeconds, refreshedCooldownSeconds }`.
- `data/settings.default.json`: add `"character": "rat"` (selected character id). Quota cap presets (`planLimits`, `limitHistoryDays`, `limitMinCredibleTokens`) stay as-is.

### `src-tauri/src/config.rs`
- Replace `ApproachingCfg` with `QuotaCfg { start_percent, full_percent }`; remove `long_running_seconds`/`refreshed_hold_seconds`; add `EventsCfg` and `character` to `Settings`. Update the `thresholds()` test fixture in `state.rs`/`events.rs` accordingly.

### `src-tauri/src/userconfig.rs`
- `UserConfig` gains `character: String` (default from settings). `load`/`save` unchanged in shape.

### Tray — `src-tauri/src/lib.rs` `build_tray`
- Build the character loader in `setup()` **before** `build_tray`, pass the valid id/name list in.
- Add a **"Character" submenu** mirroring the Opacity submenu (lines 377–393): one `CheckMenuItem` per character, checked = active. Handler (mirror the `opacity:` arm at 408–416): on `character:<id>`, set `shared.user.character`, `shared.persist()`, update the selected id in `Shared`, and `app.emit("character-changed", id)` so the frontend re-fetches. No window rebuild needed.

---

## Build order (PR-sized stages, each independently verifiable)

**Stage 0 — Lock the rat folder.** Create `characters/rat/character.json` + 10 PNGs by reusing existing art: `idle.png→thinking.png`, `stressed*.png→frantic*.png`, copy placeholders for `nearlimit/refreshed/error`, carry existing `sleeping/working/spent/done` (with their `_1`/`_2` frames declared in the manifest). Output a precise **art-needs list** (the 4 genuinely-new poses at 300×300, <100 KB). `src/sprites/` stays until Stage 2 cutover.

**Stage 1 — Three-layer state model (no character system yet).** Parts A + D. Rewrite `state.rs`, add `events.rs`, reshape `GameState` and the poll loop, update `config.rs`/`data`. Frontend updated to the new `GameState` shape but **still** using the build-time sprites glob (keyed by base-state names) so this stage runs and is verifiable on its own. Update unit tests.

**Stage 2 — Character loader + frontend cutover.** Parts B + C + tray. Add `character.rs`, `bundle.resources`, the `active_character` command + `character-changed` event, the tray submenu. Frontend switches from the glob to `invoke("active_character")`, adds the overlay image, plays events one-shot, uses anchor/canvas. Remove `src/sprites/` once the rat renders from its folder. Add loader-validation tests.

**Stage 3 — Prove the contract with the flaming skull.** Build `characters/skull/` as a full ~10-asset folder (placeholder art fine) + manifest. Confirm it discovers, validates, tray-swaps live with the rat, and runs with **zero code changes**. Whatever friction it exposes is the real backlog. Do not build more of the roster. (Detailed plan: see "Stage 3 — detailed plan" below.)

---

## Risks & mitigations
- **Runtime asset loading in prod vs dev** (biggest risk). Mitigated by data URLs (no asset-protocol/CSP/scope config) + the dev/resource/user dir scan order mirroring the proven `config.rs` pattern. Verify the bundled `resource_dir()/characters` path resolves inside the macOS `.app`.
- **Character swap without window jump.** Manifest `canvas`/`anchor` + a fixed 150×150 display box; test rat↔skull live.
- **Frontend creeping business logic.** Opacity ramp and rate unit computed in Rust; the view only eases, ping-pongs, and plays one-shots.
- **Per-character cost creep.** Required set stays 10; extra frames are optional manifest polish, never required.
- **Held→transient behavior change** for `refreshed`/`error` is intentional (spec) — call it out in the Stage 1 PR since the felt behavior changes (brief celebration/startle instead of a held pose).

## Verification
- `cd src-tauri && cargo test` — state machine (renamed poses, sent→thinking, spent crash), event resolver (priority + debounce), rate unit, message classification, **character loader validation** (missing asset / missing state / bad renderer → excluded + logged).
- `cargo clippy -- -D warnings` and `cargo fmt`.
- `npm run tauri dev` and observe on the live rat: poses track the rate (`thinking→working→frantic→onfire→spent`); near-limit overlay fades in past ~90% with a `%` readout, then a refresh countdown at ≥100%; `refreshed`/`error` play as brief one-shots and return to base; the tray **Character** submenu swaps rat↔skull instantly with no window jump.
- Loud-failure check: temporarily delete one skull asset → confirm the skull is excluded + logged, rat keeps working.
- Quota sanity vs `npx ccusage@latest blocks --json` (active-window `input+output`/`totalTokens`), per `CLAUDE.md`.
- Note: this is a Tauri desktop window, not a browser app — verify by observation/manual screenshots, not Playwright.

---

## Stage 3 — detailed plan (flaming skull)

**Goal.** Prove the character system works end-to-end with a *second, independently-authored* character and **zero engine code changes** — and resolve the one piece of friction Stage 2 deliberately deferred (manifest-driven **anchor/canvas** placement). A flaming skull reads better than a furnace and exercises a non-rat silhouette. This is a contract test, not an art deliverable — placeholder art is fine and the real skull art drops in over it with no code changes.

### Deliverables
1. **`characters/skull/character.json`** — same fixed contract as the rat: the 7 base states (`sleeping/thinking/working/frantic/onfire/spent/done`), the `quotaProximity` modifier (`nearlimit.png`), and the `refreshed`/`error`/`flinch` events. Asset filenames are semantic (`sleeping.png`, …, `nearlimit.png`, `refreshed.png`, `error.png`, `flinch.png`) so the user authoring fresh art names files by what they *are*. `canvas`/`anchor` are declared and **actually honored** by the view (see code change below).
2. **`characters/skull/` placeholder PNGs** — every required file present so the character validates and appears in the tray *now*, letting the rat↔skull swap be verified this stage. Placeholders are copied from the rat **only where the real asset is absent**, so they never clobber art the user has already dropped in. Documented as placeholders in the skull's `ART_NEEDS.md`.
3. **`characters/skull/ART_NEEDS.md`** — the exact asset contract (filenames, layer, the overlay caveat for `nearlimit`) + the 300×300 / <100 KB sizing guidance, mirroring the rat's.

### Code change — honor `anchor` (the deferred friction)
Stage 2 rendered every character into a fixed 150×150 box with `object-fit: contain` (always centers), ignoring the manifest `anchor`. That's correct *only* for a centered, square character. To make the system robust to whatever silhouette/framing the skull uses (and any future character), the view now applies the manifest **`anchor`** as `object-position` on the base sprite and the overlay:

- `reloadCharacter()` reads `resolved.anchor` and sets `--anchor-x`/`--anchor-y` (or `object-position: ${x*100}% ${y*100}%`) on `#sprite` and `#overlay`. Anchor `0.5/0.5` ⇒ `50% 50%` ⇒ centered ⇒ **identical to today** (the rat is unaffected — backward compatible).
- The **window-jump** guarantee is already met by the fixed 150×150 footprint (the window never resizes on swap); `object-fit: contain` keeps any aspect ratio undistorted. `canvas` therefore needs no CSS sizing for the sprite path — it stays in the manifest/`ResolvedAsset` for a future renderer and is left as a documented no-op for now (call this out rather than pretend it's wired). The hat stays centered/global (no per-character hats; no hat art exists yet).

This is the minimal honest interpretation of "size from `canvas`, align via `anchor`": anchor is wired and meaningful; canvas is acknowledged as a no-op under `object-fit: contain` instead of being faked.

### Tests
- Generalize the shipped-character check: assert **every** discovered character under `dev_characters_dir()` is contract-valid and resolves (so the rat *and* the skull are both guaranteed valid, and any future folder is auto-covered).
- Keep the existing rat-specific assertion (2 sleeping frames) as a regression guard.

### Build order
1. Manifest + `ART_NEEDS.md` + placeholder PNGs (copy-if-absent) → skull validates, appears in tray.
2. Wire `anchor` → `object-position` in `main.ts` (+ a CSS default of `50% 50%`).
3. `cargo test` (incl. the generalized all-characters-valid test), `cargo clippy -- -D warnings`, `cargo fmt`, `npm run build`.
4. Commit `Stage 3: prove the contract with the flaming skull`.

### Live verification (needs eyes on the window — user-run)
- Tray **Character** now lists **Rat** and **Flaming Skull**; selecting one swaps live with no window jump and the readout/poses keep tracking the rate.
- Loud-failure check: rename one `characters/skull/*.png` away → the skull drops out of the tray + an `eprintln!` logs why; the rat keeps working. Restore it and it reappears on restart.
- As the user replaces each placeholder with real skull art, it shows up on the next `character-changed`/restart with **zero code changes** — the whole point.

### Explicitly out of scope (do not build)
- More of the roster beyond the skull.
- Per-character hats, the `mesh` renderer, or `canvas`-driven CSS sizing (only add the last if a real character actually needs a non-contain fit — none does yet).
