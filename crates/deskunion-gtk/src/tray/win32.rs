//! Notification-area icon through `Shell_NotifyIcon`, driven by the `tray-icon`
//! crate. Its hidden window receives messages on the GTK thread, whose GDK
//! loop already dispatches the thread's Win32 message queue.

use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};

use super::{
    TrayAction,
    menu::{Entry, TrayInfo, action_id, escape_ampersands, menu_model, parse_action},
};

/// Resource ordinal of the executable icon declared in `deskunion.rc`.
const ICON_ORDINAL: u16 = 1;
/// The notification area truncates tooltips at 127 characters.
const TOOLTIP_LIMIT: usize = 127;

pub struct Tray {
    icon: TrayIcon,
}

impl Tray {
    pub fn update(&self, info: &TrayInfo, window_visible: bool) {
        match build_menu(&menu_model(info, window_visible)) {
            Ok(menu) => self.icon.set_menu(Some(Box::new(menu))),
            Err(error) => log::warn!("tray menu: {error}"),
        }
        if let Err(error) = self.icon.set_tooltip(Some(tooltip(info))) {
            log::warn!("tray tooltip: {error}");
        }
    }
}

fn tooltip(info: &TrayInfo) -> String {
    let text = if info.status.is_empty() {
        "DeskUnion".to_string()
    } else {
        format!("DeskUnion\n{}", info.status)
    };
    text.chars().take(TOOLTIP_LIMIT).collect()
}

enum Built {
    Item(MenuItem),
    Check(CheckMenuItem),
    Separator(PredefinedMenuItem),
    Submenu(Submenu),
}

impl Built {
    fn as_item(&self) -> &dyn IsMenuItem {
        match self {
            Built::Item(item) => item,
            Built::Check(item) => item,
            Built::Separator(item) => item,
            Built::Submenu(item) => item,
        }
    }
}

fn build(entry: &Entry) -> Result<Built, tray_icon::menu::Error> {
    Ok(match entry {
        Entry::Item {
            label,
            action,
            enabled,
            ..
        } => Built::Item(MenuItem::with_id(
            action_id(*action).unwrap_or_default(),
            escape_ampersands(label),
            *enabled,
            None,
        )),
        Entry::Check {
            label,
            checked,
            action,
        } => Built::Check(CheckMenuItem::with_id(
            action_id(*action).unwrap_or_default(),
            escape_ampersands(label),
            true,
            *checked,
            None,
        )),
        Entry::Separator => Built::Separator(PredefinedMenuItem::separator()),
        Entry::Submenu { label, entries, .. } => {
            let children = entries.iter().map(build).collect::<Result<Vec<_>, _>>()?;
            let items = children.iter().map(Built::as_item).collect::<Vec<_>>();
            Built::Submenu(Submenu::with_items(escape_ampersands(label), true, &items)?)
        }
    })
}

fn build_menu(entries: &[Entry]) -> Result<Menu, tray_icon::menu::Error> {
    let built = entries.iter().map(build).collect::<Result<Vec<_>, _>>()?;
    let items = built.iter().map(Built::as_item).collect::<Vec<_>>();
    let menu = Menu::new();
    menu.append_items(&items)?;
    Ok(menu)
}

pub fn spawn(actions: async_channel::Sender<TrayAction>) -> Option<Tray> {
    let menu = build_menu(&menu_model(&TrayInfo::default(), true))
        .map_err(|error| log::warn!("tray menu: {error}"))
        .ok()?;

    let icon = Icon::from_resource(ICON_ORDINAL, None)
        .map_err(|error| log::warn!("tray icon resource: {error}"))
        .ok()?;

    MenuEvent::set_event_handler(Some({
        let actions = actions.clone();
        move |event: MenuEvent| {
            let Some(action) = parse_action(event.id.as_ref()) else {
                return;
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

    let icon = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("DeskUnion")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|error| log::warn!("tray icon unavailable: {error}"))
        .ok()?;
    Some(Tray { icon })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tooltip_names_the_application_and_the_status() {
        let info = TrayInfo {
            status: "Client · connected".to_string(),
            ..Default::default()
        };
        assert_eq!(tooltip(&info), "DeskUnion\nClient · connected");
        assert_eq!(tooltip(&TrayInfo::default()), "DeskUnion");
    }

    #[test]
    fn the_tooltip_fits_the_notification_area() {
        let info = TrayInfo {
            status: "x".repeat(500),
            ..Default::default()
        };
        assert_eq!(tooltip(&info).chars().count(), TOOLTIP_LIMIT);
    }
}
