#[cfg(unix)]
mod background;
#[cfg(unix)]
mod linux;
mod menu;
#[cfg(windows)]
mod win32;

#[cfg(unix)]
use linux as platform;
#[cfg(windows)]
use win32 as platform;

use std::{cell::Cell, cell::RefCell, rc::Rc};

use adw::prelude::*;
use deskunion_ipc::ClientHandle;
use gtk::glib;

pub use menu::{TrayClient, TrayInfo};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayAction {
    ToggleWindow,
    ShowWindow,
    HideWindow,
    Quit,
    SetRunning(bool),
    ReconnectNow,
    SetClientShared(ClientHandle, bool),
    #[cfg(unix)]
    Shown(bool),
}

/// What the tray asks the application to do with the service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    SetRunning(bool),
    ReconnectNow,
    SetClientShared(ClientHandle, bool),
}

thread_local! {
    static INFO: RefCell<Option<async_channel::Sender<TrayInfo>>> = const { RefCell::new(None) };
}

/// Tells the tray what to show. Does nothing while there is no tray.
pub fn publish(info: TrayInfo) {
    INFO.with_borrow(|sender| {
        if let Some(sender) = sender {
            let _ = sender.try_send(info);
        }
    });
}

#[cfg(unix)]
fn sandboxed() -> bool {
    background::sandboxed()
}

#[cfg(windows)]
fn sandboxed() -> bool {
    false
}

pub fn setup(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    commands: impl Fn(TrayCommand) + 'static,
) {
    let (actions, receiver) = async_channel::unbounded();
    let tray = Rc::new(platform::spawn(actions));
    let shown = Rc::new(Cell::new(tray.is_some()));

    let (info_sender, info_receiver) = async_channel::unbounded();
    INFO.set(Some(info_sender));
    let info = Rc::new(RefCell::new(TrayInfo::default()));
    let refresh = {
        let tray = tray.clone();
        let info = info.clone();
        let window = window.downgrade();
        move || {
            if let (Some(tray), Some(window)) = ((*tray).as_ref(), window.upgrade()) {
                tray.update(&info.borrow(), window.is_visible());
            }
        }
    };
    refresh();
    window.connect_visible_notify({
        let refresh = refresh.clone();
        move |_| refresh()
    });

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

    glib::spawn_future_local(async move {
        while let Ok(update) = info_receiver.recv().await {
            if *info.borrow() != update {
                *info.borrow_mut() = update;
                refresh();
            }
        }
    });

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
                TrayAction::HideWindow => window.set_visible(false),
                TrayAction::Quit => {
                    if let Some(app) = app.upgrade() {
                        app.quit();
                    }
                }
                TrayAction::SetRunning(on) => commands(TrayCommand::SetRunning(on)),
                TrayAction::ReconnectNow => commands(TrayCommand::ReconnectNow),
                TrayAction::SetClientShared(handle, on) => {
                    commands(TrayCommand::SetClientShared(handle, on));
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
