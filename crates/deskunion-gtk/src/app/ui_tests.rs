use super::*;
use deskunion_ipc::{ClientState, DEFAULT_PORT, Position, Status};
use gtk::glib;
use session::ClientEmulation;
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

/// every widget below `widget`, including internal children such as a
/// banner's label
fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut child = widget.first_child();
    while let Some(current) = child {
        found.push(current.clone());
        found.extend(descendants(&current));
        child = current.next_sibling();
    }
    found
}

fn labels_with_text(widget: &impl IsA<gtk::Widget>, text: &str) -> usize {
    descendants(widget.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
        .filter(|label| label.is_mapped() && label.label() == text)
        .count()
}

/// the requests the window sent since the last call
fn drain(connection: &mut std::os::unix::net::UnixStream) -> String {
    let mut sent = Vec::new();
    let mut buffer = [0; 4096];
    while let Ok(count) = connection.read(&mut buffer) {
        if count == 0 {
            break;
        }
        sent.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8_lossy(&sent).into_owned()
}

/// the requested window width covers the client-side decoration, which the
/// allocated content width excludes by a compositor-dependent margin
fn fits(window: &adw::ApplicationWindow, widget: &impl IsA<gtk::Widget>) -> bool {
    widget.measure(gtk::Orientation::Horizontal, -1).0 <= window.width_request()
}

#[test]
fn sharing_workflow_preserves_intent_feedback_and_navigation() {
    adw::init().expect("ui-tests requires a graphical display");
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    gtk::gio::resources_register_include!("deskunion.gresource").unwrap();
    crate::load_css();
    crate::load_icons();
    // GNOME shows only Close; a bare X11 session defaults to all three
    gtk::Settings::default()
        .unwrap()
        .set_gtk_decoration_layout(Some(":close"));
    let app = adw::Application::builder()
        .application_id("io.github.luminusos.DeskUnion.UITest")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();

    let directory = std::env::temp_dir().join(format!("deskunion-ui-test-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let socket_path = directory.join("deskunion-socket.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let original_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR");
    // SAFETY: this is the only test in the crate that touches `XDG_RUNTIME_DIR`, and no
    // other thread reads it while the variable is swapped.
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", &directory) };
    let (_events, writer) = deskunion_ipc::connect().unwrap();
    if let Some(original) = original_runtime_dir {
        // SAFETY: as above.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", original) };
    } else {
        // SAFETY: as above.
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }
    let (mut connection, _) = listener.accept().unwrap();
    connection
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    let controller = AppModel::builder().launch(AppInit { app, writer }).detach();
    let window = controller.widget();
    window.present();
    let fingerprint = (0..32)
        .map(|n| format!("{n:02x}"))
        .collect::<Vec<_>>()
        .join(":");
    controller.emit(AppMsg::Frontend(FrontendEvent::OperationMode(
        OperationMode::Unconfigured,
    )));
    controller.emit(AppMsg::Frontend(FrontendEvent::PublicKeyFingerprint(
        fingerprint.clone(),
    )));
    settle();
    capture(window, "first-run-after");
    // the role is chosen on the first-run page, not in Settings
    assert!(!controller.model().settings.widgets().mode_row.is_mapped());

    let mut buffer = [0; 4096];
    let count = connection.read(&mut buffer).unwrap();
    let requests = std::str::from_utf8(&buffer[..count]).unwrap();
    assert!(
        !requests.contains("SetOperationMode"),
        "first run must not choose a role: {requests}"
    );

    // the sidebar only navigates: no role switch or status line next to it
    let sidebar = controller.widgets().split_view.sidebar().unwrap();
    assert!(
        descendants(sidebar.upcast_ref())
            .iter()
            .all(|widget| !widget.is::<gtk::ToggleButton>()
                || widget.is::<gtk::MenuButton>()
                || widget
                    .parent()
                    .is_some_and(|parent| parent.is::<gtk::MenuButton>())),
        "the sidebar must not hold the role switch"
    );

    controller.emit(AppMsg::Frontend(FrontendEvent::ConnectionAttempt {
        fingerprint: fingerprint.clone(),
    }));
    settle();
    controller.emit(AppMsg::AuthorizationConfirmed(
        controller.model().authorization_dialog_generation,
        fingerprint.clone(),
    ));
    settle();
    controller
        .model()
        .fingerprint_dialog
        .as_ref()
        .unwrap()
        .widget()
        .close();
    settle();
    assert!(
        controller
            .model()
            .pending_authorization_fingerprint
            .is_none()
    );
    controller.emit(AppMsg::Frontend(FrontendEvent::ConnectionAttempt {
        fingerprint,
    }));
    settle();
    assert!(controller.model().authorization_dialog.is_some());
    let device_a = controller
        .model()
        .pending_authorization_fingerprint
        .clone()
        .unwrap();
    controller.emit(AppMsg::AuthorizationConfirmed(
        controller.model().authorization_dialog_generation,
        device_a,
    ));
    settle();
    let device_b = (1..33)
        .map(|n| format!("{n:02x}"))
        .collect::<Vec<_>>()
        .join(":");
    controller.emit(AppMsg::Frontend(FrontendEvent::ConnectionAttempt {
        fingerprint: device_b.clone(),
    }));
    settle();
    controller
        .model()
        .fingerprint_dialog
        .as_ref()
        .unwrap()
        .widget()
        .close();
    settle();
    assert_eq!(
        controller
            .model()
            .pending_authorization_fingerprint
            .as_deref(),
        Some(device_b.as_str())
    );
    controller.emit(AppMsg::AuthorizationCancelled(
        controller.model().authorization_dialog_generation,
    ));
    settle();
    assert!(
        window.visible_dialog().is_none(),
        "cancelled authorization must not leave a modal open"
    );

    let mode_row = controller.model().settings.widgets().mode_row.clone();
    controller.emit(AppMsg::Page(PageOutput::SetMode(OperationMode::Server)));
    settle();
    assert_eq!(controller.model().session.mode, OperationMode::Server);
    assert_eq!(mode_row.selected(), 0);
    mode_row.set_selected(1);
    settle();
    assert_eq!(controller.model().session.mode, OperationMode::Client);

    let connections = controller
        .model()
        .connections
        .widgets()
        .server_connect_button
        .clone();
    let host_entry = controller
        .model()
        .connections
        .widgets()
        .server_host_entry
        .clone();
    let error_row = controller
        .model()
        .connections
        .widgets()
        .connection_error_row
        .clone();
    assert!(!connections.is_sensitive());
    host_entry.set_text("workstation.local");
    settle();
    host_entry.emit_by_name::<()>("entry-activated", &[]);
    settle();
    let pending = controller
        .model()
        .connections
        .model()
        .pending_test()
        .cloned()
        .unwrap();
    assert!(!connections.is_sensitive());
    assert!(!host_entry.is_sensitive());
    let spinner = controller
        .model()
        .connections
        .widgets()
        .server_connect_spinner
        .clone();
    assert!(spinner.is_spinning(), "checking the server shows activity");
    capture(window, "connection-checking");

    controller
        .model()
        .connections
        .emit(pages::connections::ConnectionsMsg::ServerConnect);
    settle();
    assert_eq!(
        controller
            .model()
            .connections
            .model()
            .pending_test()
            .unwrap()
            .id,
        pending.id
    );
    controller.emit(AppMsg::Frontend(FrontendEvent::ConnectionTested {
        request_id: pending.id.wrapping_add(1),
        error: Some("Stale result".to_string()),
    }));
    settle();
    assert!(
        controller
            .model()
            .connections
            .model()
            .pending_test()
            .is_some()
    );
    assert!(
        controller
            .model()
            .connections
            .model()
            .connection_error()
            .is_none()
    );

    controller.emit(AppMsg::Frontend(FrontendEvent::ConnectionTested {
        request_id: pending.id,
        error: Some("No response from workstation.local:4242".to_string()),
    }));
    settle();
    assert!(
        controller
            .model()
            .connections
            .model()
            .pending_test()
            .is_none()
    );
    assert!(!spinner.is_spinning());
    assert!(error_row.is_visible());
    assert!(connections.is_sensitive());
    assert!(connections.has_css_class("suggested-action"));
    assert_eq!(host_entry.text(), "workstation.local");
    capture(window, "connection-error");
    assert_eq!(
        descendants(window.upcast_ref())
            .iter()
            .filter(|w| w.is_mapped() && w.has_css_class("suggested-action"))
            .count(),
        1,
        "one primary action per view"
    );
    drain(&mut connection);

    // a verified server is saved and sharing starts
    host_entry.emit_by_name::<()>("entry-activated", &[]);
    settle();
    let pending = controller
        .model()
        .connections
        .model()
        .pending_test()
        .cloned()
        .unwrap();
    controller.emit(AppMsg::Frontend(FrontendEvent::ConnectionTested {
        request_id: pending.id,
        error: None,
    }));
    settle();
    let requests = drain(&mut connection);
    assert!(requests.contains("SetServer"), "{requests}");
    assert!(requests.contains("SetServiceRunning"), "{requests}");
    assert!(controller.model().session.wanted_running);
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerEndpoint {
        hostname: Some("workstation.local".to_owned()),
        ips: Vec::new(),
        port: DEFAULT_PORT,
    }));
    settle();
    assert!(!connections.is_visible(), "Start/Stop runs a saved server");
    // a resync of the same endpoint keeps what the user is typing
    host_entry.set_text("laptop.local");
    settle();
    assert!(connections.is_visible());
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerEndpoint {
        hostname: Some("workstation.local".to_owned()),
        ips: Vec::new(),
        port: DEFAULT_PORT,
    }));
    settle();
    assert_eq!(host_entry.text(), "laptop.local");
    controller.emit(AppMsg::Tray(crate::tray::TrayCommand::SetRunning(false)));
    settle();
    assert!(!controller.model().session.wanted_running);

    // narrow: the sidebar fills the window and a page opens over it
    let split_view = controller.widgets().split_view.clone();
    window.set_default_size(600, 750);
    settle();
    assert!(
        window.width() <= 600,
        "window did not shrink: {}",
        window.width()
    );
    assert!(split_view.is_collapsed());
    assert!(
        !split_view.shows_content(),
        "a collapsed window starts on the sidebar"
    );
    // the collapse animates; wait for the sidebar to settle
    let deadline = Instant::now() + Duration::from_secs(3);
    while sidebar.width() != window.width() && Instant::now() < deadline {
        settle();
    }
    assert_eq!(
        sidebar.width(),
        window.width(),
        "the sidebar fills the window"
    );
    capture(window, "sidebar-narrow-dark");
    let nav = controller.widgets().nav_list.clone();
    let settings_row = nav.row_at_index(Page::Settings.index()).unwrap();
    nav.select_row(Some(&settings_row));
    nav.emit_by_name::<()>("row-activated", &[&settings_row]);
    settle();
    assert_eq!(controller.model().page.get(), Page::Settings);
    assert!(split_view.shows_content());
    assert_eq!(split_view.content().unwrap().title(), "Settings");
    assert!(window.child_focus(gtk::DirectionType::TabForward));
    // the header bar's back button returns to the sidebar
    let settings_page = controller.model().settings.widget().clone();
    assert!(
        settings_page
            .activate_action("navigation.pop", None)
            .is_ok()
    );
    settle();
    assert!(!split_view.shows_content());
    nav.emit_by_name::<()>("row-activated", &[&settings_row]);
    settle();
    assert!(split_view.shows_content());

    controller.emit(AppMsg::Page(PageOutput::SetMode(OperationMode::Server)));
    settle();
    let port_row = controller.model().settings.widgets().port_row.clone();
    assert!(port_row.is_mapped());
    drain(&mut connection);
    assert_eq!(port_row.text(), DEFAULT_PORT.to_string());
    port_row.set_text("4243");
    settle();
    port_row.emit_by_name::<()>("apply", &[]);
    settle();
    let requests = drain(&mut connection);
    assert!(
        requests.contains("ChangePort") && requests.contains("4243"),
        "{requests}"
    );
    controller.emit(AppMsg::Frontend(FrontendEvent::PortChanged(4243, None)));
    settle();
    assert_eq!(controller.model().settings.model().port(), 4243);
    assert_eq!(controller.model().session.port, 4243);

    port_row.set_text("65536");
    settle();
    port_row.emit_by_name::<()>("apply", &[]);
    settle();
    assert!(controller.model().settings.model().port_invalid());
    assert!(port_row.has_css_class("error"));
    assert!(port_row.tooltip_text().is_some());
    assert_eq!(controller.model().settings.model().port(), 4243);
    capture(window, "settings-invalid-port");
    // Escape drops the edit
    port_row.set_text("5000");
    settle();
    controller
        .model()
        .settings
        .emit(pages::settings::SettingsMsg::PortReset);
    settle();
    assert_eq!(port_row.text(), "4243");
    assert!(!controller.model().settings.model().port_invalid());

    window.set_default_size(360, 750);
    settle();
    assert!(
        window.width() <= 360,
        "minimum width is too large: {}",
        window.width()
    );
    let styles = adw::StyleManager::default();
    styles.set_color_scheme(adw::ColorScheme::ForceLight);
    settle();
    capture(window, "settings-narrow-light");
    styles.set_color_scheme(adw::ColorScheme::ForceDark);
    settle();
    for page in [Page::Audio, Page::Logs, Page::Settings, Page::Connections] {
        let row = nav.row_at_index(page.index()).unwrap();
        nav.emit_by_name::<()>("row-activated", &[&row]);
        settle();
        assert_eq!(controller.model().page.get(), page);
        assert!(split_view.shows_content());
        assert!(
            fits(window, &controller.widgets().toast_overlay),
            "{} must fit a narrow window",
            page.name()
        );
        if page == Page::Settings {
            controller.emit(AppMsg::Frontend(FrontendEvent::ClipboardStatus {
                enabled: true,
                restart_required: true,
            }));
            settle();
            assert_eq!(
                controller.model().settings.model().clipboard(),
                (true, true)
            );
        }
        capture(window, &format!("{}-narrow-dark", page.name()));
    }

    nav.emit_by_name::<()>(
        "row-activated",
        &[&nav.row_at_index(Page::Logs.index()).unwrap()],
    );
    settle();
    assert!(controller.model().logs.model().has_visible_entries());
    let settings = gtk::Settings::default().unwrap();
    let font = settings.gtk_font_name();
    settings.set_gtk_font_name(Some("Cantarell 16"));
    settle();
    assert!(
        fits(window, &controller.widgets().toast_overlay),
        "content must fit with large text"
    );
    assert!(
        window.width() <= 360,
        "large text must not force a wider window"
    );
    capture(window, "logs-large-text-dark");
    settings.set_gtk_font_name(font.as_deref());
    nav.emit_by_name::<()>(
        "row-activated",
        &[&nav.row_at_index(Page::Connections.index()).unwrap()],
    );
    settle();
    // focus restoration alone must not activate another destination
    nav.select_row(nav.row_at_index(Page::Audio.index()).as_ref());
    settle();
    assert_eq!(controller.model().page.get(), Page::Connections);
    assert_eq!(
        nav.selected_row().unwrap().index(),
        Page::Connections.index()
    );

    window.set_default_size(1100, 750);
    settle();
    assert!(!split_view.is_collapsed());
    controller.emit(AppMsg::Page(PageOutput::SetMode(OperationMode::Server)));
    settle();
    let arrangement = controller
        .model()
        .connections
        .widgets()
        .screen_arrangement
        .clone();
    assert_eq!(arrangement.height_request(), 190);
    let connections_page = controller.model().connections.sender().clone();
    connections_page.emit(pages::connections::ConnectionsMsg::ToggleArrangementSize);
    settle();
    assert_eq!(arrangement.height_request(), 320);
    connections_page.emit(pages::connections::ConnectionsMsg::ToggleArrangementSize);
    settle();
    assert_eq!(arrangement.height_request(), 190);
    assert!(
        controller
            .model()
            .connections
            .widgets()
            .connect_hint
            .is_visible()
    );
    for (handle, name, pos) in [(1, "Biah", Position::Top), (2, "Laptop", Position::Right)] {
        controller.emit(AppMsg::Frontend(FrontendEvent::Created(
            handle,
            ClientConfig {
                hostname: Some(name.to_string()),
                pos,
                ..Default::default()
            },
            ClientState {
                active: true,
                // sharing is stopped, so no client is connected yet
                ..Default::default()
            },
        )));
    }
    settle();
    assert!(
        !controller
            .model()
            .connections
            .widgets()
            .connect_hint
            .is_visible()
    );
    assert_eq!(controller.model().session.clients.len(), 2);
    assert_eq!(controller.model().session.connected_clients(), 0);
    capture(window, "screens-server-dark");
    settings.set_gtk_font_name(Some("Cantarell 20"));
    settle();
    capture(window, "screens-server-large-text-dark");
    settings.set_gtk_font_name(font.as_deref());
    settle();
    let service_toggle = controller
        .model()
        .connections
        .widgets()
        .service_toggle
        .clone();
    assert_eq!(controller.model().session.status(), "Server · stopped");
    assert!(service_toggle.has_css_class("suggested-action"));
    controller.emit(AppMsg::Frontend(FrontendEvent::CaptureStatus(
        Status::Enabled,
    )));
    settle();
    assert!(!service_toggle.has_css_class("suggested-action"));
    let status = controller.model().session.status();
    assert_eq!(
        labels_with_text(window, &status),
        1,
        "the status is shown once"
    );
    assert_eq!(service_toggle.label().as_deref(), Some("Stop"));
    assert_eq!(
        controller.model().session.status(),
        "Server · 0 clients connected"
    );
    controller.emit(AppMsg::Frontend(FrontendEvent::CaptureStatus(
        Status::Disabled,
    )));
    settle();
    assert!(service_toggle.has_css_class("suggested-action"));
    assert_eq!(service_toggle.label().as_deref(), Some("Start"));
    // the capture warning is shown once, as a banner
    let attention = controller
        .model()
        .connections
        .widgets()
        .attention_banner
        .clone();
    assert!(attention.is_revealed());
    assert_eq!(labels_with_text(window, "Input capture is disabled"), 1);
    styles.set_color_scheme(adw::ColorScheme::ForceLight);
    settle();
    capture(window, "screens-server-light");

    controller.emit(AppMsg::Frontend(FrontendEvent::CaptureStatus(
        Status::Enabled,
    )));
    settle();
    assert!(!attention.is_revealed());
    capture(window, "screens-running-light");
    for (handle, name, pos, connected) in [
        (3, "MacBook-Air", Position::Left, false),
        (4, "ThinkPad-X1", Position::Bottom, true),
    ] {
        controller.emit(AppMsg::Frontend(FrontendEvent::Created(
            handle,
            ClientConfig {
                hostname: Some(name.to_string()),
                pos,
                ..Default::default()
            },
            ClientState {
                active: true,
                active_addr: connected.then(|| "192.168.1.24:50798".parse().unwrap()),
                ..Default::default()
            },
        )));
    }
    connections_page.emit(pages::connections::ConnectionsMsg::ToggleArrangementSize);
    settle();
    assert_eq!(
        controller.model().session.status(),
        "Server · 1 client connected"
    );
    capture(window, "screens-all-edges-light");
    arrangement.preview_drag(Some((1, (150.0, 70.0), Position::Right)));
    settle();
    capture(window, "screens-drag-light");
    arrangement.preview_drag(None);
    connections_page.emit(pages::connections::ConnectionsMsg::ToggleArrangementSize);
    controller.emit(AppMsg::Frontend(FrontendEvent::CaptureStatus(
        Status::Disabled,
    )));
    settle();

    window.set_default_size(1100, 294);
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

    controller.emit(AppMsg::Page(PageOutput::SetMode(OperationMode::Client)));
    window.set_default_size(1100, 750);
    // emulation that restarts on its own is not a failure the user must fix
    controller.emit(AppMsg::Frontend(FrontendEvent::EmulationStatus(
        Status::Enabled,
    )));
    controller.emit(AppMsg::Frontend(FrontendEvent::EmulationRestarting {
        retry_in_ms: 250,
    }));
    settle();
    assert_eq!(
        controller.model().session.client_emulation(),
        ClientEmulation::Restarting
    );
    assert!(!attention.is_revealed());
    assert!(
        controller.model().session.service_running(),
        "offers Stop, not Start"
    );
    capture(window, "client-restarting-emulation");
    controller.emit(AppMsg::Frontend(FrontendEvent::EmulationStatus(
        Status::Enabled,
    )));
    settle();
    assert_eq!(
        controller.model().session.client_emulation(),
        ClientEmulation::Running
    );
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerRetry {
        retry_in_secs: 5,
        awaiting_authorization: false,
    }));
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerEndpoint {
        hostname: Some("luminus-desktop".to_owned()),
        ips: Vec::new(),
        port: DEFAULT_PORT,
    }));
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerRetry {
        retry_in_secs: 5,
        awaiting_authorization: false,
    }));
    settle();
    let retry = controller
        .model()
        .connections
        .widgets()
        .retry_banner
        .clone();
    assert!(retry.is_revealed());
    assert_eq!(
        controller.model().session.status(),
        "Client · retrying in 5 s"
    );
    capture(window, "client-retrying");

    // a server whose certificate changed is refused until the user trusts it
    let mismatch_row = controller
        .model()
        .connections
        .widgets()
        .fingerprint_mismatch_row
        .clone();
    let pinned_row = controller
        .model()
        .connections
        .widgets()
        .server_fingerprint_row
        .clone();
    let pinned = "aa:".repeat(31) + "aa";
    let shown = "bb:".repeat(31) + "bb";
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerFingerprint(Some(
        pinned.clone(),
    ))));
    settle();
    assert!(pinned_row.is_visible());
    assert_eq!(pinned_row.subtitle().as_deref(), Some(pinned.as_str()));
    assert!(!mismatch_row.is_visible());

    controller.emit(AppMsg::Frontend(FrontendEvent::ServerFingerprintMismatch {
        expected: pinned.clone(),
        got: shown.clone(),
    }));
    settle();
    assert!(mismatch_row.is_visible());
    let dialog = window
        .visible_dialog()
        .expect("the identity warning is shown");
    capture(window, "client-server-certificate-changed");
    dialog.force_close();
    settle();
    // the same report on the next dial does not ask again
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerFingerprintMismatch {
        expected: pinned,
        got: shown.clone(),
    }));
    settle();
    assert!(window.visible_dialog().is_none());
    // a resync repeats the endpoint: the server is still refused
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerEndpoint {
        hostname: Some("luminus-desktop".to_owned()),
        ips: Vec::new(),
        port: DEFAULT_PORT,
    }));
    settle();
    assert!(mismatch_row.is_visible());

    // trusting it pins the shown certificate, which clears the warning
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerFingerprint(Some(
        shown.clone(),
    ))));
    settle();
    assert!(!mismatch_row.is_visible());

    // a newer mismatch asks again
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerFingerprintMismatch {
        expected: shown,
        got: "cc:".repeat(31) + "cc",
    }));
    settle();
    window.visible_dialog().expect("warning").force_close();
    // a dial that proves the pinned certificate settles the warning
    controller.emit(AppMsg::Frontend(FrontendEvent::DeviceConnected {
        addr: "192.0.2.1:4242".parse().unwrap(),
        fingerprint: "dd:".repeat(31) + "dd",
    }));
    settle();
    assert!(!mismatch_row.is_visible());
    assert!(!retry.is_revealed(), "a connected client is not retrying");
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerFingerprintMismatch {
        expected: "aa".to_owned(),
        got: "ee".to_owned(),
    }));
    settle();
    window.visible_dialog().expect("warning").force_close();
    // another server makes the warning obsolete
    controller.emit(AppMsg::Frontend(FrontendEvent::ServerEndpoint {
        hostname: Some("other-desktop".to_owned()),
        ips: Vec::new(),
        port: DEFAULT_PORT,
    }));
    settle();
    assert!(!mismatch_row.is_visible());

    window.close();
    drop(controller);
    drop(connection);
    drop(listener);
    std::fs::remove_file(socket_path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
