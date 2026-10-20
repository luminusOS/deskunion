//! Notification-area icon through `Shell_NotifyIcon`, driven by the `tray-icon`
//! crate. Its hidden window receives messages on the GTK thread, whose GDK
//! loop already dispatches the thread's Win32 message queue.

use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

use super::TrayAction;

const OPEN_ID: &str = "open";
const QUIT_ID: &str = "quit";
/// Resource ordinal of the executable icon declared in `deskunion.rc`.
const ICON_ORDINAL: u16 = 1;

pub fn spawn(actions: async_channel::Sender<TrayAction>) -> Option<TrayIcon> {
    let menu = Menu::new();
    let open = MenuItem::with_id(OPEN_ID, "Open DeskUnion", true, None);
    let quit = MenuItem::with_id(QUIT_ID, "Quit DeskUnion", true, None);
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])
        .map_err(|error| log::warn!("tray menu: {error}"))
        .ok()?;

    let icon = Icon::from_resource(ICON_ORDINAL, None)
        .map_err(|error| log::warn!("tray icon resource: {error}"))
        .ok()?;

    MenuEvent::set_event_handler(Some({
        let actions = actions.clone();
        move |event: MenuEvent| {
            let action = match event.id.as_ref() {
                OPEN_ID => TrayAction::ShowWindow,
                QUIT_ID => TrayAction::Quit,
                _ => return,
            };
            let _ = actions.try_send(action);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = actions.try_send(TrayAction::ToggleWindow);
        }
    }));

    TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("DeskUnion")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|error| log::warn!("tray icon unavailable: {error}"))
        .ok()
}
