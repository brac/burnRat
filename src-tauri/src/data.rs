//! Vendored Claude Code JSONL reader.
//!
//! Claude Code appends one JSON object per line to
//! `~/.claude/projects/<encoded-cwd>/<conversation-id>.jsonl`. Assistant turns
//! carry a `message.usage` block with exact token counts. We tail these files
//! incrementally (byte cursor per file), dedup repeated usage records, and
//! accumulate recent entries for the rate/block math. No `ccusage` dependency.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use walkdir::WalkDir;

/// What the latest conversational line implies about who we're waiting on.
/// `Done` = Claude finished its turn (task complete, awaiting user). `Asking` =
/// Claude is actively asking the user something (AskUserQuestion / ExitPlanMode).
/// `Sent` = the user just sent a message and we're awaiting Claude — the rat
/// holds the idle pose longer here so we don't nap through the "dead air" before
/// Claude starts responding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awaiting {
    None,
    Done,
    Asking,
    Sent,
    /// Claude Code surfaced an API error (`isApiErrorMessage`). Treated like a
    /// question — the rat shows concern and holds rather than napping.
    Error,
}

/// Interactive tools that block on the user — treated as "asking".
const INTERACTIVE_TOOLS: [&str; 2] = ["AskUserQuestion", "ExitPlanMode"];

/// One usage record extracted from an assistant turn.
#[derive(Debug, Clone)]
pub struct UsageEntry {
    pub ts: DateTime<Utc>,
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
}

impl UsageEntry {
    /// "Real work" tokens: model input + output, excluding cache. The historical
    /// burn signal — small, lively numbers that swing with real generation.
    pub fn work(&self) -> u64 {
        self.input + self.output
    }

    /// Cache tokens (creation + read). These dwarf work (~70× in practice) and
    /// are mixed into the burn signal via `rateCacheWeight` — bigger numbers,
    /// more rat action — with thresholds calibrated to that larger scale.
    pub fn cache(&self) -> u64 {
        self.cache_create + self.cache_read
    }
}

/// How often to re-walk the projects tree for new files. Between scans we just
/// tail the already-known files, so frequent polling stays cheap.
const RESCAN_SECS: i64 = 10;

pub struct DataMonitor {
    projects_dir: PathBuf,
    /// Byte offset already consumed, per file.
    cursors: HashMap<PathBuf, u64>,
    /// Dedup keys (requestId / message id / uuid) already counted.
    seen: HashSet<String>,
    /// Recent entries, kept within the retention window for block/rate math.
    pub entries: Vec<UsageEntry>,
    /// Monotonic total of `work()` tokens ever seen (never pruned) — the rate
    /// tracker samples this so pruning can't corrupt the burn-rate signal.
    pub cumulative_work: u64,
    /// Monotonic total of `cache()` tokens ever seen — combined with
    /// `cumulative_work` (weighted by `rateCacheWeight`) into the burn signal.
    pub cumulative_cache: u64,
    /// How far back to retain entries (block window + margin).
    retention: Duration,
    /// Cached list of JSONL files, refreshed every RESCAN_SECS.
    files: Vec<PathBuf>,
    last_scan: Option<DateTime<Utc>>,
    /// The most recent conversational line seen (across all sessions): its
    /// timestamp and what kind of awaiting-user signal it is.
    latest_conv: Option<(DateTime<Utc>, Awaiting)>,
    /// Model id of the most recent assistant line (e.g. "claude-opus-4-8"),
    /// used to pick the rat's hat.
    latest_model: Option<String>,
}

impl DataMonitor {
    pub fn new(projects_dir: PathBuf, block_window_hours: i64) -> Self {
        DataMonitor {
            projects_dir,
            cursors: HashMap::new(),
            seen: HashSet::new(),
            entries: Vec::new(),
            cumulative_work: 0,
            cumulative_cache: 0,
            retention: Duration::hours(block_window_hours + 1),
            files: Vec::new(),
            last_scan: None,
            latest_conv: None,
            latest_model: None,
        }
    }

    /// The model id of the most recent assistant line, if any.
    pub fn current_model(&self) -> Option<&str> {
        self.latest_model.as_deref()
    }

