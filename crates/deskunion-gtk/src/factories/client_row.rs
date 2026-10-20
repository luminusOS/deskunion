use adw::prelude::*;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::prelude::*;

use deskunion_ipc::{ClientConfig, ClientHandle, ClientState, Position};

const NO_HOSTNAME_TITLE: &str = "No hostname";
const PORT_ERROR: &str = "Enter a port from 1 to 65535";

fn position_from_selected(selected: u32) -> Position {
    match selected {
        1 => Position::Right,
        2 => Position::Top,
        3 => Position::Bottom,
        _ => Position::Left,
    }
}

fn parse_port(text: &str) -> Option<u16> {
    text.trim().parse::<u16>().ok().filter(|port| *port != 0)
}

fn set_port_validity(entry: &adw::EntryRow, invalid: bool) {
    let widget: &gtk::Widget = entry.upcast_ref();
    let (state, description) = if invalid {
        (gtk::AccessibleInvalidState::True, PORT_ERROR)
    } else {
        (gtk::AccessibleInvalidState::False, "")
    };
    widget.update_state(&[gtk::accessible::State::Invalid(state)]);
    widget.update_property(&[gtk::accessible::Property::Description(description)]);
}

pub struct ClientRowInit {
    pub handle: ClientHandle,
    pub config: ClientConfig,
    pub state: ClientState,
}

#[derive(Debug)]
struct ClientDraft {
    confirmed_hostname: Option<String>,
    confirmed_port: u16,
    hostname: String,
    port: String,
    port_error: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AdoptedFields {
    hostname: bool,
    port: bool,
}

impl ClientDraft {
    fn new(hostname: Option<String>, port: u16) -> Self {
        Self {
            hostname: hostname.clone().unwrap_or_default(),
            confirmed_hostname: hostname,
            confirmed_port: port,
            port: port.to_string(),
            port_error: false,
        }
    }

    fn title(&self) -> &str {
        self.confirmed_hostname
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(NO_HOSTNAME_TITLE)
    }

    fn hostname_dirty(&self) -> bool {
        self.hostname.trim() != self.confirmed_hostname.as_deref().unwrap_or_default()
    }

    fn port_dirty(&self) -> bool {
        parse_port(&self.port) != Some(self.confirmed_port)
    }

    /// Unapplied edits are kept so a config refresh never discards what the
    /// user is typing; only fields without pending edits adopt the new value.
    fn apply_config(&mut self, hostname: Option<String>, port: u16) -> AdoptedFields {
        let adopted = AdoptedFields {
            hostname: !self.hostname_dirty(),
            port: !self.port_dirty(),
        };
        self.confirmed_hostname = hostname;
        self.confirmed_port = port;
        if adopted.hostname {
            self.hostname = self.confirmed_hostname.clone().unwrap_or_default();
        }
        if adopted.port {
            self.port = port.to_string();
        }
        adopted
    }

    fn edit_hostname(&mut self, text: String) {
        self.hostname = text;
    }

    fn edit_port(&mut self, text: String) {
        self.port = text;
        self.port_error = false;
    }

    fn submit_hostname(&self) -> String {
        self.hostname.trim().to_string()
    }

    fn submit_port(&mut self) -> Option<u16> {
        let port = parse_port(&self.port);
        self.port_error = port.is_none();
        port
    }
}

pub struct ClientRowModel {
    index: DynamicIndex,
    handle: ClientHandle,
    draft: ClientDraft,
    position: Position,
    active: bool,
    /// used by `AppModel` to correlate `FrontendEvent::AudioStream` (which
    /// only carries a socket addr) back to this row's `ClientHandle`
    pub active_addr: Option<String>,
    audio_active: bool,
    /// last latency reported by the audio stream from this client;
    /// `None` while no stream is active
    audio_latency_ms: Option<u32>,
    /// written back to the entry widgets by `update_with_view`, so only
    /// fields replaced by a config refresh get their text rewritten
    adopted: AdoptedFields,
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
    HostnameEdited(String),
    PortEdited(String),
    SubmitHostname,
    SubmitPort,
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
        self.draft.confirmed_hostname.as_deref()
    }

    pub fn port(&self) -> u16 {
        self.draft.confirmed_port
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
            set_use_markup: false,
            #[watch]
            set_title: self.draft.title(),
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

            #[name(hostname_entry)]
            add_row = &adw::EntryRow {
                set_title: "Hostname or IP address",
                set_show_apply_button: true,
                set_text: &self.draft.hostname,
                connect_changed[sender] => move |row| {
                    sender.input(ClientRowInput::HostnameEdited(row.text().to_string()));
                },
                connect_apply[sender] => move |_| {
                    sender.input(ClientRowInput::SubmitHostname);
                },
            },

            #[name(port_entry)]
            add_row = &adw::EntryRow {
                set_title: "Port",
                set_show_apply_button: true,
                set_input_purpose: gtk::InputPurpose::Digits,
                set_text: &self.draft.port,
                connect_changed[sender] => move |row| {
                    sender.input(ClientRowInput::PortEdited(row.text().to_string()));
                },
                connect_apply[sender] => move |_| {
                    sender.input(ClientRowInput::SubmitPort);
                },
            },

            #[name(port_error_row)]
            add_row = &adw::ActionRow {
                set_title: PORT_ERROR,
                set_activatable: false,
                add_css_class: "error",
                add_prefix = &gtk::Image {
                    set_icon_name: Some("dialog-warning-symbolic"),
                },
                #[watch]
                set_visible: self.draft.port_error,
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
            draft: ClientDraft::new(config.hostname, config.port),
            position: config.pos,
            active: state.active,
            active_addr: state.active_addr.map(|a| a.to_string()),
            audio_active: false,
            audio_latency_ms: None,
            adopted: AdoptedFields::default(),
        }
    }

