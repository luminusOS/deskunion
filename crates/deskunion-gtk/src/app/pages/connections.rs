use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;

use adw::prelude::*;
use gtk::glib;
use relm4::factory::{DynamicIndex, FactoryVecDeque};
use relm4::prelude::*;

use deskunion_ipc::{
    ClientConfig, ClientHandle, ClientState, DEFAULT_PORT, FrontendEvent, FrontendRequest,
    OperationMode, Position,
};

use super::PageOutput;
use crate::app::copy;
use crate::app::session::{Attention, ClientSummary, Session};
use crate::factories::{
    ClientRowInit, ClientRowInput, ClientRowModel, ClientRowOutput, IncomingDeviceRowInit,
    IncomingDeviceRowInput, IncomingDeviceRowModel, NO_HOSTNAME_TITLE, ParkedDeviceRowInit,
    ParkedDeviceRowModel, ParkedDeviceRowOutput,
};
use crate::screen_arrangement::{ScreenArrangement, ScreenItem};

#[derive(Debug)]
pub enum ConnectionsMsg {
    Frontend(Box<FrontendEvent>),
    Session(Session),
    SetMode(OperationMode),
    ToggleSharing,
    ServerHostChanged(String),
    ServerPortChanged(u16),
    ServerConnect,
    ReviewServerFingerprint,
    AddClient,
    /// re-read the macOS Accessibility permission the banner shows
    #[cfg(target_os = "macos")]
    Refresh,
    TrustServerFingerprint(String),
    ScreenPositionChanged(ClientHandle, String),
    ToggleArrangementSize,
    AttentionAction,
    ReconnectNow,
    ClientRow(ClientRowOutput),
    ParkedDeviceRow(ParkedDeviceRowOutput),
}

pub struct ConnectionsInit {
    pub hostname: String,
}

/// A connection test in flight for the client's server form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTest {
    pub id: u64,
    pub hostname: String,
    pub port: u16,
}

pub struct ConnectionsPage {
    root: adw::ToolbarView,
    session: Session,
    hostname: String,
    arrangement_expanded: bool,

    client_rows: FactoryVecDeque<ClientRowModel>,
    /// built from the events, not the rows: a row applies its messages later
    clients: Vec<ClientSummary>,
    incoming_device_rows: FactoryVecDeque<IncomingDeviceRowModel>,
    incoming_devices_by_addr: HashMap<SocketAddr, DynamicIndex>,
    /// authorized devices that connected while every screen edge is taken
    parked_device_rows: FactoryVecDeque<ParkedDeviceRowModel>,
    parked_devices_by_addr: HashMap<SocketAddr, DynamicIndex>,

    /// the endpoint the service confirmed; the drafts mirror the entries,
    /// which are only written when the endpoint changes so a refresh never
    /// clobbers what the user is typing
    server_hostname: Option<String>,
    server_port: u16,
    server_host_draft: String,
    server_port_draft: u16,
    server_host_entry: adw::EntryRow,
    server_port_spin: adw::SpinRow,
    pending_test: Option<PendingTest>,
    next_test_id: u64,
    connection_error: Option<String>,
    server_fingerprint: Option<String>,
    /// (pinned, shown) when the server showed another certificate; the
    /// connection stays refused until the user trusts the shown one
    fingerprint_mismatch: Option<(String, String)>,
}

impl ConnectionsPage {
    #[cfg(all(test, feature = "ui-tests"))]
    pub fn pending_test(&self) -> Option<&PendingTest> {
        self.pending_test.as_ref()
    }

    #[cfg(all(test, feature = "ui-tests"))]
    pub fn connection_error(&self) -> Option<&str> {
        self.connection_error.as_deref()
    }

    fn output(sender: &ComponentSender<Self>, output: PageOutput) {
        let _ = sender.output(output);
    }

    fn toast(sender: &ComponentSender<Self>, text: impl Into<String>) {
        Self::output(sender, PageOutput::Toast(text.into()));
    }

