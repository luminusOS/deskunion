// `AdwActionRow::set_icon_name` is deprecated since libadwaita 1.3 with no
// replacement offered in the bindings (the C API is still fully
// supported, just flagged) — same situation relm4's own `components.rs`
// example hits, silenced the same way there.
#![allow(deprecated)]

mod audio;
mod logs;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;

use adw::prelude::*;
use gtk::glib;
use relm4::factory::{DynamicIndex, FactoryVecDeque};
use relm4::prelude::*;

use deskunion_ipc::{
    AudioDeviceInfo, ClientConfig, ClientHandle, ClientState, DEFAULT_PORT, FrontendEvent,
    FrontendRequest, FrontendRequestWriter, Position, Status,
};

use crate::dialogs::{
    AuthorizationDialogInit, AuthorizationDialogModel, AuthorizationDialogOutput,
    FingerprintDialogInit, FingerprintDialogModel, FingerprintDialogOutput,
};
use crate::factories::{
    AudioStreamRowInit, AudioStreamRowInput, AudioStreamRowModel, ClientRowInit, ClientRowInput,
    ClientRowModel, ClientRowOutput, IncomingDeviceRowInit, IncomingDeviceRowModel, KeyRowModel,
    KeyRowOutput,
};
use crate::screen_arrangement::{ScreenArrangement, ScreenItem};

use audio::{AUDIO_BITRATES, audio_bitrate_index, audio_device_model, selected_audio_device};
pub use logs::LogCategory;
use logs::LogState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Screens,
    Audio,
    Logs,
    Settings,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Screens => "Screens",
            Page::Audio => "Audio",
            Page::Logs => "Logs",
            Page::Settings => "Settings",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Page::Screens => "screens",
            Page::Audio => "audio",
            Page::Logs => "logs",
            Page::Settings => "settings",
        }
    }

    fn from_nav_index(index: i32) -> Self {
        match index {
            0 => Page::Screens,
            1 => Page::Audio,
            2 => Page::Logs,
            _ => Page::Settings,
        }
    }
}

pub struct AppInit {
    pub app: adw::Application,
    pub writer: FrontendRequestWriter,
}

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum AppMsg {
    Frontend(FrontendEvent),

    NavigateTo(Page),
    AddClient,
    CopyHostname,
    CopyFingerprint,
    PortEntryChanged(String),
    PortEditApply,
    PortEditCancel,
    ScreenPositionChanged(ClientHandle, String),
    OpenFingerprintDialog(Option<String>),
    ToggleCapture,
    ToggleEmulation,

    AudioSendToggled(bool),
    AudioReceiveToggled(bool),
    AudioCaptureDeviceChanged(u32),
    AudioPlaybackDeviceChanged(u32),
    AudioBitrateChanged(u32),
    AudioBufferChanged(u32),

    LogFilterChanged(Option<LogCategory>),
    LogCopy,
    LogClear,

    ClientRow(ClientRowOutput),
    KeyRow(KeyRowOutput),

    AuthorizationConfirmed(String),
    AuthorizationCancelled,
    FingerprintConfirmed(String, String),

    #[cfg(target_os = "macos")]
    MacosAccessibilityGranted,
    #[cfg(target_os = "macos")]
    MacosAccessibilityRevoked,
}

pub struct AppModel {
    writer: FrontendRequestWriter,
    root: adw::ApplicationWindow,
    toast_overlay: adw::ToastOverlay,
    current_page: Page,

    hostname: String,
    port: u16,
    /// mirrors the port entry widget's live (possibly uncommitted) text;
    /// see the view!{} comment on `port_entry` for why this field exists
    /// instead of a `#[watch]` binding.
    port_draft: String,
    port_editing: bool,
    /// suppresses `PortEntryChanged` reacting to our own programmatic
    /// `set_text` (see `handle_frontend_event`'s `PortChanged` arm)
    updating_port_entry: bool,
    port_entry: gtk::Entry,
    pk_fingerprint: String,

    capture_active: bool,
    emulation_active: bool,

    client_rows: FactoryVecDeque<ClientRowModel>,
    authorized_rows: FactoryVecDeque<KeyRowModel>,
    incoming_device_rows: FactoryVecDeque<IncomingDeviceRowModel>,
    incoming_devices_by_addr: HashMap<SocketAddr, DynamicIndex>,
    audio_stream_rows: FactoryVecDeque<AudioStreamRowModel>,
    audio_streams_by_addr: HashMap<SocketAddr, DynamicIndex>,

    /// suppresses audio-page change handlers while an incoming
    /// `AudioStatus`/`AudioDevices` event is being applied to the
    /// widgets — see the note on `AppMsg::Frontend`'s handler.
    updating_audio_ui: bool,
    audio_send: bool,
    audio_receive: bool,
    audio_bitrate: u32,
    audio_buffer_ms: u32,
    audio_loopback_supported: bool,
    audio_capture_devices: Vec<AudioDeviceInfo>,
    audio_playback_devices: Vec<AudioDeviceInfo>,

    log: LogState,

    authorization_dialog: Option<Controller<AuthorizationDialogModel>>,
    fingerprint_dialog: Option<Controller<FingerprintDialogModel>>,
}

impl AppModel {
    fn request(&mut self, request: FrontendRequest) {
        if let Err(e) = self.writer.request(request) {
            log::error!("error sending message: {e}");
        }
    }

