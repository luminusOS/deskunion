#[cfg(unix)]
mod background;
#[cfg(unix)]
mod linux;
#[cfg(windows)]
mod win32;

#[cfg(unix)]
use linux as platform;
#[cfg(windows)]
use win32 as platform;

use std::{cell::Cell, rc::Rc};

use adw::prelude::*;
use gtk::glib;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayAction {
    ToggleWindow,
    ShowWindow,
    Quit,
    #[cfg(unix)]
    Shown(bool),
}

#[cfg(unix)]
fn sandboxed() -> bool {
    background::sandboxed()
}

#[cfg(windows)]
fn sandboxed() -> bool {
    false
}

pub fn setup(app: &adw::Application, window: &adw::ApplicationWindow) {
    let (actions, receiver) = async_channel::unbounded();
    let tray = platform::spawn(actions);
    let shown = Rc::new(Cell::new(tray.is_some()));

    window.connect_close_request({
        let shown = shown.clone();
        move |window| {
            if shown.get() || sandboxed() {
                window.set_visible(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });

    #[cfg(unix)]
    if sandboxed() {
        background::set_status("Sharing keyboard and mouse");
    }

    let app = app.downgrade();
    let window = window.downgrade();
    glib::spawn_future_local(async move {
        let _tray = tray;
        while let Ok(action) = receiver.recv().await {
            let Some(window) = window.upgrade() else {
                break;
            };
            match action {
                TrayAction::ToggleWindow if window.is_visible() => window.set_visible(false),
                TrayAction::ToggleWindow | TrayAction::ShowWindow => window.present(),
                TrayAction::Quit => {
                    if let Some(app) = app.upgrade() {
                        app.quit();
                    }
                }
                #[cfg(unix)]
                TrayAction::Shown(visible) => {
                    shown.set(visible);
                    if !visible && !sandboxed() && !window.is_visible() {
                        window.present();
                    }
                }
            }
        }
    });
}
