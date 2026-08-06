use adw::prelude::*;
use gtk::glib;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::prelude::*;

use deskunion_ipc::{ClientConfig, ClientHandle, ClientState, DEFAULT_PORT, Position};

fn position_from_selected(selected: u32) -> Position {
    match selected {
        1 => Position::Right,
        2 => Position::Top,
        3 => Position::Bottom,
        _ => Position::Left,
    }
}

/// Render the 8-byte ASCII commit hash carried in
/// [`deskunion_ipc::ClientState::peer_commit`] as a `String`. `None` in →
/// `None` out (peer hasn't sent a Hello yet, or speaks an older proto).
fn peer_commit_to_string(commit: Option<[u8; 8]>) -> Option<String> {
    commit.and_then(|c| std::str::from_utf8(&c).ok().map(str::to_string))
}

pub struct ClientRowInit {
    pub handle: ClientHandle,
    pub config: ClientConfig,
    pub state: ClientState,
}

pub struct ClientRowModel {
    handle: ClientHandle,
    hostname: Option<String>,
    port: u32,
    position: Position,
    active: bool,
    resolving: bool,
    ips: Vec<String>,
    peer_commit: Option<String>,
    /// used by `AppModel` to correlate `FrontendEvent::AudioStream` (which
    /// only carries a socket addr) back to this row's `ClientHandle`
    pub active_addr: Option<String>,
    audio_active: bool,
}

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum ClientRowInput {
    SetConfig(ClientConfig),
    SetState(ClientState),
    SetAudioActive(bool),
}

#[derive(Debug)]
pub enum ClientRowOutput {
    Activate(DynamicIndex, bool),
    Delete(DynamicIndex),
    RequestDns(DynamicIndex),
    HostnameChange(DynamicIndex, String),
    PortChange(DynamicIndex, u16),
    PositionChange(DynamicIndex, Position),
}

impl ClientRowModel {
    pub fn handle(&self) -> ClientHandle {
        self.handle
    }

    pub fn audio_active(&self) -> bool {
        self.audio_active
    }

    pub fn position(&self) -> Position {
        self.position
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn hostname(&self) -> Option<&str> {
        self.hostname.as_deref()
    }

    /// hostname if set, else a placeholder — used both as the row's title
    /// and (via `ScreenArrangement`) the canvas satellite label.
    fn display_title(&self) -> String {
        self.hostname.clone().unwrap_or_else(|| {
            "<span font_style=\"italic\" font_weight=\"light\" foreground=\"darkgrey\">no hostname!</span>".to_string()
        })
    }

    fn port_text(&self) -> String {
        if self.port == DEFAULT_PORT as u32 {
            String::new()
        } else {
            self.port.to_string()
        }
    }

    fn dns_tooltip(&self) -> String {
        if self.ips.is_empty() {
            "no ip addresses associated with this client".to_string()
        } else {
            self.ips.join("\n")
        }
    }

    /// Collapsed subtitle (Pango markup) + peer-match CSS class, based on
    /// `peer_commit` vs. the local build's commit. Soft-warn semantics: a
    /// missing or mismatched peer commit surfaces as orange text but never
    /// blocks traffic.
    fn version_subtitle(&self) -> String {
        let local = crate::local_commit_str();
        match self.peer_commit.as_deref() {
            None => format!("Peer version: unknown · Ours: {local}"),
            Some(p) if p == local.as_str() => format!("Peer version: {p} · matched"),
            Some(p) => format!("Peer version: {p} · Ours: {local}"),
        }
    }

    fn version_css_class(&self) -> &'static str {
        let local = crate::local_commit_str();
        match self.peer_commit.as_deref() {
            Some(p) if p == local.as_str() => "peer-match",
            Some(_) => "peer-mismatch",
            None => "peer-unknown",
        }
    }

    fn dns_css_class(&self) -> &'static str {
        if self.ips.is_empty() {
            "warning"
        } else {
            "success"
        }
    }
}

