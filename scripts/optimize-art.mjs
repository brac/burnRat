// Resize + compress character/hat PNGs down to their display target.
//
// The pet renders at 150x150 CSS px, so source art should be 300x300 (2x for
// HiDPI) and well under ~100 KB each (see CLAUDE.md). The hand-drawn exports are
// multi-MB full-res, which bloats the bundle and memory. This tool fits each PNG
// inside a TARGET_PX box (preserving aspect + transparency, never upscaling) and
// palette-quantizes it down under a size budget, rewriting the file in place.
//
// Originals are recoverable from git — this overwrites, so commit (or stash)
// first if you want a clean before/after. Use --dry-run to preview with no writes.
//
// Usage:
//   node scripts/optimize-art.mjs [dirs...] [--max=300] [--budget=100] [--dry-run]
//   npm run optimize-art -- --dry-run
//
// Defaults to the character art (characters/) and the hats (src/hats/). Pass one
// or more dirs/files to override (e.g. `node scripts/optimize-art.mjs characters/rat`).

import { readdir, stat, readFile, writeFile } from "node:fs/promises";
import { join, extname } from "node:path";
import sharp from "sharp";

// ---- args ----
const args = process.argv.slice(2);
const flags = new Map();
const paths = [];
for (const a of args) {
  if (a.startsWith("--")) {
    const [k, v] = a.slice(2).split("=");
    flags.set(k, v ?? true);
  } else {
    paths.push(a);
  }
}
// A numeric flag must be given a value (`--max=300`, not a bare `--max`) and
// parse to a positive number — otherwise a bare flag becomes boolean `true`,
// `Number(true)` is 1, and the tool would silently resize every PNG to 1x1 and
// overwrite it in place. Fail loudly instead.
function numFlag(name, def) {
  const raw = flags.get(name);
  if (raw === undefined) return def;
  if (raw === true) {
    console.error(`--${name} needs a value, e.g. --${name}=${def}`);
    process.exit(1);
  }
  const n = Number(raw);
  if (!Number.isFinite(n) || n <= 0) {
    console.error(`--${name} must be a positive number (got "${raw}")`);
    process.exit(1);
  }
  return n;
}
// A boolean flag is true when bare (`--dry-run`) or `=true/1`, false at `=false/0`.
function boolFlag(name) {
  const raw = flags.get(name);
  if (raw === undefined) return false;
  if (raw === false || raw === "false" || raw === "0") return false;
  return true;
}
const TARGET_PX = numFlag("max", 300); // longest side, px
const BUDGET_KB = numFlag("budget", 100); // per-file size ceiling
const DRY_RUN = boolFlag("dry-run");
const ROOTS = paths.length ? paths : ["characters", "src/hats"];
const BUDGET_BYTES = BUDGET_KB * 1024;

// ---- helpers ----
const kb = (n) => `${(n / 1024).toFixed(1)} KB`;

// Recursively collect every .png under a path (a file path is returned as-is).
async function collectPngs(p) {
  let s;
  try {
    s = await stat(p);
  } catch {
    console.warn(`! skip (not found): ${p}`);
    return [];
  }
  if (s.isFile()) return extname(p).toLowerCase() === ".png" ? [p] : [];
  const entries = await readdir(p, { withFileTypes: true });
  const out = [];
  for (const e of entries) {
    out.push(...(await collectPngs(join(p, e.name))));
  }
  return out;
}

// Encode `pipeline` to a PNG buffer under BUDGET_BYTES if possible. Start lossless
// and step down the palette quality ladder until it fits (alpha is preserved at
// every step). Returns { buf, quality } — quality null means lossless was kept.
async function encodeUnderBudget(pipeline) {
  // First try a high-effort lossless deflate — best for flat/cel-shaded art.
  const lossless = await pipeline
    .clone()
    .png({ compressionLevel: 9, effort: 10 })
    .toBuffer();
  if (lossless.length <= BUDGET_BYTES) return { buf: lossless, quality: null };

  // Otherwise quantize to an 8-bit palette, lowering quality until it fits.
  let best = lossless;
  let bestQ = null;
  for (const quality of [90, 80, 70, 60, 50, 40, 30]) {
    const buf = await pipeline
      .clone()
      .png({ palette: true, quality, compressionLevel: 9, effort: 10 })
      .toBuffer();
    if (buf.length < best.length) {
      best = buf;
      bestQ = quality;
    }
    if (buf.length <= BUDGET_BYTES) return { buf, quality };
  }
  // Never got under budget — hand back the smallest we managed (caller warns).
  return { buf: best, quality: bestQ };
}

// ---- main ----
const files = [];
for (const r of ROOTS) files.push(...(await collectPngs(r)));

if (files.length === 0) {
  console.error("No PNGs found under:", ROOTS.join(", "));
  process.exit(1);
}

console.log(
  `Optimizing ${files.length} PNG(s) → fit ${TARGET_PX}×${TARGET_PX}, budget ${BUDGET_KB} KB` +
    (DRY_RUN ? "  [DRY RUN — no writes]" : ""),
);

let totalBefore = 0;
let totalAfter = 0;
let overBudget = 0;
let changed = 0;

for (const file of files) {
  const original = await readFile(file);
  const meta = await sharp(original).metadata();
  totalBefore += original.length;

  // Fit inside the target box, preserving aspect; never enlarge small art.
  const pipeline = sharp(original).resize(TARGET_PX, TARGET_PX, {
    fit: "inside",
    withoutEnlargement: true,
  });

  const { buf, quality } = await encodeUnderBudget(pipeline);

  // Keep whichever is smaller — if the source was already tiny/optimal, our
  // re-encode might be larger, so don't bloat it.
  const useOptimized = buf.length < original.length;
  const finalLen = useOptimized ? buf.length : original.length;
  totalAfter += finalLen;

  const newMeta = useOptimized ? await sharp(buf).metadata() : meta;
  const dims = `${newMeta.width}×${newMeta.height}`;
  const tag = quality === null ? "lossless" : `q${quality}`;
  const over = finalLen > BUDGET_BYTES;
  if (over) overBudget++;

  if (!useOptimized) {
    console.log(`  = ${file}  ${kb(original.length)} (${dims}, already optimal)`);
  } else {
    changed++;
    const mark = over ? "⚠" : "✓";
    console.log(
      `  ${mark} ${file}  ${kb(original.length)} → ${kb(finalLen)} (${dims}, ${tag})` +
        (over ? `  STILL OVER ${BUDGET_KB} KB` : ""),
    );
    if (!DRY_RUN) await writeFile(file, buf);
  }
}

console.log(
  `\n${DRY_RUN ? "Would rewrite" : "Rewrote"} ${changed}/${files.length} file(s): ` +
    `${kb(totalBefore)} → ${kb(totalAfter)} ` +
    `(saved ${kb(totalBefore - totalAfter)}, ${((1 - totalAfter / totalBefore) * 100).toFixed(0)}%)`,
);
if (overBudget > 0) {
  console.warn(
    `⚠ ${overBudget} file(s) still over ${BUDGET_KB} KB — lower --budget tolerance, ` +
      `reduce --max, or hand-tune those in an image editor.`,
  );
}
