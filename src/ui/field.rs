//! Declarative scalar-field descriptors for building simple GTK forms.
//!
//! A `Field<T>` pairs a label with a getter/setter pair for one field of
//! `T`, plus the widget kind used to edit it. [`FieldForm::build`] turns a
//! slice of these into a `Grid`; [`FieldForm::load`]/[`FieldForm::store`]
//! move values between the widgets and a `T`. Adding a new scalar field to
//! `Config` or `Provider` only requires one new `Field` entry — no new
//! widget-wiring code.

//use gio::prelude::*;
use gtk::prelude::*;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

pub enum FieldKind<T> {
    Text { get: fn(&T) -> String, set: fn(&mut T, String) },
    OptionalText { get: fn(&T) -> Option<String>, set: fn(&mut T, Option<String>) },
    Bool { get: fn(&T) -> bool, set: fn(&mut T, bool) },
    F64Spin { min: f64, max: f64, step: f64, digits: u32, get: fn(&T) -> f64, set: fn(&mut T, f64) },
    U64Spin { min: f64, max: f64, step: f64, get: fn(&T) -> u64, set: fn(&mut T, u64) },
    /// A filesystem directory, edited via a text entry plus a "Browse…"
    /// button that opens a native folder chooser. An empty entry means `None`
    /// (fall back to whatever default the caller uses).
    Path { get: fn(&T) -> Option<PathBuf>, set: fn(&mut T, Option<PathBuf>) },
}

pub struct Field<T> {
    pub label: &'static str,
    pub kind: FieldKind<T>,
}

impl<T> Field<T> {
    pub fn text(label: &'static str, get: fn(&T) -> String, set: fn(&mut T, String)) -> Self {
        Field { label, kind: FieldKind::Text { get, set } }
    }

    #[allow(dead_code)]
    pub fn optional_text(
        label: &'static str,
        get: fn(&T) -> Option<String>,
        set: fn(&mut T, Option<String>),
    ) -> Self {
        Field { label, kind: FieldKind::OptionalText { get, set } }
    }

    pub fn bool_switch(label: &'static str, get: fn(&T) -> bool, set: fn(&mut T, bool)) -> Self {
        Field { label, kind: FieldKind::Bool { get, set } }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn f64_spin(
        label: &'static str,
        min: f64,
        max: f64,
        step: f64,
        digits: u32,
        get: fn(&T) -> f64,
        set: fn(&mut T, f64),
    ) -> Self {
        Field { label, kind: FieldKind::F64Spin { min, max, step, digits, get, set } }
    }

    pub fn u64_spin(
        label: &'static str,
        min: f64,
        max: f64,
        step: f64,
        get: fn(&T) -> u64,
        set: fn(&mut T, u64),
    ) -> Self {
        Field { label, kind: FieldKind::U64Spin { min, max, step, get, set } }
    }

    pub fn path(
        label: &'static str,
        get: fn(&T) -> Option<PathBuf>,
        set: fn(&mut T, Option<PathBuf>),
    ) -> Self {
        Field { label, kind: FieldKind::Path { get, set } }
    }
}

/// A built widget bound to one field's getter/setter.
enum FieldWidget<T> {
    Text(gtk::Entry, fn(&T) -> String, fn(&mut T, String)),
    OptionalText(gtk::Entry, fn(&T) -> Option<String>, fn(&mut T, Option<String>)),
    Bool(gtk::Switch, fn(&T) -> bool, fn(&mut T, bool)),
    F64Spin(gtk::SpinButton, fn(&T) -> f64, fn(&mut T, f64)),
    U64Spin(gtk::SpinButton, fn(&T) -> u64, fn(&mut T, u64)),
    Path(gtk::Entry, fn(&T) -> Option<PathBuf>, fn(&mut T, Option<PathBuf>)),
}

/// A form built from a list of [`Field`]s: a `Grid` of label/widget rows,
/// plus [`load`](FieldForm::load)/[`store`](FieldForm::store) to move values
/// between the widgets and a `T`.
pub struct FieldForm<T> {
    pub grid: gtk::Grid,
    widgets: Vec<FieldWidget<T>>,
    /// Set around programmatic updates ([`load`](FieldForm::load)) so
    /// [`connect_changed`](FieldForm::connect_changed) can tell those apart
    /// from genuine user edits.
    suppress_changed: Rc<Cell<bool>>,
}

impl<T> FieldForm<T> {
    /// `parent` is used to make the folder-chooser dialog opened by a
    /// [`FieldKind::Path`] field transient for the preferences window; pass
    /// `None` if the form has no such field.
    pub fn build(fields: &[Field<T>], initial: &T, parent: Option<&gtk::Window>) -> FieldForm<T> {
        let grid = gtk::Grid::new();
        grid.set_row_spacing(8);
        grid.set_column_spacing(8);
        grid.set_hexpand(true);

        let mut widgets = Vec::with_capacity(fields.len());
        for (row, field) in fields.iter().enumerate() {
            let label = gtk::Label::new(Some(field.label));
            label.set_halign(gtk::Align::Start);
            grid.attach(&label, 0, row as i32, 1, 1);

            let widget = match field.kind {
                FieldKind::Text { get, set } => {
                    let entry = gtk::Entry::new();
                    entry.set_hexpand(true);
                    entry.set_text(&get(initial));
                    grid.attach(&entry, 1, row as i32, 1, 1);
                    FieldWidget::Text(entry, get, set)
                }
                FieldKind::OptionalText { get, set } => {
                    let entry = gtk::Entry::new();
                    entry.set_hexpand(true);
                    entry.set_text(&get(initial).unwrap_or_default());
                    grid.attach(&entry, 1, row as i32, 1, 1);
                    FieldWidget::OptionalText(entry, get, set)
                }
                FieldKind::Bool { get, set } => {
                    let sw = gtk::Switch::new();
                    sw.set_halign(gtk::Align::Start);
                    sw.set_active(get(initial));
                    grid.attach(&sw, 1, row as i32, 1, 1);
                    FieldWidget::Bool(sw, get, set)
                }
                FieldKind::F64Spin { min, max, step, digits, get, set } => {
                    let spin = gtk::SpinButton::with_range(min, max, step);
                    spin.set_digits(digits);
                    spin.set_value(get(initial));
                    grid.attach(&spin, 1, row as i32, 1, 1);
                    FieldWidget::F64Spin(spin, get, set)
                }
                FieldKind::U64Spin { min, max, step, get, set } => {
                    let spin = gtk::SpinButton::with_range(min, max, step);
                    spin.set_value(get(initial) as f64);
                    grid.attach(&spin, 1, row as i32, 1, 1);
                    FieldWidget::U64Spin(spin, get, set)
                }
                FieldKind::Path { get, set } => {
                    let entry = gtk::Entry::new();
                    entry.set_hexpand(true);
                    if let Some(p) = get(initial) {
                        entry.set_text(&p.display().to_string());
                    }
                    let browse = gtk::Button::with_label("Browse…");
                    {
                        let entry = entry.clone();
                        let parent = parent.cloned();
                        browse.connect_clicked(move |_| {
                            let dialog =
                                gtk::FileDialog::builder().title("Select Directory").build();
                            let current = entry.text();
                            if !current.is_empty() {
                                dialog.set_initial_folder(Some(&gio::File::for_path(
                                    current.as_str(),
                                )));
                            }
                            let entry = entry.clone();
                            dialog.select_folder(
                                parent.as_ref(),
                                None::<&gio::Cancellable>,
                                move |result| {
                                    if let Ok(file) = result {
                                        if let Some(path) = file.path() {
                                            entry.set_text(&path.display().to_string());
                                        }
                                    }
                                },
                            );
                        });
                    }
                    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    hbox.set_hexpand(true);
                    hbox.append(&entry);
                    hbox.append(&browse);
                    grid.attach(&hbox, 1, row as i32, 1, 1);
                    FieldWidget::Path(entry, get, set)
                }
            };
            widgets.push(widget);
        }

        FieldForm { grid, widgets, suppress_changed: Rc::new(Cell::new(false)) }
    }

