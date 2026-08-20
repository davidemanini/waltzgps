//! Preferences: a single non-modal dialog covering general/cache settings
//! and TMS provider management, with classic OK / Cancel / Apply semantics.
//!
//! All edits happen against a local draft `Config`; nothing is written to
//! `state`/disk until OK or Apply is pressed, and Cancel simply drops the
//! draft. Apply and OK both take effect immediately in the running app
//! (provider list, cache policy, scroll sensitivity) via `on_commit`.

use crate::app_state::SharedState;
use crate::cache::Cache;
use crate::config::{Config, Provider};
use crate::ui::field::{Field, FieldForm};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

/// Scalar `Config` fields shown on the "General" tab. Adding a future field
/// here (plus the struct field itself in `config.rs`) is all that's needed
/// to expose it in the dialog.
fn general_fields() -> Vec<Field<Config>> {
    vec![
        Field::f64_spin(
            "Mouse-wheel sensitivity",
            0.1,
            5.0,
            0.1,
            2,
            |c| c.general.scroll_sensitivity,
            |c, v| c.general.scroll_sensitivity = v,
        ),
        Field::optional_text(
            "Cache directory",
            |c| c.cache.directory.clone(),
            |c, v| c.cache.directory = v,
        ),
        Field::u64_spin(
            "Max cache size (MB, 0 = unlimited)",
            0.0,
            1_000_000.0,
            50.0,
            |c| c.cache.max_size_mb,
            |c, v| c.cache.max_size_mb = v,
        ),
        Field::u64_spin(
            "Max cache age (days, 0 = never)",
            0.0,
            36_500.0,
            1.0,
            |c| c.cache.max_age_days,
            |c, v| c.cache.max_age_days = v,
        ),
    ]
}

/// Scalar `Provider` fields shown in the provider edit form.
fn provider_fields() -> Vec<Field<Provider>> {
    vec![
        Field::text("Name", |p| p.name.clone(), |p, v| p.name = v),
        Field::text("URL template", |p| p.url.clone(), |p, v| p.url = v),
        Field::bool_switch("TMS (flip Y)", |p| p.tms, |p, v| p.tms = v),
        Field::u64_spin(
            "Max zoom",
            1.0,
            22.0,
            1.0,
            |p| p.max_zoom as u64,
            |p, v| p.max_zoom = v as u8,
        ),
    ]
}

