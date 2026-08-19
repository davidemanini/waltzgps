//! Main window assembly: overlay + nav buttons, provider menu, keyboard input,
//! right-click "show coordinates", and the tile-result pump.

use crate::app_state::SharedState;
use crate::downloader::Downloader;
use crate::ui::map_view;
use futures_util::StreamExt;
use gtk::gdk::Key;
use gtk::glib;
use gtk::glib::prelude::*;
use gtk::prelude::*;
use gtk::{gio, DrawingArea};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Pixels moved per keyboard/button pan step.
const PAN_STEP: f64 = 120.0;

pub fn build_ui(app: &gtk::Application, state: SharedState, downloader: Rc<Downloader>) {
    install_css();

    // HUD widgets, refreshed from the draw function and the result pump.
    let zoom_label = gtk::Label::new(None);
    zoom_label.add_css_class("hud");
    zoom_label.set_width_chars(2);
    let queue_label = gtk::Label::new(None);
    queue_label.add_css_class("hud");
    queue_label.set_visible(false);

    let area = map_view::build(
        state.clone(),
        downloader.clone(),
        zoom_label.clone(),
        queue_label.clone(),
    );

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&area));
    overlay.add_overlay(&build_nav(state.clone(), area.clone(), downloader.clone(), zoom_label));
    overlay.add_overlay(&build_queue_indicator(queue_label.clone()));

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("WaltzGPS")
        .default_width(1024)
        .default_height(768)
        .child(&overlay)
        .build();
    // No titlebar or window-manager decorations: the map fills the whole
    // window and the floating menu/quit buttons take their place.
    window.set_decorated(false);

    let provider_action = install_actions(&window, &state, &area, &downloader);
    overlay.add_overlay(&build_controls(
        &window,
        &state,
        &area,
        &downloader,
        &provider_action,
    ));
    install_context_menu(&state, &area);
    install_keyboard(&window, &state, &area, &downloader);
    install_state_persistence(&window, &state);
    pump_results(state, area, downloader, queue_label);

    window.present();
}

/// Save the map position on close and periodically (so it survives a crash).
fn install_state_persistence(window: &gtk::ApplicationWindow, state: &SharedState) {
    {
        let state = state.clone();
        window.connect_close_request(move |_| {
            state.borrow().save_map_state();
            glib::Propagation::Proceed
        });
    }
    {
        let state = state.clone();
        // Only write when the view actually moved since the last save.
        let last = Cell::new((f64::NAN, f64::NAN, u8::MAX));
        glib::timeout_add_seconds_local(5, move || {
            let st = state.borrow();
            let cur = (st.map.center_lon, st.map.center_lat, st.map.zoom);
            if cur != last.get() {
                last.set(cur);
                st.save_map_state();
            }
            glib::ControlFlow::Continue
        });
    }
}

/// Install the application-wide CSS used by the floating HUD widgets.
fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        ".hud { background-color: rgba(0,0,0,0.65); color: #fff; \
         padding: 2px 8px; border-radius: 6px; }",
    );
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// Bottom-left floating download-queue counter.
fn build_queue_indicator(queue_label: gtk::Label) -> gtk::Box {
    let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    container.set_halign(gtk::Align::Start);
    container.set_valign(gtk::Align::End);
    container.set_margin_start(12);
    container.set_margin_bottom(12);
    container.append(&queue_label);
    container
}

