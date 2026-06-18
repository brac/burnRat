# Plan: lifecycle-hook ingest (#1) + permission interaction (#2)

Implementation plan for the first two clawd-on-desk learnings in `NEXT.md`
("Learn from clawd-on-desk"). Read that section first for the motivation. This
doc is the *how*.

## Where these come from

clawd-on-desk drives its pet from Claude Code **hooks** instead of inferring
state from JSONL. Two distinct hook mechanisms:

- **Command hooks** (fire-and-forget, `async`, ~5s timeout): `SessionStart`,
  `Stop`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `SubagentStart/Stop`,
  `Notification`, … Each runs a script that reads the event JSON on stdin and
  **POSTs it to a local HTTP server** (`127.0.0.1:23333/state`). The app reacts;
  it never replies. This is learning **#1**.
- **One HTTP hook** (`type: "http"`, *blocking*, 600s timeout):
  `PermissionRequest` → `127.0.0.1:23333/permission`. Claude Code **holds the
  connection open** until the app writes back a JSON decision
  (`{ hookSpecificOutput: { decision: { behavior: "allow"|"deny", … } } }`).
  That held-open response is the entire trick behind the Allow/Deny bubble. This
  is learning **#2**.

Both share the same local server + the same "install hooks into
`~/.claude/settings.json`" machinery. That shared piece is the real prerequisite
— call it **Phase 0**.

---

## Do we need #1 before #2?

**No hard functional dependency — but a strong "build the foundation first"
ordering, and #1 is the right vehicle for that foundation.**

- #2 does **not** consume #1's lifecycle events. The permission flow is its own
  hook (`PermissionRequest`, HTTP type) with its own request→response cycle. You
  could technically ship the permission bubble without ever wiring a single
  `SessionStart`/`Stop` pose.
- What #2 *does* need is everything in **Phase 0**: a loopback HTTP server in the
  Rust process, and the installer that registers hooks into
  `~/.claude/settings.json` (+ uninstall/repair). That foundation is shared.
- The safe way to stand up, debug, and prove Phase 0 is **#1**, because #1 is
  read-only and zero-trust: fire-and-forget POSTs, no held connections, no
  control surface. If the server, port discovery, installer, and round-trip are
  already proven by #1, then #2 is mostly additive (one blocking endpoint + a
  bubble UI + hotkeys + the held-open response). Attempting #2 first means
  debugging the hardest part (a blocking response Claude Code is waiting on)
  *and* the foundation at the same time.

**Recommendation:** Phase 0 → #1 (proves the rails, ships visible value on its
own) → #2 (adds the control surface on proven rails). Don't start #2 before
Phase 0 exists and #1 has shown the transport works end-to-end.

⚠️ This is a deliberate departure from burnRat's current positioning ("no
network, trivially auditable, tiny trust surface"). Phase 0 opens a loopback
listener; #2 turns burnRat from observer into a control surface. Both must be
**opt-in and off by default**, loopback-only, with the hook install gated behind
an explicit user action. Flag this in the README before shipping.

---

## Phase 0 — the hook bridge (shared foundation) ✅ CODE-COMPLETE (live check owed)

Goal: a local listener Claude Code hooks can reach, plus install/uninstall of
the hook entries. No behavior change to the existing rate/blocks/state core.

**Status (built):** `hookbridge.rs` (std::net loopback HTTP server + `burnrat
hook <Event>` subcommand client), `hookinstall.rs` (idempotent settings.json
merge/remove with backup), `localServer` config (off by default), tray "Connect
to Claude Code", main.rs subcommand dispatch. 58 tests / clippy -D / fmt all
green. Schema verified vs the installed Claude Code (`async`/`shell` omitted —
undocumented; rely on the 5s timeout). **Remaining: the live check** (run the
app, Connect, confirm `/state` fires on a real session, Disconnect cleans up) —
see the LIVE CHECK note in `NEXT.md`, plus the Windows GUI-subsystem-stdin and
no-`shell` caveats.

1. **Local loopback server (Rust).** Bind `127.0.0.1` on a port from a small
   candidate range (clawd uses `23333..=23337`; first free wins). Persist the
   chosen port so the hook script and the app agree. Reject any non-loopback
   peer. Add `localServer.enabled` (default **false**) + `localServer.ports` to
   `data/settings.default.json`; mirror the opt-in into `userconfig.rs`.
   - Tauri already pulls in an async runtime; a tiny `axum`/`hyper` server on a
     dedicated thread is enough. Keep it behind a cargo feature or the runtime
     `enabled` flag so release builds without it pay nothing.
2. **Hook installer.** A command (tray item "Connect to Claude Code…" or a
   one-shot on first opt-in) that merges burnRat's hook entries into
   `~/.claude/settings.json` and a matching uninstaller that removes only our
   entries (match on a stable marker, e.g. a `# burnRat` tag or our script
   path). Back up the file before writing. Resolve `~` via `dirs::home_dir()`.
