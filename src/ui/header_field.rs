//! An add/remove-able list of custom HTTP header name/value pairs for a
//! [`Provider`].

use crate::config::{Header, Provider};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

struct HeaderRow {
    row_box: gtk::Box,
    name_entry: gtk::Entry,
    value_entry: gtk::Entry,
}

/// Dirty-tracking callback, checked at fire-time rather than captured at
/// row-creation-time (see [`HeaderListField::changed_cb`]).
type ChangedCallback = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

pub struct HeaderListField {
    container: gtk::Box,
    rows_box: gtk::Box,
    rows: Rc<RefCell<Vec<HeaderRow>>>,
    /// Invoked on every row add/remove/edit once set via
    /// [`connect_changed`](Self::connect_changed). Checked at fire-time (not
    /// captured at row-creation-time), so rows added by
    /// [`load`](Self::load) before `connect_changed` is called still wire
    /// up correctly.
    changed_cb: ChangedCallback,
}

impl HeaderListField {
    pub fn build() -> HeaderListField {
        let rows_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let add_button = gtk::Button::with_label("Add header");
        let rows: Rc<RefCell<Vec<HeaderRow>>> = Rc::new(RefCell::new(Vec::new()));
        let changed_cb: ChangedCallback = Rc::new(RefCell::new(None));

        {
            let rows_box = rows_box.clone();
            let rows = rows.clone();
            let changed_cb = changed_cb.clone();
            add_button.connect_clicked(move |_| {
                add_row(&rows_box, &rows, &changed_cb, "", "");
                if let Some(cb) = changed_cb.borrow().as_ref() {
                    cb();
                }
            });
        }

        let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        container.append(&rows_box);
        container.append(&add_button);

        HeaderListField { container, rows_box, rows, changed_cb }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.container
    }

    /// Rebuild the row list from `provider.headers`.
    pub fn load(&self, provider: &Provider) {
        for row in self.rows.borrow_mut().drain(..) {
            self.rows_box.remove(&row.row_box);
        }
        for h in &provider.headers {
            add_row(&self.rows_box, &self.rows, &self.changed_cb, &h.name, &h.value);
        }
    }

    /// Collect all rows with a non-empty name into `provider.headers`,
    /// silently dropping blank-name rows (e.g. a trailing empty row left
    /// over from "Add header").
    pub fn store(&self, provider: &mut Provider) {
        provider.headers = self
            .rows
            .borrow()
            .iter()
            .filter_map(|r| {
                let name = r.name_entry.text().trim().to_string();
                if name.is_empty() {
                    return None;
                }
                let value = r.value_entry.text().trim().to_string();
                Some(Header { name, value })
            })
            .collect();
    }

    /// Invoke `f` whenever a row is added, removed, or edited. Rows created
    /// by [`load`](Self::load) do not trigger it (populating from a
    /// selection change isn't a user edit).
    pub fn connect_changed(&self, f: Rc<dyn Fn()>) {
        *self.changed_cb.borrow_mut() = Some(f);
    }
}

/// Build one row (name entry + value entry + remove button), wire its
/// signals, and append it to both `rows_box` and `rows`.
fn add_row(
    rows_box: &gtk::Box,
    rows: &Rc<RefCell<Vec<HeaderRow>>>,
    changed_cb: &ChangedCallback,
    name: &str,
    value: &str,
) {
    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let name_entry = gtk::Entry::new();
    name_entry.set_placeholder_text(Some("Header name"));
    name_entry.set_text(name);
    name_entry.set_hexpand(true);
    let value_entry = gtk::Entry::new();
    value_entry.set_placeholder_text(Some("Header value"));
    value_entry.set_text(value);
    value_entry.set_hexpand(true);
    let remove_button = gtk::Button::from_icon_name("list-remove-symbolic");

    row_box.append(&name_entry);
    row_box.append(&value_entry);
    row_box.append(&remove_button);
    rows_box.append(&row_box);

    // Wired after `set_text` above, so populating a row from `load` never
    // fires `changed_cb`.
    for entry in [&name_entry, &value_entry] {
        let changed_cb = changed_cb.clone();
        entry.connect_changed(move |_| {
            if let Some(cb) = changed_cb.borrow().as_ref() {
                cb();
            }
        });
    }

    {
        let rows_box = rows_box.clone();
        let rows = rows.clone();
        let changed_cb = changed_cb.clone();
        let row_box_for_remove = row_box.clone();
        remove_button.connect_clicked(move |_| {
            rows_box.remove(&row_box_for_remove);
            rows.borrow_mut().retain(|r| r.row_box != row_box_for_remove);
            if let Some(cb) = changed_cb.borrow().as_ref() {
                cb();
            }
        });
    }

    rows.borrow_mut().push(HeaderRow { row_box, name_entry, value_entry });
}
