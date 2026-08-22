//! A mutually-exclusive picker for a [`Provider`]'s API-key source: none, a
//! literal value, a file to read, or a command to run. Exactly one entry is
//! ever sensitive/editable at a time, mirroring [`ApiKeySource`]'s mutual
//! exclusivity in the UI.

use crate::config::{ApiKeySource, Provider};
use gtk::prelude::*;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

pub struct ApiKeySourceField {
    container: gtk::Box,
    none_radio: gtk::CheckButton,
    literal_radio: gtk::CheckButton,
    literal_entry: gtk::Entry,
    file_radio: gtk::CheckButton,
    file_entry: gtk::Entry,
    file_browse: gtk::Button,
    command_radio: gtk::CheckButton,
    command_entry: gtk::Entry,
    /// Set around [`load`](ApiKeySourceField::load) so
    /// [`connect_changed`](ApiKeySourceField::connect_changed) can tell
    /// programmatic updates apart from genuine user edits.
    suppress_changed: Rc<Cell<bool>>,
}

impl ApiKeySourceField {
    /// `parent` makes the file-chooser dialog transient for the preferences
    /// window.
    pub fn build(parent: Option<&gtk::Window>) -> Self {
        let none_radio = gtk::CheckButton::with_label("None (no API key)");
        none_radio.set_active(true);

        let literal_radio = gtk::CheckButton::with_label("Literal value:");
        literal_radio.set_group(Some(&none_radio));
        let literal_entry = gtk::Entry::new();
        literal_entry.set_hexpand(true);
        literal_entry.set_sensitive(false);

        let file_radio = gtk::CheckButton::with_label("Read from file:");
        file_radio.set_group(Some(&none_radio));
        let file_entry = gtk::Entry::new();
        file_entry.set_hexpand(true);
        file_entry.set_sensitive(false);
        let file_browse = gtk::Button::with_label("Browse…");
        file_browse.set_sensitive(false);

        let command_radio = gtk::CheckButton::with_label("Run command:");
        command_radio.set_group(Some(&none_radio));
        let command_entry = gtk::Entry::new();
        command_entry.set_hexpand(true);
        command_entry.set_sensitive(false);

        // Sensitivity follows the active radio; this is what makes the
        // mutual exclusivity visually obvious (only one entry is ever
        // editable). Independent of dirty-tracking, so it's wired directly
        // rather than through `connect_changed`.
        {
            let literal_entry = literal_entry.clone();
            literal_radio.connect_toggled(move |cb| literal_entry.set_sensitive(cb.is_active()));
        }
        {
            let file_entry = file_entry.clone();
            let file_browse = file_browse.clone();
            file_radio.connect_toggled(move |cb| {
                file_entry.set_sensitive(cb.is_active());
                file_browse.set_sensitive(cb.is_active());
            });
        }
        {
            let command_entry = command_entry.clone();
            command_radio.connect_toggled(move |cb| command_entry.set_sensitive(cb.is_active()));
        }

        {
            let entry = file_entry.clone();
            let parent = parent.cloned();
            file_browse.connect_clicked(move |_| {
                let dialog = gtk::FileDialog::builder().title("Select API Key File").build();
                let current = entry.text();
                if !current.is_empty() {
                    dialog.set_initial_file(Some(&gio::File::for_path(current.as_str())));
                }
                let entry = entry.clone();
                dialog.open(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            entry.set_text(&path.display().to_string());
                        }
                    }
                });
            });
        }

        let literal_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        literal_row.append(&literal_radio);
        literal_row.append(&literal_entry);

        let file_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        file_row.append(&file_radio);
        file_row.append(&file_entry);
        file_row.append(&file_browse);

        let command_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        command_row.append(&command_radio);
        command_row.append(&command_entry);

        let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        container.append(&none_radio);
        container.append(&literal_row);
        container.append(&file_row);
        container.append(&command_row);

        ApiKeySourceField {
            container,
            none_radio,
            literal_radio,
            literal_entry,
            file_radio,
            file_entry,
            file_browse,
            command_radio,
            command_entry,
            suppress_changed: Rc::new(Cell::new(false)),
        }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.container
    }

    /// Repopulate the radios/entries from `provider`.
    pub fn load(&self, provider: &Provider) {
        self.suppress_changed.set(true);
        self.literal_entry.set_text("");
        self.file_entry.set_text("");
        self.command_entry.set_text("");
        match &provider.api_key_source {
            None => self.none_radio.set_active(true),
            Some(ApiKeySource::Literal(s)) => {
                self.literal_entry.set_text(s);
                self.literal_radio.set_active(true);
            }
            Some(ApiKeySource::File(p)) => {
                self.file_entry.set_text(&p.display().to_string());
                self.file_radio.set_active(true);
            }
            Some(ApiKeySource::Command(c)) => {
                self.command_entry.set_text(c);
                self.command_radio.set_active(true);
            }
        }
        // `set_active(true)` on an already-active radio doesn't emit
        // `toggled`, so entry sensitivity must be synced explicitly rather
        // than relying solely on that signal.
        self.literal_entry.set_sensitive(self.literal_radio.is_active());
        self.file_entry.set_sensitive(self.file_radio.is_active());
        self.file_browse.set_sensitive(self.file_radio.is_active());
        self.command_entry.set_sensitive(self.command_radio.is_active());
        self.suppress_changed.set(false);
    }

    /// Pull the widgets' current selection back into `provider`. Reflects
    /// whatever is in the active entry, including a blank one — validation
    /// of "selected but empty" happens at the call site.
    pub fn store(&self, provider: &mut Provider) {
        provider.api_key_source = if self.literal_radio.is_active() {
            Some(ApiKeySource::Literal(self.literal_entry.text().trim().to_string()))
        } else if self.file_radio.is_active() {
            Some(ApiKeySource::File(PathBuf::from(self.file_entry.text().trim().to_string())))
        } else if self.command_radio.is_active() {
            Some(ApiKeySource::Command(self.command_entry.text().trim().to_string()))
        } else {
            None
        };
    }

    /// Invoke `f` whenever the user changes the selected radio or edits the
    /// active entry. Programmatic updates via [`load`](Self::load) do not
    /// trigger it.
    pub fn connect_changed(&self, f: Rc<dyn Fn()>) {
        for radio in [&self.none_radio, &self.literal_radio, &self.file_radio, &self.command_radio]
        {
            let f = f.clone();
            let suppress = self.suppress_changed.clone();
            radio.connect_toggled(move |_| {
                if !suppress.get() {
                    f();
                }
            });
        }
        for entry in [&self.literal_entry, &self.file_entry, &self.command_entry] {
            let f = f.clone();
            let suppress = self.suppress_changed.clone();
            entry.connect_changed(move |_| {
                if !suppress.get() {
                    f();
                }
            });
        }
    }
}