#[relm4::factory(pub)]
impl FactoryComponent for ClientRowModel {
    type Init = ClientRowInit;
    type Input = ClientRowInput;
    type Output = ClientRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        root = adw::ExpanderRow {
            #[watch]
            set_title: &self.display_title(),
            #[watch]
            set_use_markup: true,
            #[watch]
            set_subtitle: &self.version_subtitle(),
            #[watch]
            set_css_classes: &[self.version_css_class()],

            add_prefix = &gtk::Switch {
                set_valign: gtk::Align::Center,
                set_halign: gtk::Align::End,
                set_tooltip_text: Some("enable"),
                #[watch]
                #[block_signal(active_handler)]
                set_state: self.active,
                #[watch]
                #[block_signal(active_handler)]
                set_active: self.active,
                connect_state_set[sender, index] => move |_, state| {
                    sender.output(ClientRowOutput::Activate(index.clone(), state)).unwrap();
                    glib::Propagation::Proceed
                } @active_handler,
            },

            add_suffix = &gtk::Spinner {
                #[watch]
                set_spinning: self.resolving,
            },

            add_suffix = &gtk::Button {
                set_valign: gtk::Align::Center,
                set_halign: gtk::Align::End,
                set_tooltip_text: Some("resolve host"),
                set_icon_name: "network-wired-symbolic",
                #[watch]
                set_css_classes: &[self.dns_css_class()],
                #[watch]
                set_tooltip_text: Some(&self.dns_tooltip()),
                connect_clicked[sender, index] => move |_| {
                    sender.output(ClientRowOutput::RequestDns(index.clone())).unwrap();
                },
            },

            add_row = &adw::ActionRow {
                set_title: "hostname",
                #[watch]
                set_subtitle: &self.port_text(),

                add_suffix = &gtk::Entry {
                    set_property: ("xalign", 0.5f32),
                    set_valign: gtk::Align::Center,
                    set_placeholder_text: Some("hostname"),
                    set_width_chars: -1,
                    #[watch]
                    #[block_signal(hostname_handler)]
                    set_text: self.hostname.as_deref().unwrap_or(""),
                    connect_changed[sender, index] => move |entry| {
                        sender.output(ClientRowOutput::HostnameChange(index.clone(), entry.text().to_string())).unwrap();
                    } @hostname_handler,
                },

                add_suffix = &gtk::Entry {
                    set_max_width_chars: 5,
                    set_input_purpose: gtk::InputPurpose::Number,
                    set_property: ("xalign", 0.5f32),
                    set_valign: gtk::Align::Center,
                    set_placeholder_text: Some("4242"),
                    set_width_chars: 5,
                    #[watch]
                    #[block_signal(port_handler)]
                    set_text: &self.port_text(),
                    connect_changed[sender, index] => move |entry| {
                        if let Ok(port) = entry.text().parse::<u16>() {
                            sender.output(ClientRowOutput::PortChange(index.clone(), port)).unwrap();
                        }
                    } @port_handler,
                },
            },

            add_row = &adw::ComboRow {
                set_title: "position",
                set_model: Some(&gtk::StringList::new(&["Left", "Right", "Top", "Bottom"])),
                #[watch]
                #[block_signal(position_handler)]
                set_selected: self.position as u32,
                connect_selected_notify[sender, index] => move |combo| {
                    sender.output(ClientRowOutput::PositionChange(
                        index.clone(),
                        position_from_selected(combo.selected()),
                    )).unwrap();
                } @position_handler,
            },

            add_row = &adw::ActionRow {
                set_title: "delete this client",

                add_suffix = &gtk::Button {
                    set_valign: gtk::Align::Center,
                    set_halign: gtk::Align::Center,
                    set_icon_name: "user-trash-symbolic",
                    add_css_class: "error",
                    connect_clicked[sender, index] => move |_| {
                        sender.output(ClientRowOutput::Delete(index.clone())).unwrap();
                    },
                },
            },
        }
    }

    fn init_model(init: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        let ClientRowInit {
            handle,
            config,
            state,
        } = init;
        Self {
            handle,
            hostname: config.hostname,
            port: config.port as u32,
            position: config.pos,
            active: state.active,
            resolving: state.resolving,
            ips: state.ips.iter().map(|ip| ip.to_string()).collect(),
            peer_commit: peer_commit_to_string(state.peer_commit),
            active_addr: state.active_addr.map(|a| a.to_string()),
            audio_active: false,
        }
    }

    fn update(&mut self, message: Self::Input, _sender: FactorySender<Self>) {
        match message {
            ClientRowInput::SetConfig(config) => {
                self.hostname = config.hostname;
                self.port = config.port as u32;
                self.position = config.pos;
            }
            ClientRowInput::SetState(state) => {
                self.active = state.active;
                self.resolving = state.resolving;
                self.ips = state.ips.iter().map(|ip| ip.to_string()).collect();
                self.peer_commit = peer_commit_to_string(state.peer_commit);
                self.active_addr = state.active_addr.map(|a| a.to_string());
            }
            ClientRowInput::SetAudioActive(active) => {
                self.audio_active = active;
            }
        }
    }
}