3. **Hook script.** Ship a tiny script (Node or a burnRat subcommand —
   `burnrat hook <Event>`) that reads the event JSON on stdin, reads the saved
   port, and POSTs to the server. A self-invoking subcommand avoids a Node
   dependency and keeps everything in one binary; prefer that.
4. **Verify against the installed Claude Code version.** Hook schemas have
   drifted. Before relying on field names/exit semantics, confirm the current
   `PreToolUse`/`PermissionRequest` payloads and the `type: "http"` hook support
   against the actual installed version (the `claude-code-guide` agent or
   `claude --help`/docs). Treat field maps as config in `data/`.

**Gate:** with `localServer.enabled = true` and hooks installed, hitting
`/state` by hand (curl) updates a debug counter; uninstall cleanly removes the
entries. No pose wiring yet.

---

## #1 — lifecycle-hook ingest ✅ v1 SHIPPED

**Status (built):** the canonical `HookState` lives in `Shared`; the poll loop
reads a snapshot each tick and fuses it with the JSONL inference via
`hookbridge::fuse_awaiting` (more-recent source wins, `hookSignalTtlSeconds`
backstop). `UserPromptSubmit`→Sent, `Stop`→Done, `PreToolUse`(interactive)→Asking
arrive instantly; `PreToolUse`/`PostToolUse`/`SubagentStop` perk the rat to
`working` (OR'd into `recent_activity`); any hook edge resets the nap clock
(`nap_idle.min(hook_idle)`). Pure no-op when the bridge is disconnected (empty
snapshot). 64 tests / clippy -D / fmt all green; no frontend change (signals flow
through the existing `Awaiting`→pose mapping). **Deferred to v1.1:** transient
hook one-shots (e.g. SubagentStart→flinch) and the `Source`-trait refactor — see
below; not required for the headline value.

Goal: precise, instant lifecycle edges feed the **existing** state/event system
as a *second input alongside* the JSONL-derived burn signal. The burn rate stays
the headline; hooks sharpen the discrete states. **Do not rip out `data.rs`** —
JSONL inference remains the fallback and the source of rate/blocks/quota.

1. **Endpoint.** `POST /state` on the Phase 0 server. Parse the normalized body
   (`event`, `session_id`, `tool_name`, `cwd`, …). Keep a lenient JSON probe
   like `data.rs` already does.
2. **Map events → signals.** Fold into the existing model, don't invent a new
   path:
   - Discrete poses (turn started, working, done/awaiting, error) → feed the
     **`StateMachine`** / the existing `Awaiting` enum. e.g. `Stop`/`end_turn`
     → `Done`, `UserPromptSubmit` → `Sent`, `Notification`(error) → `Error`.
     These currently come from `classify_assistant`/`classify_user`; hooks give
     the same signals *instantly* instead of on the next 1s poll.
   - Transient one-shots (subagent spawned, tool flinch, permission incoming)
     → feed **`events.rs` `EventResolver`** as new Layer-3 events
     (`flinch`/`surprised`/etc.), reusing its priority + debounce. Add the new
     event names to the character contract (placeholder art is fine; the engine
     is already wired for unknown→fallback).
3. **Fuse hook signal with JSONL.** The poll loop currently derives `kind`
   (`awaiting()`) and `latest_model` from `DataMonitor`. Introduce a shared,
   thread-safe "last hook event" slot (an `Arc<Mutex<HookSignal>>` or a `watch`
   channel) the server writes and the poll loop reads each tick. Precedence:
   a **fresh** hook event (within a short TTL) wins over the JSONL inference for
   the discrete pose; otherwise fall back to JSONL. This keeps the rat correct
   whether or not hooks are installed.
4. **Generalize via the `Source` trait.** This is the natural moment to land the
   `Source`/`ParsedLine` refactor sketched in `docs/other-agents.md`: the JSONL
   tail and the hook listener become two sources of the same normalized signal.
   (Optional for v1, but the hook work pushes squarely in that direction —
   capture it so #1 doesn't bolt on a parallel path.)
5. **Tunables in `data/`.** Hook TTL ("how long a hook edge overrides JSONL"),
   event→pose map, and the port range live in `data/`, not in logic
   (CLAUDE.md). New event durations belong in `thresholds.json` next to the
   existing `events` block.

**Gate (live, needs eyes):** start a Claude Code session with hooks installed →
the rat reacts to turn start / done / tool use *faster* than the 1s poll, and
behaves identically (falls back to JSONL) when hooks are disabled. Unit-test the
`/state` body → signal mapping (matches `data.rs`'s test style).

---

## #2 — permission interaction ✅ BUILT (live verify owed)

**Status (built this session):** the full Allow/Deny permission bubble.
- `permission.rs`: `PermissionRegistry` (register→id+Receiver, resolve, latest,
  forget) + `Decision` (Allow/Deny/Defer). Plain Rust, unit-tested.
- Bridge `POST /permission`: parks the request, drives the bubble via a `Notifier`
  closure, blocks the connection thread on the channel up to
  `permissionTimeoutSeconds` (default 300), then replies with the decision.
- `burnrat permission` subcommand: a blocking command hook — reads the request on
  stdin, forwards to `/permission`, prints the `hookSpecificOutput` decision JSON
  on stdout (Allow/Deny) or nothing (Defer → Claude's own prompt). Exits 0 if the
  app's down (graceful fallback).
- `hookinstall`: installs a `PermissionRequest` command hook; `command_is_ours`
  widened to match it so uninstall stays clean.
- lib: `resolve_permission` Tauri command; global `Ctrl+Shift+Y/N` resolve the
  latest pending; notifier emits + shows/hides the bubble window.
- Frontend: a dedicated `permission` window (`permission.html`/`permission.ts`,
  Vite multipage, second window in tauri.conf) — tool + detail + Allow/Deny,
  Esc = defer. 72 Rust tests (8 new), clippy/fmt/npm build clean.

**On timeout / app-down: defers to Claude's own terminal prompt** (never silently
allows or blocks). **Owed: live verification** — trigger a real tool permission,
confirm Allow lets it run, Deny blocks it with a message, hotkeys work, and a
timeout falls back. ⚠️ On the next launch with the bridge connected, burnRat now
installs the `PermissionRequest` hook and will gate tool prompts. v1 shows one
bubble at a time (a second concurrent request waits out its timeout → defer).

### Original plan

Goal: a floating Allow/Deny bubble. When Claude Code requests a tool permission,
the user decides from the pet (click or global hotkey) and the decision is sent
back so the call proceeds or is blocked. Highest-value missing feature; also the
biggest trust jump.

1. **Verify the mechanism first** (Phase 0 step 4, specifically the permission
   path). clawd registers a **`type: "http"` `PermissionRequest` hook** with a
   600s timeout and replies on the held connection with:
   ```json
   { "hookSpecificOutput": { "hookEventName": "PermissionRequest",
       "decision": { "behavior": "allow" } } }
   ```
   (deny adds `"message"`; permission suggestions add `"updatedPermissions"`).
   Confirm this exact schema + that the installed Claude Code supports HTTP hooks
   before building. If HTTP hooks aren't available, the fallback is a blocking
   **command** hook whose decision is returned via exit code / stdout JSON —
   different return-path, so pin this down up front.
2. **Blocking endpoint.** `POST /permission` on the Phase 0 server. Unlike
   `/state`, **do not respond immediately** — park the request: store the
   responder + payload in a `pendingPermissions` map keyed by a request id, start
   the bubble, and only write the response when the user decides (or on timeout).
   In `axum`, hold the handler future open (await a `oneshot::Receiver` that the
   decision fulfills) — that *is* the held-open connection.
3. **Bubble UI.** A new always-on-top, focusable window (or an in-window panel
   over the pet) showing tool name + a short input fingerprint + Allow/Deny.
   This is presentation, so it lives in the frontend; the Rust side owns the
   pending-request state and the decision channel. Tie it to a `waiting`-style
   pose (we already detect `AskUserQuestion`/`ExitPlanMode`).
4. **Decision path → resolve.** Both inputs resolve the same pending entry:
   - **Buttons** → a Tauri `#[command]` (`resolve_permission(id, behavior)`) →
     fulfill the `oneshot` → server writes the JSON decision → connection closes.
   - **Global hotkeys** `Ctrl+Shift+Y` / `Ctrl+Shift+N` (register via the
     existing `tauri-plugin-global-shortcut`, the same plugin the pass-through
     `Ctrl+Shift+M` already uses) → same resolve path. Only register them while a
     permission is pending; unregister after.
5. **Timeout / disconnect = default-deny (or no-decision).** If the user doesn't
   decide before the hook timeout, or the connection drops (`res.on("close")`
   equivalent — detect via the dropped responder), resolve as **no-decision** so
   Claude Code falls back to its own terminal prompt (clawd's behavior), rather
   than silently allowing. Make the default explicit and configurable in `data/`.
6. **Trust + safety.** Loopback-only, opt-in, off by default, read-only until the
   user enables control. Surface clearly in the tray + README that burnRat can
   now approve tool calls. Consider an allowlist/denylist of tools that
   auto-resolve vs. always-ask (later).

**Gate (live):** trigger a real permission prompt in Claude Code → bubble
appears → Allow via click and via hotkey both let the tool proceed; Deny blocks
it with a message; timeout falls back to the terminal prompt. Verify the held
connection returns the exact `hookSpecificOutput` schema the installed version
expects.

---

## Sequencing summary

```
Phase 0 (server + installer + verify schema)   ← prerequisite for both
   │
   ├── #1 lifecycle ingest   (read-only; proves the rails; ships value alone)
   │
   └── #2 permission bubble  (control surface; build on proven rails; high trust)
```

- #2 has **no functional dependency** on #1's events, but **shares Phase 0** and
  is best built *after* #1 has proven the transport.
- Land Phase 0 + #1 as one milestone (the hook bridge + observational poses),
  then #2 as a second, separately-reviewed milestone given its trust surface.
