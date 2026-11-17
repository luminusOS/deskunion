use adw::prelude::*;
use gtk::glib;
use relm4::factory::FactoryVecDeque;
use relm4::prelude::*;

use deskunion_ipc::{DEFAULT_PORT, FrontendEvent, FrontendRequest, OperationMode};

use super::PageOutput;
use crate::app::copy;
use crate::app::session::Session;
use crate::factories::{KeyRowModel, KeyRowOutput};

pub(crate) fn parse_listening_port(text: &str) -> Option<u16> {
    let text = text.trim();
    if text.is_empty() {
        Some(DEFAULT_PORT)
    } else {
        text.parse::<u16>().ok().filter(|port| *port != 0)
    }
}

const PORT_RANGE: &str = "Enter a port between 1 and 65535";

/// The role row offers only the two roles; an unconfigured computer
/// chooses one on the first-run page and does not show the row.
pub(crate) fn operation_mode_index(mode: OperationMode) -> u32 {
    match mode {
        OperationMode::Client => 1,
        OperationMode::Server | OperationMode::Unconfigured => 0,
    }
}

fn operation_mode_at(index: u32) -> OperationMode {
    match index {
        1 => OperationMode::Client,
        _ => OperationMode::Server,
    }
}

#[derive(Debug)]
pub enum SettingsMsg {
    Frontend(Box<FrontendEvent>),
    Session(Session),
    ModeSelected(u32),
    CopyHostname,
    CopyFingerprint,
    PortEdited,
    PortApply,
    PortReset,
    Authorize,
    KeyRow(KeyRowOutput),
    ClipboardToggled(bool),
}

pub struct SettingsInit {
    pub hostname: String,
}

pub struct SettingsPage {
    mode: OperationMode,
    hostname: String,
    port: u16,
    /// why the port row is marked as an error
    port_error: Option<String>,
    port_row: adw::EntryRow,
    fingerprint: String,
    authorized_rows: FactoryVecDeque<KeyRowModel>,
    clipboard_enabled: bool,
    clipboard_restart_required: bool,
}

impl SettingsPage {
    #[cfg(all(test, feature = "ui-tests"))]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[cfg(all(test, feature = "ui-tests"))]
    pub fn port_invalid(&self) -> bool {
        self.port_error.is_some()
    }

    #[cfg(all(test, feature = "ui-tests"))]
    pub fn clipboard(&self) -> (bool, bool) {
        (self.clipboard_enabled, self.clipboard_restart_required)
    }

    fn reset_port_row(&self) {
        // a programmatic change is not an edit: keep the apply button hidden
        self.port_row.set_show_apply_button(false);
        self.port_row.set_text(&self.port.to_string());
        self.port_row.set_show_apply_button(true);
    }

    fn copy(sender: &ComponentSender<Self>, text: &str) {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(text);
            let _ = sender.output(PageOutput::Toast("Copied".to_string()));
        }
    }

    fn handle_frontend_event(&mut self, event: FrontendEvent, sender: &ComponentSender<Self>) {
        match event {
            FrontendEvent::PortChanged(port, error) => {
                self.port = port;
                self.port_error = error.clone();
                match error {
                    Some(error) => {
                        let _ = sender.output(PageOutput::Error(error));
                    }
                    None => self.reset_port_row(),
                }
            }
            FrontendEvent::PublicKeyFingerprint(fingerprint) => self.fingerprint = fingerprint,
            FrontendEvent::AuthorizedUpdated(keys) => {
                let mut guard = self.authorized_rows.guard();
                guard.clear();
                for (fingerprint, description) in keys {
                    guard.push_back((description, fingerprint));
                }
            }
            FrontendEvent::ClipboardStatus {
                enabled,
                restart_required,
            } => {
                self.clipboard_enabled = enabled;
                self.clipboard_restart_required = restart_required;
            }
            _ => {}
        }
    }
}

#[relm4::component(pub)]
impl SimpleComponent for SettingsPage {
    type Init = SettingsInit;
    type Input = SettingsMsg;
    type Output = PageOutput;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {},

            #[wrap(Some)]
            set_content = &adw::PreferencesPage {
                adw::PreferencesGroup {
                    // choosing a role happens on the first-run page
                    #[watch]
                    set_visible: model.mode != OperationMode::Unconfigured,

                    #[name(mode_row)]
                    adw::ComboRow {
                        set_title: "Role",
                        set_model: Some(&gtk::StringList::new(&["Server", "Client"])),
                        #[watch]
                        #[block_signal(mode_handler)]
                        set_selected: operation_mode_index(model.mode),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(SettingsMsg::ModeSelected(row.selected()));
                        } @mode_handler,
                    },
                },

                adw::PreferencesGroup {
                    set_title: "This computer",

                    adw::ActionRow {
                        set_title: "Hostname",
                        set_subtitle: &model.hostname,
                        set_use_markup: false,
                        add_css_class: "property",
                        add_suffix = &gtk::Button {
                            set_valign: gtk::Align::Center,
                            set_icon_name: "edit-copy-symbolic",
                            add_css_class: "flat",
                            set_tooltip_text: Some("Copy hostname"),
                            update_property: &[gtk::accessible::Property::Label("Copy hostname")],
                            connect_clicked => SettingsMsg::CopyHostname,
                        },
                    },

                    #[local_ref]
                    port_row -> adw::EntryRow {
                        set_title: "Listening port",
                        set_input_purpose: gtk::InputPurpose::Digits,
                        set_show_apply_button: true,
                        #[watch]
                        set_visible: model.mode != OperationMode::Client,
                        #[watch]
                        set_class_active: ("error", model.port_error.is_some()),
                        #[watch]
                        set_tooltip_text: model.port_error.as_deref(),
                        connect_changed => SettingsMsg::PortEdited,
                        connect_apply => SettingsMsg::PortApply,
                    },

                    adw::ActionRow {
                        set_title: "Certificate fingerprint",
                        set_use_markup: false,
                        set_subtitle_lines: 0,
                        set_subtitle_selectable: true,
                        add_css_class: "property",
                        add_css_class: "fingerprint",
                        #[watch]
                        set_subtitle: &model.fingerprint,
                        add_suffix = &gtk::Button {
                            set_valign: gtk::Align::Center,
                            set_icon_name: "edit-copy-symbolic",
                            add_css_class: "flat",
                            set_tooltip_text: Some("Copy fingerprint"),
                            update_property: &[gtk::accessible::Property::Label("Copy fingerprint")],
                            connect_clicked => SettingsMsg::CopyFingerprint,
                        },
                    },
                },

