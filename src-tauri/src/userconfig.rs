//! User overrides (plan + opacity) persisted to the OS app-config dir.
//!
//! Window position is persisted separately by `tauri-plugin-window-state`.
//! Defaults come from `data/settings.default.json`; this file only stores the
//! user's runtime changes from the tray menu.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserConfig {
    pub plan: String,
    pub opacity: f64,
    /// Active character id (selected from the tray "Character" submenu).
    /// Defaults to the value in `settings.default.json`.
    #[serde(default)]
    pub character: String,
    /// Whether the user has opted into the loopback hook bridge (tray "Connect
    /// to Claude Code"). Persisted so the server auto-starts on the next launch.
    /// Defaults to the value in `settings.default.json`.
    #[serde(default)]
    pub local_server_enabled: bool,
}

impl UserConfig {
    pub fn load(
        path: &PathBuf,
        default_plan: String,
        default_opacity: f64,
        default_character: String,
        default_local_server_enabled: bool,
    ) -> Self {
        let mut cfg: UserConfig = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| UserConfig {
                plan: default_plan,
                opacity: default_opacity,
                character: default_character.clone(),
                local_server_enabled: default_local_server_enabled,
            });
        // An older settings.json (pre-character) deserializes with an empty
        // character via `#[serde(default)]` — backfill the configured default.
        if cfg.character.is_empty() {
            cfg.character = default_character;
        }
        cfg
    }

    pub fn save(&self, path: &PathBuf) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, t);
        }
    }
}
