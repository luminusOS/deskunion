use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use deskunion_ipc::{ClientConfig, Position};

pub struct AddClientDialogInit {
    pub parent: adw::ApplicationWindow,
}

pub struct AddClientDialogModel {
    error: Option<String>,
}

#[derive(Debug)]
pub enum AddClientDialogInput {
    Invalid(String),
}

#[derive(Debug)]
pub enum AddClientDialogOutput {
    PairRequested(ClientConfig),
    Cancelled,
}

fn position_from_selected(selected: u32) -> Position {
    match selected {
        1 => Position::Right,
        2 => Position::Top,
        3 => Position::Bottom,
        _ => Position::Left,
    }
}

fn pairing_config(
    fingerprint: &adw::EntryRow,
    name: &adw::EntryRow,
    position: &adw::ComboRow,
) -> Result<ClientConfig, String> {
    let fingerprint = fingerprint.text().trim().to_owned();
    if fingerprint.is_empty() {
        return Err("Enter the device's certificate fingerprint.".to_owned());
    }

    let name = name.text().trim().to_owned();

    Ok(ClientConfig {
        hostname: (!name.is_empty()).then_some(name),
        pos: position_from_selected(position.selected()),
        fingerprint: Some(fingerprint),
        ..ClientConfig::default()
    })
}

#[relm4::component(pub)]
impl SimpleComponent for AddClientDialogModel {
    type Init = AddClientDialogInit;
    type Input = AddClientDialogInput;
    type Output = AddClientDialogOutput;

    view! {
        #[name(root)]
        adw::AlertDialog {
            set_heading: Some("Add a Client"),
            set_body: "Pair an authorized device by its certificate fingerprint. The device shows its fingerprint on its own Settings page.",
            add_response: ("cancel", "Cancel"),
            add_response: ("add", "Add"),
            set_response_appearance: ("add", adw::ResponseAppearance::Suggested),
            set_default_response: Some("add"),
            set_close_response: "cancel",

            #[wrap(Some)]
            set_extra_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 10,
                set_margin_top: 6,
                set_width_request: 360,

                adw::PreferencesGroup {
                    #[name(fingerprint)]
                    add = &adw::EntryRow {
                        set_title: "Certificate fingerprint",
                        set_activates_default: true,
                        set_enable_undo: true,
                    },

                    #[name(name)]
                    add = &adw::EntryRow {
                        set_title: "Name (optional)",
                        set_activates_default: true,
                        set_enable_undo: true,
                    },

                    #[name(position)]
                    add = &adw::ComboRow {
                        set_title: "Screen position",
                        set_model: Some(&gtk::StringList::new(&["Left", "Right", "Top", "Bottom"])),
                        // matches the auto-pairing preference
                        set_selected: 1,
                    },
                },

                gtk::Label {
                    #[watch]
                    set_label: model.error.as_deref().unwrap_or(""),
                    #[watch]
                    set_visible: model.error.is_some(),
                    set_wrap: true,
                    set_xalign: 0.0,
                    add_css_class: "error",
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Self { error: None };
        let widgets = view_output!();

        widgets.root.connect_response(
            None,
            glib::clone!(
                #[strong]
                sender,
                #[strong(rename_to = fingerprint_widget)]
                widgets.fingerprint,
                #[strong(rename_to = name_widget)]
                widgets.name,
                #[strong(rename_to = position_widget)]
                widgets.position,
                move |_dialog, response| match response {
                    "add" =>
                        match pairing_config(&fingerprint_widget, &name_widget, &position_widget) {
                            Ok(config) => sender
                                .output(AddClientDialogOutput::PairRequested(config))
                                .unwrap(),
                            Err(error) => sender.input(AddClientDialogInput::Invalid(error)),
                        },
                    _ => sender.output(AddClientDialogOutput::Cancelled).unwrap(),
                }
            ),
        );

        widgets.root.present(Some(&init.parent));
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>) {
        match message {
            AddClientDialogInput::Invalid(error) => self.error = Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_match_combo_order() {
        assert_eq!(position_from_selected(0), Position::Left);
        assert_eq!(position_from_selected(1), Position::Right);
        assert_eq!(position_from_selected(2), Position::Top);
        assert_eq!(position_from_selected(3), Position::Bottom);
    }
}
