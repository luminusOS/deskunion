//! The main window: navigation between the pages, the dialogs and toasts
//! they share, and the tray. Pages own their widgets (see `pages`); the
//! window owns the `Session` they all read.

mod copy;
mod navigation;
mod pages;
mod session;
#[cfg(all(test, target_os = "linux", feature = "ui-tests"))]
mod ui_tests;

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use deskunion_ipc::{
    ClientConfig, FrontendEvent, FrontendRequest, FrontendRequestWriter, OperationMode,
};

use crate::dialogs::{
    AddClientDialogInit, AddClientDialogModel, AddClientDialogOutput, AuthorizationDialogInit,
    AuthorizationDialogModel, AuthorizationDialogOutput, FingerprintDialogInit,
    FingerprintDialogModel, FingerprintDialogOutput,
};

pub use navigation::Page;
use pages::PageOutput;
use pages::audio::{AudioMsg, AudioPage};
use pages::connections::{ConnectionsInit, ConnectionsMsg, ConnectionsPage};
use pages::logs::{LogsMsg, LogsPage};
use pages::settings::{SettingsInit, SettingsMsg, SettingsPage};
use session::Session;

/// Below this width the sidebar fills the window and pages open on top of it.
const COLLAPSE_BELOW: &str = "max-width: 600sp";

pub struct AppInit {
    pub app: adw::Application,
    pub writer: FrontendRequestWriter,
}

#[derive(Debug)]
pub enum AppMsg {
    Frontend(FrontendEvent),
    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    Tray(crate::tray::TrayCommand),
    NavigateTo(Page),
    Page(PageOutput),
    AddClientPairRequested(ClientConfig),
    AddClientCancelled,
    AuthorizationConfirmed(u64, String),
    AuthorizationCancelled(u64),
    FingerprintConfirmed(u64, String, String),
    FingerprintCancelled(u64, Option<String>),
    #[cfg(target_os = "macos")]
    MacosAccessibilityGranted,
    #[cfg(target_os = "macos")]
    MacosAccessibilityRevoked,
}

pub struct AppModel {
    writer: FrontendRequestWriter,
    root: adw::ApplicationWindow,
    toast_overlay: adw::ToastOverlay,
    split_view: adw::NavigationSplitView,
    page_stack: gtk::Stack,
    /// read by the sidebar's selection handler, which runs outside `update`
    page: Rc<Cell<Page>>,
    session: Session,
    error_toast: Option<adw::Toast>,

    connections: Controller<ConnectionsPage>,
    audio: Controller<AudioPage>,
    logs: Controller<LogsPage>,
    settings: Controller<SettingsPage>,

    authorization_dialog: Option<Controller<AuthorizationDialogModel>>,
    authorization_dialog_generation: u64,
    pending_authorization_fingerprint: Option<String>,
    fingerprint_dialog: Option<Controller<FingerprintDialogModel>>,
    fingerprint_dialog_generation: u64,
    add_client_dialog: Option<Controller<AddClientDialogModel>>,
}

impl AppModel {
    fn request(&mut self, request: FrontendRequest) {
        if let Err(e) = self.writer.request(request) {
            log::error!("error sending message: {e}");
        }
    }