    /// What Claude is awaiting from the user, based on the most recent
    /// conversational line (assistant `end_turn` → Done; an interactive tool
    /// call → Asking; anything else / a newer user line → None).
    pub fn awaiting(&self) -> Awaiting {
        self.latest_conv.map(|(_, a)| a).unwrap_or(Awaiting::None)
    }

    /// Timestamp of the most recent conversational line (user *or* assistant).
    /// Unlike the last token time, this advances when the user sends a message,
    /// so it can drive a nap clock that resets on user activity.
    pub fn last_activity(&self) -> Option<DateTime<Utc>> {
        self.latest_conv.map(|(ts, _)| ts)
    }

    /// Resolve `~/.claude/projects`, honoring `BURNRAT_PROJECTS_DIR` for tests.
    pub fn default_projects_dir() -> Option<PathBuf> {
        if let Ok(dir) = std::env::var("BURNRAT_PROJECTS_DIR") {
            return Some(PathBuf::from(dir));
        }
        dirs::home_dir().map(|h| h.join(".claude").join("projects"))
    }

    /// Read any newly-appended usage records. Returns the number added.
    pub fn poll(&mut self) -> usize {
        let now = Utc::now();
        self.refresh_files(now);

        let mut added = 0;
        // Take the list out to satisfy the borrow checker, then restore it.
        let files = std::mem::take(&mut self.files);
        for file in &files {
            added += self.read_file(file, now);
        }
        self.files = files;

        // Prune entries outside the retention window; keep memory bounded.
        let cutoff = now - self.retention;
        self.entries.retain(|e| e.ts >= cutoff);
        self.entries.sort_by_key(|e| e.ts);

        added
    }

    /// Re-walk the projects tree only every RESCAN_SECS; otherwise reuse the
    /// cached file list so per-poll cost is just tailing known files.
    fn refresh_files(&mut self, now: DateTime<Utc>) {
        let stale = self
            .last_scan
            .map(|t| now - t >= Duration::seconds(RESCAN_SECS))
            .unwrap_or(true);
        if stale {
            self.files = self.discover();
            self.last_scan = Some(now);
        }
    }

    fn discover(&self) -> Vec<PathBuf> {
        WalkDir::new(&self.projects_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
            .collect()
    }

    fn read_file(&mut self, path: &Path, now: DateTime<Utc>) -> usize {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return 0,
        };
        let len = meta.len();
        let cursor = self.cursors.get(path).copied();

        // First time we see a file that hasn't been touched within the
        // retention window: skip its history entirely (cursor to end).
        if cursor.is_none() {
            let stale = meta
                .modified()
                .ok()
                .and_then(|m| DateTime::<Utc>::from(m).into())
                .map(|mtime: DateTime<Utc>| now - mtime > self.retention)
                .unwrap_or(false);
            if stale {
                self.cursors.insert(path.to_path_buf(), len);
                return 0;
            }
        }

        let mut start = cursor.unwrap_or(0);
        // File was truncated/rotated — restart from the top.
        if start > len {
            start = 0;
        }
        if start == len {
            return 0;
        }

        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return 0,
        };
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(start)).is_err() {
            return 0;
        }

        let mut pos = start;
        let mut added = 0;
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            // A line without a trailing newline is a partial write — stop and
            // leave the cursor before it so we re-read it next poll.
            if !line.ends_with('\n') {
                break;
            }
            pos += bytes as u64;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                self.note_conversational(&v);
                if let Some(entry) = self.usage_entry(&v) {
                    self.cumulative_work += entry.work();
                    self.cumulative_cache += entry.cache();
                    self.entries.push(entry);
                    added += 1;
                }
            }
        }

        self.cursors.insert(path.to_path_buf(), pos);
        added
    }

    /// Track the latest conversational (assistant/user) line so we can tell when
    /// Claude has finished its turn and is awaiting input. Meta line types
    /// (ai-title, mode, …) are ignored; they mostly lack timestamps anyway.
    fn note_conversational(&mut self, v: &serde_json::Value) {
        let line_type = match v.get("type").and_then(|x| x.as_str()) {
            Some(k @ ("assistant" | "user")) => k,
            _ => return,
        };
        let Some(ts) = parse_ts(v) else {
            return;
        };
        // Remember the model so the frontend can pick a hat. Only assistant
        // lines carry `message.model`.
        if line_type == "assistant" {
            if let Some(m) = v
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(|x| x.as_str())
            {
                self.latest_model = Some(m.to_string());
            }
        }

        // An API error (overloaded, bad model, etc.) is flagged on the assistant
        // line. Treat it like a question: the rat shows concern and holds.
        // TODO: only assistant-typed error lines are seen here; if Claude Code
        // ever emits errors under another `type`, widen this check.
        let is_api_error = v
            .get("isApiErrorMessage")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let awaiting = if is_api_error {
            Awaiting::Error
        } else if line_type == "assistant" {
            classify_assistant(v)
        } else {
            classify_user(v)
        };
        if self.latest_conv.is_none_or(|(prev, _)| ts >= prev) {
            self.latest_conv = Some((ts, awaiting));
        }
    }

    fn usage_entry(&mut self, v: &serde_json::Value) -> Option<UsageEntry> {
        if v.get("type")?.as_str()? != "assistant" {
            return None;
        }
        v.get("message")?.get("usage")?; // require a usage block before deduping
        if let Some(k) = dedup_key(v) {
            if !self.seen.insert(k) {
                return None;
            }
        }
        usage_fields(v)
    }
}

