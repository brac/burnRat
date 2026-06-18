//! Phase 0 — install/remove burnRat's lifecycle hooks in `~/.claude/settings.json`.
//!
//! Claude Code reads `settings.json` for a `hooks` map: each event name holds an
//! array of matcher-groups, each group an array of hook objects. We register one
//! fire-and-forget `command` hook per lifecycle event that invokes
//! `burnrat hook <Event>` (this same binary), which POSTs to the loopback bridge.
//!
//! Everything is **idempotent and reversible**: install removes any prior burnRat
//! entries before re-adding (so re-running is safe), uninstall removes only our
//! entries (matched by the command referencing this binary), and the existing
//! file is backed up before the first write. We never touch other tools' hooks
//! or any non-`hooks` settings.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Lifecycle events we register. These are the precise edges #1 will map to
/// poses/events; Phase 0 just proves they arrive. `matcher: ""` matches all.
const HOOK_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SubagentStop",
    "Notification",
    "SessionEnd",
];

/// Per-hook timeout (seconds). The POST is fire-and-forget against a local
/// socket, so this only bounds the pathological case where the app is wedged.
const HOOK_TIMEOUT_SECS: u64 = 5;

/// `~/.claude/settings.json`.
pub fn claude_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

/// The command string for a given event: `"<exe>" hook <Event>`. The quoted exe
/// path tolerates spaces; the `burnrat` stem in the path is our removal marker.
fn hook_command(exe: &Path, event: &str) -> String {
    format!("\"{}\" hook {event}", exe.display())
}

/// Whether a hook command was written by us — it invokes a `burnrat` binary with
/// the `hook` subcommand. Used to remove/replace only our entries.
fn command_is_ours(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    lower.contains("burnrat") && lower.contains(" hook ")
}

/// Are burnRat hooks currently present in `settings.json`? Used to reconcile the
/// tray checkmark with the real on-disk state (consumed once the tray surfaces a
/// "hooks were removed externally" reconcile; part of the Phase 0 API surface).
#[allow(dead_code)]
pub fn is_installed() -> bool {
    let Some(path) = claude_settings_path() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let Some(hooks) = root.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };
    hooks.values().any(|groups| {
        groups
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
            .flatten()
            .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
            .any(command_is_ours)
    })
}

/// Install (or refresh) burnRat's hooks. `exe` is the path this binary was
/// launched from (`std::env::current_exe()`).
pub fn install(exe: &Path) -> Result<(), String> {
    let path = claude_settings_path().ok_or("could not resolve ~/.claude/settings.json")?;
    let mut root = read_settings(&path)?;
    backup(&path)?;

    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("settings.json `hooks` is not an object")?;

    for event in HOOK_EVENTS {
        let groups = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| format!("settings.json hooks.{event} is not an array"))?;

        // Drop any stale burnRat entries first so re-install is idempotent.
        strip_our_hooks(groups);

        groups.push(json!({
            "matcher": "",
            "hooks": [ {
                "type": "command",
                "command": hook_command(exe, event),
                "timeout": HOOK_TIMEOUT_SECS,
            } ],
        }));
    }

    write_settings(&path, &root)
}

/// Remove every burnRat hook, pruning emptied groups/events/`hooks`. Leaves all
/// other settings and other tools' hooks untouched.
pub fn uninstall() -> Result<(), String> {
    let path = claude_settings_path().ok_or("could not resolve ~/.claude/settings.json")?;
    if !path.exists() {
        return Ok(());
    }
    let mut root = read_settings(&path)?;

    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok(());
    };
    for groups in hooks.values_mut() {
        if let Some(arr) = groups.as_array_mut() {
            strip_our_hooks(arr);
        }
    }
    // Prune events whose group list is now empty, then `hooks` itself.
    hooks.retain(|_, groups| groups.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    let hooks_empty = hooks.is_empty();
    if hooks_empty {
        root.remove("hooks");
    }

    write_settings(&path, &root)
}

/// From a list of matcher-groups, remove our hook objects; drop any group left
/// with no hooks. A group mixing our hook with others keeps the others.
fn strip_our_hooks(groups: &mut Vec<Value>) {
    for group in groups.iter_mut() {
        if let Some(inner) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
            inner.retain(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|cmd| !command_is_ours(cmd))
                    .unwrap_or(true)
            });
        }
    }
    groups.retain(|group| {
        group
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(true)
    });
}

