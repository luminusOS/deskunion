mod app;
mod dialogs;
mod factories;
#[cfg(target_os = "macos")]
mod macos_privacy;
#[cfg(target_os = "macos")]
mod macos_status_item;
mod screen_arrangement;

use std::{env, process, str};

use gtk::CssProvider;
use relm4::prelude::*;

use adw::Application;
use gtk::{IconTheme, gdk::Display, prelude::*};
use gtk::{gio, glib, prelude::ApplicationExt};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum GtkError {
    #[error("gtk frontend exited with non zero exit code: {0}")]
    NonZeroExitCode(i32),
}

pub fn run() -> Result<(), GtkError> {
    log::debug!("running gtk frontend");

    #[cfg(windows)]
    configure_windows_runtime();

    #[cfg(windows)]
    let ret = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024) // https://gitlab.gnome.org/GNOME/gtk/-/commit/52dbb3f372b2c3ea339e879689c1de535ba2c2c3 -> caused crash on windows
        .name("gtk".into())
        .spawn(gtk_main)
        .unwrap()
        .join()
        .unwrap();
    #[cfg(not(windows))]
    let ret = gtk_main();

    match ret {
        glib::ExitCode::SUCCESS => Ok(()),
        e => Err(GtkError::NonZeroExitCode(e.get() as i32)),
    }
}

#[cfg(windows)]
fn configure_windows_runtime() {
    if env::var_os("GSK_RENDERER").is_none() {
        // The OpenGL renderer may select EGL/ANGLE even when the Windows guest
        // cannot expose a usable native surface (common with virtual GPUs).
        // Cairo keeps the UI functional, while an explicit user setting can
        // still opt back into a hardware renderer.
        unsafe { env::set_var("GSK_RENDERER", "cairo") };
    }

    let Ok(exe) = env::current_exe() else {
        return;
    };
    let Some(exe_dir) = exe.parent() else {
        return;
    };
    let root = if exe_dir.file_name().is_some_and(|name| name == "bin") {
        exe_dir.parent().unwrap_or(exe_dir)
    } else {
        exe_dir
    };
    let share = root.join("share");
    if !share.exists() {
        return;
    }

    let vars = [
        ("XDG_DATA_DIRS", share.clone()),
        (
            "GSETTINGS_SCHEMA_DIR",
            share.join("glib-2.0").join("schemas"),
        ),
        (
            "GDK_PIXBUF_MODULEDIR",
            root.join("lib")
                .join("gdk-pixbuf-2.0")
                .join("2.10.0")
                .join("loaders"),
        ),
        (
            "GIO_MODULE_DIR",
            root.join("lib").join("gio").join("modules"),
        ),
        (
            "SSL_CERT_FILE",
            root.join("ssl").join("certs").join("ca-bundle.crt"),
        ),
    ];

    for (key, value) in vars {
        if value.exists() {
            // SAFETY: this runs once, before GTK starts its worker thread.
            unsafe { env::set_var(key, value) };
        }
    }
}

fn gtk_main() -> glib::ExitCode {
    #[cfg(target_os = "macos")]
    {
        configure_macos_bundle_environment();
        install_macos_gtk_log_filter();
    }

    gio::resources_register_include!("deskunion.gresource").expect("Failed to register resources.");

    let app = Application::builder()
        .application_id("io.github.luminusos.DeskUnion")
        .build();

    app.connect_startup(|app| {
        load_css();
        load_icons();
        setup_actions(app);
        setup_menu(app);
    });
    app.connect_activate(build_ui);

    let args: Vec<&'static str> = vec![];
    app.run_with_args(&args)
}

#[cfg(target_os = "macos")]
fn install_macos_gtk_log_filter() {
    glib::log_set_writer_func(|level, fields| {
        if level == glib::LogLevel::Warning && is_gtk_theme_parser_warning(fields) {
            return glib::LogWriterOutput::Handled;
        }

        glib::log_writer_default(level, fields)
    });
}

