# Plan: Dev State Override (debug-build-only)

## Goal

A developer-only affordance to force the rat into any creature state on demand —
to verify poses, CSS glows, and the readout; to eyeball the EMA glide and the
upcoming character art; and to record the "rat cycling through states" GIF that
is already a TODO under `NEXT.md`'s GitHub section. It removes the need to
reproduce the exact telemetry that triggers each state (`onfire`, `atlimit`,
`refreshed`, `error`, the approaching bands), most of which are painful to
reach from real usage and several of which have no dedicated art yet.

## Key decisions

- **Debug-build only** (`#[cfg(debug_assertions)]`) — never appears in a shipped
  release; no end-user-facing surface.
- **Rust-side override, not a frontend hack** — keeps the house convention (the
  frontend stays a dumb view). The forced state slots into the *existing* emit
  pipeline, so glows / readout / hat render exactly as production does. The
  frontend needs **zero changes**.
- **Two modes:** manual force (pick one state, hold it) + auto-cycle (sweep all
  states on a fixed cadence — also serves the README GIF capture).

## Implementation

### `src-tauri/src/state.rs`
- Add `const DEV_STATES: &[&str]` (or `CreatureState::ALL`) covering the 15
  `CreatureState::as_str()` values plus `"surprised"` (the transient perk-up
  pose the frontend renders from an event).
- Add a unit test asserting `DEV_STATES` contains every `CreatureState::as_str()`
  value, so the dev menu can't silently drift out of date when a state is added.

### `src-tauri/src/lib.rs` — `Shared`
Add two fields, mutated only by the dev tray:
- `dev_forced: Mutex<Option<&'static str>>` — the forced state, or `None` =
  normal operation.
- `dev_cycle: AtomicBool` — auto-cycle toggle.

(Both can be unconditional fields to avoid sprinkling `cfg` through `Shared`;
only the menu that mutates them and the block that reads them are cfg-gated.)

### `src-tauri/src/lib.rs` — poll loop
- Just before building `GameState`, override the resolved state when forced:
  ```rust
  let state_str = dev_forced.unwrap_or(creature.as_str());
  ```
- **Cycle mode:** hold a `dev_cycle_idx`; when `dev_cycle` is on, advance it each
  iteration and force `DEV_STATES[idx]`. The loop normally waits on a filesystem
  event up to a 10 s idle tick — too slow to *watch* a sweep — so when cycling,
  use a short fixed `recv_timeout(DEV_CYCLE_MS)` (~1.2 s dev constant) instead so
  the rat steps visibly.

### `src-tauri/src/lib.rs` — `build_tray`
Under `#[cfg(debug_assertions)]`, add a **"Dev" submenu** mirroring the existing
Opacity submenu:
- `Cycle states` — CheckMenuItem, toggles `dev_cycle`.
- `Live (off)` — clears the override.
- one CheckMenuItem per state (checked = currently forced).

Menu-event handler arms `dev:cycle` / `dev:off` / `dev:<state>`, mirroring the
`opacity:` arm. **No persistence** — dev-only, so it never writes to
`settings.json`.

### `src/main.ts`
No changes. (Optional later nicety: surface the forced state name in the
readout — skipped for the first cut to keep the frontend dumb.)

## Interaction with the character-system refactor

The character refactor replaces the single `state` field with `base_state` +
`event` + layers. This override is ~20 lines and will need a small adaptation
then (force `base_state` / `event` instead of one string) — cheap. Building it
now is worth it precisely because it is the tool that makes verifying both the
EMA glide *and* the new character art tractable; it should land before Stage 2
of the character plan.

## Scope / non-goals

- Release builds untouched (cfg-gated).
- First cut forces only the **state string** (pose + glow). The readout may not
  match a forced state (e.g. `onfire` with a real rate of ~0, `atlimit` with a
  stale countdown). Acceptable for pose/art verification; synthesizing plausible
  readout values is a noted follow-up, not in scope.

## Verification

- `cd src-tauri && cargo test` — the `DEV_STATES` completeness test, plus the
  existing suites stay green.
- `cargo build` — confirm the cfg-gated tray compiles.
- `npm run tauri dev` → tray → **Dev** → pick each state, confirm pose + glow;
  toggle **Cycle states** and watch the full sweep (also the GIF capture path).