                adw::PreferencesGroup {
                    set_title: "Authorized devices",
                    #[watch]
                    set_visible: model.mode != OperationMode::Client,
                    #[wrap(Some)]
                    set_header_suffix = &gtk::Button {
                        set_label: "Authorize…",
                        add_css_class: "flat",
                        connect_clicked => SettingsMsg::Authorize,
                    },
                    #[local_ref]
                    authorized_list -> gtk::ListBox {
                        set_selection_mode: gtk::SelectionMode::None,
                        add_css_class: "boxed-list",
                    },
                },

                adw::PreferencesGroup {
                    adw::SwitchRow {
                        set_title: "Share clipboard text",
                        #[watch]
                        set_subtitle: if model.clipboard_restart_required {
                            "Restart DeskUnion on both computers to apply"
                        } else {
                            ""
                        },
                        #[watch]
                        #[block_signal(clipboard_handler)]
                        set_active: model.clipboard_enabled,
                        connect_active_notify[sender] => move |row| {
                            sender.input(SettingsMsg::ClipboardToggled(row.is_active()));
                        } @clipboard_handler,
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
        let authorized_list = gtk::ListBox::new();
        authorized_list.set_placeholder(Some(
            &adw::ActionRow::builder()
                .title(copy::NO_AUTHORIZED_DEVICES)
                .build(),
        ));
        let authorized_rows = FactoryVecDeque::<KeyRowModel>::builder()
            .launch(authorized_list.clone())
            .forward(sender.input_sender(), SettingsMsg::KeyRow);
        let port_row = adw::EntryRow::new();
        let model = SettingsPage {
            mode: OperationMode::default(),
            hostname: init.hostname,
            port: DEFAULT_PORT,
            port_error: None,
            port_row: port_row.clone(),
            fingerprint: String::new(),
            authorized_rows,
            clipboard_enabled: true,
            clipboard_restart_required: false,
        };
        let widgets = view_output!();
        model.reset_port_row();
        let escape = gtk::EventControllerKey::new();
        escape.connect_key_pressed({
            let sender = sender.clone();
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    sender.input(SettingsMsg::PortReset);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        model.port_row.add_controller(escape);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            SettingsMsg::Frontend(event) => self.handle_frontend_event(*event, &sender),
            SettingsMsg::Session(session) => self.mode = session.mode,
            SettingsMsg::ModeSelected(index) => {
                let _ = sender.output(PageOutput::SetMode(operation_mode_at(index)));
            }
            SettingsMsg::CopyHostname => Self::copy(&sender, &self.hostname),
            SettingsMsg::CopyFingerprint => Self::copy(&sender, &self.fingerprint),
            SettingsMsg::PortEdited => self.port_error = None,
            SettingsMsg::PortReset => {
                self.port_error = None;
                self.reset_port_row();
            }
            SettingsMsg::PortApply => match parse_listening_port(&self.port_row.text()) {
                Some(port) => {
                    let _ = sender.output(PageOutput::Request(FrontendRequest::ChangePort(port)));
                }
                None => {
                    self.port_error = Some(PORT_RANGE.to_string());
                    // a toast is announced and seen without hovering the row
                    let _ = sender.output(PageOutput::Toast(PORT_RANGE.to_string()));
                }
            },
            SettingsMsg::Authorize => {
                let _ = sender.output(PageOutput::OpenFingerprintDialog(None));
            }
            SettingsMsg::KeyRow(KeyRowOutput::Delete(index)) => {
                if let Some(row) = self.authorized_rows.get(index.current_index()) {
                    let _ = sender.output(PageOutput::Request(
                        FrontendRequest::RemoveAuthorizedKey(row.fingerprint().to_string()),
                    ));
                }
            }
            SettingsMsg::ClipboardToggled(enabled) => {
                let _ = sender.output(PageOutput::Request(FrontendRequest::SetClipboardEnabled(
                    enabled,
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_port_uses_the_default() {
        assert_eq!(parse_listening_port(""), Some(DEFAULT_PORT));
        assert_eq!(parse_listening_port("  "), Some(DEFAULT_PORT));
    }

    #[test]
    fn listening_port_accepts_the_valid_range() {
        assert_eq!(parse_listening_port("1"), Some(1));
        assert_eq!(parse_listening_port("65535"), Some(u16::MAX));
        assert_eq!(parse_listening_port(" 4243 "), Some(4243));
    }

    #[test]
    fn invalid_port_does_not_silently_change_to_the_default() {
        for value in ["0", "65536", "-1", "abc", "42.42"] {
            assert_eq!(parse_listening_port(value), None, "{value}");
        }
    }

    #[test]
    fn the_role_row_offers_both_roles() {
        for mode in [OperationMode::Server, OperationMode::Client] {
            assert_eq!(operation_mode_at(operation_mode_index(mode)), mode);
        }
    }
}
