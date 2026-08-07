//! Shared, main-thread-only application state.

use crate::config::Config;
use crate::downloader::TileKey;
use crate::geo::MapState;
use gtk::gdk_pixbuf::Pixbuf;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Soft cap on decoded tiles kept in memory; the disk cache is the real store.
const PIXBUF_CAP: usize = 2048;

pub type SharedState = Rc<RefCell<AppState>>;

pub struct AppState {
    pub config: Config,
    pub map: MapState,
    pub active_provider: usize,
    /// Decoded tiles ready to paint.
    pub pixbufs: HashMap<TileKey, Pixbuf>,
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
    pub fn new(config: Config) -> SharedState {
        let map = MapState::new(
            config.general.start_lon,
            config.general.start_lat,
            config.general.start_zoom,
        );
        let active_provider = config.default_provider_index();
        Rc::new(RefCell::new(AppState {
            config,
            map,
            active_provider,
            pixbufs: HashMap::new(),
            inflight: HashSet::new(),
            last_pointer: (0.0, 0.0),
            drag_start_center: (0.0, 0.0),
            last_click_lonlat: (0.0, 0.0),
        }))
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
    pub fn insert_pixbuf(&mut self, key: TileKey, pb: Pixbuf) {
        if self.pixbufs.len() >= PIXBUF_CAP {
            self.pixbufs.clear();
        }
        self.pixbufs.insert(key, pb);
    }
}
