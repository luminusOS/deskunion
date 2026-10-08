use super::*;
use std::io::Read;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::{Duration, Instant};

fn settle() {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn capture(window: &adw::ApplicationWindow, name: &str) {
    let Ok(directory) = std::env::var("DESKUNION_UI_CAPTURE_DIR") else {
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    let (node, width, height) = loop {
        let width = window.width();
        let height = window.height();
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(window)).snapshot(&snapshot, width as f64, height as f64);
        if let Some(node) = snapshot.to_node() {
            break (node, width, height);
        }
        assert!(Instant::now() < deadline, "window did not render {name}");
        // X11 presentation and the first render node arrive asynchronously.
        settle();
    };
    let renderer = gtk::gsk::CairoRenderer::new();
    renderer.realize(None::<&gtk::gdk::Surface>).unwrap();
    let bounds = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    renderer
        .render_texture(&node, Some(&bounds))
        .save_to_png(Path::new(&directory).join(format!("{name}.png")))
        .unwrap();
    renderer.unrealize();
}

#[test]
fn sharing_workflow_preserves_intent_feedback_and_navigation() {
    adw::init().expect("ui-tests requires a graphical display");
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    gtk::gio::resources_register_include!("deskunion.gresource").unwrap();
    crate::load_css();
    crate::load_icons();
    let app = adw::Application::builder()
        .application_id("io.github.luminusos.DeskUnion.UITest")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();

    let directory = std::env::temp_dir().join(format!("deskunion-ui-test-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let socket_path = directory.join("deskunion-socket.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let original_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR");
    std::env::set_var("XDG_RUNTIME_DIR", &directory);
    let (_events, writer) = deskunion_ipc::connect().unwrap();
    if let Some(original) = original_runtime_dir {
        std::env::set_var("XDG_RUNTIME_DIR", original);
    } else {
        std::env::remove_var("XDG_RUNTIME_DIR");
    }
    let (mut connection, _) = listener.accept().unwrap();
    connection
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    let controller = AppModel::builder().launch(AppInit { app, writer }).detach();
    let window = controller.widget();
    window.present();
    controller.emit(AppMsg::Frontend(FrontendEvent::OperationMode(
        OperationMode::Unconfigured,
    )));
    controller.emit(AppMsg::Frontend(FrontendEvent::PublicKeyFingerprint(
        (0..32)
            .map(|n| format!("{n:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )));
    settle();
    capture(window, "first-run-after");

    let mut buffer = [0; 4096];
    let count = connection.read(&mut buffer).unwrap();
    let requests = std::str::from_utf8(&buffer[..count]).unwrap();
    assert!(
        !requests.contains("SetOperationMode"),
        "first run must not choose a role: {requests}"
    );

    controller.emit(AppMsg::SetOperationMode(OperationMode::Client));
    settle();
    assert_eq!(controller.model().operation_mode, OperationMode::Client);
    assert!(!controller.widgets().server_connect_button.is_sensitive());
    controller
        .widgets()
        .server_host_entry
        .set_text("workstation.local");
    settle();
    controller
        .widgets()
        .server_host_entry
        .emit_by_name::<()>("entry-activated", &[]);
    settle();
    let pending = controller.model().pending_server_test.clone().unwrap();
    assert!(!controller.widgets().server_connect_button.is_sensitive());
    assert!(!controller.widgets().server_host_entry.is_sensitive());
    capture(window, "connection-checking");

    controller.emit(AppMsg::ServerConnect);
    settle();
    assert_eq!(
        controller.model().pending_server_test.as_ref().unwrap().0,
        pending.0
    );
    controller.emit(AppMsg::Frontend(FrontendEvent::ConnectionTested {
        request_id: pending.0.wrapping_add(1),
        error: Some("Stale result".to_string()),
    }));
    settle();
    assert!(controller.model().pending_server_test.is_some());
    assert!(controller.model().server_connection_error.is_none());

    controller.emit(AppMsg::Frontend(FrontendEvent::ConnectionTested {
        request_id: pending.0,
        error: Some("Server <offline>. Check its address and input sharing status.".to_string()),
    }));
    settle();
    assert!(controller.model().pending_server_test.is_none());
    assert!(
        controller
            .widgets()
            .server_connection_error_row
            .is_visible()
    );
    assert!(controller.widgets().server_connect_button.is_sensitive());
    assert_eq!(
        controller.widgets().server_host_entry.text(),
        "workstation.local"
    );
    capture(window, "connection-error");

    window.set_default_size(600, 750);
    settle();
    assert!(
        window.width() <= 600,
        "window did not shrink: {}",
        window.width()
    );
    assert!(controller.widgets().split_view.is_collapsed());
    assert!(!controller.widgets().split_view.shows_sidebar());
    controller.widgets().sidebar_toggle.set_active(true);
    settle();
    assert!(controller.widgets().split_view.shows_sidebar());
    let nav = controller.widgets().nav_list.clone();
    let settings_row = nav.row_at_index(3).unwrap();
    nav.select_row(Some(&settings_row));
    nav.emit_by_name::<()>("row-activated", &[&settings_row]);
    settle();
    assert_eq!(controller.model().current_page.get(), Page::Settings);
    assert!(!controller.widgets().split_view.shows_sidebar());
    assert!(window.child_focus(gtk::DirectionType::TabForward));

    controller.widgets().port_entry.set_text("4243");
    settle();
    controller.emit(AppMsg::PortEditApply);
    controller.emit(AppMsg::Frontend(FrontendEvent::PortChanged(4243, None)));
    settle();
    assert_eq!(controller.model().port, 4243);
    assert!(!controller.model().port_editing);

    controller.widgets().port_entry.set_text("65536");
    settle();
    controller.emit(AppMsg::PortEditApply);
    settle();
    assert!(controller.model().port_error.is_some());
    assert_eq!(controller.model().port, 4243);
    capture(window, "settings-invalid-port");
    controller.emit(AppMsg::PortEditCancel);
    settle();
    assert!(controller.model().port_error.is_none());
    assert!(
        !controller.model().port_editing,
        "programmatic reset must not restart editing"
    );
    assert_eq!(controller.widgets().port_entry.text(), "4243");

    window.set_default_size(480, 750);
    settle();
    assert!(
        window.width() <= 480,
        "minimum width is too large: {}",
        window.width()
    );
    let styles = adw::StyleManager::default();
    styles.set_color_scheme(adw::ColorScheme::ForceLight);
    settle();
    capture(window, "settings-narrow-light");
    styles.set_color_scheme(adw::ColorScheme::ForceDark);
    settle();
    capture(window, "settings-narrow-dark");
    for page in [Page::Audio, Page::Logs, Page::Screens] {
        let row = nav.row_at_index(page.nav_index()).unwrap();
        nav.emit_by_name::<()>("row-activated", &[&row]);
        settle();
        assert_eq!(controller.model().current_page.get(), page);
        if page == Page::Audio {
            assert!(controller.widgets().audio_receive_switch.is_visible());
            controller.emit(AppMsg::Frontend(FrontendEvent::ClipboardStatus {
                enabled: true,
                restart_required: true,
            }));
            settle();
            assert!(controller.model().clipboard_enabled);
            assert!(controller.model().clipboard_restart_required);
            assert!(controller.widgets().clipboard_switch.is_active());
            assert!(
                controller
                    .widgets()
                    .toast_overlay
                    .measure(gtk::Orientation::Horizontal, -1)
                    .0
                    <= window.width(),
                "audio page content must fit a narrow window"
            );
        }
        capture(window, &format!("{}-narrow-dark", page.name()));
    }

    nav.emit_by_name::<()>("row-activated", &[&nav.row_at_index(2).unwrap()]);
    settle();
    let settings = gtk::Settings::default().unwrap();
    let font = settings.gtk_font_name();
    settings.set_gtk_font_name(Some("Cantarell 16"));
    settle();
    assert!(
        controller
            .widgets()
            .toast_overlay
            .measure(gtk::Orientation::Horizontal, -1)
            .0
            <= window.width(),
        "content must fit with large text"
    );
    assert!(
        window.width() <= 480,
        "large text must not force a wider window"
    );
    capture(window, "logs-large-text-dark");
    settings.set_gtk_font_name(font.as_deref());
    nav.emit_by_name::<()>("row-activated", &[&nav.row_at_index(0).unwrap()]);
    settle();
    // Focus restoration alone must not activate another destination.
    nav.select_row(nav.row_at_index(1).as_ref());
    settle();
    assert_eq!(controller.model().current_page.get(), Page::Screens);
    assert_eq!(nav.selected_row().unwrap().index(), 0);

    window.set_default_size(1100, 750);
    settle();
    assert!(!controller.widgets().split_view.is_collapsed());
    assert_eq!(controller.model().current_page.get(), Page::Screens);
    controller.emit(AppMsg::SetOperationMode(OperationMode::Server));
    settle();
    assert!(controller.model().server_connection_error.is_none());
    capture(window, "screens-server-dark");
    assert_eq!(controller.model().status_text(), "Sharing stopped");
    assert!(
        controller
            .widgets()
            .service_toggle
            .has_css_class("suggested-action")
    );
    controller.emit(AppMsg::Frontend(FrontendEvent::CaptureStatus(
        Status::Enabled,
    )));
    settle();
    assert!(
        controller
            .widgets()
            .service_toggle
            .has_css_class("destructive-action")
    );
    assert_eq!(controller.model().status_text(), "Listening on port 4243");
    assert_eq!(
        controller
            .widgets()
            .service_toggle
            .tooltip_text()
            .as_deref(),
        Some("Stop input sharing")
    );
    controller.emit(AppMsg::Frontend(FrontendEvent::CaptureStatus(
        Status::Disabled,
    )));
    settle();
    assert!(
        controller
            .widgets()
            .service_toggle
            .has_css_class("suggested-action")
    );
    assert_eq!(controller.model().status_text(), "Capture disabled");
    assert_eq!(
        controller
            .widgets()
            .service_toggle
            .tooltip_text()
            .as_deref(),
        Some("Start input sharing")
    );
    styles.set_color_scheme(adw::ColorScheme::ForceLight);
    settle();
    capture(window, "screens-server-light");

    window.set_default_size(1100, 360);
    settle();
    assert!(
        controller
            .widgets()
            .toast_overlay
            .measure(gtk::Orientation::Vertical, window.width())
            .0
            <= window.height(),
        "content must fit at the minimum height"
    );
    capture(window, "screens-short-light");

    window.close();
    drop(controller);
    drop(connection);
    drop(listener);
    std::fs::remove_file(socket_path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
