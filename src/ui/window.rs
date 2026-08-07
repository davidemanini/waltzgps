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
use std::rc::Rc;

/// Pixels moved per keyboard/button pan step.
const PAN_STEP: f64 = 120.0;

pub fn build_ui(app: &gtk::Application, state: SharedState, downloader: Rc<Downloader>) {
    let area = map_view::build(state.clone(), downloader.clone());

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&area));
    overlay.add_overlay(&build_nav(state.clone(), area.clone()));

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("WaltzGPS")
        .default_width(1024)
        .default_height(768)
        .child(&overlay)
        .build();

    build_titlebar(&window, &state);
    install_actions(&window, &state, &area);
    install_context_menu(&state, &area);
    install_keyboard(&window, &state, &area);
    pump_results(state, area, downloader);

    window.present();
}

/// Header bar with a provider-selection menu.
fn build_titlebar(window: &gtk::ApplicationWindow, state: &SharedState) {
    let header = gtk::HeaderBar::new();
    let menu = gio::Menu::new();
    for (i, provider) in state.borrow().config.providers.iter().enumerate() {
        let item = gio::MenuItem::new(Some(&provider.name), None);
        item.set_action_and_target_value(Some("win.provider"), Some(&(i as i32).to_variant()));
        menu.append_item(&item);
    }
    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.set_popover(Some(&gtk::PopoverMenu::from_model(Some(&menu))));
    header.pack_end(&menu_button);
    window.set_titlebar(Some(&header));
}

/// Register the `provider` (stateful) and `show-coords` actions on the window.
fn install_actions(window: &gtk::ApplicationWindow, state: &SharedState, area: &DrawingArea) {
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
        provider_action.connect_activate(move |action, param| {
            if let Some(idx) = param.and_then(|v| v.get::<i32>()) {
                set_provider(&state, &area, idx as usize);
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
fn install_keyboard(window: &gtk::ApplicationWindow, state: &SharedState, area: &DrawingArea) {
    let keys = gtk::EventControllerKey::new();
    let state = state.clone();
    let area = area.clone();
    let window_for_closure = window.clone();
    keys.connect_key_pressed(move |_, keyval, _code, _mods| {
        let window = &window_for_closure;
        match keyval {
            Key::Up => pan(&state, &area, 0.0, -PAN_STEP),
            Key::Down => pan(&state, &area, 0.0, PAN_STEP),
            Key::Left => pan(&state, &area, -PAN_STEP, 0.0),
            Key::Right => pan(&state, &area, PAN_STEP, 0.0),
            Key::i | Key::I => zoom_center(&state, &area, true),
            Key::o | Key::O => zoom_center(&state, &area, false),
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
fn build_nav(state: SharedState, area: DrawingArea) -> gtk::Box {
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
        b.connect_clicked(move |_| zoom_center(&state, &area, zoom_in));
        b
    };
    zoom.append(&zoom_button("zoom-out-symbolic", false));
    zoom.append(&zoom_button("zoom-in-symbolic", true));

    nav.append(&pad);
    nav.append(&zoom);
    nav
}

/// Drive tile results from the downloader into the pixbuf cache and redraw.
fn pump_results(state: SharedState, area: DrawingArea, downloader: Rc<Downloader>) {
    let mut results = downloader.take_results();
    glib::spawn_future_local(async move {
        while let Some(res) = results.next().await {
            let redraw = {
                let mut st = state.borrow_mut();
                st.inflight.remove(&res.key);
                match res.data.and_then(|bytes| map_view::decode_pixbuf(&bytes)) {
                    Some(pb) => {
                        st.insert_pixbuf(res.key, pb);
                        true
                    }
                    None => false,
                }
            };
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

fn zoom_center(state: &SharedState, area: &DrawingArea, zoom_in: bool) {
    {
        let mut st = state.borrow_mut();
        let (w, h) = (area.width() as f64, area.height() as f64);
        let max_zoom = st.max_zoom();
        let new_zoom = if zoom_in {
            (st.map.zoom + 1).min(max_zoom)
        } else {
            st.map.zoom.saturating_sub(1)
        };
        st.map.zoom_around(w / 2.0, h / 2.0, w, h, new_zoom);
    }
    area.queue_draw();
}

fn set_provider(state: &SharedState, area: &DrawingArea, idx: usize) {
    {
        let mut st = state.borrow_mut();
        if idx < st.config.providers.len() {
            st.active_provider = idx;
            // Clamp zoom to the new provider's maximum.
            let max_zoom = st.max_zoom();
            if st.map.zoom > max_zoom {
                st.map.zoom = max_zoom;
            }
        }
    }
    area.queue_draw();
}
