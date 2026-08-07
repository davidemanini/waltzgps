//! The map rendering widget: a `DrawingArea` plus pan/zoom controllers.

use crate::app_state::SharedState;
use crate::downloader::{Downloader, TileKey};
use crate::geo::{self, TILE_SIZE};
use crate::tile::TileId;
use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::gdk_pixbuf::{Pixbuf, PixbufLoader};
use gtk::prelude::*;
use std::rc::Rc;

/// Decode raw image bytes (PNG/JPEG/…) into a `Pixbuf`.
pub fn decode_pixbuf(bytes: &[u8]) -> Option<Pixbuf> {
    let loader = PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    loader.pixbuf()
}

/// Build the map `DrawingArea`, wiring drawing plus drag/scroll/motion input.
pub fn build(state: SharedState, downloader: Rc<Downloader>) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);

    // --- Rendering -------------------------------------------------------
    {
        let state = state.clone();
        let downloader = downloader.clone();
        area.set_draw_func(move |_area, cr, width, height| {
            let mut st = state.borrow_mut();
            let provider = st.active_provider;
            let z = st.map.zoom;
            let (w, h) = (width as f64, height as f64);
            let (tlx, tly) = st.map.top_left_world(w, h);
            let ntiles = geo::tile_count(z);

            // Range of tile columns/rows intersecting the viewport.
            let x0 = (tlx / TILE_SIZE).floor() as i64;
            let y0 = (tly / TILE_SIZE).floor() as i64;
            let x1 = ((tlx + w) / TILE_SIZE).floor() as i64;
            let y1 = ((tly + h) / TILE_SIZE).floor() as i64;

            for tx in x0..=x1 {
                for ty in y0..=y1 {
                    // No vertical wraparound; skip out-of-range rows.
                    if ty < 0 || ty >= ntiles {
                        continue;
                    }
                    let wrapped_x = tx.rem_euclid(ntiles) as u32;
                    let tile = TileId::new(z, wrapped_x, ty as u32);
                    let key = TileKey { provider, tile };
                    let dst_x = tx as f64 * TILE_SIZE - tlx;
                    let dst_y = ty as f64 * TILE_SIZE - tly;

                    if let Some(pb) = st.pixbufs.get(&key) {
                        cr.set_source_pixbuf(pb, dst_x, dst_y);
                        let _ = cr.paint();
                    } else {
                        // Placeholder while the tile loads.
                        cr.set_source_rgb(0.85, 0.85, 0.85);
                        cr.rectangle(dst_x, dst_y, TILE_SIZE, TILE_SIZE);
                        let _ = cr.fill();
                        if st.inflight.insert(key) {
                            downloader.request(key);
                        }
                    }
                }
            }
        });
    }

    // --- Panning (drag) --------------------------------------------------
    {
        let drag = gtk::GestureDrag::new();
        let state_begin = state.clone();
        drag.connect_drag_begin(move |_, _x, _y| {
            let mut st = state_begin.borrow_mut();
            st.drag_start_center = st.map.center_world();
        });
        let state_update = state.clone();
        let area_weak = area.downgrade();
        drag.connect_drag_update(move |_, offset_x, offset_y| {
            let Some(area) = area_weak.upgrade() else { return };
            let mut st = state_update.borrow_mut();
            let (sx, sy) = st.drag_start_center;
            st.map.set_center_world(sx - offset_x, sy - offset_y);
            drop(st);
            area.queue_draw();
        });
        area.add_controller(drag);
    }

    // --- Pointer tracking (for cursor-anchored zoom) ---------------------
    {
        let motion = gtk::EventControllerMotion::new();
        let state = state.clone();
        motion.connect_motion(move |_, x, y| {
            state.borrow_mut().last_pointer = (x, y);
        });
        area.add_controller(motion);
    }

    // --- Zoom (scroll wheel) ---------------------------------------------
    {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let state = state.clone();
        let area_weak = area.downgrade();
        scroll.connect_scroll(move |_, _dx, dy| {
            if dy == 0.0 {
                return gtk::glib::Propagation::Proceed;
            }
            let Some(area) = area_weak.upgrade() else {
                return gtk::glib::Propagation::Proceed;
            };
            let mut st = state.borrow_mut();
            let (w, h) = (area.width() as f64, area.height() as f64);
            let (px, py) = st.last_pointer;
            let max_zoom = st.max_zoom();
            let new_zoom = if dy < 0.0 {
                (st.map.zoom + 1).min(max_zoom)
            } else {
                st.map.zoom.saturating_sub(1)
            };
            st.map.zoom_around(px, py, w, h, new_zoom);
            drop(st);
            area.queue_draw();
            gtk::glib::Propagation::Stop
        });
        area.add_controller(scroll);
    }

    area
}