    fn request(sender: &ComponentSender<Self>, request: FrontendRequest) {
        Self::output(sender, PageOutput::Request(request));
    }

    /// no server is saved yet, or the form differs from the saved one
    fn endpoint_edited(&self) -> bool {
        self.pending_test.is_some()
            || self.server_hostname.as_deref() != Some(self.server_host_draft.trim())
            || self.server_port != self.server_port_draft
    }

    fn server_form_pending(&self) -> bool {
        self.mode() == OperationMode::Client && self.endpoint_edited()
    }

    fn size_toggle_label(&self) -> &'static str {
        if self.arrangement_expanded {
            "Shrink arrangement"
        } else {
            "Enlarge arrangement"
        }
    }

    fn mode(&self) -> OperationMode {
        self.session.mode
    }

    fn show_connect_hint(&self) -> bool {
        self.mode() == OperationMode::Server
            && self.client_rows.is_empty()
            && self.parked_device_rows.is_empty()
    }

    fn attention_title(&self) -> &'static str {
        match self.session.attention() {
            Some(Attention::Capture) => capture_title(),
            Some(Attention::Emulation) => "Input emulation is disabled",
            None => "",
        }
    }

    fn attention_button(&self) -> &'static str {
        match self.session.attention() {
            Some(Attention::Capture) => capture_button(),
            _ => "Re-enable",
        }
    }

    fn retry_title(&self) -> &'static str {
        match self.session.server_retry {
            Some(retry) if retry.awaiting_authorization => {
                "Waiting for the server to allow this computer"
            }
            Some(_) => "Can't reach the server",
            None => "",
        }
    }

    fn show_retry_banner(&self) -> bool {
        self.mode() == OperationMode::Client && self.session.server_retry.is_some()
    }

    fn screen_items(&self) -> Vec<ScreenItem> {
        self.client_rows
            .iter()
            .map(|row| ScreenItem {
                handle: row.handle(),
                hostname: row.hostname().map(str::to_string),
                position: row.position(),
                active: row.active(),
                connected: row.active_addr.is_some(),
                audio_active: row.audio_active(),
            })
            .collect()
    }

    fn client_index(&self, handle: ClientHandle) -> Option<usize> {
        self.client_rows.iter().position(|c| c.handle() == handle)
    }

    fn upsert_client(&mut self, handle: ClientHandle, config: ClientConfig, state: ClientState) {
        let summary = ClientSummary {
            handle,
            name: config
                .hostname
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| NO_HOSTNAME_TITLE.to_string()),
            shared: state.active,
            connected: state.active_addr.is_some(),
        };
        match self.clients.iter_mut().find(|c| c.handle == handle) {
            Some(existing) => *existing = summary,
            None => self.clients.push(summary),
        }
        if let Some(index) = self.client_index(handle) {
            self.client_rows
                .send(index, ClientRowInput::SetConfig(config));
            self.client_rows
                .send(index, ClientRowInput::SetState(state));
        } else {
            self.client_rows.guard().push_back(ClientRowInit {
                handle,
                config,
                state,
            });
        }
    }

    fn set_session(&mut self, session: Session) {
        if session.mode != self.session.mode {
            self.pending_test = None;
            self.connection_error = None;
        }
        self.session = session;
    }

    /// Ask before trusting a server whose certificate changed: either it was
    /// reinstalled, or another machine answers in its place.
    fn present_fingerprint_warning(&self, sender: &ComponentSender<Self>) {
        let Some((expected, got)) = self.fingerprint_mismatch.clone() else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(copy::SERVER_IDENTITY_CHANGED),
            Some(&copy::server_identity_changed(&expected, &got)),
        );
        dialog.set_body_use_markup(true);
        dialog.add_response("cancel", "_Keep blocked");
        dialog.add_response("trust", "_Trust new certificate");
        dialog.set_response_appearance("trust", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let sender = sender.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "trust" {
                sender.input(ConnectionsMsg::TrustServerFingerprint(got.clone()));
            }
        });
        dialog.present(Some(&self.root));
    }

    fn handle_frontend_event(&mut self, event: FrontendEvent, sender: &ComponentSender<Self>) {
        let clients_before = self.clients.clone();
        match event {
            FrontendEvent::Created(handle, config, state) => {
                // an entry with a fingerprint comes from pairing a device:
                // the service picked the position, so say which one
                if config.fingerprint.is_some() {
                    Self::toast(sender, copy::device_paired(config.pos));
                }
                self.upsert_client(handle, config, state);
            }
            FrontendEvent::Enumerate(clients) => {
                for (handle, config, state) in clients {
                    self.upsert_client(handle, config, state);
                }
            }
            FrontendEvent::State(handle, config, state) => {
                if self.client_index(handle).is_some() {
                    self.upsert_client(handle, config, state);
                }
            }
            FrontendEvent::Deleted(handle) => {
                self.clients.retain(|c| c.handle != handle);
                if let Some(index) = self.client_index(handle) {
                    self.client_rows.guard().remove(index);
                }
            }
            FrontendEvent::ConnectionTested { request_id, error } => {
                if self.pending_test.as_ref().map(|test| test.id) != Some(request_id) {
                    return;
                }
                let Some(test) = self.pending_test.take() else {
                    return;
                };
                match error {
                    Some(error) => self.connection_error = Some(error),
                    None => {
                        let ip = test.hostname.parse::<std::net::IpAddr>().ok();
                        Self::request(
                            sender,
                            FrontendRequest::SetServer {
                                hostname: ip.is_none().then_some(test.hostname),
                                ips: ip.into_iter().collect(),
                                port: test.port,
                            },
                        );
                        // Connect is the intent to start: otherwise the saved
                        // server is never dialed until Start is pressed too
                        Self::output(sender, PageOutput::StartSharing);
                    }
                }
            }
            FrontendEvent::ServerFingerprint(fingerprint) => {
                if self
                    .fingerprint_mismatch
                    .as_ref()
                    .is_some_and(|(_, shown)| Some(shown) == fingerprint.as_ref())
                {
                    self.fingerprint_mismatch = None;
                }
                self.server_fingerprint = fingerprint;
            }
            FrontendEvent::ServerFingerprintMismatch { expected, got } => {
                let mismatch = (expected, got);
                if self.fingerprint_mismatch.as_ref() != Some(&mismatch) {
                    self.fingerprint_mismatch = Some(mismatch);
                    self.present_fingerprint_warning(sender);
                }
            }
            FrontendEvent::DeviceConnected { fingerprint, addr } => {
                if self.mode() == OperationMode::Server {
                    if !self.parked_devices_by_addr.contains_key(&addr) {
                        let index =
                            self.parked_device_rows
                                .guard()
                                .push_back(ParkedDeviceRowInit {
                                    addr: addr.to_string(),
                                    fingerprint,
                                });
                        self.parked_devices_by_addr.insert(addr, index);
                    }
                    Self::toast(sender, copy::device_waiting(addr));
                } else {
                    // the server proved the pinned certificate: offering to
                    // trust the other one now could pin an impostor
                    self.fingerprint_mismatch = None;
                    Self::toast(sender, copy::connected_to_server(addr));
                }
            }
            FrontendEvent::DeviceEntered {
                fingerprint,
                addr,
                pos,
            } => {
                if let Some(index) = self.incoming_devices_by_addr.get(&addr) {
                    self.incoming_device_rows.send(
                        index.current_index(),
                        IncomingDeviceRowInput::Entered {
                            position: pos,
                            fingerprint,
                        },
                    );
                } else {
                    let index =
                        self.incoming_device_rows
                            .guard()
                            .push_back(IncomingDeviceRowInit {
                                addr: addr.to_string(),
                                position: Some(pos),
                                fingerprint,
                            });
                    self.incoming_devices_by_addr.insert(addr, index);
                }
            }
            FrontendEvent::IncomingDisconnected(addr) => {
                if let Some(index) = self.incoming_devices_by_addr.remove(&addr) {
                    self.incoming_device_rows
                        .guard()
                        .remove(index.current_index());
                }
                if let Some(index) = self.parked_devices_by_addr.remove(&addr) {
                    self.parked_device_rows
                        .guard()
                        .remove(index.current_index());
                }
            }
            FrontendEvent::AudioStream {
                addr,
                active,
                latency_ms,
                ..
            } => {
                let addr = addr.to_string();
                if let Some(index) = self
                    .client_rows
                    .iter()
                    .position(|c| c.active_addr.as_deref() == Some(addr.as_str()))
                {
                    self.client_rows.send(
                        index,
                        ClientRowInput::SetAudioStats {
                            active,
                            latency_ms: active.then_some(latency_ms),
                        },
                    );
                }
            }
            FrontendEvent::ServerEndpoint {
                hostname,
                ips,
                port,
            } => {
                // a server entered as a bare IP is saved without a hostname
                let hostname = hostname.or_else(|| ips.first().map(ToString::to_string));
                // the service repeats the endpoint on every sync; only another
                // server replaces what the user is typing or makes a reported
                // certificate change obsolete
                if (&hostname, port) == (&self.server_hostname, self.server_port) {
                    return;
                }
                self.fingerprint_mismatch = None;
                self.server_hostname = hostname;
                self.server_port = port;
                self.server_host_draft = self.server_hostname.clone().unwrap_or_default();
                self.server_port_draft = port;
                self.server_host_entry.set_text(&self.server_host_draft);
                self.server_port_spin.set_value(f64::from(port));
            }
            _ => {}
        }
        if self.clients != clients_before {
            Self::output(sender, PageOutput::Clients(self.clients.clone()));
        }
    }

    fn handle_client_row_output(
        &mut self,
        output: ClientRowOutput,
        sender: &ComponentSender<Self>,
    ) {
        let row = |index: &DynamicIndex| self.client_rows.get(index.current_index());
        let request = match output {
            ClientRowOutput::Activate(index, active) => {
                row(&index).map(|row| FrontendRequest::Activate(row.handle(), active))
            }
            ClientRowOutput::Delete(index) => {
                row(&index).map(|row| FrontendRequest::Delete(row.handle()))
            }
            ClientRowOutput::HostnameChange(index, hostname) => row(&index).and_then(|row| {
                let hostname = Some(hostname).filter(|s| !s.is_empty());
                (row.hostname() != hostname.as_deref())
                    .then(|| FrontendRequest::UpdateHostname(row.handle(), hostname))
            }),
            ClientRowOutput::PortChange(index, port) => row(&index)
                .filter(|row| row.port() != port)
                .map(|row| FrontendRequest::UpdatePort(row.handle(), port)),
            ClientRowOutput::PositionChange(index, position) => {
                row(&index).map(|row| FrontendRequest::UpdatePosition(row.handle(), position))
            }
        };
        if let Some(request) = request {
            Self::request(sender, request);
        }
    }
}