/// Floating top-right controls: a menu button (provider list + editor +
/// settings) and a quit button. Replaces the window titlebar.
fn build_controls(
    window: &gtk::ApplicationWindow,
    state: &SharedState,
    area: &DrawingArea,
    downloader: &Rc<Downloader>,
    provider_action: &gio::SimpleAction,
) -> gtk::Box {
    // Provider radio section (rebuilt when the provider list changes) plus a
    // static section of editor/settings entries.
    let provider_section = gio::Menu::new();
    populate_provider_section(&provider_section, state);
    let menu_model = gio::Menu::new();
    menu_model.append_section(None, &provider_section);
    let actions_section = gio::Menu::new();
    actions_section.append(Some("Preferences…"), Some("win.preferences"));
    menu_model.append_section(None, &actions_section);

    // Callback the preferences dialog invokes after a successful Apply/OK.
    let on_commit: Rc<dyn Fn()> = {
        let provider_section = provider_section.clone();
        let state = state.clone();
        let area = area.clone();
        let downloader = downloader.clone();
        let provider_action = provider_action.clone();
        Rc::new(move || {
            // Push the edited list to the worker pool and drop now-stale work
            // and decoded tiles (indices/URLs may have changed).
            downloader.set_providers(state.borrow().config.providers.clone());
            downloader.clear_queue();
            {
                let mut st = state.borrow_mut();
                st.pixbufs.clear();
                st.inflight.clear();
            }
            populate_provider_section(&provider_section, &state);
            let idx = state.borrow().active_provider as i32;
            provider_action.set_state(&idx.to_variant());
            area.queue_draw();
        })
    };

    // "Preferences…" action: non-modal, so a second activation while it's
    // already open re-presents the existing window instead of opening a
    // second editor with a second draft.
    let preferences_window: Rc<RefCell<Option<gtk::Window>>> = Rc::new(RefCell::new(None));
    let preferences_action = gio::SimpleAction::new("preferences", None);
    {
        let window = window.clone();
        let state = state.clone();
        let cache = downloader.cache();
        let on_commit = on_commit.clone();
        let guard = preferences_window.clone();
        preferences_action.connect_activate(move |_, _| {
            if let Some(existing) = guard.borrow().as_ref() {
                existing.present();
                return;
            }
            let dlg = crate::ui::preferences::show_preferences(
                &window,
                state.clone(),
                cache.clone(),
                on_commit.clone(),
            );
            {
                let guard = guard.clone();
                dlg.connect_close_request(move |_| {
                    *guard.borrow_mut() = None;
                    glib::Propagation::Proceed
                });
            }
            *guard.borrow_mut() = Some(dlg);
        });
    }
    window.add_action(&preferences_action);

    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.add_css_class("osd");
    menu_button.set_popover(Some(&gtk::PopoverMenu::from_model(Some(&menu_model))));

    let quit_button = gtk::Button::from_icon_name("window-close-symbolic");
    quit_button.add_css_class("osd");
    quit_button.set_tooltip_text(Some("Quit"));
    {
        let window = window.clone();
        quit_button.connect_clicked(move |_| window.close());
    }

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.set_halign(gtk::Align::End);
    controls.set_valign(gtk::Align::Start);
    controls.set_margin_end(12);
    controls.set_margin_top(12);
    controls.append(&menu_button);
    controls.append(&quit_button);
    controls
}

/// Fill (or refill) the provider radio section of the menu.
fn populate_provider_section(section: &gio::Menu, state: &SharedState) {
    section.remove_all();
    for (i, provider) in state.borrow().config.providers.iter().enumerate() {
        let item = gio::MenuItem::new(Some(&provider.name), None);
        item.set_action_and_target_value(Some("win.provider"), Some(&(i as i32).to_variant()));
        section.append_item(&item);
    }
}

/// Register the `provider` (stateful) and `show-coords` actions on the window.
/// Returns the provider action so callers can sync its state with the menu.
fn install_actions(
    window: &gtk::ApplicationWindow,
    state: &SharedState,
    area: &DrawingArea,
    downloader: &Rc<Downloader>,
) -> gio::SimpleAction {
    // Provider switching (radio-style stateful action, i32 target = index).
    let initial = state.borrow().active_provider as i32;
    let provider_action = gio::SimpleAction::new_stateful(
        "provider",
        Some(glib::VariantTy::INT32),
        &initial.to_variant(),
    );
    {
        let state = state.clone();
        let area = area.clone();
        let downloader = downloader.clone();
        provider_action.connect_activate(move |action, param| {
            if let Some(idx) = param.and_then(|v| v.get::<i32>()) {
                set_provider(&state, &area, &downloader, idx as usize);
                action.set_state(&idx.to_variant());
            }
        });
    }
    window.add_action(&provider_action);

    // Show coordinates of the last right-click.
    let coords_action = gio::SimpleAction::new("show-coords", None);
    {
        let state = state.clone();
        let window = window.clone();
        coords_action.connect_activate(move |_, _| {
            let (lon, lat) = state.borrow().last_click_lonlat;
            println!("Clicked position: lat={lat:.6}, lon={lon:.6}");
            gtk::AlertDialog::builder()
                .message("Clicked position")
                .detail(format!("Latitude:  {lat:.6}\nLongitude: {lon:.6}"))
                .build()
                .show(Some(&window));
        });
    }
    window.add_action(&coords_action);

    provider_action
}

