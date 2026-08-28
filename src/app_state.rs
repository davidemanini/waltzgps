//! Shared, main-thread-only application state.

use crate::config::Config;
use crate::downloader::TileKey;
use crate::geo::MapState;
use crate::persist::MapPersist;
use gtk::gdk::Texture;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

/// Soft cap on decoded tiles kept in memory; the disk cache is the real store.
const TEXTURE_CAP: usize = 2048;

pub type SharedState = Rc<RefCell<AppState>>;

pub struct AppState {
    pub config: Config,
    /// Path the config was loaded from; edits are written back here.
    pub config_path: PathBuf,
    pub map: MapState,
    pub active_provider: usize,
    /// Decoded tiles, uploaded once as GPU textures, ready to paint.
    pub textures: HashMap<TileKey, Texture>,
    /// Tiles currently requested from the downloader (avoids duplicate fetches).
    pub inflight: HashSet<TileKey>,
    /// Last known pointer position over the map (for cursor-anchored zoom).
    pub last_pointer: (f64, f64),
    /// Map centre (world px) captured at the start of a drag gesture.
    pub drag_start_center: (f64, f64),
    /// Geographic point of the most recent right-click.
    pub last_click_lonlat: (f64, f64),
}

impl AppState {
    pub fn new(config: Config, persist: MapPersist, config_path: PathBuf) -> SharedState {
        let map = MapState{
	    center_lon: persist.lon,
	    center_lat: persist.lat,
	    zoom: persist.zoom,
	    zoom_frac: 0.0,
	};
        let active_provider = config.provider_index(persist.provider);
        Rc::new(RefCell::new(AppState {
            config,
            config_path,
            map,
            active_provider,
            textures: HashMap::new(),
            inflight: HashSet::new(),
            last_pointer: (0.0, 0.0),
            drag_start_center: (0.0, 0.0),
            last_click_lonlat: (0.0, 0.0),
        }))
    }

    /// Persist the current config back to its file.
    pub fn save_config(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.config.save(&self.config_path)
    }

    /// Path of the map-state file (last position/zoom).
    pub fn state_path(&self) -> std::path::PathBuf {
        crate::persist::MapPersist::path()
    }

    /// Snapshot the current map view for persistence.
    pub fn map_persist(&self) -> crate::persist::MapPersist {
        crate::persist::MapPersist {
            lon: self.map.center_lon,
            lat: self.map.center_lat,
            zoom: self.map.zoom,
	    provider: self.config.providers[self.active_provider].name.clone(),
        }
    }

    /// Best-effort write of the current map position to the state file.
    pub fn save_map_state(&self) {
        self.map_persist().save(&self.state_path());
    }

    /// Max zoom permitted by the active provider.
    pub fn max_zoom(&self) -> u8 {
        self.config
            .providers
            .get(self.active_provider)
            .map(|p| p.max_zoom)
            .unwrap_or(19)
    }

    /// Insert a decoded tile, discarding the whole in-memory set if it grows
    /// past the soft cap (tiles reload quickly from disk).
    pub fn insert_texture(&mut self, key: TileKey, texture: Texture) {
        if self.textures.len() >= TEXTURE_CAP {
            self.textures.clear();
        }
        self.textures.insert(key, texture);
    }
}