fn capture_title() -> &'static str {
    #[cfg(target_os = "macos")]
    if crate::macos_privacy::accessibility_granted() {
        return "Relaunch DeskUnion to start capturing input";
    }
    "Input capture is disabled"
}

fn capture_button() -> &'static str {
    #[cfg(target_os = "macos")]
    return if crate::macos_privacy::accessibility_granted() {
        "Relaunch"
    } else {
        "Grant"
    };
    #[cfg(not(target_os = "macos"))]
    "Re-enable"
}

#[relm4::component(pub)]
impl SimpleComponent for ConnectionsPage {
    type Init = ConnectionsInit;
    type Input = ConnectionsMsg;
    type Output = PageOutput;

    view! {
        #[root]
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: crate::app::navigation::Page::Connections.title(),
                    #[watch]
                    set_subtitle: &model.session.status(),
                },

                #[name(service_toggle)]
                pack_end = &gtk::Button {
                    #[watch]
                    set_visible: model.mode() != OperationMode::Unconfigured,
                    #[watch]
                    set_label: if model.session.service_running() { "Stop" } else { "Start" },
                    #[watch]
                    // an unsaved server form has its own primary action
                    set_class_active: ("suggested-action", !model.session.service_running() && !model.server_form_pending()),
                    connect_clicked => ConnectionsMsg::ToggleSharing,
                },
            },

            #[name(attention_banner)]
            add_top_bar = &adw::Banner {
                #[watch]
                set_title: model.attention_title(),
                #[watch]
                set_button_label: Some(model.attention_button()),
                #[watch]
                set_revealed: model.session.attention().is_some(),
                connect_button_clicked => ConnectionsMsg::AttentionAction,
            },

            #[name(retry_banner)]
            add_top_bar = &adw::Banner {
                #[watch]
                set_title: model.retry_title(),
                set_button_label: Some("Reconnect"),
                #[watch]
                set_revealed: model.show_retry_banner(),
                connect_button_clicked => ConnectionsMsg::ReconnectNow,
            },

            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                adw::StatusPage {
                    set_vexpand: true,
                    #[watch]
                    set_visible: model.mode() == OperationMode::Unconfigured,
                    set_icon_name: Some("io.github.luminusos.DeskUnion"),
                    set_title: "Share a keyboard and mouse",
                    set_description: Some("Choose what this computer does"),

                    #[wrap(Some)]
                    set_child = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        set_halign: gtk::Align::Center,

                        gtk::Button {
                            set_label: "Share this keyboard and mouse",
                            add_css_class: "pill",
                            add_css_class: "suggested-action",
                            connect_clicked => ConnectionsMsg::SetMode(OperationMode::Server),
                        },
                        gtk::Button {
                            set_label: "Use another computer's keyboard",
                            add_css_class: "pill",
                            connect_clicked => ConnectionsMsg::SetMode(OperationMode::Client),
                        },
                    },
                },

                gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    #[watch]
                    set_visible: model.mode() != OperationMode::Unconfigured,
                    #[wrap(Some)]
                    set_child = &adw::Clamp {
                        set_maximum_size: 760,
                        set_tightening_threshold: 0,
                        set_margin_top: 24,
                        set_margin_bottom: 24,
                        set_margin_start: 12,
                        set_margin_end: 12,

                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 24,

                            adw::PreferencesGroup {
                                set_title: "Screen arrangement",
                                #[watch]
                                set_visible: model.mode() == OperationMode::Server,

                                #[wrap(Some)]
                                set_header_suffix = &gtk::Box {
                                    set_spacing: 6,

                                    gtk::Button {
                                        add_css_class: "flat",
                                        #[watch]
                                        set_icon_name: if model.arrangement_expanded { "view-restore-symbolic" } else { "view-fullscreen-symbolic" },
                                        #[watch]
                                        set_tooltip_text: Some(model.size_toggle_label()),
                                        #[watch]
                                        update_property: &[gtk::accessible::Property::Label(model.size_toggle_label())],
                                        connect_clicked => ConnectionsMsg::ToggleArrangementSize,
                                    },

                                    gtk::Button {
                                        add_css_class: "flat",
                                        set_icon_name: "list-add-symbolic",
                                        set_tooltip_text: Some("Add client"),
                                        update_property: &[gtk::accessible::Property::Label("Add client")],
                                        connect_clicked => ConnectionsMsg::AddClient,
                                    },
                                },

                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_overflow: gtk::Overflow::Hidden,
                                    add_css_class: "card",
                                    add_css_class: "arrangement-card",

                                    #[name(screen_arrangement)]
                                    ScreenArrangement {
                                        #[watch]
                                        set_height_request: if model.arrangement_expanded { 320 } else { 190 },
                                        set_vexpand: false,
                                        set_margin_all: 12,
                                        set_host_label: &model.hostname,
                                        set_host_caption: "This computer",
                                        #[watch]
                                        set_items: model.screen_items(),
                                    },

                                    gtk::Separator {
                                        #[watch]
                                        set_visible: !model.client_rows.is_empty(),
                                    },

                                    #[local_ref]
                                    client_list -> gtk::ListBox {
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },

                            #[name(connect_hint)]
                            adw::PreferencesGroup {
                                set_title: "Connect a client",
                                set_description: Some("On the other computer, choose Client and enter this address"),
                                #[watch]
                                set_visible: model.show_connect_hint(),

                                adw::ActionRow {
                                    set_title: &model.hostname,
                                    set_use_markup: false,
                                    #[watch]
                                    set_subtitle: &format!("Port {}", model.session.port),
                                    add_prefix = &gtk::Image {
                                        set_icon_name: Some("computer-symbolic"),
                                    },
                                },
                            },

                            adw::PreferencesGroup {
                                set_title: "Waiting for a position",
                                #[watch]
                                set_visible: model.mode() == OperationMode::Server && !model.parked_device_rows.is_empty(),
                                #[local_ref]
                                parked_device_list -> gtk::ListBox {
                                    set_selection_mode: gtk::SelectionMode::None,
                                    add_css_class: "boxed-list",
                                },
                            },

                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 12,
                                #[watch]
                                set_visible: model.mode() == OperationMode::Client,

                                adw::PreferencesGroup {
                                    set_title: "Server",

                                    #[name(server_host_entry)]
                                    adw::EntryRow {
                                        set_title: "Hostname or IP address",
                                        set_enable_undo: true,
                                        #[watch]
                                        set_sensitive: model.pending_test.is_none(),
                                        connect_entry_activated => ConnectionsMsg::ServerConnect,
                                        connect_changed[sender] => move |entry| {
                                            sender.input(ConnectionsMsg::ServerHostChanged(entry.text().to_string()));
                                        },
                                    },

                                    #[name(server_port_spin)]
                                    adw::SpinRow {
                                        set_title: "Port",
                                        set_adjustment: Some(&gtk::Adjustment::new(
                                            DEFAULT_PORT as f64,
                                            1.0,
                                            u16::MAX as f64,
                                            1.0,
                                            100.0,
                                            0.0,
                                        )),
                                        set_numeric: true,
                                        #[watch]
                                        set_sensitive: model.pending_test.is_none(),
                                        connect_value_notify[sender] => move |row| {
                                            sender.input(ConnectionsMsg::ServerPortChanged(row.value() as u16));
                                        },
                                    },

                                    #[name(connection_error_row)]
                                    adw::ActionRow {
                                        set_title: "Connection failed",
                                        set_use_markup: false,
                                        set_subtitle_lines: 0,
                                        add_css_class: "error",
                                        add_prefix = &gtk::Image {
                                            set_icon_name: Some("dialog-warning-symbolic"),
                                        },
                                        #[watch]
                                        set_visible: model.connection_error.is_some(),
                                        #[watch]
                                        set_subtitle: model.connection_error.as_deref().unwrap_or(""),
                                    },

                                    #[name(fingerprint_mismatch_row)]
                                    adw::ActionRow {
                                        set_title: copy::SERVER_IDENTITY_CHANGED,
                                        set_subtitle: copy::SERVER_IDENTITY_BLOCKED,
                                        set_subtitle_lines: 0,
                                        add_css_class: "error",
                                        add_prefix = &gtk::Image {
                                            set_icon_name: Some("dialog-warning-symbolic"),
                                        },
                                        #[watch]
                                        set_visible: model.fingerprint_mismatch.is_some(),

                                        add_suffix = &gtk::Button {
                                            set_label: "Review…",
                                            set_valign: gtk::Align::Center,
                                            connect_clicked => ConnectionsMsg::ReviewServerFingerprint,
                                        },
                                    },

                                    #[name(server_fingerprint_row)]
                                    adw::ActionRow {
                                        set_title: "Server certificate",
                                        set_use_markup: false,
                                        set_subtitle_lines: 0,
                                        add_css_class: "property",
                                        add_css_class: "fingerprint",
                                        #[watch]
                                        set_visible: model.server_fingerprint.is_some(),
                                        #[watch]
                                        set_subtitle: model.server_fingerprint.as_deref().unwrap_or(""),
                                    },
                                },

                                #[name(server_connect_button)]
                                gtk::Button {
                                    set_halign: gtk::Align::Center,
                                    // Start/Stop runs a saved server; this saves another
                                    #[watch]
                                    set_visible: model.endpoint_edited(),
                                    add_css_class: "pill",
                                    add_css_class: "suggested-action",
                                    #[watch]
                                    set_label: if model.pending_test.is_some() { "Checking…" } else { "Connect" },
                                    #[watch]
                                    set_sensitive: model.pending_test.is_none() && !model.server_host_draft.trim().is_empty(),
                                    connect_clicked => ConnectionsMsg::ServerConnect,
                                },
                            },

                            adw::PreferencesGroup {
                                set_title: "Controlling this computer",
                                #[watch]
                                set_visible: model.mode() == OperationMode::Client,
                                #[local_ref]
                                incoming_device_list -> gtk::ListBox {
                                    set_selection_mode: gtk::SelectionMode::None,
                                    add_css_class: "boxed-list",
                                },
                            },
                        },
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
        let client_list = gtk::ListBox::new();
        let client_rows = FactoryVecDeque::<ClientRowModel>::builder()
            .launch(client_list.clone())
            .forward(sender.input_sender(), ConnectionsMsg::ClientRow);

        let incoming_device_list = gtk::ListBox::new();
        incoming_device_list.set_placeholder(Some(
            &adw::ActionRow::builder()
                .title(copy::NO_INCOMING_DEVICES)
                .build(),
        ));
        let incoming_device_rows = FactoryVecDeque::<IncomingDeviceRowModel>::builder()
            .launch(incoming_device_list.clone())
            .detach();

        let parked_device_list = gtk::ListBox::new();
        let parked_device_rows = FactoryVecDeque::<ParkedDeviceRowModel>::builder()
            .launch(parked_device_list.clone())
            .forward(sender.input_sender(), ConnectionsMsg::ParkedDeviceRow);

        let mut model = ConnectionsPage {
            root: root.clone(),
            session: Session::default(),
            hostname: init.hostname,
            arrangement_expanded: false,
            client_rows,
            clients: Vec::new(),
            incoming_device_rows,
            incoming_devices_by_addr: HashMap::new(),
            parked_device_rows,
            parked_devices_by_addr: HashMap::new(),
            server_hostname: None,
            server_port: DEFAULT_PORT,
            server_host_draft: String::new(),
            server_port_draft: DEFAULT_PORT,
            server_host_entry: adw::EntryRow::new(),
            server_port_spin: adw::SpinRow::new(None::<&gtk::Adjustment>, 1.0, 0),
            pending_test: None,
            next_test_id: 0,
            connection_error: None,
            server_fingerprint: None,
            fingerprint_mismatch: None,
        };

        let widgets = view_output!();
        model.server_host_entry = widgets.server_host_entry.clone();
        model.server_port_spin = widgets.server_port_spin.clone();

        // a hand-rolled GObject widget: no typed signal for the view! macro
        widgets.screen_arrangement.connect_closure(
            "position-changed",
            false,
            glib::closure_local!(
                #[strong]
                sender,
                move |_: &ScreenArrangement, handle: u64, position: String| {
                    sender.input(ConnectionsMsg::ScreenPositionChanged(handle, position));
                }
            ),
        );

        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            ConnectionsMsg::Frontend(event) => self.handle_frontend_event(*event, &sender),
            ConnectionsMsg::Session(session) => self.set_session(session),
            ConnectionsMsg::SetMode(mode) => Self::output(&sender, PageOutput::SetMode(mode)),
            ConnectionsMsg::ToggleSharing => Self::output(&sender, PageOutput::ToggleSharing),
            ConnectionsMsg::ServerHostChanged(text) => {
                self.server_host_draft = text;
                self.connection_error = None;
            }
            ConnectionsMsg::ServerPortChanged(port) => {
                self.server_port_draft = port;
                self.connection_error = None;
            }
            ConnectionsMsg::ServerConnect => {
                if self.pending_test.is_some() {
                    return;
                }
                self.connection_error = None;
                let hostname = self.server_host_draft.trim().to_string();
                if hostname.is_empty() {
                    self.connection_error = Some("Enter the server's address".to_string());
                    self.server_host_entry.grab_focus();
                    return;
                }
                let test = PendingTest {
                    id: self.next_test_id,
                    hostname,
                    port: self.server_port_draft,
                };
                self.next_test_id = self.next_test_id.wrapping_add(1);
                Self::request(
                    &sender,
                    FrontendRequest::TestConnection {
                        request_id: test.id,
                        hostname: test.hostname.clone(),
                        port: test.port,
                    },
                );
                self.pending_test = Some(test);
            }
            ConnectionsMsg::AddClient => Self::output(&sender, PageOutput::AddClient),
            #[cfg(target_os = "macos")]
            ConnectionsMsg::Refresh => {}
            ConnectionsMsg::ReviewServerFingerprint => self.present_fingerprint_warning(&sender),
            ConnectionsMsg::TrustServerFingerprint(fingerprint) => {
                Self::request(
                    &sender,
                    FrontendRequest::TrustServerFingerprint(fingerprint),
                );
            }
            ConnectionsMsg::ScreenPositionChanged(handle, position) => {
                if let Ok(position) = Position::from_str(&position) {
                    if let Some(index) = self.client_index(handle) {
                        self.client_rows
                            .send(index, ClientRowInput::SetPosition(position));
                    }
                    Self::request(&sender, FrontendRequest::UpdatePosition(handle, position));
                }
            }
            ConnectionsMsg::ToggleArrangementSize => {
                self.arrangement_expanded = !self.arrangement_expanded;
            }
            ConnectionsMsg::AttentionAction => match self.session.attention() {
                Some(Attention::Capture) => {
                    #[cfg(target_os = "macos")]
                    {
                        use crate::macos_privacy;
                        if macos_privacy::accessibility_granted() {
                            macos_privacy::relaunch_bundle();
                            relm4::main_application().quit();
                            return;
                        }
                        macos_privacy::open_accessibility_settings();
                    }
                    #[cfg(not(target_os = "macos"))]
                    Self::request(&sender, FrontendRequest::EnableCapture);
                }
                Some(Attention::Emulation) => {
                    Self::request(&sender, FrontendRequest::EnableEmulation);
                }
                None => {}
            },
            ConnectionsMsg::ReconnectNow => Self::request(&sender, FrontendRequest::ReconnectNow),
            ConnectionsMsg::ClientRow(output) => self.handle_client_row_output(output, &sender),
            ConnectionsMsg::ParkedDeviceRow(ParkedDeviceRowOutput::Assign(index)) => {
                let current = index.current_index();
                if let Some(row) = self.parked_device_rows.get(current) {
                    Self::request(
                        &sender,
                        FrontendRequest::AssignPosition {
                            fingerprint: row.fingerprint().to_string(),
                            pos: row.position(),
                        },
                    );
                    self.parked_devices_by_addr
                        .retain(|_, i| i.current_index() != current);
                    self.parked_device_rows.guard().remove(current);
                    Self::toast(&sender, "Device paired");
                }
            }
        }
    }
}
