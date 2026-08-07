//! Modal editor dialogs: TMS provider management and cache-policy settings.
//!
//! Both write their changes straight back to the on-disk config via
//! [`crate::app_state::AppState::save_config`]. Provider edits also invoke an
//! `on_change` callback so the caller can rebuild the provider menu and redraw.

use crate::app_state::SharedState;
use crate::config::{CachePolicy, Provider};
use gtk::prelude::*;
use std::rc::Rc;

/// Open the provider editor: a selectable list on the left and an edit form on
/// the right, with Add / Remove / Apply controls.
pub fn show_provider_editor(
    parent: &gtk::ApplicationWindow,
    state: SharedState,
    on_change: Rc<dyn Fn()>,
) {
    let window = gtk::Window::builder()
        .title("TMS Providers")
        .transient_for(parent)
        .modal(true)
        .default_width(640)
        .default_height(420)
        .build();

    // --- Provider list ---------------------------------------------------
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    refresh_list(&list, &state);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&list));
    scroller.set_min_content_width(220);
    scroller.set_vexpand(true);
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    // --- Edit form -------------------------------------------------------
    let name_entry = gtk::Entry::new();
    let url_entry = gtk::Entry::new();
    url_entry.set_placeholder_text(Some("https://host/{z}/{x}/{y}.png"));
    let tms_switch = gtk::Switch::new();
    tms_switch.set_halign(gtk::Align::Start);
    let zoom_spin = gtk::SpinButton::with_range(1.0, 22.0, 1.0);

    let form = gtk::Grid::new();
    form.set_row_spacing(8);
    form.set_column_spacing(8);
    form.set_hexpand(true);
    let label = |text: &str| {
        let l = gtk::Label::new(Some(text));
        l.set_halign(gtk::Align::Start);
        l
    };
    name_entry.set_hexpand(true);
    url_entry.set_hexpand(true);
    form.attach(&label("Name"), 0, 0, 1, 1);
    form.attach(&name_entry, 1, 0, 1, 1);
    form.attach(&label("URL template"), 0, 1, 1, 1);
    form.attach(&url_entry, 1, 1, 1, 1);
    form.attach(&label("TMS (flip Y)"), 0, 2, 1, 1);
    form.attach(&tms_switch, 1, 2, 1, 1);
    form.attach(&label("Max zoom"), 0, 3, 1, 1);
    form.attach(&zoom_spin, 1, 3, 1, 1);

    let status = gtk::Label::new(None);
    status.set_halign(gtk::Align::Start);
    status.set_wrap(true);

    // --- Buttons ---------------------------------------------------------
    let add_button = gtk::Button::with_label("Add");
    let remove_button = gtk::Button::with_label("Remove");
    let apply_button = gtk::Button::with_label("Apply");
    let close_button = gtk::Button::with_label("Close");

    // Load the selected provider into the form whenever the selection changes.
    {
        let state = state.clone();
        let name_entry = name_entry.clone();
        let url_entry = url_entry.clone();
        let tms_switch = tms_switch.clone();
        let zoom_spin = zoom_spin.clone();
        list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let idx = row.index() as usize;
            let st = state.borrow();
            if let Some(p) = st.config.providers.get(idx) {
                name_entry.set_text(&p.name);
                url_entry.set_text(&p.url);
                tms_switch.set_active(p.tms);
                zoom_spin.set_value(p.max_zoom as f64);
            }
        });
    }
    // Select the active provider initially.
    let initial = state.borrow().active_provider as i32;
    if let Some(row) = list.row_at_index(initial) {
        list.select_row(Some(&row));
    }

    // Add: append a placeholder provider, select it for editing.
    {
        let state = state.clone();
        let list = list.clone();
        let status = status.clone();
        let on_change = on_change.clone();
        add_button.connect_clicked(move |_| {
            {
                let mut st = state.borrow_mut();
                st.config.providers.push(Provider {
                    name: "New provider".into(),
                    url: "https://host/{z}/{x}/{y}.png".into(),
                    tms: false,
                    max_zoom: 19,
                });
            }
            let last = state.borrow().config.providers.len() as i32 - 1;
            after_change(&state, &on_change, &status);
            refresh_list(&list, &state);
            if let Some(row) = list.row_at_index(last) {
                list.select_row(Some(&row));
            }
        });
    }

    // Remove: drop the selected provider (never below one).
    {
        let state = state.clone();
        let list = list.clone();
        let status = status.clone();
        let on_change = on_change.clone();
        remove_button.connect_clicked(move |_| {
            let Some(row) = list.selected_row() else {
                status.set_text("Select a provider to remove.");
                return;
            };
            let idx = row.index() as usize;
            {
                let mut st = state.borrow_mut();
                if st.config.providers.len() <= 1 {
                    status.set_text("At least one provider is required.");
                    return;
                }
                st.config.providers.remove(idx);
            }
            after_change(&state, &on_change, &status);
            refresh_list(&list, &state);
            let count = state.borrow().config.providers.len() as i32;
            let sel = idx.min((count - 1).max(0) as usize) as i32;
            if let Some(row) = list.row_at_index(sel) {
                list.select_row(Some(&row));
            }
        });
    }

    // Apply: write the form back into the selected provider.
    {
        let state = state.clone();
        let list = list.clone();
        let status = status.clone();
        let on_change = on_change.clone();
        let name_entry = name_entry.clone();
        let url_entry = url_entry.clone();
        let tms_switch = tms_switch.clone();
        let zoom_spin = zoom_spin.clone();
        apply_button.connect_clicked(move |_| {
            let Some(row) = list.selected_row() else {
                status.set_text("Select a provider to edit.");
                return;
            };
            let idx = row.index() as usize;
            let name = name_entry.text().trim().to_string();
            let url = url_entry.text().trim().to_string();
            if name.is_empty() || url.is_empty() {
                status.set_text("Name and URL are required.");
                return;
            }
            {
                let mut st = state.borrow_mut();
                if let Some(p) = st.config.providers.get_mut(idx) {
                    p.name = name;
                    p.url = url;
                    p.tms = tms_switch.is_active();
                    p.max_zoom = zoom_spin.value_as_int() as u8;
                }
            }
            after_change(&state, &on_change, &status);
            refresh_list(&list, &state);
            if let Some(row) = list.row_at_index(idx as i32) {
                list.select_row(Some(&row));
            }
        });
    }

    {
        let window = window.clone();
        close_button.connect_clicked(move |_| window.close());
    }

    // --- Assembly --------------------------------------------------------
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&add_button);
    buttons.append(&remove_button);
    buttons.append(&apply_button);
    buttons.append(&close_button);

    let right = gtk::Box::new(gtk::Orientation::Vertical, 12);
    right.set_hexpand(true);
    right.append(&form);
    right.append(&status);
    right.append(&buttons);

    let split = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    split.set_margin_top(12);
    split.set_margin_bottom(12);
    split.set_margin_start(12);
    split.set_margin_end(12);
    split.append(&scroller);
    split.append(&right);

    window.set_child(Some(&split));
    window.present();
}

