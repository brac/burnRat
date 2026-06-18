# Rat character — art needs

This folder is the reference character for burnRat's character system (see
`../../CHARACTER_SYSTEM_PLAN.md`). Every asset below already exists so the engine
can wire up and render, but several are **placeholders reusing other poses**.
Dropping a real PNG in over a placeholder needs **zero code changes** — the
filename is the contract.

## Asset contract (all present)

| Asset | Layer | Source today | Status |
|---|---|---|---|
| `sleeping.png` (+`_1`) | base | original `sleeping` | ✅ real |
| `thinking.png` | base | original `idle` (old *calm*) | ⚠️ reused — fine, a distinct "thinking/latency" pose is optional polish |
| `working.png` (+`_1`,`_2`) | base | original `working` | ✅ real |
| `frantic.png` (+`_1`,`_2`) | base | original `stressed` (renamed) | ✅ real |
| `onfire.png` (+`_1`) | base | original `onfire` | ✅ real |
| `spent.png` | base | original `spent` | ✅ real |
| `done.png` | base | original `done` | ✅ real (see note on `waiting` below) |
| `nearlimit.png` | modifier (`quotaProximity`) | placeholder = `idle` | ❌ **needs real art** |
| `refreshed.png` | event | placeholder = `idle` | ❌ **needs real art** |
| `error.png` | event | placeholder = `surprised` | ❌ **needs real art** (startle is a rough stand-in) |
| `surprised.png` | event (`flinch`) | original `surprised` | ✅ real |

## Genuinely-new art required (priority order)

1. **`nearlimit.png`** — this is an **overlay** drawn *on top of* the base pose at
   a variable opacity (0 → 1 as the user approaches their quota ceiling). It must
   read as a warning *accent* (aura / sweat / gauge / "!" bubble) that looks right
   composited over any base pose — not a full second rat. The placeholder (a whole
   idle rat) will look wrong overlaid; this is the most important one to replace.
2. **`refreshed.png`** — a brief celebratory "fresh quota" pose (played as a
   one-shot when the 5h window rolls over).
3. **`error.png`** — a concern/error pose (played as a one-shot on an API error).
   Currently borrows the `surprised` startle.

Optional polish: a dedicated **`thinking`** pose distinct from idle.

## Notes

- **`waiting` art is now unused.** The old `Waiting` (Claude asking an interactive
  question) and `Done` (finished turn) collapse into a single **`done`** base
  state, which uses `done.png`. The original 3-frame `waiting` animation
  (`waiting.png`/`_1`/`_2` in `src/sprites/`) is therefore orphaned — it could be
  revived as the `done` loop (declare `frames` for `done` in `character.json`) if a
  livelier "awaiting you" pose is wanted.
- **Sizes are far too big.** Every PNG here is ~0.8–1.6 MB (copied from the
  oversized originals). Target per `CLAUDE.md`: **300×300** (2× the 150×150 display)
  and **well under ~100 KB** each (run through pngquant/oxipng). Do this in the art
  pass; not auto-applied here (irreversible).