/// Read `settings.json` into an object. Missing file → empty object. Present but
/// invalid JSON or a non-object → error (so we never clobber a file we don't
/// understand).
fn read_settings(path: &Path) -> Result<Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => serde_json::from_str::<Value>(&text)
            .map_err(|e| format!("~/.claude/settings.json is not valid JSON: {e}"))?
            .as_object()
            .cloned()
            .ok_or_else(|| "~/.claude/settings.json is not a JSON object".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(format!("could not read ~/.claude/settings.json: {e}")),
    }
}

/// Back up the existing file to `settings.json.burnrat-bak` before the first
/// modification. No-op if the file doesn't exist yet, or if a backup already
/// exists — we want to preserve the *original* pre-burnRat settings, not
/// overwrite it on every re-install (boot re-installs to self-heal the exe path).
fn backup(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let bak = path.with_extension("json.burnrat-bak");
    if bak.exists() {
        return Ok(());
    }
    std::fs::copy(path, &bak)
        .map(|_| ())
        .map_err(|e| format!("could not back up settings.json: {e}"))
}

fn write_settings(path: &Path, root: &Map<String, Value>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("could not create ~/.claude: {e}"))?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(root.clone()))
        .map_err(|e| format!("could not serialize settings.json: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("could not write settings.json: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("/opt/burnrat/burnrat")
    }

    fn install_into(root: &mut Map<String, Value>, exe: &Path) {
        let hooks = root
            .entry("hooks")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .unwrap();
        for event in HOOK_EVENTS {
            let groups = hooks
                .entry(event.to_string())
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .unwrap();
            strip_our_hooks(groups);
            groups.push(json!({
                "matcher": "",
                "hooks": [ { "type": "command", "command": hook_command(exe, event), "timeout": HOOK_TIMEOUT_SECS } ],
            }));
        }
    }

    fn count_our_hooks(root: &Map<String, Value>) -> usize {
        root.get("hooks")
            .and_then(|h| h.as_object())
            .map(|hooks| {
                hooks
                    .values()
                    .filter_map(|g| g.as_array())
                    .flatten()
                    .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
                    .flatten()
                    .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
                    .filter(|c| command_is_ours(c))
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn command_marker_matches_only_ours() {
        assert!(command_is_ours("\"/opt/burnrat/burnrat\" hook Stop"));
        assert!(command_is_ours(
            "\"C:\\\\Users\\\\x\\\\burnRat.exe\" hook PreToolUse"
        ));
        assert!(!command_is_ours("/usr/bin/some-other-tool notify"));
        assert!(!command_is_ours("echo burnrat")); // no ` hook `
    }

    #[test]
    fn install_adds_one_hook_per_event() {
        let mut root = Map::new();
        install_into(&mut root, &exe());
        assert_eq!(count_our_hooks(&root), HOOK_EVENTS.len());
    }

    #[test]
    fn reinstall_is_idempotent() {
        let mut root = Map::new();
        install_into(&mut root, &exe());
        install_into(&mut root, &exe());
        // Still exactly one per event — no duplicates.
        assert_eq!(count_our_hooks(&root), HOOK_EVENTS.len());
    }

    #[test]
    fn uninstall_removes_ours_and_keeps_foreign_hooks() {
        // Pre-populate with a foreign hook sharing the Stop event.
        let mut root = Map::new();
        root.insert(
            "hooks".into(),
            json!({
                "Stop": [
                    { "matcher": "", "hooks": [ { "type": "command", "command": "/usr/bin/other --notify" } ] }
                ]
            }),
        );
        // Also preserve an unrelated top-level setting.
        root.insert("theme".into(), json!("dark"));

        install_into(&mut root, &exe());
        assert_eq!(count_our_hooks(&root), HOOK_EVENTS.len());

        // Mimic uninstall's strip+prune on the in-memory object.
        let hooks = root.get_mut("hooks").unwrap().as_object_mut().unwrap();
        for groups in hooks.values_mut() {
            if let Some(arr) = groups.as_array_mut() {
                strip_our_hooks(arr);
            }
        }
        hooks.retain(|_, g| g.as_array().map(|a| !a.is_empty()).unwrap_or(true));

        assert_eq!(count_our_hooks(&root), 0);
        // Foreign Stop hook survived.
        let stop = root.get("hooks").unwrap().get("Stop").unwrap();
        assert_eq!(stop.as_array().unwrap().len(), 1);
        // Unrelated setting untouched.
        assert_eq!(root.get("theme").unwrap(), &json!("dark"));
    }

    #[test]
    fn read_settings_rejects_invalid_json() {
        let dir = std::env::temp_dir().join(format!("burnrat-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(read_settings(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