/// Right-click popover on the map with a single "Show coordinates" item.
fn install_context_menu(state: &SharedState, area: &DrawingArea) {
    let menu = gio::Menu::new();
    menu.append(Some("Show coordinates"), Some("win.show-coords"));
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_parent(area);

    let click = gtk::GestureClick::builder().button(3).build();
    let state = state.clone();
    let area_weak = area.downgrade();
    click.connect_pressed(move |_, _n, x, y| {
        let Some(area) = area_weak.upgrade() else { return };
        {
            let mut st = state.borrow_mut();
            let (w, h) = (area.width() as f64, area.height() as f64);
            st.last_click_lonlat = st.map.screen_to_lonlat(x, y, w, h);
        }
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    });
    area.add_controller(click);
}

/// Arrow keys pan, `i`/`o` zoom, digit keys switch provider.
fn install_keyboard(
    window: &gtk::ApplicationWindow,
    state: &SharedState,
    area: &DrawingArea,
    downloader: &Rc<Downloader>,
) {
    let keys = gtk::EventControllerKey::new();
    let state = state.clone();
    let area = area.clone();
    let downloader = downloader.clone();
    let window_for_closure = window.clone();
    keys.connect_key_pressed(move |_, keyval, _code, _mods| {
        let window = &window_for_closure;
        match keyval {
            Key::Up => pan(&state, &area, 0.0, -PAN_STEP),
            Key::Down => pan(&state, &area, 0.0, PAN_STEP),
            Key::Left => pan(&state, &area, -PAN_STEP, 0.0),
            Key::Right => pan(&state, &area, PAN_STEP, 0.0),
            Key::i | Key::I => zoom_center(&state, &area, &downloader, true),
            Key::o | Key::O => zoom_center(&state, &area, &downloader, false),
            _ => {
                if let Some(idx) = digit_index(keyval) {
                    if idx < state.borrow().config.providers.len() {
                        // `activate_action` exists on both WidgetExt and
                        // ActionGroupExt; the "provider" action lives on the
                        // window's own action group, so use the latter.
                        gtk::prelude::ActionGroupExt::activate_action(
                            window,
                            "provider",
                            Some(&(idx as i32).to_variant()),
                        );
                    }
                    return glib::Propagation::Stop;
                }
                return glib::Propagation::Proceed;
            }
        }
        glib::Propagation::Stop
    });
    window.add_controller(keys);
}

/// Map digit keys `1`..=`9` to zero-based provider indices.
fn digit_index(key: Key) -> Option<usize> {
    match key {
        Key::_1 => Some(0),
        Key::_2 => Some(1),
        Key::_3 => Some(2),
        Key::_4 => Some(3),
        Key::_5 => Some(4),
        Key::_6 => Some(5),
        Key::_7 => Some(6),
        Key::_8 => Some(7),
        Key::_9 => Some(8),
        _ => None,
    }
}

