//! The map rendering widget: a custom `gtk::Widget` plus pan/zoom controllers.
//!
//! Rendering goes through GSK (`WidgetImpl::snapshot`), not the cairo
//! `DrawingArea` compatibility path: each decoded tile is uploaded once as a
//! `gdk::Texture`, and every redraw just appends texture nodes to a
//! `gtk::Snapshot` at the right position/size — the GPU-backed renderer
//! composites and scales them (correctly, at whatever the real device scale
//! is, integer or fractional) instead of us re-rasterizing pixels on the CPU
//! every frame. This is both faster (panning no longer re-copies every
//! visible tile's pixels on every pointer-move) and simpler than the old
//! cairo path, which needed manual device-pixel-grid snapping and pattern
//! extend-mode workarounds to avoid HiDPI seams.

use crate::app_state::SharedState;
use crate::downloader::{Downloader, TileKey};
use crate::geo::{self, TILE_SIZE};
use crate::tile::TileId;
use gtk::gdk::{self, MemoryFormat, MemoryTexture, Texture};
use gtk::gdk_pixbuf::PixbufLoader;
use gtk::glib;
use gtk::graphene;
use gtk::gsk::ScalingFilter;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};
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

/// Decode raw image bytes (PNG/JPEG/…) and upload them as a GPU texture.
/// Done once per tile arrival, not per frame.
pub fn decode_texture(bytes: &[u8]) -> Option<Texture> {
    let loader = PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    let pb = loader.pixbuf()?;
    let format = if pb.has_alpha() { MemoryFormat::R8g8b8a8 } else { MemoryFormat::R8g8b8 };
    let pixels = pb.read_pixel_bytes();
    let texture = MemoryTexture::new(pb.width(), pb.height(), format, &pixels, pb.rowstride() as usize);
    Some(texture.upcast())
}

/// Append one tile's `TILE_SIZE`×`TILE_SIZE` slot at `(dst_x, dst_y)`.
///
/// Tries, in order: a HiDPI composite from four zoom+1 tiles (if `hidpi`),
/// the tile itself, a cropped/upscaled ancestor tile (coarser but present),
/// then finally a grey placeholder — requesting whatever's missing from the
/// downloader along the way.
fn append_tile(
    snapshot: &gtk::Snapshot,
    st: &mut crate::app_state::AppState,
    downloader: &Downloader,
    key: TileKey,
    dst_x: f64,
    dst_y: f64,
    hidpi: bool,
    filter: ScalingFilter,
) {
    if hidpi && append_hidpi_composite(snapshot, st, downloader, key, dst_x, dst_y, filter) {
        return;
    }

    if let Some(texture) = st.textures.get(&key).cloned() {
        let bounds = graphene::Rect::new(dst_x as f32, dst_y as f32, TILE_SIZE as f32, TILE_SIZE as f32);
        snapshot.append_scaled_texture(&texture, filter, &bounds);
        return;
    }

    if append_ancestor_fallback(snapshot, st, key, dst_x, dst_y, filter) {
        if st.inflight.insert(key) {
            downloader.request(key);
        }
        return;
    }

    // Placeholder: no tile, and no usable ancestor either.
    let bounds = graphene::Rect::new(dst_x as f32, dst_y as f32, TILE_SIZE as f32, TILE_SIZE as f32);
    let grey = gdk::RGBA::new(0.85, 0.85, 0.85, 1.0);
    snapshot.append_color(&grey, &bounds);
    if st.inflight.insert(key) {
        downloader.request(key);
    }
}

