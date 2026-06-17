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
    /// Tokens that represent real work (drives the creature state).
    /// Cache reads/writes are excluded — they dwarf real work and would peg the
    /// rat permanently on fire.
    pub fn work(&self) -> u64 {
        self.input + self.output
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
    /// How far back to retain entries (block window + margin).
    retention: Duration,
    /// Cached list of JSONL files, refreshed every RESCAN_SECS.
    files: Vec<PathBuf>,
    last_scan: Option<DateTime<Utc>>,
}

impl DataMonitor {
    pub fn new(projects_dir: PathBuf, block_window_hours: i64) -> Self {
        DataMonitor {
            projects_dir,
            cursors: HashMap::new(),
            seen: HashSet::new(),
            entries: Vec::new(),
            cumulative_work: 0,
            retention: Duration::hours(block_window_hours + 1),
            files: Vec::new(),
            last_scan: None,
        }
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
            if let Some(entry) = self.parse_line(&line) {
                self.cumulative_work += entry.work();
                self.entries.push(entry);
                added += 1;
            }
        }

        self.cursors.insert(path.to_path_buf(), pos);
        added
    }

    fn parse_line(&mut self, line: &str) -> Option<UsageEntry> {
        let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        if v.get("type")?.as_str()? != "assistant" {
            return None;
        }
        let usage = v.get("message")?.get("usage")?;

        // Dedup: prefer requestId, then message.id, then uuid.
        let key = v
            .get("requestId")
            .and_then(|x| x.as_str())
            .or_else(|| v.get("message").and_then(|m| m.get("id")).and_then(|x| x.as_str()))
            .or_else(|| v.get("uuid").and_then(|x| x.as_str()))
            .map(|s| s.to_string());
        if let Some(k) = key {
            if !self.seen.insert(k) {
                return None;
            }
        }

        let ts = v
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))?;

        let n = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        Some(UsageEntry {
            ts,
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_create: n("cache_creation_input_tokens"),
            cache_read: n("cache_read_input_tokens"),
        })
    }
}