    fn screen_items(&self) -> Vec<ScreenItem> {
        self.client_rows
            .iter()
            .map(|row| ScreenItem {
                handle: row.handle(),
                hostname: row.hostname().map(str::to_string),
                position: row.position(),
                active: row.active(),
                audio_active: row.audio_active(),
            })
            .collect()
    }

    fn client_index_for_handle(&self, handle: ClientHandle) -> Option<usize> {
        self.client_rows.iter().position(|c| c.handle() == handle)
    }

    fn status_text(&self) -> &'static str {
        match (self.capture_active, self.emulation_active) {
            (true, true) => "Running",
            (false, false) => "Capture & emulation disabled",
            (false, true) => "Capture disabled",
            (true, false) => "Emulation disabled",
        }
    }

    fn status_led_css(&self) -> &'static str {
        if self.capture_active && self.emulation_active {
            "success"
        } else {
            "warning"
        }
    }

    // The view!{} macro's DSL doesn't support #[cfg(...)] attributes
    // inside the widget tree, so unlike the pre-Relm4 code (which had
    // separate #[cfg(target_os = "macos")] blocks calling different
    // methods), these are single always-present methods with the
    // platform branch *inside* the body — completely ordinary Rust,
    // fully cfg-able, called unconditionally from view!{}.
    //
    // On macOS, capture and emulation share one TCC (Accessibility)
    // gate — collapse to a single warning row; the emulation row stays
    // hidden and this text/button reflects whichever of the two
    // (AX-missing vs. AX-granted-but-relaunch-required) applies.
    fn capture_row_visible(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            !self.capture_active || !self.emulation_active
        }
        #[cfg(not(target_os = "macos"))]
        {
            !self.capture_active
        }
    }

    fn capture_row_title(&self) -> &'static str {
        #[cfg(target_os = "macos")]
        {
            if crate::macos_privacy::accessibility_granted() {
                "relaunch required"
            } else {
                "input capture is disabled"
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            "input capture is disabled"
        }
    }

    fn capture_row_subtitle(&self) -> &'static str {
        #[cfg(target_os = "macos")]
        {
            if crate::macos_privacy::accessibility_granted() {
                "Accessibility granted — restart to activate capture and emulation"
            } else {
                "grant Accessibility permission to enable"
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            "required for outgoing and incoming connections"
        }
    }

    fn capture_row_button_label(&self) -> &'static str {
        #[cfg(target_os = "macos")]
        {
            if crate::macos_privacy::accessibility_granted() {
                "Relaunch"
            } else {
                "Grant"
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            "Reenable"
        }
    }

    fn emulation_row_visible(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            false
        }
        #[cfg(not(target_os = "macos"))]
        {
            !self.emulation_active
        }
    }

    fn port_entry_text(&self) -> String {
        if self.port == DEFAULT_PORT {
            String::new()
        } else {
            self.port.to_string()
        }
    }

    fn open_fingerprint_dialog(
        &mut self,
        sender: &ComponentSender<Self>,
        fingerprint: Option<String>,
    ) {
        if let Some(existing) = self.fingerprint_dialog.take() {
            existing.widget().force_close();
        }
        let controller = FingerprintDialogModel::builder()
            .launch(FingerprintDialogInit {
                fingerprint,
                parent: self.root.clone(),
            })
            .forward(sender.input_sender(), |out| match out {
                FingerprintDialogOutput::Confirmed(desc, fp) => {
                    AppMsg::FingerprintConfirmed(desc, fp)
                }
            });
        self.fingerprint_dialog = Some(controller);
    }

    fn open_authorization_dialog(&mut self, sender: &ComponentSender<Self>, fingerprint: String) {
        if let Some(existing) = self.authorization_dialog.take() {
            existing.widget().force_close();
        }
        let controller = AuthorizationDialogModel::builder()
            .launch(AuthorizationDialogInit {
                fingerprint,
                parent: self.root.clone(),
            })
            .forward(sender.input_sender(), |out| match out {
                AuthorizationDialogOutput::Confirmed(fp) => AppMsg::AuthorizationConfirmed(fp),
                AuthorizationDialogOutput::Cancelled => AppMsg::AuthorizationCancelled,
            });
        self.authorization_dialog = Some(controller);
    }

    /// dispatch table for every `FrontendEvent` variant — mirrors the
    /// pre-Relm4 `Window`'s per-method handlers 1:1, just relocated.
    fn handle_frontend_event(&mut self, event: FrontendEvent, sender: &ComponentSender<Self>) {
        match event {
            FrontendEvent::Created(handle, config, state) => {
                self.upsert_client(handle, config, state)
            }
            FrontendEvent::NoSuchClient(_) => {}
            FrontendEvent::State(handle, config, state) => {
                if let Some(index) = self.client_index_for_handle(handle) {
                    self.client_rows
                        .send(index, ClientRowInput::SetConfig(config));
                    self.client_rows
                        .send(index, ClientRowInput::SetState(state));
                }
            }
            FrontendEvent::Deleted(handle) => {
                if let Some(index) = self.client_index_for_handle(handle) {
                    self.client_rows.guard().remove(index);
                }
            }
            FrontendEvent::PortChanged(port, msg) => {
                self.port = port;
                self.port_editing = false;
                self.updating_port_entry = true;
                self.port_entry.set_text(&self.port_entry_text());
                self.updating_port_entry = false;
                if let Some(msg) = msg {
                    self.toast_overlay.add_toast(adw::Toast::new(&msg));
                }
            }
            FrontendEvent::Enumerate(clients) => {
                for (handle, config, state) in clients {
                    self.upsert_client(handle, config, state);
                }
            }
            FrontendEvent::Error(e) => self.toast_overlay.add_toast(adw::Toast::new(&e)),
            FrontendEvent::CaptureStatus(status) => self.capture_active = status == Status::Enabled,
            FrontendEvent::EmulationStatus(status) => {
                self.emulation_active = status == Status::Enabled;
            }
            FrontendEvent::AuthorizedUpdated(keys) => {
                let mut guard = self.authorized_rows.guard();
                guard.clear();
                for (fingerprint, description) in keys {
                    guard.push_back((description, fingerprint));
                }
            }
            FrontendEvent::PublicKeyFingerprint(fp) => self.pk_fingerprint = fp,
            FrontendEvent::ConnectionAttempt { fingerprint } => {
                self.open_authorization_dialog(sender, fingerprint);
            }
            FrontendEvent::DeviceConnected {
                fingerprint: _,
                addr,
            } => {
                self.toast_overlay
                    .add_toast(adw::Toast::new(&format!("device connected: {addr}")));
            }
            FrontendEvent::DeviceEntered {
                fingerprint,
                addr,
                pos,
            } => {
                let index = self
                    .incoming_device_rows
                    .guard()
                    .push_back(IncomingDeviceRowInit {
                        addr: addr.to_string(),
                        position: pos,
                        fingerprint,
                    });
                self.incoming_devices_by_addr.insert(addr, index);
            }
            FrontendEvent::IncomingDisconnected(addr) => {
                if let Some(index) = self.incoming_devices_by_addr.remove(&addr) {
                    self.incoming_device_rows
                        .guard()
                        .remove(index.current_index());
                }
            }
            FrontendEvent::AudioStream {
                addr,
                active,
                latency_ms,
                packets_lost,
                level,
            } => {
                let addr_str = addr.to_string();
                if let Some(index) = self
                    .client_rows
                    .iter()
                    .position(|c| c.active_addr.as_deref() == Some(addr_str.as_str()))
                {
                    self.client_rows
                        .send(index, ClientRowInput::SetAudioActive(active));
                }

                if active {
                    if let Some(index) = self.audio_streams_by_addr.get(&addr) {
                        self.audio_stream_rows.send(
                            index.current_index(),
                            AudioStreamRowInput::UpdateStats {
                                latency_ms,
                                packets_lost,
                                level,
                            },
                        );
                    } else {
                        let index = self
                            .audio_stream_rows
                            .guard()
                            .push_back(AudioStreamRowInit {
                                addr: addr_str,
                                latency_ms,
                                packets_lost,
                                level,
                            });
                        self.audio_streams_by_addr.insert(addr, index);
                    }
                } else if let Some(index) = self.audio_streams_by_addr.remove(&addr) {
                    self.audio_stream_rows.guard().remove(index.current_index());
                }
            }
            FrontendEvent::AudioStatus {
                send,
                receive,
                bitrate,
                buffer_ms,
                loopback_supported,
            } => {
                self.updating_audio_ui = true;
                self.audio_send = send;
                self.audio_receive = receive;
                self.audio_bitrate = bitrate;
                self.audio_buffer_ms = buffer_ms;
                self.updating_audio_ui = false;
                self.audio_loopback_supported = loopback_supported;
            }
            FrontendEvent::AudioDevices { capture, playback } => {
                self.updating_audio_ui = true;
                self.audio_capture_devices = capture;
                self.audio_playback_devices = playback;
                self.updating_audio_ui = false;
            }
            FrontendEvent::AudioError { addr, message } => {
                let msg = match addr {
                    Some(addr) => format!("audio error ({addr}): {message}"),
                    None => format!("audio error: {message}"),
                };
                self.toast_overlay.add_toast(adw::Toast::new(&msg));
            }
        }
    }

    fn upsert_client(&mut self, handle: ClientHandle, config: ClientConfig, state: ClientState) {
        if let Some(index) = self.client_index_for_handle(handle) {
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

    fn handle_client_row_output(&mut self, output: ClientRowOutput) {
        match output {
            ClientRowOutput::Activate(index, active) => {
                if let Some(row) = self.client_rows.get(index.current_index()) {
                    self.request(FrontendRequest::Activate(row.handle(), active));
                }
            }
            ClientRowOutput::Delete(index) => {
                if let Some(row) = self.client_rows.get(index.current_index()) {
                    self.request(FrontendRequest::Delete(row.handle()));
                }
            }
            ClientRowOutput::RequestDns(index) => {
                if let Some(row) = self.client_rows.get(index.current_index()) {
                    self.request(FrontendRequest::ResolveDns(row.handle()));
                }
            }
            ClientRowOutput::HostnameChange(index, hostname) => {
                if let Some(row) = self.client_rows.get(index.current_index()) {
                    let hostname = Some(hostname).filter(|s| !s.is_empty());
                    self.request(FrontendRequest::UpdateHostname(row.handle(), hostname));
                }
            }
            ClientRowOutput::PortChange(index, port) => {
                if let Some(row) = self.client_rows.get(index.current_index()) {
                    self.request(FrontendRequest::UpdatePort(row.handle(), port));
                }
            }
            ClientRowOutput::PositionChange(index, position) => {
                if let Some(row) = self.client_rows.get(index.current_index()) {
                    self.request(FrontendRequest::UpdatePosition(row.handle(), position));
                }
            }
        }
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
            set_width_request: 720,
            set_height_request: 560,
            set_default_width: 1100,
            set_default_height: 750,
            set_title: Some("DeskUnion"),
            set_icon_name: Some("io.github.luminusos.DeskUnion"),

            #[name(toast_overlay)]
            adw::ToastOverlay {

                #[name(split_view)]
                adw::OverlaySplitView {
                    set_min_sidebar_width: 200.0,
                    set_max_sidebar_width: 280.0,

                    #[wrap(Some)]
                    set_sidebar = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,

                        adw::HeaderBar {
                            set_show_end_title_buttons: false,
                            add_css_class: "flat",
                            #[wrap(Some)]
                            set_title_widget = &adw::WindowTitle {
                                set_title: "DeskUnion",
                            },
                        },

                        #[name(nav_list)]
                        gtk::ListBox {
                            set_vexpand: true,
                            set_selection_mode: gtk::SelectionMode::Browse,
                            add_css_class: "navigation-sidebar",

                            gtk::ListBoxRow {
                                gtk::Box {
                                    set_spacing: 12,
                                    set_margin_all: 6,
                                    gtk::Image { set_icon_name: Some("network-wired-symbolic") },
                                    gtk::Label { set_label: "Screens", set_xalign: 0.0 },
                                },
                            },
                            gtk::ListBoxRow {
                                gtk::Box {
                                    set_spacing: 12,
                                    set_margin_all: 6,
                                    gtk::Image { set_icon_name: Some("audio-speakers-symbolic") },
                                    gtk::Label { set_label: "Audio", set_xalign: 0.0 },
                                },
                            },
                            gtk::ListBoxRow {
                                gtk::Box {
                                    set_spacing: 12,
                                    set_margin_all: 6,
                                    gtk::Image { set_icon_name: Some("text-x-generic-symbolic") },
                                    gtk::Label { set_label: "Logs", set_xalign: 0.0 },
                                },
                            },
                            gtk::ListBoxRow {
                                gtk::Box {
                                    set_spacing: 12,
                                    set_margin_all: 6,
                                    gtk::Image { set_icon_name: Some("emblem-system-symbolic") },
                                    gtk::Label { set_label: "Settings", set_xalign: 0.0 },
                                },
                            },

                            connect_row_selected[sender] => move |_, row| {
                                if let Some(row) = row {
                                    sender.input(AppMsg::NavigateTo(Page::from_nav_index(row.index())));
                                }
                            },
                        },

                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 8,
                            set_margin_start: 12,
                            set_margin_end: 12,
                            set_margin_top: 8,
                            set_margin_bottom: 12,

                            gtk::Box {
                                set_width_request: 10,
                                set_height_request: 10,
                                set_valign: gtk::Align::Center,
                                #[watch]
                                set_css_classes: &["status-led", model.status_led_css()],
                            },

                            gtk::Label {
                                set_xalign: 0.0,
                                set_hexpand: true,
                                set_ellipsize: gtk::pango::EllipsizeMode::End,
                                add_css_class: "dim-label",
                                add_css_class: "caption",
                                #[watch]
                                set_label: model.status_text(),
                            },

                            gtk::Label {
                                set_xalign: 1.0,
                                add_css_class: "dim-label",
                                add_css_class: "caption",
                                set_label: &format!("v{}", crate::local_commit_str()),
                            },
                        },
                    },

                    #[wrap(Some)]
                    set_content = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,

                        adw::HeaderBar {
                            #[wrap(Some)]
                            set_title_widget = &adw::WindowTitle {
                                #[watch]
                                set_title: model.current_page.title(),
                            },
                            #[name(sidebar_toggle)]
                            pack_start = &gtk::ToggleButton {
                                set_icon_name: "sidebar-show-symbolic",
                            },
                        },

                        #[name(page_stack)]
                        adw::ViewStack {
                            set_vexpand: true,

                            add = &gtk::ScrolledWindow {
                                set_hscrollbar_policy: gtk::PolicyType::Never,
                                #[wrap(Some)]
                                set_child = &adw::Clamp {
                                    set_maximum_size: 600,
                                    set_tightening_threshold: 0,
                                    set_margin_top: 18,
                                    set_margin_bottom: 18,
                                    set_margin_start: 12,
                                    set_margin_end: 12,
                                    #[wrap(Some)]
                                    set_child = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 12,

                                        gtk::Frame {
                                            add_css_class: "card",
                                            #[wrap(Some)]
                                            set_child: screen_arrangement = &ScreenArrangement {
                                                set_height_request: 200,
                                                set_margin_all: 12,
                                                #[watch]
                                                set_items: model.screen_items(),
                                            },
                                        },

                                        adw::PreferencesGroup {
                                            set_title: "Connected devices",
                                            #[local_ref]
                                            incoming_device_list -> gtk::ListBox {
                                                set_selection_mode: gtk::SelectionMode::None,
                                                add_css_class: "boxed-list",
                                            },
                                        },

                                        adw::PreferencesGroup {
                                            set_title: "Capture / Emulation Status",
                                            #[watch]
                                            set_visible: !model.capture_active || !model.emulation_active,

                                            adw::ActionRow {
                                                #[watch]
                                                set_visible: model.capture_row_visible(),
                                                add_css_class: "warning",
                                                set_icon_name: Some("dialog-warning-symbolic"),
                                                #[watch]
                                                set_title: model.capture_row_title(),
                                                #[watch]
                                                set_subtitle: model.capture_row_subtitle(),

                                                add_suffix = &gtk::Button {
                                                    set_valign: gtk::Align::Center,
                                                    add_css_class: "pill",
                                                    add_css_class: "flat",
                                                    #[watch]
                                                    set_label: model.capture_row_button_label(),
                                                    connect_clicked => AppMsg::ToggleCapture,
                                                },
                                            },

                                            adw::ActionRow {
                                                #[watch]
                                                set_visible: model.emulation_row_visible(),
                                                add_css_class: "warning",
                                                set_icon_name: Some("dialog-warning-symbolic"),
                                                set_title: "input emulation is disabled",
                                                set_subtitle: "required for incoming connections",

                                                add_suffix = &gtk::Button {
                                                    set_valign: gtk::Align::Center,
                                                    add_css_class: "pill",
                                                    add_css_class: "flat",
                                                    set_label: "Reenable",
                                                    connect_clicked => AppMsg::ToggleEmulation,
                                                },
                                            },
                                        },

                                        adw::PreferencesGroup {
                                            set_title: "Identity",

                                            adw::ActionRow {
                                                set_title: "hostname &amp; port",

                                                add_suffix = &gtk::Button {
                                                    set_valign: gtk::Align::Center,
                                                    connect_clicked => AppMsg::CopyHostname,
                                                    #[wrap(Some)]
                                                    set_child = &gtk::Box {
                                                        set_spacing: 30,
                                                        gtk::Label {
                                                            set_label: &model.hostname,
                                                            set_valign: gtk::Align::Center,
                                                        },
                                                        gtk::Image { set_icon_name: Some("edit-copy-symbolic") },
                                                    },
                                                },

                                                #[name(port_entry)]
                                                add_suffix = &gtk::Entry {
                                                    set_max_width_chars: 5,
                                                    set_width_chars: 5,
                                                    set_valign: gtk::Align::Center,
                                                    set_property: ("xalign", 0.5f32),
                                                    set_placeholder_text: Some("4242"),
                                                    set_input_purpose: gtk::InputPurpose::Digits,
                                                    // deliberately NOT #[watch] — this entry lives
                                                    // on AppModel (re-renders on *every* message,
                                                    // unlike factory-scoped rows), so a #[watch]
                                                    // set_text would clobber in-progress typing on
                                                    // any unrelated event. Set once here; updated
                                                    // explicitly (see `AppModel::port_entry`) only
                                                    // when the server actually confirms a new port.
                                                    // starts empty: initial port is always
                                                    // `DEFAULT_PORT`, whose placeholder-text
                                                    // convention is an empty field (see
                                                    // `AppModel::port_entry_text`)
                                                    set_text: "",
                                                    connect_changed[sender] => move |entry| {
                                                        sender.input(AppMsg::PortEntryChanged(entry.text().to_string()));
                                                    },
                                                    connect_activate => AppMsg::PortEditApply,
                                                },

                                                add_suffix = &gtk::Button {
                                                    set_valign: gtk::Align::Center,
                                                    set_icon_name: "object-select-symbolic",
                                                    add_css_class: "success",
                                                    #[watch]
                                                    set_visible: model.port_editing,
                                                    connect_clicked => AppMsg::PortEditApply,
                                                },
                                                add_suffix = &gtk::Button {
                                                    set_valign: gtk::Align::Center,
                                                    set_icon_name: "process-stop-symbolic",
                                                    add_css_class: "error",
                                                    #[watch]
                                                    set_visible: model.port_editing,
                                                    connect_clicked => AppMsg::PortEditCancel,
                                                },
                                            },

                                            adw::ActionRow {
                                                set_title: "certificate fingerprint",
                                                set_icon_name: Some("auth-fingerprint-symbolic"),
                                                #[watch]
                                                set_subtitle: &model.pk_fingerprint,

                                                add_suffix = &gtk::Button {
                                                    set_valign: gtk::Align::Center,
                                                    set_icon_name: "edit-copy-symbolic",
                                                    connect_clicked => AppMsg::CopyFingerprint,
                                                },
                                            },
                                        },

                                        adw::PreferencesGroup {
                                            set_title: "Connections",
                                            #[wrap(Some)]
                                            set_header_suffix = &gtk::Button {
                                                add_css_class: "flat",
                                                connect_clicked => AppMsg::AddClient,
                                                #[wrap(Some)]
                                                set_child = &gtk::Box {
                                                    set_spacing: 6,
                                                    gtk::Image { set_icon_name: Some("list-add-symbolic") },
                                                    gtk::Label { set_label: "Add" },
                                                },
                                            },

                                            #[local_ref]
                                            client_list -> gtk::ListBox {
                                                set_selection_mode: gtk::SelectionMode::None,
                                                add_css_class: "boxed-list",
                                            },
                                        },
                                    },
                                },
                            } -> {
                                set_name: Some(Page::Screens.name()),
                                set_title: Some(Page::Screens.title()),
                            },

                            add = &gtk::ScrolledWindow {
                                set_hscrollbar_policy: gtk::PolicyType::Never,
                                #[wrap(Some)]
                                set_child = &adw::Clamp {
                                    set_maximum_size: 600,
                                    set_tightening_threshold: 0,
                                    set_margin_top: 18,
                                    set_margin_bottom: 18,
                                    set_margin_start: 12,
                                    set_margin_end: 12,
                                    #[wrap(Some)]
                                    set_child = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 12,

                                        adw::Banner {
                                            set_title: "System audio capture requires macOS 14.6 or later. Only microphone input is available.",
                                            #[watch]
                                            set_revealed: !model.audio_loopback_supported,
                                        },

                                        adw::PreferencesGroup {
                                            set_title: "Receiving",

                                            #[name(audio_receive_switch)]
                                            adw::SwitchRow {
                                                set_title: "Play audio from clients",
                                                #[watch]
                                                #[block_signal(audio_receive_handler)]
                                                set_active: model.audio_receive,
                                                connect_active_notify[sender] => move |row| {
                                                    sender.input(AppMsg::AudioReceiveToggled(row.is_active()));
                                                } @audio_receive_handler,
                                            },

                                            #[name(audio_playback_combo)]
                                            adw::ComboRow {
                                                set_title: "Output device",
                                                #[watch]
                                                #[block_signal(audio_playback_handler)]
                                                set_model: Some(&audio_device_model(&model.audio_playback_devices)),
                                                connect_selected_notify[sender] => move |row| {
                                                    sender.input(AppMsg::AudioPlaybackDeviceChanged(row.selected()));
                                                } @audio_playback_handler,
                                            },
                                        },

                                        adw::PreferencesGroup {
                                            set_title: "Sending",

                                            #[name(audio_send_switch)]
                                            adw::SwitchRow {
                                                set_title: "Send this computer's audio",
                                                #[watch]
                                                #[block_signal(audio_send_handler)]
                                                set_active: model.audio_send,
                                                connect_active_notify[sender] => move |row| {
                                                    sender.input(AppMsg::AudioSendToggled(row.is_active()));
                                                } @audio_send_handler,
                                            },

                                            #[name(audio_capture_combo)]
                                            adw::ComboRow {
                                                set_title: "Capture source",
                                                #[watch]
                                                #[block_signal(audio_capture_handler)]
                                                set_model: Some(&audio_device_model(&model.audio_capture_devices)),
                                                connect_selected_notify[sender] => move |row| {
                                                    sender.input(AppMsg::AudioCaptureDeviceChanged(row.selected()));
                                                } @audio_capture_handler,
                                            },

                                            #[name(audio_bitrate_combo)]
                                            adw::ComboRow {
                                                set_title: "Bitrate",
                                                set_model: Some(&gtk::StringList::new(&[
                                                    "64 kbps", "96 kbps", "128 kbps", "192 kbps", "256 kbps",
                                                ])),
                                                #[watch]
                                                #[block_signal(audio_bitrate_handler)]
                                                set_selected: audio_bitrate_index(model.audio_bitrate),
                                                connect_selected_notify[sender] => move |row| {
                                                    sender.input(AppMsg::AudioBitrateChanged(row.selected()));
                                                } @audio_bitrate_handler,
                                            },

                                            #[name(audio_buffer_spin)]
                                            adw::SpinRow {
                                                set_title: "Jitter buffer",
                                                set_subtitle: "milliseconds",
                                                set_adjustment: Some(&gtk::Adjustment::new(80.0, 20.0, 200.0, 10.0, 10.0, 0.0)),
                                                #[watch]
                                                #[block_signal(audio_buffer_handler)]
                                                set_value: model.audio_buffer_ms as f64,
                                                connect_value_notify[sender] => move |row| {
                                                    sender.input(AppMsg::AudioBufferChanged(row.value() as u32));
                                                } @audio_buffer_handler,
                                            },
                                        },

                                        adw::PreferencesGroup {
                                            set_title: "Active streams",
                                            #[local_ref]
                                            audio_stream_list -> gtk::ListBox {
                                                set_selection_mode: gtk::SelectionMode::None,
                                                add_css_class: "boxed-list",
                                            },
                                        },
                                    },
                                },
                            } -> {
                                set_name: Some(Page::Audio.name()),
                                set_title: Some(Page::Audio.title()),
                            },

                            add = &gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,

                                gtk::Box {
                                    set_spacing: 6,
                                    set_margin_all: 12,

                                    gtk::Box {
                                        set_hexpand: true,
                                        set_spacing: 6,
                                        add_css_class: "linked",

                                        #[name(log_filter_all)]
                                        gtk::ToggleButton {
                                            set_label: "All",
                                            set_active: true,
                                            connect_toggled[sender] => move |btn| {
                                                if btn.is_active() {
                                                    sender.input(AppMsg::LogFilterChanged(None));
                                                }
                                            },
                                        },
                                        gtk::ToggleButton {
                                            set_label: "Connections",
                                            set_group: Some(&log_filter_all),
                                            connect_toggled[sender] => move |btn| {
                                                if btn.is_active() {
                                                    sender.input(AppMsg::LogFilterChanged(Some(LogCategory::Connections)));
                                                }
                                            },
                                        },
                                        gtk::ToggleButton {
                                            set_label: "Audio",
                                            set_group: Some(&log_filter_all),
                                            connect_toggled[sender] => move |btn| {
                                                if btn.is_active() {
                                                    sender.input(AppMsg::LogFilterChanged(Some(LogCategory::Audio)));
                                                }
                                            },
                                        },
                                        gtk::ToggleButton {
                                            set_label: "Errors",
                                            set_group: Some(&log_filter_all),
                                            connect_toggled[sender] => move |btn| {
                                                if btn.is_active() {
                                                    sender.input(AppMsg::LogFilterChanged(Some(LogCategory::Errors)));
                                                }
                                            },
                                        },
                                    },

                                    gtk::Button {
                                        set_icon_name: "edit-copy-symbolic",
                                        set_tooltip_text: Some("Copy visible log lines"),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                        connect_clicked => AppMsg::LogCopy,
                                    },
                                    gtk::Button {
                                        set_icon_name: "user-trash-symbolic",
                                        set_tooltip_text: Some("Clear log"),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                        connect_clicked => AppMsg::LogClear,
                                    },
                                },

                                gtk::Separator {},

                                gtk::ScrolledWindow {
                                    set_hscrollbar_policy: gtk::PolicyType::Never,
                                    set_vexpand: true,
                                    set_child: Some(&log_list_box),
                                },
                            } -> {
                                set_name: Some(Page::Logs.name()),
                                set_title: Some(Page::Logs.title()),
                            },

                            add = &gtk::ScrolledWindow {
                                set_hscrollbar_policy: gtk::PolicyType::Never,
                                #[wrap(Some)]
                                set_child = &adw::Clamp {
                                    set_maximum_size: 600,
                                    set_tightening_threshold: 0,
                                    set_margin_top: 18,
                                    set_margin_bottom: 18,
                                    set_margin_start: 12,
                                    set_margin_end: 12,
                                    #[wrap(Some)]
                                    set_child = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 12,

                                        adw::PreferencesGroup {
                                            set_title: "Authorized devices",
                                            #[wrap(Some)]
                                            set_header_suffix = &gtk::Button {
                                                add_css_class: "flat",
                                                connect_clicked => AppMsg::OpenFingerprintDialog(None),
                                                #[wrap(Some)]
                                                set_child = &gtk::Box {
                                                    set_spacing: 6,
                                                    gtk::Image { set_icon_name: Some("auth-fingerprint-symbolic") },
                                                    gtk::Label { set_label: "Authorize" },
                                                },
                                            },
                                            #[local_ref]
                                            authorized_list -> gtk::ListBox {
                                                set_selection_mode: gtk::SelectionMode::None,
                                                add_css_class: "boxed-list",
                                            },
                                        },

                                        adw::PreferencesGroup {
                                            set_title: "Behavior",
                                            adw::ActionRow {
                                                set_title: "release keys",
                                                set_subtitle: "Ctrl + Shift + Meta + Alt (default, not yet user-configurable from this page)",
                                            },
                                            adw::ActionRow {
                                                set_title: "enter hook",
                                                set_subtitle: "configured per-client — see the Screens page's client row",
                                            },
                                        },
                                    },
                                },
                            } -> {
                                set_name: Some(Page::Settings.name()),
                                set_title: Some(Page::Settings.title()),
                            },

                            #[watch]
                            set_visible_child_name: model.current_page.name(),
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
        let client_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        client_list.set_placeholder(Some(
            &adw::ActionRow::builder()
                .title("No connections!")
                .subtitle("add a new client via the + button")
                .build(),
        ));
        let client_rows = FactoryVecDeque::<ClientRowModel>::builder()
            .launch(client_list.clone())
            .forward(sender.input_sender(), AppMsg::ClientRow);

        let authorized_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        authorized_list.set_placeholder(Some(
            &adw::ActionRow::builder()
                .title("no devices registered!")
                .subtitle("authorize a new device via the \"Authorize\" button")
                .build(),
        ));
        let authorized_rows = FactoryVecDeque::<KeyRowModel>::builder()
            .launch(authorized_list.clone())
            .forward(sender.input_sender(), AppMsg::KeyRow);

        let incoming_device_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        incoming_device_list.set_placeholder(Some(
            &adw::ActionRow::builder()
                .title("no devices connected")
                .subtitle("devices entering this screen will show up here")
                .build(),
        ));
        let incoming_device_rows = FactoryVecDeque::<IncomingDeviceRowModel>::builder()
            .launch(incoming_device_list.clone())
            .detach();

        let audio_stream_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        audio_stream_list.set_placeholder(Some(
            &adw::ActionRow::builder()
                .title("no active audio streams")
                .build(),
        ));
        let audio_stream_rows = FactoryVecDeque::<AudioStreamRowModel>::builder()
            .launch(audio_stream_list.clone())
            .detach();

        let log_list_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .build();

        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_default();

        let mut model = AppModel {
            writer: init.writer,
            root: root.clone(),
            toast_overlay: adw::ToastOverlay::new(), // throwaway, overwritten below
            current_page: Page::Screens,
            hostname,
            port: DEFAULT_PORT,
            port_draft: String::new(),
            port_editing: false,
            updating_port_entry: false,
            port_entry: gtk::Entry::new(), // throwaway, overwritten below once `widgets` exists
            pk_fingerprint: String::new(),
            capture_active: false,
            emulation_active: false,
            client_rows,
            authorized_rows,
            incoming_device_rows,
            incoming_devices_by_addr: HashMap::new(),
            audio_stream_rows,
            audio_streams_by_addr: HashMap::new(),
            updating_audio_ui: false,
            audio_send: false,
            audio_receive: false,
            audio_bitrate: 96_000,
            audio_buffer_ms: 80,
            audio_loopback_supported: true,
            audio_capture_devices: Vec::new(),
            audio_playback_devices: Vec::new(),
            log: LogState::new(log_list_box.clone()),
            authorization_dialog: None,
            fingerprint_dialog: None,
        };

        let widgets = view_output!();

        model.port_entry = widgets.port_entry.clone();
        model.toast_overlay = widgets.toast_overlay.clone();

        // `ScreenArrangement` is a hand-rolled custom-GObject widget, not
        // gir-generated — it has no typed `connect_position_changed()`
        // method for the view!{} macro's `connect_NAME => closure` sugar
        // to call, so wire its custom signal manually (the same
        // `connect_closure` mechanism the widget's own signal machinery
        // already uses internally).
        widgets.screen_arrangement.connect_closure(
            "position-changed",
            false,
            glib::closure_local!(
                #[strong]
                sender,
                move |_widget: &ScreenArrangement, handle: u64, position: String| {
                    sender.input(AppMsg::ScreenPositionChanged(handle, position));
                }
            ),
        );

        widgets
            .sidebar_toggle
            .bind_property("active", &widgets.split_view, "show-sidebar")
            .sync_create()
            .bidirectional()
            .build();

        model.request(FrontendRequest::EnumerateAudioDevices);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            AppMsg::Frontend(event) => {
                self.log.log_event(&event);
                self.handle_frontend_event(event, &sender);
            }
            AppMsg::NavigateTo(page) => self.current_page = page,
            AppMsg::AddClient => self.request(FrontendRequest::Create),
            AppMsg::CopyHostname => {
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&self.hostname);
                }
            }
            AppMsg::CopyFingerprint => {
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&self.pk_fingerprint);
                }
            }
            AppMsg::PortEntryChanged(text) => {
                if !self.updating_port_entry {
                    self.port_draft = text;
                    self.port_editing = true;
                }
            }
            AppMsg::PortEditApply => {
                let port = self.port_draft.parse::<u16>().unwrap_or(DEFAULT_PORT);
                self.request(FrontendRequest::ChangePort(port));
            }
            AppMsg::PortEditCancel => {
                self.port_editing = false;
                self.updating_port_entry = true;
                self.port_entry.set_text(&self.port_entry_text());
                self.updating_port_entry = false;
            }
            AppMsg::ScreenPositionChanged(handle, position) => {
                if let Ok(position) = Position::from_str(&position) {
                    self.request(FrontendRequest::UpdatePosition(handle, position));
                }
            }
            AppMsg::OpenFingerprintDialog(fp) => self.open_fingerprint_dialog(&sender, fp),
            AppMsg::ToggleCapture => {
                #[cfg(target_os = "macos")]
                {
                    use crate::macos_privacy;
                    if macos_privacy::accessibility_granted() {
                        macos_privacy::relaunch_bundle();
                        relm4::main_application().quit();
                        return;
                    }
                    macos_privacy::open_accessibility_settings();
                    return;
                }
                #[cfg(not(target_os = "macos"))]
                self.request(FrontendRequest::EnableCapture);
            }
            AppMsg::ToggleEmulation => self.request(FrontendRequest::EnableEmulation),

            AppMsg::AudioSendToggled(active) => {
                if !self.updating_audio_ui {
                    self.request(FrontendRequest::SetAudioSend(active));
                }
            }
            AppMsg::AudioReceiveToggled(active) => {
                if !self.updating_audio_ui {
                    self.request(FrontendRequest::SetAudioReceive(active));
                }
            }
            AppMsg::AudioCaptureDeviceChanged(selected) => {
                if !self.updating_audio_ui {
                    let device = selected_audio_device(&self.audio_capture_devices, selected);
                    self.request(FrontendRequest::SetAudioCaptureDevice(device));
                }
            }
            AppMsg::AudioPlaybackDeviceChanged(selected) => {
                if !self.updating_audio_ui {
                    let device = selected_audio_device(&self.audio_playback_devices, selected);
                    self.request(FrontendRequest::SetAudioPlaybackDevice(device));
                }
            }
            AppMsg::AudioBitrateChanged(selected) => {
                if !self.updating_audio_ui {
                    let bitrate = AUDIO_BITRATES
                        .get(selected as usize)
                        .copied()
                        .unwrap_or(96_000);
                    self.audio_bitrate = bitrate;
                    self.request(FrontendRequest::UpdateAudioSettings {
                        bitrate,
                        buffer_ms: self.audio_buffer_ms,
                    });
                }
            }
            AppMsg::AudioBufferChanged(buffer_ms) => {
                if !self.updating_audio_ui {
                    self.audio_buffer_ms = buffer_ms;
                    self.request(FrontendRequest::UpdateAudioSettings {
                        bitrate: self.audio_bitrate,
                        buffer_ms,
                    });
                }
            }

            AppMsg::LogFilterChanged(filter) => self.log.apply_filter(filter),
            AppMsg::LogCopy => {
                let text = self.log.copy_visible();
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&text);
                }
            }
            AppMsg::LogClear => self.log.clear(),

            AppMsg::ClientRow(output) => self.handle_client_row_output(output),
            AppMsg::KeyRow(KeyRowOutput::Delete(index)) => {
                if let Some(row) = self.authorized_rows.get(index.current_index()) {
                    self.request(FrontendRequest::RemoveAuthorizedKey(
                        row.fingerprint().to_string(),
                    ));
                }
            }

            AppMsg::AuthorizationConfirmed(fingerprint) => {
                if let Some(dialog) = self.authorization_dialog.take() {
                    dialog.widget().force_close();
                }
                self.open_fingerprint_dialog(&sender, Some(fingerprint));
            }
            AppMsg::AuthorizationCancelled => {
                if let Some(dialog) = self.authorization_dialog.take() {
                    dialog.widget().force_close();
                }
            }
            AppMsg::FingerprintConfirmed(desc, fp) => {
                if let Some(dialog) = self.fingerprint_dialog.take() {
                    dialog.widget().force_close();
                }
                self.request(FrontendRequest::AuthorizeKey(desc, fp));
            }

            #[cfg(target_os = "macos")]
            AppMsg::MacosAccessibilityGranted => {}
            #[cfg(target_os = "macos")]
            AppMsg::MacosAccessibilityRevoked => {
                relm4::main_application().quit();
            }
        }
    }
}
