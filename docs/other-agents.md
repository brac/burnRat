# Supporting other coding agents — lift estimate

> Backlog: *"A report on the lift required to support other coding agents."*

## TL;DR

burnRat's reactive core (rate → blocks → state machine → sprite) is **already
agent-agnostic**. The only Claude-specific piece is the **ingest layer**
(`src-tauri/src/data.rs`): where the logs live and how each line is shaped.
Supporting another agent = writing one new *source adapter* that emits the same
`UsageEntry` stream (and, ideally, the same `Awaiting` / model signals).
Everything downstream is reused unchanged.

Rough lift per agent: **small–medium** (a few hundred lines + tests) when the
agent writes structured local logs with token counts; **large/blocked** when it
doesn't expose tokens locally.

## What's reusable as-is

- `rate.rs` (rolling burn rate), `blocks.rs` (5h windowing), `state.rs`
  (creature state machine), the whole frontend, all of `data/` tuning. These
  only consume `UsageEntry { ts, input, output, cache_create, cache_read }` and a
  few derived signals — none of it knows what produced the tokens.

## What's Claude-specific today

All in `data.rs`:

1. **Discovery** — hardcoded `~/.claude/projects/**/*.jsonl`
   (`default_projects_dir`).
2. **Line schema** — `type: "assistant"`, `message.usage.{input,output,
   cache_*}_tokens`, `message.model`, `stop_reason`, `isApiErrorMessage`,
   interactive-tool names (`AskUserQuestion` / `ExitPlanMode`), and the
   `type: "user"` tool-result convention.
3. **Dedup keys** — `requestId` / `message.id` / `uuid`.

## Proposed shape

Introduce a `Source` trait that yields `UsageEntry`s plus the optional
`Awaiting`/model signals, and make `DataMonitor` generic over (or fan in across)
multiple sources:

```
trait Source {
    fn discover(&self) -> Vec<PathBuf>;          // or a stream
    fn parse_line(&self, v: &Value) -> Option<ParsedLine>;  // usage + awaiting + model
}
```

`ParsedLine` carries the already-normalized fields so the rest of the monitor
(cursors, dedup, retention, blocks) stays source-independent. Run multiple
sources concurrently and merge their entries by timestamp.

## Per-agent notes (survey)

| Agent | Local token logs? | Lift |
|---|---|---|
| **Claude Code** | Yes (current) | — |
| **Gemini CLI** | Local session logs; token usage available | Small–medium: new adapter, map fields. |
| **Cursor** | Usage is mostly server-side; local SQLite has limited token data | Medium–large; may need approximation. |
| **Cline / Roo (VS Code)** | Per-task logs with token + cost in the extension storage | Medium: locate storage, parse task files. |
| **Aider** | `.aider.chat.history.md` + analytics; tokens in analytics events | Medium. |
| **OpenAI Codex CLI / others** | Varies; often no local token ledger | Large/blocked without a usable local signal. |

## Risks / unknowns

- **No standard.** Each agent's schema and log location differ and drift across
  versions — adapters need version tolerance (like our lenient JSON probing).
- **Cache semantics differ.** Our burn signal weighting (`rateCacheWeight`) and
  the limit ceiling assume Claude-style cache accounting; other agents may not
  report cache at all, so thresholds would need per-source calibration.
- **Multiple agents at once.** Merging streams is straightforward, but "which
  model/hat" and "which window" get ambiguous — probably scope v1 to one active
  source at a time.

## Recommendation

Land the `Source` trait refactor first (pure internal reshuffle of `data.rs`,
no behavior change), ship a second adapter (Gemini CLI looks lowest-friction) as
the proof, then add others on demand. Keep all field maps and log paths in
`data/` so new agents are mostly config.