    /// Invoke `f` whenever the user edits any widget's value. Programmatic
    /// updates via [`load`](FieldForm::load) do not trigger it.
    pub fn connect_changed(&self, f: Rc<dyn Fn()>) {
        for widget in &self.widgets {
            let f = f.clone();
            let suppress = self.suppress_changed.clone();
            let fire = move || {
                if !suppress.get() {
                    f();
                }
            };
            match widget {
                FieldWidget::Text(entry, ..) | FieldWidget::OptionalText(entry, ..) | FieldWidget::Path(entry, ..) => {
                    entry.connect_changed(move |_| fire());
                }
                FieldWidget::Bool(sw, ..) => {
                    sw.connect_active_notify(move |_| fire());
                }
                FieldWidget::F64Spin(spin, ..) | FieldWidget::U64Spin(spin, ..) => {
                    spin.connect_value_changed(move |_| fire());
                }
            }
        }
    }

    /// Repopulate the widgets from `value` (e.g. after a list selection changes).
    pub fn load(&self, value: &T) {
        self.suppress_changed.set(true);
        for widget in &self.widgets {
            match widget {
                FieldWidget::Text(entry, get, _) => entry.set_text(&get(value)),
                FieldWidget::OptionalText(entry, get, _) => {
                    entry.set_text(&get(value).unwrap_or_default())
                }
                FieldWidget::Bool(sw, get, _) => sw.set_active(get(value)),
                FieldWidget::F64Spin(spin, get, _) => spin.set_value(get(value)),
                FieldWidget::U64Spin(spin, get, _) => spin.set_value(get(value) as f64),
                FieldWidget::Path(entry, get, _) => {
                    entry.set_text(&get(value).map(|p| p.display().to_string()).unwrap_or_default())
                }
            }
        }
        self.suppress_changed.set(false);
    }

    /// Pull the widgets' current values back into `value`.
    pub fn store(&self, value: &mut T) {
        for widget in &self.widgets {
            match widget {
                FieldWidget::Text(entry, _, set) => set(value, entry.text().trim().to_string()),
                FieldWidget::OptionalText(entry, _, set) => {
                    let text = entry.text().trim().to_string();
                    set(value, if text.is_empty() { None } else { Some(text) });
                }
                FieldWidget::Path(entry, _, set) => {
                    let text = entry.text().trim().to_string();
                    set(value, if text.is_empty() { None } else { Some(PathBuf::from(text)) });
                }
                FieldWidget::Bool(sw, _, set) => set(value, sw.is_active()),
                FieldWidget::F64Spin(spin, _, set) => set(value, spin.value()),
                FieldWidget::U64Spin(spin, _, set) => set(value, spin.value_as_int() as u64),
            }
        }
    }
}
