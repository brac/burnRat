# Rat character — art needs

The **rat** is burnRat's reference character. Most poses are finalized, optimized
art. A few assets are still **placeholders reusing another pose** so the contract
validates and the engine renders today — drop a real PNG in over the contract
filename and it appears on the next character switch / app restart, **zero code
changes** (the filename is the contract).

## Outstanding art (placeholders today)

| Asset | Placeholder | What it should be |
|---|---|---|
| `asking.png` | copy of `done.png` | The rat is **asking *you*** — Claude posed a question and is waiting on your answer. Two cases share this pose: (a) a **multiple-choice question** (`AskUserQuestion` / plan approval — you answer in the terminal; the pet can't show the choices, so it just reacts with this pose), and (b) a **pending tool-permission** request (which *also* shows the Allow/Deny pet bubble). Must read as "I need your input," visually distinct from `done` (a finished turn, your move). |
| `nearlimit.png` | copy of `idle.png` | **OVERLAY accent**, not a full second rat. Drawn on top of whatever base pose is showing at a variable opacity (0→1 as you approach your quota ceiling) — a hotter aura / warning glyph / gauge. Top priority: the current full-rat placeholder looks wrong overlaid. |
| `refreshed.png` | placeholder | Brief one-shot: your 5-hour quota window just rolled over — a happy "fresh quota" beat. |
| `error.png` | placeholder | Brief one-shot: Claude Code hit an API error — a concerned/sputter pose. |

## Optional polish

- A distinct `thinking.png` (the rat currently reuses idle art for it).
- Per-pose ping-pong frames: drop `asking_1.png`, `asking_2.png`, … next to
  `asking.png` and they auto-discover into that pose's loop in index order
  (1 = static, 2 = alternate, 3+ = smooth ping-pong). No manifest edit needed.

## Sizing

The pet renders at **150×150 CSS px**, so source PNGs should be **300×300**
(2× for HiDPI) and optimized to **well under ~100 KB** each. After dropping new
art in, run **`npm run optimize-art`** to resize/compress in place (lossless
first, palette-quantize only if over budget, alpha preserved). The `canvas` in
`character.json` is `300×300`; set an `anchor` per-entry only if a pose's natural
silhouette shifts (`0.5,0.5` = center; the view honors it).
