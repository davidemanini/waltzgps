//! Persistence of transient map state (last viewed position and zoom).
//!
//! Kept separate from [`crate::config::Config`] so the user's configured start
//! position is not overwritten: this is "where I was", not "where I start".
//! Stored in the XDG state directory (`$XDG_STATE_HOME/waltzgps/state.toml`),
//! which the spec designates for exactly this kind of persistent-but-transient
//! data — not `$XDG_CONFIG_HOME`.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::path::PathBuf;

/// Last-viewed map position, restored on the next launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapPersist {
    pub lon: f64,
    pub lat: f64,
    pub zoom: u8,
    pub provider: String,
}

impl MapPersist {
    /// State-file path: `$XDG_STATE_HOME/waltzgps/state.toml`.
    pub fn path() -> PathBuf {
        crate::config::state_home().join("waltzgps").join("state.toml")
    }

    /// Load persisted state, or `None` if absent/unreadable/corrupt.
    pub fn load(path: &Path) -> Option<MapPersist> {
        let text = std::fs::read_to_string(path).ok()?;
        toml::from_str(&text).ok().map(|s: Self| s.validate())
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

    fn validate(mut self) -> Self {
	self.lat = self.lat.clamp(-crate::geo::MAX_LAT, crate::geo::MAX_LAT);
	self.lon = self.lon.clamp(-180., 180.);
	self.zoom = self.zoom.min(22);
	self
    }
}

impl Default for MapPersist {
    fn default() -> Self {
	MapPersist {
	    // Wien
	    lon: 16.3686,
	    lat: 48.2068,
	    zoom: 12,
	    provider: "OpenStreetMap".into(),
	}
    }
}
