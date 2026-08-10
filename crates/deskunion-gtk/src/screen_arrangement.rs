mod imp;

use adw::subclass::prelude::*;
use gtk::glib;

use deskunion_ipc::{ClientHandle, Position};

glib::wrapper! {
    pub struct ScreenArrangement(ObjectSubclass<imp::ScreenArrangement>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// one screen shown on the canvas — plain data pushed in via
/// [`ScreenArrangement::set_items`] on every relevant `AppModel` update,
/// rather than bound to a `gio::ListStore`.
#[derive(Clone)]
pub struct ScreenItem {
    pub handle: ClientHandle,
    pub hostname: Option<String>,
    pub position: Position,
    pub active: bool,
    pub audio_active: bool,
}

impl ScreenArrangement {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// replace the full set of screens drawn on the canvas and redraw.
    /// Called on every `AppModel` update that could affect the arrangement
    /// (relm4 already re-renders per-message, so there's no need to track
    /// fine-grained "did this specific item change" the way the old
    /// `gio::ListStore` binding did).
    pub fn set_items(&self, items: Vec<ScreenItem>) {
        self.imp().set_items(items);
    }

    /// label drawn on the local (blue) screen — usually this machine's
    /// hostname; empty string falls back to "This device"
    pub fn set_host_label(&self, label: &str) {
        self.imp().set_host_label(label);
    }
}

impl Default for ScreenArrangement {
    fn default() -> Self {
        Self::new()
    }
}
