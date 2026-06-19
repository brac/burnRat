// burnRat permission bubble — a tiny dedicated window. The backend (hookbridge)
// parks an incoming tool-permission request, shows this window, and blocks on a
// decision. We send the verdict back via the `resolve_permission` command (the
// backend's held HTTP connection then replies to Claude Code).
//
// We PULL the active request (current_permission) whenever the window gains
// focus, rather than relying only on the pushed "permission-request" event — a
// freshly-shown window can miss the emit, which would leave the id unknown and
// make the buttons no-ops. Global Ctrl/Cmd+Shift+Y/N resolve it in Rust too;
// "permission-resolved" tells us to hide. Escape = defer to Claude's prompt.

import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

interface PermissionInfo {
  id: number;
  tool: string;
  detail: string;
}

const win = getCurrentWindow();

window.addEventListener("DOMContentLoaded", () => {
  // The global hotkey uses the platform's primary modifier (Cmd on macOS),
  // so label the hint to match what the user actually presses.
  if (/Mac/i.test(navigator.userAgent || navigator.platform)) {
    document
      .querySelectorAll<HTMLElement>(".mod")
      .forEach((el) => (el.textContent = "Cmd"));
  }

  const toolEl = document.querySelector<HTMLElement>("#tool")!;
  const detailEl = document.querySelector<HTMLElement>("#detail")!;
  const allowBtn = document.querySelector<HTMLButtonElement>("#allow")!;
  const denyBtn = document.querySelector<HTMLButtonElement>("#deny")!;

  // The request currently shown. null when idle/hidden.
  let currentId: number | null = null;

  function populate(info: PermissionInfo | null) {
    if (!info) return;
    currentId = info.id;
    toolEl.textContent = info.tool || "a tool";
    detailEl.textContent = info.detail || "";
    detailEl.style.display = info.detail ? "block" : "none";
  }

  // Send the verdict for the active request and hide. If we somehow don't know
  // the id yet, pull it first so a click is never a no-op. `resolve_permission`
  // is idempotent on the backend, so racing a hotkey is harmless.
  async function decide(behavior: "allow" | "deny" | "defer") {
    let id = currentId;
    if (id === null) {
      const info = await invoke<PermissionInfo | null>("current_permission");
      id = info?.id ?? null;
    }
    currentId = null;
    if (id !== null) {
      const args: Record<string, unknown> = { id, behavior };
      if (behavior === "deny") args.message = "Denied via burnRat";
      try {
        await invoke("resolve_permission", args);
      } catch (e) {
        console.error("burnRat: resolve_permission failed", e);
      }
    }
    void win.hide();
  }

  allowBtn.addEventListener("click", () => void decide("allow"));
  denyBtn.addEventListener("click", () => void decide("deny"));
  // Esc / dismiss → let Claude Code fall back to its own terminal prompt.
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") void decide("defer");
  });

  // Robust path: when the window is shown it gains focus → pull the active
  // request so the id + text are always correct.
  void win.onFocusChanged(({ payload: focused }) => {
    if (focused) void invoke<PermissionInfo | null>("current_permission").then(populate);
  });

  // Fast path: a new request arrived while we're already up.
  listen<PermissionInfo>("permission-request", (e) => populate(e.payload));

  // Resolved elsewhere (hotkey or timeout) — drop our copy and hide.
  listen<{ id: number }>("permission-resolved", (e) => {
    if (currentId === e.payload.id) currentId = null;
    void win.hide();
  });

  // In case the window is already visible with a request pending at load.
  void invoke<PermissionInfo | null>("current_permission").then(populate);
});
