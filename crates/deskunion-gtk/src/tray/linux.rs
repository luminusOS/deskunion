//! StatusNotifierItem with its menu exported over `com.canonical.dbusmenu`,
//! both served by ksni on its own thread. KDE and other hosts show it
//! natively; GNOME needs the AppIndicator extension.

use std::rc::Rc;

use gtk::{gdk, gdk::prelude::TextureExt, gio, glib};
use ksni::{
    MenuItem, OfflineReason, ToolTip,
    blocking::TrayMethods,
    menu::{CheckmarkItem, StandardItem, SubMenu},
};

use super::{
    TrayAction, background,
    menu::{Entry, TrayInfo, escape_underscores, menu_model},
};

const ICON_NAME: &str = "io.github.luminusos.DeskUnion-symbolic";
const ICON_RESOURCE: &str =
    "/io/github/luminusos/DeskUnion/icons/symbolic/apps/io.github.luminusos.DeskUnion-symbolic.svg";
const ICON_SIZE: u32 = 22;

pub struct Tray {
    pixmap: Vec<ksni::Icon>,
    actions: async_channel::Sender<TrayAction>,
    info: TrayInfo,
    window_visible: bool,
}

impl Tray {
    fn emit(&self, action: TrayAction) {
        let _ = self.actions.try_send(action);
    }

    fn render(entry: Entry) -> MenuItem<Self> {
        match entry {
            Entry::Item {
                label,
                icon,
                action,
                enabled,
            } => StandardItem {
                label: escape_underscores(&label),
                icon_name: icon.into(),
                enabled,
                activate: Box::new(move |tray: &mut Self| tray.emit(action)),
                ..Default::default()
            }
            .into(),
            Entry::Check {
                label,
                checked,
                action,
            } => CheckmarkItem {
                label: escape_underscores(&label),
                checked,
                activate: Box::new(move |tray: &mut Self| tray.emit(action)),
                ..Default::default()
            }
            .into(),
            Entry::Separator => MenuItem::Separator,
            Entry::Submenu {
                label,
                icon,
                entries,
            } => SubMenu {
                label: escape_underscores(&label),
                icon_name: icon.into(),
                submenu: entries.into_iter().map(Self::render).collect(),
                ..Default::default()
            }
            .into(),
        }
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "io.github.luminusos.DeskUnion".into()
    }

    fn title(&self) -> String {
        "DeskUnion".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_name(&self) -> String {
        ICON_NAME.into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.pixmap.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "DeskUnion".into(),
            description: self.info.status.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.emit(TrayAction::ToggleWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        menu_model(&self.info, self.window_visible)
            .into_iter()
            .map(Self::render)
            .collect()
    }

    fn watcher_online(&self) {
        self.emit(TrayAction::Shown(true));
    }

    fn watcher_offline(&self, _reason: OfflineReason) -> bool {
        // Keep serving: the host may come back, as when GNOME's extension is
        // switched off and on again.
        self.emit(TrayAction::Shown(false));
        true
    }
}

/// The bundled symbolic icon as ARGB32. The name resolves through the host's
/// icon theme where the app is installed (Flatpak); when it is missing there
/// (`cargo run`, AppImage) hosts draw these pixels instead. A pixmap cannot be
/// recoloured by the host, so it is light on a dark theme and dark on a light one.
fn pixmap(dark_theme: bool) -> Vec<ksni::Icon> {
    let render = || -> Option<ksni::Icon> {
        let data = gio::resources_lookup_data(ICON_RESOURCE, gio::ResourceLookupFlags::NONE)
            .map_err(|error| log::warn!("tray icon resource: {error}"))
            .ok()?;
        let size = ICON_SIZE.to_string();
        let svg = std::str::from_utf8(&data)
            .ok()?
            .replace("#2e3436", if dark_theme { "#eeeeec" } else { "#2e3436" })
            .replacen(
                r#"width="16" height="16""#,
                &format!(r#"width="{size}" height="{size}""#),
                1,
            );
        let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(svg.into_bytes()))
            .map_err(|error| log::warn!("tray pixmap: {error}"))
            .ok()?;
        let mut downloader = gdk::TextureDownloader::new(&texture);
        downloader.set_format(gdk::MemoryFormat::A8r8g8b8);
        let (bytes, stride) = downloader.download_bytes();
        let (width, height) = (texture.width() as usize, texture.height() as usize);
        let mut data = Vec::with_capacity(width * height * 4);
        for row in bytes.chunks(stride).take(height) {
            data.extend_from_slice(&row[..width * 4]);
        }
        Some(ksni::Icon {
            width: width as i32,
            height: height as i32,
            data,
        })
    };
    render().into_iter().collect()
}

pub struct TrayHandle {
    handle: Rc<ksni::blocking::Handle<Tray>>,
}

impl TrayHandle {
    pub fn update(&self, info: &TrayInfo, window_visible: bool) {
        let info = info.clone();
        self.handle.update(move |tray| {
            tray.info = info;
            tray.window_visible = window_visible;
        });
    }
}

pub fn spawn(actions: async_channel::Sender<TrayAction>) -> Option<TrayHandle> {
    let style = adw::StyleManager::default();
    let tray = Tray {
        pixmap: pixmap(style.is_dark()),
        actions,
        info: TrayInfo::default(),
        window_visible: true,
    };
    // Flatpak cannot own the org.kde.StatusNotifierItem-* name.
    let handle = match tray
        .disable_dbus_name(background::sandboxed())
        .assume_sni_available(true)
        .spawn()
    {
        Ok(handle) => Rc::new(handle),
        Err(error) => {
            log::warn!("tray icon unavailable: {error}");
            return None;
        }
    };
    style.connect_dark_notify({
        let handle = handle.clone();
        move |style| {
            let pixmap = pixmap(style.is_dark());
            handle.update(move |tray| tray.pixmap = pixmap);
        }
    });
    Some(TrayHandle { handle })
}