    fn update(&mut self, message: Self::Input, sender: FactorySender<Self>) {
        match message {
            ClientRowInput::SetConfig(config) => {
                self.adopted = self.draft.apply_config(config.hostname, config.port);
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
            ClientRowInput::HostnameEdited(text) => self.draft.edit_hostname(text),
            ClientRowInput::PortEdited(text) => self.draft.edit_port(text),
            ClientRowInput::SubmitHostname => {
                sender
                    .output(ClientRowOutput::HostnameChange(
                        self.index.clone(),
                        self.draft.submit_hostname(),
                    ))
                    .unwrap();
            }
            ClientRowInput::SubmitPort => {
                if let Some(port) = self.draft.submit_port() {
                    sender
                        .output(ClientRowOutput::PortChange(self.index.clone(), port))
                        .unwrap();
                }
            }
        }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: FactorySender<Self>,
    ) {
        self.update(message, sender);

        let adopted = std::mem::take(&mut self.adopted);
        if adopted.hostname {
            widgets.hostname_entry.set_text(&self.draft.hostname);
        }
        if adopted.port {
            widgets.port_entry.set_text(&self.draft.port);
        }
        set_port_validity(&widgets.port_entry, self.draft.port_error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_draft(hostname: Option<&str>, port: u16) -> ClientDraft {
        ClientDraft::new(hostname.map(str::to_owned), port)
    }

    #[test]
    fn port_accepts_only_nonzero_u16() {
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("1"), Some(1));
        assert_eq!(parse_port(" 4242 "), Some(4242));
        assert_eq!(parse_port("65535"), Some(65535));
        assert_eq!(parse_port("65536"), None);
        assert_eq!(parse_port("-1"), None);
        assert_eq!(parse_port("4a"), None);
        assert_eq!(parse_port(""), None);
    }

    #[test]
    fn invalid_port_reports_error_until_edited() {
        let mut draft = new_draft(Some("lab"), 4242);
        draft.edit_port("0".into());
        assert_eq!(draft.submit_port(), None);
        assert!(draft.port_error);

        draft.edit_port("80".into());
        assert!(!draft.port_error);
        assert_eq!(draft.submit_port(), Some(80));
        assert!(!draft.port_error);
    }

    #[test]
    fn config_refresh_keeps_unapplied_drafts() {
        let mut draft = new_draft(Some("lab"), 4242);
        draft.edit_hostname("lab-office".into());
        draft.edit_port("5000".into());

        let adopted = draft.apply_config(Some("lab-remote".into()), 4242);

        assert_eq!(
            adopted,
            AdoptedFields {
                hostname: false,
                port: false
            }
        );
        assert_eq!(draft.hostname, "lab-office");
        assert_eq!(draft.port, "5000");
        assert_eq!(draft.confirmed_hostname.as_deref(), Some("lab-remote"));
        assert!(draft.hostname_dirty());
        assert!(draft.port_dirty());
    }

    #[test]
    fn config_refresh_adopts_fields_without_pending_edits() {
        let mut draft = new_draft(Some("lab"), 4242);

        let adopted = draft.apply_config(Some("lab-2".into()), 5000);

        assert_eq!(
            adopted,
            AdoptedFields {
                hostname: true,
                port: true
            }
        );
        assert_eq!(draft.hostname, "lab-2");
        assert_eq!(draft.port, "5000");
        assert!(!draft.hostname_dirty());
        assert!(!draft.port_dirty());
    }

    #[test]
    fn echoed_apply_clears_dirty_hostname() {
        let mut draft = new_draft(Some("lab"), 4242);
        draft.edit_hostname("  lab-office ".into());
        assert!(draft.hostname_dirty());

        assert_eq!(draft.submit_hostname(), "lab-office");
        draft.apply_config(Some("lab-office".into()), 4242);

        assert!(!draft.hostname_dirty());
        assert_eq!(draft.hostname, "  lab-office ");
    }

    #[test]
    fn config_refresh_keeps_pending_invalid_port_and_its_error() {
        let mut draft = new_draft(None, 4242);
        draft.edit_port("99999".into());
        assert_eq!(draft.submit_port(), None);

        let adopted = draft.apply_config(None, 4242);

        assert!(!adopted.port);
        assert!(draft.port_error);
        assert_eq!(draft.port, "99999");
    }

    #[test]
    fn title_is_plain_text_with_placeholder_fallback() {
        assert_eq!(new_draft(Some("a&b<c>"), 1).title(), "a&b<c>");
        assert_eq!(new_draft(None, 1).title(), NO_HOSTNAME_TITLE);
        assert_eq!(new_draft(Some("  "), 1).title(), NO_HOSTNAME_TITLE);
    }
}
