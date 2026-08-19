//! Web-Mercator ("slippy map") coordinate math.
//!
//! The world at zoom `z` is a square of `256 * 2^z` pixels. We convert between
//! geographic coordinates (lon/lat in degrees) and *world pixels*, and keep the
//! visible viewport described by a centre and a zoom level.

use std::f64::consts::PI;

/// Side length of a single tile in pixels.
pub const TILE_SIZE: f64 = 256.0;

/// Maximum latitude representable in Web Mercator (~85.0511°).
pub const MAX_LAT: f64 = 85.051_128_78;

/// World size (in pixels) at the given integer zoom.
pub fn world_size(zoom: u8) -> f64 {
    TILE_SIZE * (1u64 << zoom) as f64
}

/// Number of tiles along one axis at the given zoom (`2^zoom`).
pub fn tile_count(zoom: u8) -> i64 {
    1i64 << zoom
}

/// Convert lon/lat (degrees) to world-pixel coordinates at `zoom`.
pub fn lonlat_to_world_px(lon: f64, lat: f64, zoom: u8) -> (f64, f64) {
    let n = world_size(zoom);
    let lat = lat.clamp(-MAX_LAT, MAX_LAT);
    let x = (lon + 180.0) / 360.0 * n;
    let lat_rad = lat.to_radians();
    let y = (1.0 - lat_rad.tan().asinh() / PI) / 2.0 * n;
    (x, y)
}

/// Convert world-pixel coordinates at `zoom` back to lon/lat (degrees).
pub fn world_px_to_lonlat(x: f64, y: f64, zoom: u8) -> (f64, f64) {
    let n = world_size(zoom);
    let lon = x / n * 360.0 - 180.0;
    let lat_rad = (PI * (1.0 - 2.0 * y / n)).sinh().atan();
    (lon, lat_rad.to_degrees())
}

/// Convert lon/lat to fractional tile coordinates at `zoom`.
pub fn _lonlat_to_tile(lon: f64, lat: f64, zoom: u8) -> (f64, f64) {
    let (x, y) = lonlat_to_world_px(lon, lat, zoom);
    (x / TILE_SIZE, y / TILE_SIZE)
}

/// The viewport: a geographic centre and an integer zoom level.
#[derive(Debug, Clone, Copy)]
pub struct MapState {
    pub center_lon: f64,
    pub center_lat: f64,
    pub zoom: u8,
}

impl MapState {
    /// Centre expressed in world pixels at the current zoom.
    pub fn center_world(&self) -> (f64, f64) {
        lonlat_to_world_px(self.center_lon, self.center_lat, self.zoom)
    }

    /// Top-left corner of a `w`×`h` viewport, in world pixels.
    pub fn top_left_world(&self, w: f64, h: f64) -> (f64, f64) {
        let (cx, cy) = self.center_world();
        (cx - w / 2.0, cy - h / 2.0)
    }

    /// Move the centre to the given world-pixel position, updating lon/lat.
    pub fn set_center_world(&mut self, wx: f64, wy: f64) {
        let (lon, lat) = world_px_to_lonlat(wx, wy, self.zoom);
        self.center_lon = lon;
        self.center_lat = lat.clamp(-MAX_LAT, MAX_LAT);
    }

    /// Geographic coordinate under a screen pixel within a `w`×`h` viewport.
    pub fn screen_to_lonlat(&self, sx: f64, sy: f64, w: f64, h: f64) -> (f64, f64) {
        let (tlx, tly) = self.top_left_world(w, h);
        world_px_to_lonlat(tlx + sx, tly + sy, self.zoom)
    }

    /// Pan the viewport by a pixel delta (positive x = content moves left).
    pub fn pan_px(&mut self, dx: f64, dy: f64) {
        let (cx, cy) = self.center_world();
        self.set_center_world(cx + dx, cy + dy);
    }

    /// Change zoom to `new_zoom` while keeping the geographic point currently
    /// under screen pixel `(sx, sy)` fixed on screen.
    pub fn zoom_around(&mut self, sx: f64, sy: f64, w: f64, h: f64, new_zoom: u8) {
        if new_zoom == self.zoom {
            return;
        }
        let (lon, lat) = self.screen_to_lonlat(sx, sy, w, h);
        self.zoom = new_zoom;
        let (tx, ty) = lonlat_to_world_px(lon, lat, new_zoom);
        // Place that world point back under (sx, sy): centre = point - offset + half-viewport.
        self.set_center_world(tx - sx + w / 2.0, ty - sy + h / 2.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_maps_to_center_tile_at_z1() {
        // lon/lat (0,0) at zoom 1 sits at tile (1,1).
        let (tx, ty) = _lonlat_to_tile(0.0, 0.0, 1);
        assert!((tx - 1.0).abs() < 1e-9, "tx={tx}");
        assert!((ty - 1.0).abs() < 1e-9, "ty={ty}");
    }

    #[test]
    fn known_city_tile() {
        // London (-0.1276, 51.5072) at zoom 12 -> tile (2046, 1362), per the
        // standard slippy-map formula floor((lon+180)/360 * 2^z).
        let (tx, ty) = _lonlat_to_tile(-0.1276, 51.5072, 12);
        assert_eq!(tx.floor() as i64, 2046);
        assert_eq!(ty.floor() as i64, 1362);
    }

    #[test]
    fn roundtrip_lonlat_world() {
        for &(lon, lat, z) in &[(2.3522, 48.8566, 12u8), (-73.9857, 40.7484, 15), (0.0, 0.0, 3)] {
            let (wx, wy) = lonlat_to_world_px(lon, lat, z);
            let (lon2, lat2) = world_px_to_lonlat(wx, wy, z);
            assert!((lon - lon2).abs() < 1e-6, "lon {lon} != {lon2}");
            assert!((lat - lat2).abs() < 1e-6, "lat {lat} != {lat2}");
        }
    }

    #[test]
    fn zoom_around_keeps_point_fixed() {
        let mut s = MapState { center_lon: 2.3522, center_lat: 48.8566, zoom: 12 };
        let (w, h) = (800.0, 600.0);
        let (sx, sy) = (300.0, 200.0);
        let before = s.screen_to_lonlat(sx, sy, w, h);
        s.zoom_around(sx, sy, w, h, 14);
        let after = s.screen_to_lonlat(sx, sy, w, h);
        assert!((before.0 - after.0).abs() < 1e-4, "lon drift");
        assert!((before.1 - after.1).abs() < 1e-4, "lat drift");
    }
}