/// Try to render `key`'s tile as a 2x2 composite of its four zoom+1 children,
/// each appended at native resolution into a quarter of the destination slot.
/// Requests any missing child and returns `false` (append nothing) unless all
/// four are already decoded.
fn append_hidpi_composite(
    snapshot: &gtk::Snapshot,
    st: &mut crate::app_state::AppState,
    downloader: &Downloader,
    key: TileKey,
    dst_x: f64,
    dst_y: f64,
    filter: ScalingFilter,
) -> bool {
    let TileKey { provider, tile } = key;
    let cz = match tile.z.checked_add(1) {
        Some(cz) => cz,
        None => return false,
    };
    let cx0 = tile.x * 2;
    let cy0 = tile.y * 2;
    let offsets: [(u32, u32); 4] = [(0, 0), (1, 0), (0, 1), (1, 1)];
    let mut children: [Option<Texture>; 4] = [None, None, None, None];
    let mut all_present = true;
    for (i, (ox, oy)) in offsets.iter().enumerate() {
        let ckey = TileKey { provider, tile: TileId::new(cz, cx0 + ox, cy0 + oy) };
        match st.textures.get(&ckey) {
            Some(texture) => children[i] = Some(texture.clone()),
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
    // Each child is a native 256px tile squeezed into a 128-logical-unit
    // quadrant, so that on a 2x output it lands 1:1 on device pixels — but
    // that resize must be expressed as an explicit transform (like the old
    // cairo `cr.scale`), not as a texture-vs-bounds size mismatch inside a
    // single `append_scaled_texture` call: GSK appears to pick its resample
    // quality from that *locally declared* ratio alone, before the ambient
    // device-scale transform is folded in, so a "shrink to half" node gets
    // pre-filtered as a real minification and the device scale then just
    // blows the already-softened result back up — blurry on any renderer.
    // Declaring the texture at its native size and doing the resize via
    // `translate`+`scale` instead lets the renderer compose one transform
    // (net exactly 1:1 on an integer-scale HiDPI output) and sample once.
    for (i, (ox, oy)) in offsets.iter().enumerate() {
        let texture = children[i].as_ref().expect("checked all_present above");
        let qx = dst_x + *ox as f64 * half;
        let qy = dst_y + *oy as f64 * half;
        snapshot.save();
        snapshot.translate(&graphene::Point::new(qx as f32, qy as f32));
        snapshot.scale(0.5, 0.5);
        let bounds = graphene::Rect::new(0.0, 0.0, TILE_SIZE as f32, TILE_SIZE as f32);
        snapshot.append_scaled_texture(texture, filter, &bounds);
        snapshot.restore();
    }
    true
}

/// Try to render `key`'s tile by cropping and upscaling the nearest available
/// ancestor tile (up to 4 zoom levels up). Returns `false` if none is cached.
fn append_ancestor_fallback(
    snapshot: &gtk::Snapshot,
    st: &crate::app_state::AppState,
    key: TileKey,
    dst_x: f64,
    dst_y: f64,
    filter: ScalingFilter,
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
        if let Some(texture) = st.textures.get(&akey) {
            let scale = (1u32 << n) as f64;
            let mask = (1u32 << n) - 1;
            let sub_size = TILE_SIZE / scale;
            let sub_x = (tile.x & mask) as f64 * sub_size;
            let sub_y = (tile.y & mask) as f64 * sub_size;

            // Clip to the destination slot, then append the *whole* ancestor
            // texture, scaled up via an explicit transform (not a bounds-size
            // mismatch — see `append_hidpi_composite` for why) and positioned
            // so only the wanted sub-tile quadrant lands inside that clip.
            let clip = graphene::Rect::new(dst_x as f32, dst_y as f32, TILE_SIZE as f32, TILE_SIZE as f32);
            snapshot.push_clip(&clip);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(dst_x as f32, dst_y as f32));
            snapshot.scale(scale as f32, scale as f32);
            let bounds = graphene::Rect::new(-sub_x as f32, -sub_y as f32, TILE_SIZE as f32, TILE_SIZE as f32);
            snapshot.append_scaled_texture(texture, filter, &bounds);
            snapshot.restore();
            snapshot.pop();
            return true;
        }
    }
    false
}

mod imp {
    use super::*;

    pub struct MapArea {
        pub state: RefCell<Option<SharedState>>,
        pub downloader: RefCell<Option<Rc<Downloader>>>,
        pub zoom_label: RefCell<Option<gtk::Label>>,
        pub queue_label: RefCell<Option<gtk::Label>>,
        /// Last (zoom, pending) shown on the HUD, to skip redundant updates.
        pub last_hud: Cell<(u8, usize)>,
    }

