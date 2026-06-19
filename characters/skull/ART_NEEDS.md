# Flaming Skull character — art needs

The **flaming skull** is burnRat's second character — the one that proves the
character system works with art authored independently of the rat, with **zero
engine code changes** (see `../../CHARACTER_SYSTEM_PLAN.md`, "Stage 3").

Every asset below is currently a **placeholder copied from the rat** so the
character validates and shows up in the tray today. Drop a real PNG in over a
placeholder and it appears on the next character switch / app restart — the
**filename is the contract**, nothing else to change.

## Asset contract (all required, all present as placeholders)

| Asset | Layer | Notes |
|---|---|---|
| `sleeping.png` | base | skull dormant / embers low |
| `thinking.png` | base | idle-but-awake; latency pose |
| `working.png` | base | steady burn |
| `frantic.png` | base | flames whipping; high burn |
| `onfire.png` | base | peak — fully ablaze (sustained high rate) |
| `spent.png` | base | burnt out / smoldering (post-onfire crash) |
| `done.png` | base | calm flame; "your turn" — a finished turn (your move) |
| `asking.png` | base (optional) | skull is **asking *you***: a multiple-choice question (`AskUserQuestion` / plan approval, answered in the terminal) **or** a pending tool-permission (also shows the Allow/Deny pet bubble). Distinct from `done` — "I need your input." Placeholder = `done.png`; falls back to `done` without it. |
| `nearlimit.png` | modifier (`quotaProximity`) | **OVERLAY** — see caveat below |
| `refreshed.png` | event | brief flare-up; fresh quota one-shot |
| `error.png` | event | concern / sputter; API-error one-shot |
| `flinch.png` | event | startle bounce on a rate spike |

## Important: `nearlimit.png` is an overlay, not a full pose

It is drawn **on top of** whatever base pose is showing, at a variable opacity
(0 → 1 as the user approaches their quota ceiling). It must read as a warning
*accent* composited over any base pose (e.g. a hotter aura, a warning glyph, a
gauge) — **not** a second full skull. The current placeholder is a whole rat and
will look wrong overlaid; this is the most important one to make skull-native.

## Optional polish

- Per-pose ping-pong frames: just drop `sleeping_1.png`, `working_1.png`,
  `working_2.png`, etc. next to the base pose — they auto-discover into that
  pose's loop in index order (1 = static, 2 = alternate, 3+ = smooth). No
  manifest edit needed (an explicit `frames: [...]` on an entry overrides it).
- A distinct `thinking` pose (the rat reuses idle here).

## Sizing (same as the rat)

The pet renders at **150×150 CSS px**, so source PNGs should be **300×300**
(2× for HiDPI) and optimized to **well under ~100 KB** each (pngquant/oxipng).
The `canvas` in `character.json` is `300×300`; if the skull's natural silhouette
is non-square, set `canvas` to its real pixel box and pick an `anchor` (the
point that should stay put across poses — `0.5,0.5` = center). The view honors
`anchor`; `object-fit: contain` keeps any shape undistorted within the footprint.
