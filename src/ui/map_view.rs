//! The map rendering widget: a `DrawingArea` plus pan/zoom controllers.

use crate::app_state::SharedState;
use crate::downloader::{Downloader, TileKey};
use crate::geo::{self, TILE_SIZE};
use crate::tile::TileId;
use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::gdk_pixbuf::{Pixbuf, PixbufLoader};
use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// Update the download-queue indicator: show the count while tiles are pending,
/// hide it entirely when the queue is empty.
pub fn set_queue_label(label: &gtk::Label, pending: usize) {
    if pending == 0 {
        label.set_visible(false);
    } else {
        label.set_text(&format!("\u{2b07} {pending}"));
        label.set_visible(true);
    }
}

/// Decode raw image bytes (PNG/JPEG/…) into a `Pixbuf`.
pub fn decode_pixbuf(bytes: &[u8]) -> Option<Pixbuf> {
    let loader = PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    loader.pixbuf()
}

/// Build the map `DrawingArea`, wiring drawing plus drag/scroll/motion input.
/// `zoom_label` and `queue_label` are HUD widgets refreshed on every redraw.
pub fn build(
    state: SharedState,
    downloader: Rc<Downloader>,
    zoom_label: gtk::Label,
    queue_label: gtk::Label,
) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);

    // --- Rendering -------------------------------------------------------
    {
        let state = state.clone();
        let downloader = downloader.clone();
        // Last (zoom, pending) shown on the HUD, to skip redundant updates.
        let last_hud = Rc::new(Cell::new((u8::MAX, usize::MAX)));
        area.set_draw_func(move |_area, cr, width, height| {
            // Hard edges: tiles are drawn 1:1 at integer coordinates, so any
            // antialiasing at rectangle borders only produces seams.
            cr.set_antialias(gtk::cairo::Antialias::None);

            let mut st = state.borrow_mut();
            let provider = st.active_provider;
            let z = st.map.zoom;
            let (w, h) = (width as f64, height as f64);
            let (tlx, tly) = st.map.top_left_world(w, h);
            // Snap the viewport origin to whole pixels. Every tile then lands on
            // an integer coordinate with exact TILE_SIZE spacing — this removes
            // both the inter-tile seams and the resample blur that appear when
            // a pixbuf is painted at a fractional offset.
            let origin_x = tlx.round();
            let origin_y = tly.round();
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
                    let dst_x = tx as f64 * TILE_SIZE - origin_x;
                    let dst_y = ty as f64 * TILE_SIZE - origin_y;

                    if let Some(pb) = st.pixbufs.get(&key) {
                        cr.set_source_pixbuf(pb, dst_x, dst_y);
                        // Nearest-neighbour: no interpolation at the 1:1 mapping.
                        cr.source().set_filter(gtk::cairo::Filter::Nearest);
                        // Fill only this tile's rectangle rather than paint()ing
                        // the whole surface with the (edge-transparent) pattern.
                        cr.rectangle(dst_x, dst_y, TILE_SIZE, TILE_SIZE);
                        let _ = cr.fill();
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

            // Refresh the HUD to match what we just drew, but do it *after* the
            // snapshot pass. Setting the zoom label (which lives inside the nav
            // box) here would resize the nav mid-draw and make the overlaid
            // buttons flicker/vanish; deferring to idle avoids that.
            let pending = st.inflight.len();
            if last_hud.get() != (z, pending) {
                last_hud.set((z, pending));
                let zoom_label = zoom_label.clone();
                let queue_label = queue_label.clone();
                glib::idle_add_local_once(move || {
                    zoom_label.set_text(&z.to_string());
                    set_queue_label(&queue_label, pending);
                });
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
        let downloader = downloader.clone();
        let area_weak = area.downgrade();
        // Accumulated (scaled) scroll delta; one whole unit == one zoom level.
        let accum = Rc::new(Cell::new(0.0f64));
        scroll.connect_scroll(move |_, _dx, dy| {
            if dy == 0.0 {
                return gtk::glib::Propagation::Proceed;
            }
            let Some(area) = area_weak.upgrade() else {
                return gtk::glib::Propagation::Proceed;
            };
            let mut st = state.borrow_mut();
            let sens = st.config.general.scroll_sensitivity.clamp(0.05, 10.0);
            let (w, h) = (area.width() as f64, area.height() as f64);
            let (px, py) = st.last_pointer;
            let mut acc = accum.get() + dy * sens;
            let mut changed = false;
            // Scroll up (dy < 0) zooms in; down zooms out.
            while acc <= -1.0 {
                acc += 1.0;
                let nz = (st.map.zoom + 1).min(st.max_zoom());
                st.map.zoom_around(px, py, w, h, nz);
                changed = true;
            }
            while acc >= 1.0 {
                acc -= 1.0;
                let nz = st.map.zoom.saturating_sub(1);
                st.map.zoom_around(px, py, w, h, nz);
                changed = true;
            }
            accum.set(acc);
            if changed {
                // The old zoom's queued tiles are useless now; drop them and
                // let the redraw re-request the current view (served first).
                st.inflight.clear();
                drop(st);
                downloader.clear_queue();
                area.queue_draw();
            }
            gtk::glib::Propagation::Stop
        });
        area.add_controller(scroll);
    }

    area
}