/// Dedup key for an assistant usage line: prefer requestId, then message.id,
/// then uuid. Shared by the live tail and the historical scan.
fn dedup_key(v: &serde_json::Value) -> Option<String> {
    v.get("requestId")
        .and_then(|x| x.as_str())
        .or_else(|| {
            v.get("message")
                .and_then(|m| m.get("id"))
                .and_then(|x| x.as_str())
        })
        .or_else(|| v.get("uuid").and_then(|x| x.as_str()))
        .map(|s| s.to_string())
}

/// Extract the token counts + timestamp from an assistant usage line.
fn usage_fields(v: &serde_json::Value) -> Option<UsageEntry> {
    let usage = v.get("message")?.get("usage")?;
    let ts = parse_ts(v)?;
    let n = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    Some(UsageEntry {
        ts,
        input: n("input_tokens"),
        output: n("output_tokens"),
        cache_create: n("cache_creation_input_tokens"),
        cache_read: n("cache_read_input_tokens"),
    })
}

/// One-shot historical scan for the self-calibrating usage ceiling: the largest
/// **completed** 5-hour block (total incl. cache) across all sessions touched
/// within `max_age`. Read-only, runs once at startup off the main loop. Excludes
/// the in-progress block so a record-setting current session doesn't define its
/// own ceiling. Returns 0 if there's no usable history yet.
pub fn historical_peak_block(
    projects_dir: &Path,
    window_hours: i64,
    max_age: Duration,
    now: DateTime<Utc>,
) -> u64 {
    let cutoff = now - max_age;
    let mut seen: HashSet<String> = HashSet::new();
    let mut entries: Vec<UsageEntry> = Vec::new();

    for file in WalkDir::new(projects_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
    {
        // Skip files untouched within the history window (keeps the scan bounded).
        let recent = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .map(|mt| DateTime::<Utc>::from(mt) >= cutoff)
            .unwrap_or(false);
        if !recent {
            continue;
        }
        let Ok(f) = std::fs::File::open(&file) else {
            continue;
        };
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                continue;
            }
            if v.get("message").and_then(|m| m.get("usage")).is_none() {
                continue;
            }
            if let Some(k) = dedup_key(&v) {
                if !seen.insert(k) {
                    continue;
                }
            }
            if let Some(e) = usage_fields(&v) {
                entries.push(e);
            }
        }
    }

    entries.sort_by_key(|e| e.ts);
    crate::blocks::group(&entries, window_hours, now)
        .iter()
        .filter(|b| !b.is_active)
        .map(|b| b.total_with_cache())
        .max()
        .unwrap_or(0)
}