/// On-screen navigation pad (pan arrows + zoom buttons), bottom-right.
/// `zoom_label` sits between the zoom-out and zoom-in buttons.
fn build_nav(
    state: SharedState,
    area: DrawingArea,
    downloader: Rc<Downloader>,
    zoom_label: gtk::Label,
) -> gtk::Box {
    let nav = gtk::Box::new(gtk::Orientation::Vertical, 6);
    nav.set_halign(gtk::Align::End);
    nav.set_valign(gtk::Align::End);
    nav.set_margin_end(12);
    nav.set_margin_bottom(12);

    let pan_button = |icon: &str, dx: f64, dy: f64| {
        let b = gtk::Button::from_icon_name(icon);
        let state = state.clone();
        let area = area.clone();
        b.connect_clicked(move |_| pan(&state, &area, dx, dy));
        b
    };

    let pad = gtk::Grid::new();
    pad.set_row_spacing(2);
    pad.set_column_spacing(2);
    pad.attach(&pan_button("go-up-symbolic", 0.0, -PAN_STEP), 1, 0, 1, 1);
    pad.attach(&pan_button("go-previous-symbolic", -PAN_STEP, 0.0), 0, 1, 1, 1);
    pad.attach(&pan_button("go-next-symbolic", PAN_STEP, 0.0), 2, 1, 1, 1);
    pad.attach(&pan_button("go-down-symbolic", 0.0, PAN_STEP), 1, 2, 1, 1);

    let zoom = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let zoom_button = |icon: &str, zoom_in: bool| {
        let b = gtk::Button::from_icon_name(icon);
        let state = state.clone();
        let area = area.clone();
        let downloader = downloader.clone();
        b.connect_clicked(move |_| zoom_center(&state, &area, &downloader, zoom_in));
        b
    };
    zoom_label.set_halign(gtk::Align::Center);
    zoom_label.set_valign(gtk::Align::Center);
    zoom.append(&zoom_button("zoom-out-symbolic", false));
    zoom.append(&zoom_label);
    zoom.append(&zoom_button("zoom-in-symbolic", true));

    nav.append(&pad);
    nav.append(&zoom);
    nav
}

/// Drive tile results from the downloader into the pixbuf cache and redraw.
fn pump_results(
    state: SharedState,
    area: DrawingArea,
    downloader: Rc<Downloader>,
    queue_label: gtk::Label,
) {
    let mut results = downloader.take_results();
    glib::spawn_future_local(async move {
        while let Some(res) = results.next().await {
            let (redraw, pending) = {
                let mut st = state.borrow_mut();
                st.inflight.remove(&res.key);
                let redraw = match res.data.and_then(|bytes| map_view::decode_pixbuf(&bytes)) {
                    Some(pb) => {
                        st.insert_pixbuf(res.key, pb);
                        true
                    }
                    None => false,
                };
                (redraw, st.inflight.len())
            };
            // Always reflect the queue size — failed fetches shrink it too,
            // without triggering a redraw.
            map_view::set_queue_label(&queue_label, pending);
            if redraw {
                area.queue_draw();
            }
        }
    });
}

// --- shared operations -------------------------------------------------------

fn pan(state: &SharedState, area: &DrawingArea, dx: f64, dy: f64) {
    state.borrow_mut().map.pan_px(dx, dy);
    area.queue_draw();
}

fn zoom_center(state: &SharedState, area: &DrawingArea, downloader: &Rc<Downloader>, zoom_in: bool) {
    let changed = {
        let mut st = state.borrow_mut();
        let (w, h) = (area.width() as f64, area.height() as f64);
        let max_zoom = st.max_zoom();
        let new_zoom = if zoom_in {
            (st.map.zoom + 1).min(max_zoom)
        } else {
            st.map.zoom.saturating_sub(1)
        };
        if new_zoom == st.map.zoom {
            false
        } else {
            st.map.zoom_around(w / 2.0, h / 2.0, w, h, new_zoom);
            // Old-zoom tiles are no longer wanted; re-request the new view.
            st.inflight.clear();
            true
        }
    };
    if changed {
        downloader.clear_queue();
    }
    area.queue_draw();
}

fn set_provider(state: &SharedState, area: &DrawingArea, downloader: &Rc<Downloader>, idx: usize) {
    let changed = {
        let mut st = state.borrow_mut();
        if idx < st.config.providers.len() && idx != st.active_provider {
            st.active_provider = idx;
            // Clamp zoom to the new provider's maximum.
            let max_zoom = st.max_zoom();
            if st.map.zoom > max_zoom {
                st.map.zoom = max_zoom;
            }
            // Discard the previous provider's queued/pending requests so the
            // new provider's current view is fetched first.
            st.inflight.clear();
            true
        } else {
            false
        }
    };
    if changed {
        downloader.clear_queue();
    }
    area.queue_draw();
}