#[cfg(target_os = "macos")]
fn is_gtk_theme_parser_warning(fields: &[glib::LogField<'_>]) -> bool {
    let mut domain = None;
    let mut message = None;

    for field in fields {
        match field.key() {
            "GLIB_DOMAIN" => domain = field.value_str(),
            "MESSAGE" => message = field.value_str(),
            _ => {}
        }
    }

    domain == Some("Gtk")
        && message.is_some_and(|message| message.starts_with("Theme parser warning: gtk.css:"))
}

#[cfg(target_os = "macos")]
fn configure_macos_bundle_environment() {
    let Ok(exe) = env::current_exe() else {
        return;
    };
    let Some(contents) = exe
        .parent()
        .and_then(|dir| dir.parent())
        .map(std::path::Path::to_owned)
    else {
        return;
    };

    let share = contents.join("Resources").join("share");
    if !share.exists() {
        return;
    }

    let schemas = share.join("glib-2.0").join("schemas");
    if schemas.exists() {
        env::set_var("GSETTINGS_SCHEMA_DIR", schemas);
    }

    env::set_var("XDG_DATA_DIRS", &share);
    env::set_var(
        "GTK_DATA_PREFIX",
        contents.join("Resources").to_string_lossy().as_ref(),
    );
}

fn load_css() {
    let provider = CssProvider::default();
    provider.load_from_resource("io/github/luminusos/DeskUnion/style.css");
    gtk::style_context_add_provider_for_display(
        &Display::default().expect("Could not connect to a display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn load_icons() {
    let display = &Display::default().expect("Could not connect to a display.");
    let icon_theme = IconTheme::for_display(display);
    icon_theme.add_resource_path("/io/github/luminusos/DeskUnion/icons");
}

// Add application actions
fn setup_actions(app: &adw::Application) {
    // Quit action
    // This is important on macOS, where users expect a File->Quit action with a Cmd+Q shortcut.
    let quit_action = gio::SimpleAction::new("quit", None);
    quit_action.connect_activate({
        let app = app.clone();
        move |_, _| {
            app.quit();
        }
    });
    app.add_action(&quit_action);
}

// Set up a global menu
//
// Currently this is used only on macOS
fn setup_menu(app: &adw::Application) {
    let menu = gio::Menu::new();

    let file_menu = gio::Menu::new();
    file_menu.append(Some("Quit"), Some("app.quit"));
    menu.append_submenu(Some("_File"), &file_menu);

    app.set_menubar(Some(&menu))
}

fn build_ui(app: &Application) {
    log::debug!("connecting to deskunion-socket");
    let (mut frontend_rx, writer) = match deskunion_ipc::connect() {
        Ok(conn) => conn,
        Err(e) => {
            log::error!("{e}");
            process::exit(1);
        }
    };
    log::debug!("connected to deskunion-socket");

    let mut controller = app::AppModel::builder()
        .launch(app::AppInit {
            app: app.clone(),
            writer,
        })
        .detach();
    // this is the app's single root component, living exactly as long as
    // the process — give its runtime a static lifetime so dropping
    // `controller` at the end of this function doesn't tear it down.
    controller.detach_runtime();

    let window = controller.widget().clone();
    let app_sender = controller.sender().clone();

    // bridge the sync IPC event reader into `AppMsg::Frontend`, exactly
    // mirroring the pre-Relm4 gio::spawn_blocking + async_channel pump.
    let (sender, receiver) = async_channel::bounded(10);
    gio::spawn_blocking(move || {
        while let Some(e) = frontend_rx.next_event() {
            match e {
                Ok(e) => sender.send_blocking(e).unwrap(),
                Err(e) => {
                    log::error!("{e}");
                    break;
                }
            }
        }
    });
    glib::spawn_future_local(async move {
        loop {
            let event = receiver.recv().await.unwrap_or_else(|_| process::exit(1));
            if app_sender.send(app::AppMsg::Frontend(event)).is_err() {
                process::exit(1);
            }
        }
    });

    #[cfg(target_os = "macos")]
    {
        window.connect_close_request(|window| {
            window.set_visible(false);
            glib::Propagation::Stop
        });
        macos_status_item::setup(app, &window);
        // Permission prompts are initiated by the backend selected for the
        // current operation mode. Prompting here would ask for capture and
        // emulation access before the service has synchronized that choice.
        // Watch the Accessibility grant continuously for the lifetime
        // of the process. On a grant, swap the warning row into its
        // "relaunch required" state (the daemon subprocess already
        // bailed and can't recover without a restart). On a REVOKE,
        // quit immediately — an active CGEventTap at
        // HeadInsertEventTap can wedge system input if the process
        // lingers after losing AX, and forcing the process to exit is
        // the only bulletproof way to guarantee the kernel tears the
        // tap down.
        let window_weak = window.downgrade();
        let app_weak = app.downgrade();
        let sender_for_privacy = controller.sender().clone();
        macos_privacy::watch_accessibility_state(move |change| match change {
            macos_privacy::AccessibilityChange::Granted => {
                if let Some(window) = window_weak.upgrade() {
                    window.present();
                }
                let _ = sender_for_privacy.send(app::AppMsg::MacosAccessibilityGranted);
            }
            macos_privacy::AccessibilityChange::Revoked => {
                log::warn!("Accessibility revoked — quitting to avoid wedging system input");
                let _ = sender_for_privacy.send(app::AppMsg::MacosAccessibilityRevoked);
                if let Some(app) = app_weak.upgrade() {
                    app.quit();
                }
            }
        });
    }

    #[cfg(not(target_os = "macos"))]
    window.present();

    // On macOS, default to presenting the main window on every launch
    // so the user gets a visible confirmation that the app is running
    // — including the post-grant relaunch and normal Dock/Finder/`open`
    // launches. Opt out by setting `DESKUNION_HIDDEN=1` in the
    // environment (useful for a LaunchAgent / login-item configuration
    // where the user wants the app to come up quietly into the menu
    // bar only, with no window on boot).
    #[cfg(target_os = "macos")]
    if env::var_os("DESKUNION_HIDDEN").is_none() {
        window.present();
    }
}