/// Classify an assistant line: a finished turn (`end_turn`) is `Done`; a turn
/// whose tool call is an interactive tool is `Asking`; otherwise `None` (mid-work).
fn classify_assistant(v: &serde_json::Value) -> Awaiting {
    let msg = v.get("message");
    let stop = msg
        .and_then(|m| m.get("stop_reason"))
        .and_then(|s| s.as_str());
    if stop == Some("end_turn") {
        return Awaiting::Done;
    }
    if let Some(content) = msg
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                    if INTERACTIVE_TOOLS.contains(&name) {
                        return Awaiting::Asking;
                    }
                }
            }
        }
    }
    Awaiting::None
}

/// Classify a `user` line: a genuine typed message is `Sent` (awaiting Claude);
/// a tool-result line — which Claude Code also records as `type: "user"` — is
/// mid-work, so `None`. A real message's content is either a string or an array
/// holding at least one non-`tool_result` block (text/image); a pure tool result
/// is an array of only `tool_result` blocks.
fn classify_user(v: &serde_json::Value) -> Awaiting {
    match v.get("message").and_then(|m| m.get("content")) {
        Some(serde_json::Value::String(_)) => Awaiting::Sent,
        Some(serde_json::Value::Array(blocks)) => {
            let has_message = blocks
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) != Some("tool_result"));
            if has_message {
                Awaiting::Sent
            } else {
                Awaiting::None
            }
        }
        _ => Awaiting::None,
    }
}

/// Parse a line's RFC3339 `timestamp` field into UTC.
fn parse_ts(v: &serde_json::Value) -> Option<DateTime<Utc>> {
    v.get("timestamp")
        .and_then(|x| x.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_text_message_is_sent() {
        let v = json!({"type": "user", "message": {"role": "user", "content": "hello"}});
        assert_eq!(classify_user(&v), Awaiting::Sent);
    }

    #[test]
    fn text_block_message_is_sent() {
        let v = json!({"type": "user", "message": {"role": "user",
            "content": [{"type": "text", "text": "hi"}]}});
        assert_eq!(classify_user(&v), Awaiting::Sent);
    }

    #[test]
    fn tool_result_is_not_sent() {
        // Claude Code records tool results as type:"user" — these are mid-work,
        // not a fresh message, so they must not extend the idle hold.
        let v = json!({"type": "user", "message": {"role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "x", "content": "ok"}]}});
        assert_eq!(classify_user(&v), Awaiting::None);
    }

    #[test]
    fn api_error_line_sets_error_and_model() {
        let mut m = DataMonitor::new(PathBuf::from("."), 5);
        let v = json!({
            "type": "assistant",
            "timestamp": "2026-06-17T10:00:00Z",
            "isApiErrorMessage": true,
            "message": {"role": "assistant", "model": "claude-opus-4-8",
                "content": [{"type": "text", "text": "overloaded"}]}
        });
        m.note_conversational(&v);
        assert_eq!(m.awaiting(), Awaiting::Error);
        assert_eq!(m.current_model(), Some("claude-opus-4-8"));
    }

    // Booting-into-a-state relies on these classifications: on first poll the
    // tail reads the existing file and `awaiting()` reflects its last line.
    #[test]
    fn interactive_tool_is_asking() {
        let v = json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": "AskUserQuestion"}
        ]}});
        assert_eq!(classify_assistant(&v), Awaiting::Asking);
    }

    #[test]
    fn end_turn_is_done() {
        let v = json!({"message": {"stop_reason": "end_turn"}});
        assert_eq!(classify_assistant(&v), Awaiting::Done);
    }

    #[test]
    fn work_and_cache_split_tokens() {
        let e = UsageEntry {
            ts: Utc::now(),
            input: 100,
            output: 25,
            cache_create: 4000,
            cache_read: 116_000,
        };
        assert_eq!(e.work(), 125);
        assert_eq!(e.cache(), 120_000);
    }

    #[test]
    fn mixed_content_with_text_is_sent() {
        let v = json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "x", "content": "ok"},
            {"type": "text", "text": "and also do this"}
        ]}});
        assert_eq!(classify_user(&v), Awaiting::Sent);
    }
}
