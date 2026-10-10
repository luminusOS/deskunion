use adw::prelude::*;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::prelude::*;

use deskunion_ipc::{ClientConfig, ClientHandle, ClientState, Position};

fn position_from_selected(selected: u32) -> Position {
    match selected {
        1 => Position::Right,
        2 => Position::Top,
        3 => Position::Bottom,
        _ => Position::Left,
    }
}

pub struct ClientRowInit {
    pub handle: ClientHandle,
    pub config: ClientConfig,
    pub state: ClientState,
}

pub struct ClientRowModel {
    index: DynamicIndex,
    handle: ClientHandle,
    hostname: Option<String>,
    port: u32,
    position: Position,
    active: bool,
    /// used by `AppModel` to correlate `FrontendEvent::AudioStream` (which
    /// only carries a socket addr) back to this row's `ClientHandle`
    pub active_addr: Option<String>,
    audio_active: bool,
    /// last latency reported by the audio stream from this client;
    /// `None` while no stream is active
    audio_latency_ms: Option<u32>,
}

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum ClientRowInput {
    SetConfig(ClientConfig),
    SetState(ClientState),
    SetAudioStats {
        active: bool,
        latency_ms: Option<u32>,
    },
    SetPosition(Position),
}

#[derive(Debug)]
pub enum ClientRowOutput {
    Activate(DynamicIndex, bool),
    Delete(DynamicIndex),
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

    pub fn port(&self) -> u16 {
        self.port as u16
    }

    /// hostname if set, else a placeholder — used both as the row's title
    /// and (via `ScreenArrangement`) the canvas satellite label.
    fn display_title(&self) -> String {
        self.hostname.clone().unwrap_or_else(|| {
            "<span font_style=\"italic\" font_weight=\"light\" foreground=\"darkgrey\">No hostname</span>".to_string()
        })
    }

    fn ip(&self) -> Option<&str> {
        let addr = self.active_addr.as_deref()?;
        Some(addr.rsplit_once(':').map_or(addr, |(ip, _)| ip))
    }

    fn connected(&self) -> bool {
        self.active_addr.is_some()
    }

    /// Connection details. Latency is real data from the audio stream
    /// and is shown only while streaming.
    fn connection_subtitle(&self) -> String {
        let Some(addr) = self.active_addr.as_deref() else {
            return String::new();
        };
        let ip = addr.rsplit_once(':').map(|(ip, _)| ip).unwrap_or(addr);
        let mut parts = vec![ip.to_string()];
        if self.audio_active
            && let Some(ms) = self.audio_latency_ms
        {
            parts.push(format!("audio buffer {ms} ms"));
        }
        parts.push("TLS active".to_string());
        if self.audio_active {
            parts.push("sending audio".to_string());
        }
        parts.join(" · ")
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
            set_subtitle: &self.connection_subtitle(),
            add_prefix = &gtk::Label {
                set_valign: gtk::Align::Center,
                add_css_class: "screen-number",
                #[watch]
                set_label: &(self.index.current_index() + 2).to_string(),
                update_property: &[gtk::accessible::Property::Label("Screen number")],
            },

            add_suffix = &gtk::Image {
                set_icon_name: Some("network-cellular-signal-excellent-symbolic"),
                add_css_class: "success",
                set_valign: gtk::Align::Center,
                #[watch]
                set_visible: self.connected(),
            },

            add_suffix = &gtk::Label {
                set_label: "Connected",
                set_valign: gtk::Align::Center,
                add_css_class: "status-pill",
                add_css_class: "connected",
                #[watch]
                set_visible: self.connected(),
            },

            add_suffix = &gtk::Image {
                set_icon_name: Some("channel-secure-symbolic"),
                add_css_class: "dim-label",
                set_valign: gtk::Align::Center,
                set_tooltip_text: Some("Connection is encrypted (TLS)"),
                update_property: &[gtk::accessible::Property::Label("Connection is encrypted")],
                #[watch]
                set_visible: self.connected(),
            },

            add_row = &adw::SwitchRow {
                set_title: "Enabled",
                set_subtitle: "Route input events to this client",
                #[watch]
                #[block_signal(active_handler)]
                set_active: self.active,
                connect_active_notify[sender, index] => move |row| {
                    sender.output(ClientRowOutput::Activate(index.clone(), row.is_active())).unwrap();
                } @active_handler,
            },

            add_row = &adw::EntryRow {
                set_title: "Hostname or IP address",
                set_show_apply_button: true,
                #[watch]
                set_text: self.hostname.as_deref().unwrap_or(""),
                connect_apply[sender, index] => move |row| {
                    sender.output(ClientRowOutput::HostnameChange(index.clone(), row.text().trim().to_string())).unwrap();
                },
            },

            add_row = &adw::EntryRow {
                set_title: "Port",
                set_show_apply_button: true,
                set_input_purpose: gtk::InputPurpose::Digits,
                #[watch]
                set_text: &self.port.to_string(),
                connect_apply[sender, index] => move |row| {
                    match row.text().trim().parse::<u16>() {
                        Ok(port) if port != 0 => {
                            row.remove_css_class("error");
                            sender.output(ClientRowOutput::PortChange(index.clone(), port)).unwrap();
                        }
                        _ => row.add_css_class("error"),
                    }
                },
            },

            add_row = &adw::ActionRow {
                set_title: "Connected from",
                set_subtitle_selectable: true,
                #[watch]
                set_subtitle: self.ip().unwrap_or_default(),
                #[watch]
                set_visible: self.connected(),
            },

            add_row = &adw::ComboRow {
                set_title: "Position",
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
                set_title: "Delete client",

                add_suffix = &gtk::Button {
                    set_valign: gtk::Align::Center,
                    set_halign: gtk::Align::Center,
                    set_icon_name: "user-trash-symbolic",
                    add_css_class: "error",
                    set_tooltip_text: Some("Delete client"),
                    update_property: &[gtk::accessible::Property::Label("Delete client")],
                    connect_clicked[sender, index] => move |button| {
                        let dialog = adw::AlertDialog::new(
                            Some("Delete this client?"),
                            Some("The device will need to be paired again."),
                        );
                        dialog.add_response("cancel", "_Cancel");
                        dialog.add_response("delete", "_Delete");
                        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                        dialog.set_default_response(Some("cancel"));
                        dialog.set_close_response("cancel");
                        let (sender, index) = (sender.clone(), index.clone());
                        dialog.connect_response(None, move |_, response| {
                            if response == "delete" {
                                sender.output(ClientRowOutput::Delete(index.clone())).unwrap();
                            }
                        });
                        dialog.present(Some(button));
                    },
                },
            },
        }
    }

    fn init_model(init: Self::Init, index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        let ClientRowInit {
            handle,
            config,
            state,
        } = init;
        Self {
            index: index.clone(),
            handle,
            hostname: config.hostname,
            port: config.port as u32,
            position: config.pos,
            active: state.active,
            active_addr: state.active_addr.map(|a| a.to_string()),
            audio_active: false,
            audio_latency_ms: None,
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
                self.active_addr = state.active_addr.map(|a| a.to_string());
            }
            ClientRowInput::SetAudioStats { active, latency_ms } => {
                self.audio_active = active;
                self.audio_latency_ms = latency_ms;
            }
            ClientRowInput::SetPosition(position) => {
                self.position = position;
            }
        }
    }
}
