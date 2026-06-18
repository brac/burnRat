//! Character system — runtime-loaded folders of art behind a manifest.
//!
//! A character is a *folder* (`characters/<id>/`) holding a `character.json`
//! manifest plus ~10 PNGs. The engine resolves three layers (base pose,
//! quota-proximity overlay, transient event) to names; the active character
//! supplies the matching asset. No code path is character-specific.
//!
//! Folders are discovered at startup from up to three dirs, scanned in order so
//! later dirs override earlier ones by `id`:
//!   1. dev repo `characters/` (via `CARGO_MANIFEST_DIR/../characters`)
//!   2. bundled defaults — `resource_dir()/characters` (shipped via
//!      `tauri.conf.json` `bundle.resources`)
//!   3. user drop-in — `app_data_dir()/characters` (add a folder + restart)
//!
//! Each character is validated against a fixed contract; an invalid character is
//! logged and excluded from the valid set — never silently rendered blank.
//!
//! Assets reach the frontend as `data:image/png;base64,…` URLs (resolved on
//! demand by the `active_character` command). ~10 small PNGs make the encoding
//! cost negligible, and it sidesteps the Tauri asset-protocol scope + CSP
//! entirely. If art ever grows large, swap this one resolver for `convertFileSrc`
//! without touching the frontend contract.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

/// The fixed base-state contract every character must satisfy (Layer 1).
const REQUIRED_STATES: [&str; 7] = [
    "sleeping", "thinking", "working", "frantic", "onfire", "spent", "done",
];
/// The required Layer-2 modifier key.
const REQUIRED_MODIFIER: &str = "quotaProximity";
/// The required Layer-3 event keys. `flinch` is optional polish.
const REQUIRED_EVENTS: [&str; 2] = ["refreshed", "error"];

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct Anchor {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasBox {
    pub width: f64,
    pub height: f64,
}

/// One manifest entry — a single representative `asset`, plus optional per-entry
/// `anchor`/`canvas` overrides and an optional ordered `frames` ping-pong loop.
#[derive(Debug, Clone, Deserialize)]
pub struct AssetEntry {
    pub asset: String,
    pub anchor: Option<Anchor>,
    pub canvas: Option<CanvasBox>,
    pub frames: Option<Vec<String>>,
}

impl AssetEntry {
    /// The ordered files for this entry: the explicit `frames` loop if present,
    /// otherwise just the single `asset`.
    fn files(&self) -> Vec<&str> {
        match &self.frames {
            Some(f) if !f.is_empty() => f.iter().map(String::as_str).collect(),
            _ => vec![self.asset.as_str()],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterManifest {
    pub id: String,
    pub name: String,
    #[serde(default = "default_renderer")]
    pub renderer: String,
    pub canvas: CanvasBox,
    pub anchor: Anchor,
    #[serde(default)]
    pub states: HashMap<String, AssetEntry>,
    #[serde(default)]
    pub modifiers: HashMap<String, AssetEntry>,
    #[serde(default)]
    pub events: HashMap<String, AssetEntry>,
}

fn default_renderer() -> String {
    "sprite".to_string()
}

/// A validated character: its manifest plus the folder its assets live in.
#[derive(Debug, Clone)]
pub struct LoadedCharacter {
    pub manifest: CharacterManifest,
    pub base_dir: PathBuf,
}

/// A single resolved asset, frontend-ready: data-URL frames + placement.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedAsset {
    /// Ordered ping-pong frames as `data:` URLs (one entry = static).
    pub urls: Vec<String>,
    pub anchor: Anchor,
    pub canvas: CanvasBox,
}

/// The active character resolved for the frontend. `assets` is keyed by base
/// state names + `"quotaProximity"` + event names.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedCharacter {
    pub id: String,
    pub name: String,
    pub renderer: String,
    pub canvas: CanvasBox,
    pub anchor: Anchor,
    pub assets: HashMap<String, ResolvedAsset>,
}

impl LoadedCharacter {
    /// Read + base64-encode every asset into a frontend-ready struct. Called
    /// once at startup and again on each character switch — cheap for ~10 small
    /// PNGs. An unreadable file drops that frame; an entry with no readable
    /// frames is omitted (the view falls back to the base pose).
    pub fn resolve(&self) -> ResolvedCharacter {
        let mut assets: HashMap<String, ResolvedAsset> = HashMap::new();
        let groups = [
            &self.manifest.states,
            &self.manifest.modifiers,
            &self.manifest.events,
        ];
        for group in groups {
            for (name, entry) in group {
                let urls: Vec<String> = entry
                    .files()
                    .iter()
                    .filter_map(|f| self.encode(f))
                    .collect();
                if urls.is_empty() {
                    continue;
                }
                assets.insert(
                    name.clone(),
                    ResolvedAsset {
                        urls,
                        anchor: entry.anchor.unwrap_or(self.manifest.anchor),
                        canvas: entry.canvas.unwrap_or(self.manifest.canvas),
                    },
                );
            }
        }
        ResolvedCharacter {
            id: self.manifest.id.clone(),
            name: self.manifest.name.clone(),
            renderer: self.manifest.renderer.clone(),
            canvas: self.manifest.canvas,
            anchor: self.manifest.anchor,
            assets,
        }
    }

    fn encode(&self, file: &str) -> Option<String> {
        let bytes = std::fs::read(self.base_dir.join(file)).ok()?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        Some(format!("data:image/png;base64,{b64}"))
    }
}

/// Parse + validate one character folder. Returns the loaded character or an
/// error string describing the first contract violation.
fn load_one(dir: &Path) -> Result<LoadedCharacter, String> {
    let manifest_path = dir.join("character.json");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let manifest: CharacterManifest =
        serde_json::from_str(&text).map_err(|e| format!("invalid character.json: {e}"))?;
    validate(&manifest, dir)?;
    Ok(LoadedCharacter {
        manifest,
        base_dir: dir.to_path_buf(),
    })
}

/// Enforce the fixed contract: sprite renderer, all 7 base states, the quota
/// modifier, both required events, and every referenced asset present on disk.
fn validate(manifest: &CharacterManifest, base_dir: &Path) -> Result<(), String> {
    if manifest.renderer != "sprite" {
        return Err(format!(
            "renderer '{}' unsupported (only 'sprite')",
            manifest.renderer
        ));
    }
    for s in REQUIRED_STATES {
        let entry = manifest
            .states
            .get(s)
            .ok_or_else(|| format!("missing required state '{s}'"))?;
        check_files(base_dir, entry)?;
    }
    let modifier = manifest
        .modifiers
        .get(REQUIRED_MODIFIER)
        .ok_or_else(|| format!("missing required modifier '{REQUIRED_MODIFIER}'"))?;
    check_files(base_dir, modifier)?;
    for e in REQUIRED_EVENTS {
        let entry = manifest
            .events
            .get(e)
            .ok_or_else(|| format!("missing required event '{e}'"))?;
        check_files(base_dir, entry)?;
    }
    Ok(())
}

/// Every file an entry references (its `asset` and any `frames`) must exist.
fn check_files(base_dir: &Path, entry: &AssetEntry) -> Result<(), String> {
    for f in entry.files() {
        if !base_dir.join(f).exists() {
            return Err(format!("asset '{f}' not found in {}", base_dir.display()));
        }
    }
    Ok(())
}

/// Scan one characters dir: load every subfolder that holds a `character.json`,
/// skipping (with a log line) any that fail validation. Missing dirs are silent.
fn scan_dir(dir: &Path) -> Vec<LoadedCharacter> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join("character.json").exists() {
            continue;
        }
        match load_one(&path) {
            Ok(c) => out.push(c),
            Err(e) => eprintln!("burnRat: skipping character {}: {e}", path.display()),
        }
    }
    out
}

