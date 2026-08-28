//! The map rendering widget: a `DrawingArea` plus pan/zoom controllers.

use crate::app_state::SharedState;
use crate::downloader::{Downloader, TileKey};
use crate::geo::{self, TILE_SIZE};
use crate::tile::TileId;
use gtk::gdk::prelude::{GdkCairoContextExt, SurfaceExt};
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

/// The widget's *true* (possibly fractional) output scale, e.g. `1.5` for a
/// 150% Wayland output. `Widget::scale_factor()` only ever reports the
/// nearest integer (`2` for a 150% output); rendering our own device-pixel
/// grid against that rounded value while the compositor actually scales the
/// buffer by the real fractional factor is exactly what produces the
/// intermittent tile-edge seams on fractional-scale HiDPI outputs — our grid
/// and the compositor's final resize disagree, and whether a given edge lands
/// cleanly depends on its position. Falls back to the integer scale factor
/// before the widget is realized (no surface yet) or on older GDK.
fn real_scale_factor(area: &gtk::DrawingArea) -> f64 {
    area.native()
        .and_then(|native| native.surface())
        .map(|surface| surface.scale())
        .filter(|s| *s > 0.0)
        .unwrap_or_else(|| area.scale_factor().max(1) as f64)
}

/// Paint one tile's `TILE_SIZE`×`TILE_SIZE` slot at `(dst_x, dst_y)`.
///
/// Tries, in order: a HiDPI composite from four zoom+1 tiles (if `hidpi`),
/// the tile itself, a cropped/upscaled ancestor tile (coarser but present),
/// then finally a grey placeholder — requesting whatever's missing from the
/// downloader along the way.
fn draw_tile(
    cr: &gtk::cairo::Context,
    st: &mut crate::app_state::AppState,
    downloader: &Downloader,
    key: TileKey,
    dst_x: f64,
    dst_y: f64,
    hidpi: bool,
    filter: gtk::cairo::Filter,
) {
    if hidpi && try_draw_hidpi_composite(cr, st, downloader, key, dst_x, dst_y, filter) {
        return;
    }

    if let Some(pb) = st.pixbufs.get(&key).cloned() {
        cr.set_source_pixbuf(&pb, dst_x, dst_y);
        cr.source().set_filter(filter);
        cr.source().set_extend(gtk::cairo::Extend::Pad);
        cr.rectangle(dst_x, dst_y, TILE_SIZE, TILE_SIZE);
        let _ = cr.fill();
        return;
    }

    if draw_ancestor_fallback(cr, st, key, dst_x, dst_y, filter) {
        if st.inflight.insert(key) {
            downloader.request(key);
        }
        return;
    }

    // Placeholder: no tile, and no usable ancestor either.
    cr.set_source_rgb(0.85, 0.85, 0.85);
    cr.rectangle(dst_x, dst_y, TILE_SIZE, TILE_SIZE);
    let _ = cr.fill();
    if st.inflight.insert(key) {
        downloader.request(key);
    }
}

/// Try to render `key`'s tile as a 2x2 composite of its four zoom+1 children,
/// each drawn at native resolution into a quarter of the destination slot.
/// Requests any missing child and returns `false` (draw nothing) unless all
/// four are already decoded.
fn try_draw_hidpi_composite(
    cr: &gtk::cairo::Context,
    st: &mut crate::app_state::AppState,
    downloader: &Downloader,
    key: TileKey,
    dst_x: f64,
    dst_y: f64,
    filter: gtk::cairo::Filter,
) -> bool {
    let TileKey { provider, tile } = key;
    let cz = match tile.z.checked_add(1) {
        Some(cz) => cz,
        None => return false,
    };
    let cx0 = tile.x * 2;
    let cy0 = tile.y * 2;
    let offsets: [(u32, u32); 4] = [(0, 0), (1, 0), (0, 1), (1, 1)];
    let mut children: [Option<Pixbuf>; 4] = [None, None, None, None];
    let mut all_present = true;
    for (i, (ox, oy)) in offsets.iter().enumerate() {
        let ckey = TileKey { provider, tile: TileId::new(cz, cx0 + ox, cy0 + oy) };
        match st.pixbufs.get(&ckey) {
            Some(pb) => children[i] = Some(pb.clone()),
            None => {
                all_present = false;
                if st.inflight.insert(ckey) {
                    downloader.request(ckey);
                }
            }
        }
    }
    if !all_present {
        return false;
    }

    let half = TILE_SIZE / 2.0;
    for (i, (ox, oy)) in offsets.iter().enumerate() {
        let pb = children[i].as_ref().expect("checked all_present above");
        let qx = dst_x + *ox as f64 * half;
        let qy = dst_y + *oy as f64 * half;
        let _ = cr.save();
        cr.rectangle(qx, qy, half, half);
        cr.clip();
        cr.translate(qx, qy);
//        cr.scale(half / TILE_SIZE, half / TILE_SIZE);
        cr.scale(0.5, 0.5);
        cr.set_source_pixbuf(pb, 0.0, 0.0);
        cr.source().set_filter(filter);
        cr.source().set_extend(gtk::cairo::Extend::Pad);
        let _ = cr.paint();
        let _ = cr.restore();
    }
    true
}