    fn toast(&self, text: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(text));
    }

    /// errors stay until dismissed; a newer one replaces the older
    fn show_error(&mut self, text: &str) {
        if let Some(previous) = self.error_toast.take() {
            previous.dismiss();
        }
        let toast = adw::Toast::builder().title(text).timeout(0).build();
        self.toast_overlay.add_toast(toast.clone());
        self.error_toast = Some(toast);
    }

    fn navigate(&mut self, page: Page) {
        self.page.set(page);
        self.page_stack.set_visible_child_name(page.name());
        self.split_view.set_show_content(true);
    }

    /// pushes the session to the pages that read it
    fn set_session(&mut self, session: Session) {
        if session == self.session {
            return;
        }
        self.session = session;
        self.connections
            .emit(ConnectionsMsg::Session(self.session.clone()));
        self.settings
            .emit(SettingsMsg::Session(self.session.clone()));
    }

    fn update_session(&mut self, change: impl FnOnce(&mut Session)) {
        let mut session = self.session.clone();
        change(&mut session);
        self.set_session(session);
    }

    fn set_running(&mut self, running: bool) {
        self.update_session(|session| session.wanted_running = running);
        self.request(FrontendRequest::SetServiceRunning(running));
    }

    fn set_mode(&mut self, mode: OperationMode) {
        if self.session.mode != mode {
            self.update_session(|session| session.apply(&FrontendEvent::OperationMode(mode)));
            self.request(FrontendRequest::SetOperationMode(mode));
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    fn tray_info(&self) -> crate::tray::TrayInfo {
        crate::tray::TrayInfo {
            mode: self.session.mode,
            running: self.session.service_running(),
            status: self.session.status(),
            clients: self
                .session
                .clients
                .iter()
                .map(|client| crate::tray::TrayClient {
                    handle: client.handle,
                    name: client.name.clone(),
                    shared: client.shared,
                    connected: client.connected,
                })
                .collect(),
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    fn handle_tray_command(&mut self, command: crate::tray::TrayCommand) {
        use crate::tray::TrayCommand;
        match command {
            TrayCommand::SetRunning(on) => self.set_running(on),
            TrayCommand::ReconnectNow => self.request(FrontendRequest::ReconnectNow),
            TrayCommand::SetClientShared(handle, on) => {
                self.request(FrontendRequest::Activate(handle, on));
            }
        }
    }

    fn handle_page_output(&mut self, output: PageOutput, sender: &ComponentSender<Self>) {
        match output {
            PageOutput::Request(request) => self.request(request),
            PageOutput::Toast(text) => self.toast(&text),
            PageOutput::Error(text) => self.show_error(&text),
            PageOutput::SetMode(mode) => self.set_mode(mode),
            PageOutput::ToggleSharing => self.set_running(!self.session.service_running()),
            PageOutput::StartSharing => self.set_running(true),
            PageOutput::OpenFingerprintDialog(fingerprint) => {
                self.open_fingerprint_dialog(sender, fingerprint);
            }
            PageOutput::AddClient => self.open_add_client_dialog(sender),
            PageOutput::Clients(clients) => {
                self.update_session(|session| session.clients = clients);
            }
        }
    }

    fn handle_frontend_event(&mut self, event: FrontendEvent, sender: &ComponentSender<Self>) {
        match &event {
            FrontendEvent::Error(error) => self.show_error(error),
            FrontendEvent::AudioError { addr, message } => {
                let error = match addr {
                    Some(addr) => format!("Audio error ({addr}): {message}"),
                    None => format!("Audio error: {message}"),
                };
                self.show_error(&error);
            }
            FrontendEvent::ConnectionAttempt { fingerprint } => {
                self.open_authorization_dialog(sender, fingerprint.clone());
            }
            FrontendEvent::AuthorizedUpdated(keys)
                if self
                    .pending_authorization_fingerprint
                    .as_ref()
                    .is_some_and(|fingerprint| keys.contains_key(fingerprint)) =>
            {
                self.pending_authorization_fingerprint = None;
            }
            _ => {}
        }
        // the session first, so the pages see the state the event leads to
        self.update_session(|session| session.apply(&event));
        let event = Box::new(event);
        self.logs.emit(LogsMsg::Frontend(event.clone()));
        self.audio.emit(AudioMsg::Frontend(event.clone()));
        self.settings.emit(SettingsMsg::Frontend(event.clone()));
        self.connections.emit(ConnectionsMsg::Frontend(event));
    }

    fn open_fingerprint_dialog(
        &mut self,
        sender: &ComponentSender<Self>,
        fingerprint: Option<String>,
    ) {
        self.fingerprint_dialog_generation = self.fingerprint_dialog_generation.wrapping_add(1);
        let generation = self.fingerprint_dialog_generation;
        let fingerprint_context = fingerprint.clone();
        if let Some(existing) = self.fingerprint_dialog.take() {
            existing.widget().force_close();
        }
        let controller = FingerprintDialogModel::builder()
            .launch(FingerprintDialogInit {
                fingerprint,
                parent: self.root.clone(),
            })
            .forward(sender.input_sender(), move |out| match out {
                FingerprintDialogOutput::Confirmed(desc, fp) => {
                    AppMsg::FingerprintConfirmed(generation, desc, fp)
                }
                FingerprintDialogOutput::Cancelled => {
                    AppMsg::FingerprintCancelled(generation, fingerprint_context.clone())
                }
            });
        self.fingerprint_dialog = Some(controller);
    }

    fn open_authorization_dialog(&mut self, sender: &ComponentSender<Self>, fingerprint: String) {
        if self.pending_authorization_fingerprint.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        self.pending_authorization_fingerprint = Some(fingerprint.clone());
        self.authorization_dialog_generation = self.authorization_dialog_generation.wrapping_add(1);
        let generation = self.authorization_dialog_generation;
        if let Some(existing) = self.authorization_dialog.take() {
            existing.widget().force_close();
        }
        let controller = AuthorizationDialogModel::builder()
            .launch(AuthorizationDialogInit {
                fingerprint,
                parent: self.root.clone(),
            })
            .forward(sender.input_sender(), move |out| match out {
                AuthorizationDialogOutput::Confirmed(fp) => {
                    AppMsg::AuthorizationConfirmed(generation, fp)
                }
                AuthorizationDialogOutput::Cancelled => AppMsg::AuthorizationCancelled(generation),
            });
        self.authorization_dialog = Some(controller);
    }

    fn open_add_client_dialog(&mut self, sender: &ComponentSender<Self>) {
        if let Some(existing) = self.add_client_dialog.take() {
            existing.widget().force_close();
        }
        let controller = AddClientDialogModel::builder()
            .launch(AddClientDialogInit {
                parent: self.root.clone(),
            })
            .forward(sender.input_sender(), |output| match output {
                AddClientDialogOutput::PairRequested(config) => {
                    AppMsg::AddClientPairRequested(config)
                }
                AddClientDialogOutput::Cancelled => AppMsg::AddClientCancelled,
            });
        self.add_client_dialog = Some(controller);
    }
}

#[relm4::component(pub)]
impl SimpleComponent for AppModel {
    type Init = AppInit;
    type Input = AppMsg;
    type Output = ();

    view! {
        #[name(root)]
        adw::ApplicationWindow {
            set_application: Some(&init.app),
            set_width_request: 360,
            set_height_request: 294,
            set_default_width: 1100,
            set_default_height: 750,
            set_title: Some("DeskUnion"),
            set_icon_name: Some("org.luminusos.DeskUnion"),

            #[name(toast_overlay)]
            adw::ToastOverlay {
                #[name(split_view)]
                adw::NavigationSplitView {
                    set_min_sidebar_width: 200.0,
                    set_max_sidebar_width: 260.0,

                    #[wrap(Some)]
                    set_sidebar = &adw::NavigationPage {
                        set_title: "DeskUnion",
                        set_tag: Some("sidebar"),

                        #[wrap(Some)]
                        set_child = &adw::ToolbarView {
                            add_top_bar = &adw::HeaderBar {
                                #[name(primary_menu)]
                                pack_end = &gtk::MenuButton {
                                    set_icon_name: "open-menu-symbolic",
                                    set_tooltip_text: Some("Main menu"),
                                    update_property: &[gtk::accessible::Property::Label("Main menu")],
                                    set_primary: true,
                                },
                            },

                            #[wrap(Some)]
                            set_content = &gtk::ScrolledWindow {
                                set_hscrollbar_policy: gtk::PolicyType::Never,

                                #[local_ref]
                                nav_list -> gtk::ListBox {
                                    add_css_class: "navigation-sidebar",
                                    #[watch]
                                    select_row: nav_list.row_at_index(model.page.get().index()).as_ref(),
                                    // GTK can select a row while restoring focus after a
                                    // reflow; only activating a row navigates
                                    connect_row_selected[current = model.page.clone()] => move |nav, row| {
                                        let index = current.get().index();
                                        if row.map(|row| row.index()) != Some(index) {
                                            nav.select_row(nav.row_at_index(index).as_ref());
                                        }
                                    },
                                    connect_row_activated[sender] => move |_, row| {
                                        sender.input(AppMsg::NavigateTo(Page::from_index(row.index())));
                                    },
                                },
                            },
                        },
                    },

                    #[wrap(Some)]
                    set_content = &adw::NavigationPage {
                        set_tag: Some("content"),
                        #[watch]
                        set_title: model.page.get().title(),

                        set_child: Some(&model.page_stack),
                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_default();

        let connections = ConnectionsPage::builder()
            .launch(ConnectionsInit {
                hostname: hostname.clone(),
            })
            .forward(sender.input_sender(), AppMsg::Page);
        let audio = AudioPage::builder()
            .launch(())
            .forward(sender.input_sender(), AppMsg::Page);
        let logs = LogsPage::builder()
            .launch(())
            .forward(sender.input_sender(), AppMsg::Page);
        let settings = SettingsPage::builder()
            .launch(SettingsInit { hostname })
            .forward(sender.input_sender(), AppMsg::Page);

        let page_stack = gtk::Stack::new();
        page_stack.add_named(connections.widget(), Some(Page::Connections.name()));
        page_stack.add_named(audio.widget(), Some(Page::Audio.name()));
        page_stack.add_named(logs.widget(), Some(Page::Logs.name()));
        page_stack.add_named(settings.widget(), Some(Page::Settings.name()));

        let nav_list = gtk::ListBox::new();
        for page in Page::ALL {
            let label = gtk::Label::builder()
                .label(page.title())
                .xalign(0.0)
                .build();
            let content = gtk::Box::builder().spacing(12).build();
            content.append(&gtk::Image::from_icon_name(page.icon()));
            content.append(&label);
            nav_list.append(&content);
        }

        let mut model = AppModel {
            writer: init.writer,
            root: root.clone(),
            toast_overlay: adw::ToastOverlay::new(),
            split_view: adw::NavigationSplitView::new(),
            page_stack,
            page: Rc::new(Cell::new(Page::Connections)),
            session: Session::default(),
            error_toast: None,
            connections,
            audio,
            logs,
            settings,
            authorization_dialog: None,
            authorization_dialog_generation: 0,
            pending_authorization_fingerprint: None,
            fingerprint_dialog: None,
            fingerprint_dialog_generation: 0,
            add_client_dialog: None,
        };

        let widgets = view_output!();
        model.toast_overlay = widgets.toast_overlay.clone();
        model.split_view = widgets.split_view.clone();

        let menu = gtk::gio::Menu::new();
        menu.append(Some("_Keyboard Shortcuts"), Some("app.shortcuts"));
        menu.append(Some("_About DeskUnion"), Some("app.about"));
        menu.append(Some("_Quit"), Some("app.quit"));
        widgets.primary_menu.set_menu_model(Some(&menu));

        for (number, page) in (1..).zip(Page::ALL) {
            let action = gtk::gio::SimpleAction::new(&format!("nav-{}", page.name()), None);
            action.connect_activate({
                let sender = sender.clone();
                move |_, _| sender.input(AppMsg::NavigateTo(page))
            });
            model.root.add_action(&action);
            init.app.set_accels_for_action(
                &format!("win.nav-{}", page.name()),
                &[&format!("<Control>{number}")],
            );
        }

        let breakpoint = adw::Breakpoint::new(
            adw::BreakpointCondition::parse(COLLAPSE_BELOW).expect("valid breakpoint"),
        );
        breakpoint.add_setter(&widgets.split_view, "collapsed", Some(&true.to_value()));
        model.root.add_breakpoint(breakpoint);

        model.request(FrontendRequest::EnumerateAudioDevices);

        ComponentParts { model, widgets }
    }

    fn post_view() {
        #[cfg(not(any(target_os = "macos", target_os = "android")))]
        crate::tray::publish(model.tray_info());
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            AppMsg::Frontend(event) => self.handle_frontend_event(event, &sender),
            #[cfg(not(any(target_os = "macos", target_os = "android")))]
            AppMsg::Tray(command) => self.handle_tray_command(command),
            AppMsg::NavigateTo(page) => self.navigate(page),
            AppMsg::Page(output) => self.handle_page_output(output, &sender),
            AppMsg::AddClientPairRequested(config) => {
                self.request(FrontendRequest::CreateConfigured {
                    config,
                    active: true,
                });
                if let Some(dialog) = self.add_client_dialog.take() {
                    dialog.widget().force_close();
                }
                self.toast("Client paired");
            }
            AppMsg::AddClientCancelled => {
                if let Some(dialog) = self.add_client_dialog.take() {
                    dialog.widget().force_close();
                }
            }
            AppMsg::AuthorizationConfirmed(generation, fingerprint) => {
                if generation != self.authorization_dialog_generation
                    || self.authorization_dialog.is_none()
                {
                    return;
                }
                if let Some(dialog) = self.authorization_dialog.take() {
                    dialog.widget().force_close();
                }
                self.open_fingerprint_dialog(&sender, Some(fingerprint));
            }
            AppMsg::AuthorizationCancelled(generation) => {
                if generation != self.authorization_dialog_generation
                    || self.authorization_dialog.is_none()
                {
                    return;
                }
                if let Some(dialog) = self.authorization_dialog.take() {
                    dialog.widget().force_close();
                }
                self.pending_authorization_fingerprint = None;
            }
            AppMsg::FingerprintConfirmed(generation, desc, fp) => {
                if generation != self.fingerprint_dialog_generation {
                    return;
                }
                if let Some(dialog) = self.fingerprint_dialog.take() {
                    dialog.widget().force_close();
                }
                if self.pending_authorization_fingerprint.as_deref() == Some(fp.as_str()) {
                    self.pending_authorization_fingerprint = None;
                }
                self.request(FrontendRequest::AuthorizeKey(desc, fp));
            }
            AppMsg::FingerprintCancelled(generation, fingerprint) => {
                if generation != self.fingerprint_dialog_generation {
                    return;
                }
                self.fingerprint_dialog = None;
                if self.pending_authorization_fingerprint == fingerprint {
                    self.pending_authorization_fingerprint = None;
                }
            }
            #[cfg(target_os = "macos")]
            AppMsg::MacosAccessibilityGranted => {
                // the capture banner asks the system, not the session
                self.connections.emit(ConnectionsMsg::Refresh);
            }
            #[cfg(target_os = "macos")]
            AppMsg::MacosAccessibilityRevoked => {
                relm4::main_application().quit();
            }
        }
    }
}
