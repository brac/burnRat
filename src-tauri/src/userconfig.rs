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
}

impl UserConfig {
    pub fn load(path: &PathBuf, default_plan: String, default_opacity: f64) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(UserConfig {
                plan: default_plan,
                opacity: default_opacity,
            })
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