/// Open the general settings dialog for the cache policy.
pub fn show_settings(parent: &gtk::ApplicationWindow, state: SharedState) {
    let window = gtk::Window::builder()
        .title("Settings")
        .transient_for(parent)
        .modal(true)
        .default_width(460)
        .build();

    let (dir, size_mb, age_days) = {
        let st = state.borrow();
        (
            st.config.cache.directory.clone().unwrap_or_default(),
            st.config.cache.max_size_mb,
            st.config.cache.max_age_days,
        )
    };

    let dir_entry = gtk::Entry::new();
    dir_entry.set_text(&dir);
    dir_entry.set_placeholder_text(Some("(default cache location)"));
    dir_entry.set_hexpand(true);
    // Generous ranges; 0 means "unlimited" for both.
    let size_spin = gtk::SpinButton::with_range(0.0, 1_000_000.0, 50.0);
    size_spin.set_value(size_mb as f64);
    let age_spin = gtk::SpinButton::with_range(0.0, 36_500.0, 1.0);
    age_spin.set_value(age_days as f64);

    let form = gtk::Grid::new();
    form.set_row_spacing(8);
    form.set_column_spacing(8);
    let label = |text: &str| {
        let l = gtk::Label::new(Some(text));
        l.set_halign(gtk::Align::Start);
        l
    };
    form.attach(&label("Cache directory"), 0, 0, 1, 1);
    form.attach(&dir_entry, 1, 0, 1, 1);
    form.attach(&label("Max size (MB, 0 = unlimited)"), 0, 1, 1, 1);
    form.attach(&size_spin, 1, 1, 1, 1);
    form.attach(&label("Max age (days, 0 = never)"), 0, 2, 1, 1);
    form.attach(&age_spin, 1, 2, 1, 1);

    let note = gtk::Label::new(Some("Cache changes take effect on next launch."));
    note.set_halign(gtk::Align::Start);
    note.add_css_class("dim-label");
    let status = gtk::Label::new(None);
    status.set_halign(gtk::Align::Start);

    let save_button = gtk::Button::with_label("Save");
    let close_button = gtk::Button::with_label("Close");
    {
        let state = state.clone();
        let status = status.clone();
        let dir_entry = dir_entry.clone();
        save_button.connect_clicked(move |_| {
            let dir = dir_entry.text().trim().to_string();
            {
                let mut st = state.borrow_mut();
                st.config.cache = CachePolicy {
                    directory: if dir.is_empty() { None } else { Some(dir.clone()) },
                    max_size_mb: size_spin.value_as_int() as u64,
                    max_age_days: age_spin.value_as_int() as u64,
                };
            }
            let msg = match state.borrow().save_config() {
                Ok(()) => "Saved.".to_string(),
                Err(e) => format!("Save failed: {e}"),
            };
            status.set_text(&msg);
        });
    }
    {
        let window = window.clone();
        close_button.connect_clicked(move |_| window.close());
    }

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&save_button);
    buttons.append(&close_button);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.append(&form);
    root.append(&note);
    root.append(&status);
    root.append(&buttons);

    window.set_child(Some(&root));
    window.present();
}

/// Rebuild the provider list rows from the current config.
fn refresh_list(list: &gtk::ListBox, state: &SharedState) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let st = state.borrow();
    for provider in &st.config.providers {
        let label = gtk::Label::new(Some(&provider.name));
        label.set_halign(gtk::Align::Start);
        label.set_margin_top(6);
        label.set_margin_bottom(6);
        label.set_margin_start(8);
        label.set_margin_end(8);
        list.append(&label);
    }
}

/// Clamp the active provider, persist the config, report status, and notify.
fn after_change(state: &SharedState, on_change: &Rc<dyn Fn()>, status: &gtk::Label) {
    {
        let mut st = state.borrow_mut();
        let len = st.config.providers.len();
        if len > 0 && st.active_provider >= len {
            st.active_provider = len - 1;
        }
    }
    let msg = match state.borrow().save_config() {
        Ok(()) => "Saved.".to_string(),
        Err(e) => format!("Save failed: {e}"),
    };
    status.set_text(&msg);
    on_change();
}
