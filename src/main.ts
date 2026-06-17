// burnRat frontend — a dumb clear-and-redraw view.
// Sprite rendering runs on its own frame loop (for the working animation and
// the brief surprised pop); the game-state event only updates the data.

import { listen } from "@tauri-apps/api/event";

interface GameState {
  smoothedTpm: number;
  instantTpm: number;
  consumed: number;
  consumedWithCache: number;
  projected: number;
  timeRemainingMin: number;
  isActive: boolean;
  opacity: number;
  state: string;
  event: string | null;
}

// State -> sprite file. Served from /public/sprites. `calm` uses the idle art.
const SPRITE: Record<string, string> = {
  sleeping: "/sprites/sleeping.png",
  calm: "/sprites/idle.png",
  working: "/sprites/working.png",
  stressed: "/sprites/stressed.png",
  onfire: "/sprites/onfire.png",
  spent: "/sprites/spent.png",
  surprised: "/sprites/surprised.png",
};

// Working is a 3-frame loop.
const WORKING_FRAMES = [
  "/sprites/working.png",
  "/sprites/working_1.png",
  "/sprites/working_2.png",
];

const FRAME_MS = 280; // animation cadence; also the "1 frame" surprised duration

// Preload so transitions don't flash a blank frame.
for (const src of [...Object.values(SPRITE), ...WORKING_FRAMES]) {
  const img = new Image();
  img.src = src;
}

const fmt = (n: number) =>
  n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${Math.round(n)}`;

window.addEventListener("DOMContentLoaded", () => {
  const pet = document.querySelector<HTMLElement>("#pet");
  const sprite = document.querySelector<HTMLImageElement>("#sprite");
  const readout = document.querySelector<HTMLElement>("#readout");

  let liveState = "sleeping";
  let prevState = "sleeping";
  let surprisedUntil = 0; // show the surprised sprite until this timestamp
  let frame = 0;

  // Sprite render loop — independent of the ~2s data poll.
  setInterval(() => {
    if (!sprite) return;
    const now = Date.now();
    if (now < surprisedUntil) {
      sprite.src = SPRITE.surprised;
    } else if (liveState === "working") {
      frame = (frame + 1) % WORKING_FRAMES.length;
      sprite.src = WORKING_FRAMES[frame];
    } else {
      sprite.src = SPRITE[liveState] ?? SPRITE.calm;
    }
  }, FRAME_MS);

  listen<GameState>("game-state", (event) => {
    const s = event.payload;

    // Surprised "perk-up": resting (sleeping/calm) -> busy. One frame only.
    const RESTING = new Set(["sleeping", "calm"]);
    const BUSY = new Set(["working", "stressed", "onfire"]);
    if (RESTING.has(prevState) && BUSY.has(s.state)) {
      surprisedUntil = Date.now() + FRAME_MS;
    }
    prevState = s.state;
    liveState = s.state;

    if (pet) {
      pet.className = `pet state-${s.state}`;
      pet.style.opacity = String(s.opacity);
    }

    if (readout) {
      readout.textContent = s.isActive ? `${fmt(s.smoothedTpm)}/min` : "zzz";
    }
  });
});