/// Try to render `key`'s tile by cropping and upscaling the nearest available
/// ancestor tile (up to 4 zoom levels up). Returns `false` if none is cached.
fn draw_ancestor_fallback(
    cr: &gtk::cairo::Context,
    st: &crate::app_state::AppState,
    key: TileKey,
    dst_x: f64,
    dst_y: f64,
    filter: gtk::cairo::Filter,
) -> bool {
    let TileKey { provider, tile } = key;
    for n in 1..=4u8 {
        if tile.z < n {
            break;
        }
        let akey = TileKey {
            provider,
            tile: TileId::new(tile.z - n, tile.x >> n, tile.y >> n),
        };
        if let Some(pb) = st.pixbufs.get(&akey) {
            let scale = (1u32 << n) as f64;
            let mask = (1u32 << n) - 1;
            let sub_size = TILE_SIZE / scale;
            let sub_x = (tile.x & mask) as f64 * sub_size;
            let sub_y = (tile.y & mask) as f64 * sub_size;
            let _ = cr.save();
            cr.rectangle(dst_x, dst_y, TILE_SIZE, TILE_SIZE);
            cr.clip();
            cr.translate(dst_x, dst_y);
            cr.scale(scale, scale);
            cr.set_source_pixbuf(pb, -sub_x, -sub_y);
            cr.source().set_filter(filter);
            cr.source().set_extend(gtk::cairo::Extend::Pad);
            let _ = cr.paint();
            let _ = cr.restore();
            return true;
        }
    }
    false
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
        area.set_draw_func(move |area, cr, width, height| {
            // Hard edges: tiles are drawn 1:1 at integer coordinates, so any
            // antialiasing at rectangle borders only produces seams.
            cr.set_antialias(gtk::cairo::Antialias::None);

            let mut st = state.borrow_mut();
            let provider = st.active_provider;
            let z = st.map.zoom;
            let zoom_frac = st.map.zoom_frac;
            let (w, h) = (width as f64, height as f64);
            // The *real* (possibly fractional, e.g. 1.5 on a 150% Wayland
            // output) output scale — see `real_scale_factor` for why the
            // widget's rounded integer `scale_factor()` isn't good enough
            // here (using it caused intermittent tile-edge seams).
            let scale_factor = real_scale_factor(area);
            // HiDPI: composite from zoom+1 tiles (4 native-resolution tiles per
            // slot) instead of stretching the current zoom's tiles, so the map
            // is actually sharper on a 2x+ output rather than merely aligned.
            // Restricted to (near-)integer scales: the quadrant subdivision
            // below only lands back on the device pixel grid when the scale
            // evenly divides it, which isn't guaranteed at e.g. 1.5x/1.75x —
            // those get the plain, correctly device-aligned tile instead.

	    // Very high threshold
            let hidpi = scale_factor >= 2.0 && (scale_factor - scale_factor.round()).abs() < 1e-2;
            let (tlx, tly) = st.map.top_left_world(w, h);
            // Snap the viewport origin to the device pixel grid. Every tile
            // then lands on a device-pixel-aligned coordinate with exact
            // TILE_SIZE spacing — this removes both the inter-tile seams and
            // the resample blur that appear when a pixbuf is painted at a
            // fractional (sub-device-pixel) offset, including at fractional
            // Wayland scale factors.
            let origin_x = geo::round_to_device(tlx, scale_factor);
            let origin_y = geo::round_to_device(tly, scale_factor);
            let ntiles = geo::tile_count(z);

            // Range of tile columns/rows intersecting the viewport.
            let x0 = (tlx / TILE_SIZE).floor() as i64;
            let y0 = (tly / TILE_SIZE).floor() as i64;
            let x1 = ((tlx + w) / TILE_SIZE).floor() as i64;
            let y1 = ((tly + h) / TILE_SIZE).floor() as i64;

            // While a pinch-zoom gesture is in progress, render the current
            // (integer-zoom) tiles as usual, then blow the whole thing up (or
            // down) by the fractional factor around the viewport centre — no
            // new tiles are fetched mid-gesture, only cairo's transform does
            // the work, at the cost of transient softness.
            let filter = if zoom_frac != 0.0 {
                let _ = cr.save();
                let f = 2f64.powf(zoom_frac);
                cr.translate(w / 2.0, h / 2.0);
                cr.scale(f, f);
                cr.translate(-w / 2.0, -h / 2.0);
                gtk::cairo::Filter::Good
            } else {
                gtk::cairo::Filter::Nearest
            };

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

                    draw_tile(cr, &mut st, &downloader, key, dst_x, dst_y, hidpi, filter);
                }
            }

            if zoom_frac != 0.0 {
                let _ = cr.restore();
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

    // Wayland (and X11, best-effort) can move the widget between outputs with
    // different scale factors at runtime; re-render so the HiDPI/device-pixel
    // path picks up the new factor immediately instead of on the next redraw.
    // The widget's own "scale-factor" only fires on integer changes; the
    // surface's fractional "scale" (only available once realized, hence
    // hooked from connect_realize) also fires on e.g. 150% -> 175%.
    area.connect_notify_local(Some("scale-factor"), |area, _| area.queue_draw());
    {
        let area_weak = area.downgrade();
        area.connect_realize(move |area| {
            let Some(surface) = area.native().and_then(|n| n.surface()) else { return };
            let area_weak = area_weak.clone();
            surface.connect_scale_notify(move |_| {
                if let Some(area) = area_weak.upgrade() {
                    area.queue_draw();
                }
            });
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

    // --- Pinch-to-zoom (trackpad/touch) -----------------------------------
    {
        let gzoom = gtk::GestureZoom::new();

        let state_scale = state.clone();
        let area_weak = area.downgrade();
        gzoom.connect_scale_changed(move |_, scale| {
            let Some(area) = area_weak.upgrade() else { return };
            let mut st = state_scale.borrow_mut();
            if !st.config.general.pinch_zoom_enabled {
                return;
            }
            let sens = st.config.general.pinch_zoom_sensitivity.clamp(0.05, 10.0);
            let max_zoom = st.max_zoom() as f64;
            let delta = scale.log2() * sens;
            let target = (st.map.zoom as f64 + delta).clamp(0.0, max_zoom);
            st.map.zoom_frac = target - st.map.zoom as f64;
            drop(st);
            area.queue_draw();
        });

        // Snap to the nearest integer zoom level once the gesture ends (or is
        // cancelled, e.g. a third finger touches down): re-fetch tiles at
        // that level and drop the transient scale transform.
        let state_end = state.clone();
        let area_weak = area.downgrade();
        let downloader_end = downloader.clone();
        let snap: Rc<dyn Fn()> = Rc::new(move || {
            let Some(area) = area_weak.upgrade() else { return };
            let mut st = state_end.borrow_mut();
            if st.map.zoom_frac == 0.0 {
                return;
            }
            let (w, h) = (area.width() as f64, area.height() as f64);
            let max_zoom = st.max_zoom();
            let nz = (st.map.zoom as f64 + st.map.zoom_frac)
                .round()
                .clamp(0.0, max_zoom as f64) as u8;
            st.map.zoom_frac = 0.0;
            if nz != st.map.zoom {
                st.map.zoom_around(w / 2.0, h / 2.0, w, h, nz);
                st.inflight.clear();
                drop(st);
                downloader_end.clear_queue();
            }
            area.queue_draw();
        });
        {
            let snap = snap.clone();
            gzoom.connect_end(move |_, _| snap());
        }
        {
            let snap = snap.clone();
            gzoom.connect_cancel(move |_, _| snap());
        }

        area.add_controller(gzoom);
    }

    area
}
