//! Persistence of transient map state (last viewed position and zoom).
//!
//! Kept separate from [`crate::config::Config`] so the user's configured start
//! position is not overwritten: this is "where I was", not "where I start".
//! Written next to the config file as `state.toml`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Last-viewed map position, restored on the next launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapPersist {
    pub lon: f64,
    pub lat: f64,
    pub zoom: u8,
}

impl MapPersist {
    /// State-file path: `state.toml` alongside the config file.
    pub fn path(config_path: &Path) -> PathBuf {
        match config_path.parent() {
            Some(dir) => dir.join("state.toml"),
            None => PathBuf::from("state.toml"),
        }
    }

    /// Load persisted state, or `None` if absent/unreadable/corrupt.
    pub fn load(path: &Path) -> Option<MapPersist> {
        let text = std::fs::read_to_string(path).ok()?;
        toml::from_str(&text).ok()
    }

    /// Write the state file (best effort; errors are ignored).
    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        if let Ok(text) = toml::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}