    impl Default for MapArea {
        fn default() -> Self {
            Self {
                state: RefCell::new(None),
                downloader: RefCell::new(None),
                zoom_label: RefCell::new(None),
                queue_label: RefCell::new(None),
                last_hud: Cell::new((u8::MAX, usize::MAX)),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MapArea {
        const NAME: &'static str = "WaltzGPSMapArea";
        type Type = super::MapArea;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MapArea {}

    impl WidgetImpl for MapArea {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (Some(state), Some(downloader)) =
                (self.state.borrow().clone(), self.downloader.borrow().clone())
            else {
                return;
            };
            let (width, height) = (obj.width(), obj.height());
            if width <= 0 || height <= 0 {
                return;
            }
            let (w, h) = (width as f64, height as f64);

            let mut st = state.borrow_mut();
            let provider = st.active_provider;
            let z = st.map.zoom;
            let zoom_frac = st.map.zoom_frac;
            // HiDPI: composite from zoom+1 tiles (4 native-resolution tiles
            // per slot) instead of stretching the current zoom's tiles, so
            // the map is sharper on a high-density output. Correctness no
            // longer depends on this being exact — GSK composites texture
            // nodes correctly under whatever the real (possibly fractional)
            // device scale is — so the rounded integer scale factor is a
            // fine heuristic for this purely-cosmetic decision.
            let hidpi = obj.scale_factor() >= 2;
            let (tlx, tly) = st.map.top_left_world(w, h);
            let origin_x = tlx.round();
            let origin_y = tly.round();
            let ntiles = geo::tile_count(z);

            // Range of tile columns/rows intersecting the viewport.
            let x0 = (tlx / TILE_SIZE).floor() as i64;
            let y0 = (tly / TILE_SIZE).floor() as i64;
            let x1 = ((tlx + w) / TILE_SIZE).floor() as i64;
            let y1 = ((tly + h) / TILE_SIZE).floor() as i64;

            // While a pinch-zoom gesture is in progress, render the current
            // (integer-zoom) tiles as usual, then blow the whole thing up (or
            // down) by the fractional factor around the viewport centre — no
            // new tiles are fetched mid-gesture, only the snapshot transform
            // does the work, at the cost of transient softness.
            let filter = if zoom_frac != 0.0 {
                snapshot.save();
                let f = 2f64.powf(zoom_frac) as f32;
                snapshot.translate(&graphene::Point::new((w / 2.0) as f32, (h / 2.0) as f32));
                snapshot.scale(f, f);
                snapshot.translate(&graphene::Point::new((-w / 2.0) as f32, (-h / 2.0) as f32));
                ScalingFilter::Linear
            } else {
                ScalingFilter::Nearest
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

                    append_tile(snapshot, &mut st, &downloader, key, dst_x, dst_y, hidpi, filter);
                }
            }

            if zoom_frac != 0.0 {
                snapshot.restore();
            }

            // Refresh the HUD to match what we just drew.
            let pending = st.inflight.len();
            if self.last_hud.get() != (z, pending) {
                self.last_hud.set((z, pending));
                if let (Some(zoom_label), Some(queue_label)) =
                    (self.zoom_label.borrow().clone(), self.queue_label.borrow().clone())
                {
                    glib::idle_add_local_once(move || {
                        zoom_label.set_text(&z.to_string());
                        set_queue_label(&queue_label, pending);
                    });
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct MapArea(ObjectSubclass<imp::MapArea>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MapArea {
    fn setup(
        &self,
        state: SharedState,
        downloader: Rc<Downloader>,
        zoom_label: gtk::Label,
        queue_label: gtk::Label,
    ) {
        let imp = self.imp();
        *imp.state.borrow_mut() = Some(state);
        *imp.downloader.borrow_mut() = Some(downloader);
        *imp.zoom_label.borrow_mut() = Some(zoom_label);
        *imp.queue_label.borrow_mut() = Some(queue_label);
    }
}

impl Default for MapArea {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

/// Build the map widget, wiring drawing plus drag/scroll/motion input.
/// `zoom_label` and `queue_label` are HUD widgets refreshed on every redraw.
pub fn build(
    state: SharedState,
    downloader: Rc<Downloader>,
    zoom_label: gtk::Label,
    queue_label: gtk::Label,
) -> MapArea {
    let area = MapArea::default();
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.setup(state.clone(), downloader.clone(), zoom_label, queue_label);

    // Wayland (and X11, best-effort) can move the widget between outputs with
    // different scale factors at runtime; re-render so the HiDPI compositing
    // heuristic picks up the new factor.
    area.connect_notify_local(Some("scale-factor"), |area, _| area.queue_draw());

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