/// Discover every valid character across `dirs`, scanned in order. A later dir
/// with the same `id` overrides an earlier one (keeping its original slot so the
/// tray order is stable). Sorted by id for a deterministic menu otherwise.
pub fn discover(dirs: &[PathBuf]) -> Vec<LoadedCharacter> {
    let mut out: Vec<LoadedCharacter> = Vec::new();
    for dir in dirs {
        for c in scan_dir(dir) {
            if let Some(slot) = out.iter_mut().find(|x| x.manifest.id == c.manifest.id) {
                *slot = c;
            } else {
                out.push(c);
            }
        }
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    out
}

/// The repo `characters/` dir as known at compile time (dev only; mirrors
/// `config::dev_data_dir`).
pub fn dev_characters_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("characters")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a throwaway character folder under a unique temp dir. `omit` drops a
    /// required asset file; `renderer` overrides the manifest renderer.
    fn make_character(tag: &str, renderer: &str, omit: Option<&str>) -> PathBuf {
        let root = std::env::temp_dir().join(format!("burnrat-char-test-{tag}"));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("rat");
        fs::create_dir_all(&dir).unwrap();

        // Minimal 1x1 PNG bytes are unnecessary — existence is all the loader
        // checks; resolve() just base64s whatever is there.
        let pngs = [
            "sleeping.png",
            "thinking.png",
            "working.png",
            "frantic.png",
            "onfire.png",
            "spent.png",
            "done.png",
            "nearlimit.png",
            "refreshed.png",
            "error.png",
            "surprised.png",
        ];
        for p in pngs {
            if Some(p) != omit {
                fs::write(dir.join(p), b"PNG").unwrap();
            }
        }

        let manifest = format!(
            r#"{{
              "id": "rat", "name": "Rat", "renderer": "{renderer}",
              "canvas": {{ "width": 300, "height": 300 }},
              "anchor": {{ "x": 0.5, "y": 0.5 }},
              "states": {{
                "sleeping": {{ "asset": "sleeping.png" }},
                "thinking": {{ "asset": "thinking.png" }},
                "working":  {{ "asset": "working.png" }},
                "frantic":  {{ "asset": "frantic.png" }},
                "onfire":   {{ "asset": "onfire.png" }},
                "spent":    {{ "asset": "spent.png" }},
                "done":     {{ "asset": "done.png" }}
              }},
              "modifiers": {{ "quotaProximity": {{ "asset": "nearlimit.png" }} }},
              "events": {{
                "refreshed": {{ "asset": "refreshed.png" }},
                "error":     {{ "asset": "error.png" }},
                "flinch":    {{ "asset": "surprised.png" }}
              }}
            }}"#
        );
        fs::write(dir.join("character.json"), manifest).unwrap();
        dir
    }

    #[test]
    fn valid_character_loads_and_resolves() {
        let dir = make_character("valid", "sprite", None);
        let c = load_one(&dir).expect("should load");
        assert_eq!(c.manifest.id, "rat");
        let resolved = c.resolve();
        // 7 base states + quotaProximity + 3 events.
        assert!(resolved.assets.contains_key("sleeping"));
        assert!(resolved.assets.contains_key("quotaProximity"));
        assert!(resolved.assets.contains_key("flinch"));
        assert_eq!(resolved.assets.len(), 11);
        // Each asset is a base64 data URL.
        assert!(resolved.assets["sleeping"].urls[0].starts_with("data:image/png;base64,"));
    }

    #[test]
    fn missing_asset_is_excluded() {
        let dir = make_character("missing-asset", "sprite", Some("onfire.png"));
        assert!(load_one(&dir).is_err());
    }

    #[test]
    fn bad_renderer_is_excluded() {
        let dir = make_character("bad-renderer", "mesh", None);
        let err = load_one(&dir).unwrap_err();
        assert!(err.contains("renderer"));
    }

    #[test]
    fn missing_required_state_is_excluded() {
        // A manifest without the `done` state must fail validation.
        let root = std::env::temp_dir().join("burnrat-char-test-missing-state");
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("rat");
        fs::create_dir_all(&dir).unwrap();
        for p in [
            "sleeping.png",
            "thinking.png",
            "nearlimit.png",
            "refreshed.png",
            "error.png",
        ] {
            fs::write(dir.join(p), b"PNG").unwrap();
        }
        let manifest = r#"{
          "id": "rat", "name": "Rat", "renderer": "sprite",
          "canvas": { "width": 300, "height": 300 },
          "anchor": { "x": 0.5, "y": 0.5 },
          "states": { "sleeping": { "asset": "sleeping.png" }, "thinking": { "asset": "thinking.png" } },
          "modifiers": { "quotaProximity": { "asset": "nearlimit.png" } },
          "events": { "refreshed": { "asset": "refreshed.png" }, "error": { "asset": "error.png" } }
        }"#;
        fs::write(dir.join("character.json"), manifest).unwrap();
        let err = load_one(&dir).unwrap_err();
        assert!(err.contains("state"));
    }

    /// The shipped rat folder must satisfy the contract and resolve cleanly —
    /// guards against the real manifest/files drifting out of spec.
    #[test]
    fn shipped_rat_character_is_valid() {
        let rat = dev_characters_dir().join("rat");
        let c = load_one(&rat).expect("the bundled rat must be a valid character");
        assert_eq!(c.manifest.id, "rat");
        let resolved = c.resolve();
        for s in REQUIRED_STATES {
            assert!(
                resolved.assets.contains_key(s),
                "missing resolved state {s}"
            );
        }
        assert!(resolved.assets.contains_key("quotaProximity"));
        assert!(resolved.assets["sleeping"].urls[0].starts_with("data:image/png;base64,"));
        // The multi-frame sleeping loop carries both declared frames.
        assert_eq!(resolved.assets["sleeping"].urls.len(), 2);
    }

    #[test]
    fn discover_dedupes_by_id_later_dir_wins() {
        let a = make_character("dedup-a", "sprite", None);
        let b = make_character("dedup-b", "sprite", None);
        // Both expose id "rat"; the second dir should override the first.
        let dirs = vec![
            a.parent().unwrap().to_path_buf(),
            b.parent().unwrap().to_path_buf(),
        ];
        let found = discover(&dirs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].base_dir, b);
    }
}