/// Open the preferences window. `cache` is updated live on commit; `on_commit`
/// is invoked after a successful OK/Apply so the caller can refresh anything
/// else that depends on the (now-updated) `state.config` (provider menu,
/// downloader's provider list, redraw, ...).
pub fn show_preferences(
    parent: &gtk::ApplicationWindow,
    state: SharedState,
    cache: Arc<Cache>,
    on_commit: Rc<dyn Fn()>,
) -> gtk::Window {
    let window = gtk::Window::builder()
        .title("Preferences")
        .transient_for(parent)
        .default_width(680)
        .default_height(460)
        .build();

    let draft: Rc<RefCell<Config>> = Rc::new(RefCell::new(state.borrow().config.clone()));
    let draft_active_provider: Rc<Cell<usize>> = Rc::new(Cell::new(state.borrow().active_provider));

    let status = gtk::Label::new(None);
    status.set_halign(gtk::Align::Start);
    status.set_wrap(true);

    // Bottom bar buttons, created early so `mark_dirty` (and the provider-tab
    // handlers below) can reference `apply_button`'s sensitivity.
    let cancel_button = gtk::Button::with_label("Cancel");
    let apply_button = gtk::Button::with_label("Apply");
    let ok_button = gtk::Button::with_label("OK");
    ok_button.add_css_class("suggested-action");
    // Nothing to apply until something is actually edited.
    apply_button.set_sensitive(false);

    // Tracks whether any setting has been touched since the dialog opened
    // (or since the last successful Apply/OK). Drives Apply's sensitivity
    // and whether OK needs to do anything at all.
    let dirty: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let mark_dirty: Rc<dyn Fn()> = {
        let dirty = dirty.clone();
        let apply_button = apply_button.clone();
        Rc::new(move || {
            dirty.set(true);
            apply_button.set_sensitive(true);
        })
    };

    // --- General tab -------------------------------------------------
    let general_form = Rc::new(FieldForm::<Config>::build(&general_fields(), &draft.borrow()));
    general_form.connect_changed(mark_dirty.clone());
    let general_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    general_page.set_margin_top(12);
    general_page.set_margin_bottom(12);
    general_page.set_margin_start(12);
    general_page.set_margin_end(12);
    general_page.append(&general_form.grid);

    // --- Providers tab -------------------------------------------------
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    refresh_list(&list, &draft);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&list));
    scroller.set_min_content_width(220);
    scroller.set_vexpand(true);
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    let placeholder = Provider { name: String::new(), url: String::new(), tms: false, max_zoom: 19 };
    let initial_provider = draft.borrow().providers.first().cloned().unwrap_or(placeholder);
    let provider_form = Rc::new(FieldForm::<Provider>::build(&provider_fields(), &initial_provider));
    provider_form.connect_changed(mark_dirty.clone());

    let add_button = gtk::Button::with_label("Add");
    let remove_button = gtk::Button::with_label("Remove");
    let up_button = gtk::Button::from_icon_name("go-up-symbolic");
    up_button.set_tooltip_text(Some("Move provider up"));
    let down_button = gtk::Button::from_icon_name("go-down-symbolic");
    down_button.set_tooltip_text(Some("Move provider down"));
    let apply_list_button = gtk::Button::with_label("Apply to list");

    // Load the selected provider into the form whenever the selection changes.
    {
        let draft = draft.clone();
        let provider_form = provider_form.clone();
        list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let idx = row.index() as usize;
            if let Some(p) = draft.borrow().providers.get(idx) {
                provider_form.load(p);
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
        let draft = draft.clone();
        let list = list.clone();
        let mark_dirty = mark_dirty.clone();
        add_button.connect_clicked(move |_| {
            draft.borrow_mut().providers.push(Provider {
                name: "New provider".into(),
                url: "https://host/{z}/{x}/{y}.png".into(),
                tms: false,
                max_zoom: 19,
            });
            let last = draft.borrow().providers.len() as i32 - 1;
            refresh_list(&list, &draft);
            if let Some(row) = list.row_at_index(last) {
                list.select_row(Some(&row));
            }
            mark_dirty();
        });
    }

    // Remove: drop the selected provider (never below one).
    {
        let draft = draft.clone();
        let list = list.clone();
        let status = status.clone();
        let mark_dirty = mark_dirty.clone();
        remove_button.connect_clicked(move |_| {
            let Some(row) = list.selected_row() else {
                status.set_text("Select a provider to remove.");
                return;
            };
            let idx = row.index() as usize;
            {
                let mut d = draft.borrow_mut();
                if d.providers.len() <= 1 {
                    status.set_text("At least one provider is required.");
                    return;
                }
                d.providers.remove(idx);
            }
            status.set_text("");
            refresh_list(&list, &draft);
            let count = draft.borrow().providers.len() as i32;
            let sel = idx.min((count - 1).max(0) as usize) as i32;
            if let Some(row) = list.row_at_index(sel) {
                list.select_row(Some(&row));
            }
            mark_dirty();
        });
    }

    // Move up / down: swap the selected provider with its neighbour, keeping
    // the draft active-provider index pointed at the same provider.
    let reorder = {
        let draft = draft.clone();
        let list = list.clone();
        let status = status.clone();
        let draft_active_provider = draft_active_provider.clone();
        let mark_dirty = mark_dirty.clone();
        Rc::new(move |up: bool| {
            let Some(row) = list.selected_row() else {
                status.set_text("Select a provider to move.");
                return;
            };
            let idx = row.index() as usize;
            let target = if up {
                if idx == 0 {
                    return;
                }
                idx - 1
            } else {
                idx + 1
            };
            {
                let mut d = draft.borrow_mut();
                if target >= d.providers.len() {
                    return;
                }
                d.providers.swap(idx, target);
                let ap = draft_active_provider.get();
                if ap == idx {
                    draft_active_provider.set(target);
                } else if ap == target {
                    draft_active_provider.set(idx);
                }
            }
            refresh_list(&list, &draft);
            if let Some(row) = list.row_at_index(target as i32) {
                list.select_row(Some(&row));
            }
            mark_dirty();
        })
    };
    {
        let reorder = reorder.clone();
        up_button.connect_clicked(move |_| reorder(true));
    }
    {
        let reorder = reorder.clone();
        down_button.connect_clicked(move |_| reorder(false));
    }

    // Apply to list: write the form back into the selected provider (in the
    // draft only — the dialog-wide Apply/OK below is what actually commits).
    {
        let draft = draft.clone();
        let list = list.clone();
        let status = status.clone();
        let provider_form = provider_form.clone();
        let mark_dirty = mark_dirty.clone();
        apply_list_button.connect_clicked(move |_| {
            let Some(row) = list.selected_row() else {
                status.set_text("Select a provider to edit.");
                return;
            };
            let idx = row.index() as usize;
            let Some(mut p) = draft.borrow().providers.get(idx).cloned() else { return };
            provider_form.store(&mut p);
            if p.name.is_empty() || p.url.is_empty() {
                status.set_text("Name and URL are required.");
                return;
            }
            draft.borrow_mut().providers[idx] = p;
            status.set_text("");
            refresh_list(&list, &draft);
            if let Some(row) = list.row_at_index(idx as i32) {
                list.select_row(Some(&row));
            }
            mark_dirty();
        });
    }

    let form_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    form_box.set_hexpand(true);
    form_box.append(&provider_form.grid);

    let list_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    list_buttons.set_halign(gtk::Align::End);
    list_buttons.append(&add_button);
    list_buttons.append(&remove_button);
    list_buttons.append(&up_button);
    list_buttons.append(&down_button);
    list_buttons.append(&apply_list_button);
    form_box.append(&list_buttons);

    let providers_split = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    providers_split.set_margin_top(12);
    providers_split.set_margin_bottom(12);
    providers_split.set_margin_start(12);
    providers_split.set_margin_end(12);
    providers_split.append(&scroller);
    providers_split.append(&form_box);

    // --- Assembly --------------------------------------------------------
    let notebook = gtk::Notebook::new();
    notebook.append_page(&general_page, Some(&gtk::Label::new(Some("General"))));
    notebook.append_page(&providers_split, Some(&gtk::Label::new(Some("Providers"))));
    notebook.set_vexpand(true);

    // Flush the currently-selected provider row into the draft, then
    // validate and, on success, write the draft back into `state`, persist
    // it, apply it live, and invoke `on_commit`. Returns whether it succeeded.
    let commit: Rc<dyn Fn() -> bool> = {
        let state = state.clone();
        let draft = draft.clone();
        let draft_active_provider = draft_active_provider.clone();
        let list = list.clone();
        let provider_form = provider_form.clone();
        let general_form = general_form.clone();
        let status = status.clone();
        let cache = cache.clone();
        let on_commit = on_commit.clone();
        let dirty = dirty.clone();
        let apply_button = apply_button.clone();
        Rc::new(move || {
            if let Some(row) = list.selected_row() {
                let idx = row.index() as usize;
                let existing = draft.borrow().providers.get(idx).cloned();
                if let Some(mut p) = existing {
                    provider_form.store(&mut p);
                    draft.borrow_mut().providers[idx] = p;
                }
            }

            let mut cfg = draft.borrow().clone();
            general_form.store(&mut cfg);

            if cfg.providers.is_empty()
                || cfg.providers.iter().any(|p| p.name.is_empty() || p.url.is_empty())
            {
                status.set_text("Every provider needs a non-empty name and URL.");
                return false;
            }

            {
                let mut st = state.borrow_mut();
                st.config = cfg.clone();
                let len = st.config.providers.len();
                st.active_provider = draft_active_provider.get().min(len - 1);
            }
            *draft.borrow_mut() = cfg.clone();

            let msg = match state.borrow().save_config() {
                Ok(()) => "Saved.".to_string(),
                Err(e) => format!("Save failed: {e}"),
            };
            status.set_text(&msg);

            cache.update_policy(&cfg.cache);
            {
                let cache = cache.clone();
                std::thread::spawn(move || cache.enforce_policy());
            }
            on_commit();
            dirty.set(false);
            apply_button.set_sensitive(false);
            true
        })
    };

    {
        let window = window.clone();
        cancel_button.connect_clicked(move |_| window.close());
    }
    {
        let commit = commit.clone();
        apply_button.connect_clicked(move |_| {
            commit();
        });
    }
    {
        let window = window.clone();
        let commit = commit.clone();
        let dirty = dirty.clone();
        ok_button.connect_clicked(move |_| {
            // Nothing to do: OK behaves like Cancel when nothing was touched.
            if !dirty.get() || commit() {
                window.close();
            }
        });
    }

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel_button);
    buttons.append(&apply_button);
    buttons.append(&ok_button);

    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bottom.set_margin_start(12);
    bottom.set_margin_end(12);
    bottom.set_margin_bottom(12);
    bottom.append(&status);
    status.set_hexpand(true);
    bottom.append(&buttons);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&notebook);
    root.append(&bottom);

    window.set_child(Some(&root));
    window.present();
    window
}

/// Rebuild the provider list rows from the draft config.
fn refresh_list(list: &gtk::ListBox, draft: &Rc<RefCell<Config>>) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for provider in &draft.borrow().providers {
        let label = gtk::Label::new(Some(&provider.name));
        label.set_halign(gtk::Align::Start);
        label.set_margin_top(6);
        label.set_margin_bottom(6);
        label.set_margin_start(8);
        label.set_margin_end(8);
        list.append(&label);
    }
}
