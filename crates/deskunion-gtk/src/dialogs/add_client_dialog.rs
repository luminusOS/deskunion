use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use deskunion_ipc::{ClientConfig, Position};

pub struct AddClientDialogInit {
    pub parent: adw::ApplicationWindow,
}

pub struct AddClientDialogModel;

#[derive(Debug, PartialEq, Eq)]
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

pub(crate) fn build_client_config(
    raw_fingerprint: &str,
    name: &str,
    selected_pos: u32,
) -> Option<ClientConfig> {
    let raw = raw_fingerprint.trim();
    let normalized = deskunion_ipc::normalize_fingerprint(raw).ok()?;
    let trimmed_name = name.trim();

    Some(ClientConfig {
        hostname: (!trimmed_name.is_empty()).then(|| trimmed_name.to_owned()),
        pos: position_from_selected(selected_pos),
        fingerprint: Some(normalized),
        ..ClientConfig::default()
    })
}

pub(crate) fn evaluate_add_client_response(
    response: &str,
    raw_fingerprint: &str,
    name: &str,
    selected_pos: u32,
) -> AddClientDialogOutput {
    match response {
        "add" => match build_client_config(raw_fingerprint, name, selected_pos) {
            Some(config) => AddClientDialogOutput::PairRequested(config),
            None => AddClientDialogOutput::Cancelled,
        },
        _ => AddClientDialogOutput::Cancelled,
    }
}

#[relm4::component(pub)]
impl SimpleComponent for AddClientDialogModel {
    type Init = AddClientDialogInit;
    type Input = ();
    type Output = AddClientDialogOutput;

    view! {
        #[name(root)]
        adw::AlertDialog {
            set_heading: Some("Add a client"),
            set_body: "Pair an authorized device by its certificate fingerprint. A client shows its fingerprint on its Connection page.",
            add_response: ("cancel", "_Cancel"),
            add_response: ("add", "_Add"),
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

                #[name(fingerprint_error)]
                gtk::Label {
                    set_wrap: true,
                    set_xalign: 0.0,
                    set_margin_start: 6,
                    set_margin_end: 6,
                    add_css_class: "error",
                    add_css_class: "caption",
                    set_visible: false,
                },

                #[name(fingerprint_hint)]
                gtk::Label {
                    set_wrap: true,
                    set_xalign: 0.0,
                    set_margin_start: 6,
                    set_margin_end: 6,
                    add_css_class: "dim-label",
                    add_css_class: "caption",
                    set_label: "Format: 32 colon-separated hex bytes (aa:bb:…) or 64 hex characters",
                    set_visible: true,
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Self;
        let widgets = view_output!();

        let sync_add = glib::clone!(
            #[weak(rename_to = dialog)]
            widgets.root,
            #[weak(rename_to = error_label)]
            widgets.fingerprint_error,
            #[weak(rename_to = hint_label)]
            widgets.fingerprint_hint,
            move |entry: &adw::EntryRow| {
                let text = entry.text();
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    entry.remove_css_class("error");
                    error_label.set_visible(false);
                    hint_label.set_visible(true);
                    dialog.set_response_enabled("add", false);
                } else {
                    match deskunion_ipc::normalize_fingerprint(trimmed) {
                        Ok(_) => {
                            entry.remove_css_class("error");
                            error_label.set_visible(false);
                            hint_label.set_visible(false);
                            dialog.set_response_enabled("add", true);
                        }
                        Err(err) => {
                            entry.add_css_class("error");
                            error_label.set_text(&err.to_string());
                            error_label.set_visible(true);
                            hint_label.set_visible(true);
                            dialog.set_response_enabled("add", false);
                        }
                    }
                }
            }
        );
        sync_add(&widgets.fingerprint);
        widgets.fingerprint.connect_changed(sync_add);

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
                move |_dialog, response| {
                    let output = evaluate_add_client_response(
                        response,
                        fingerprint_widget.text().as_str(),
                        name_widget.text().as_str(),
                        position_widget.selected(),
                    );
                    sender.output(output).unwrap();
                }
            ),
        );

        widgets.root.present(Some(&init.parent));
        ComponentParts { model, widgets }
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

    #[test]
    fn build_client_config_with_valid_colon_hex() {
        let raw = "01:23:45:67:89:AB:CD:EF:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef";
        let config = build_client_config(raw, "laptop", 1).expect("valid config");
        assert_eq!(
            config.fingerprint,
            Some("01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef".to_string())
        );
        assert_eq!(config.hostname, Some("laptop".to_string()));
        assert_eq!(config.pos, Position::Right);
    }

    #[test]
    fn build_client_config_with_valid_plain_hex() {
        let plain = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let config = build_client_config(plain, "", 2).expect("valid config");
        assert_eq!(
            config.fingerprint,
            Some("01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef".to_string())
        );
        assert_eq!(config.hostname, None);
        assert_eq!(config.pos, Position::Top);
    }

    #[test]
    fn build_client_config_rejects_invalid_fingerprint() {
        assert!(build_client_config("invalid", "laptop", 0).is_none());
        assert!(build_client_config("", "laptop", 0).is_none());
        assert!(build_client_config("   ", "laptop", 0).is_none());
    }

    #[test]
    fn evaluate_add_client_response_handles_add_and_cancellation() {
        let valid = "01:23:45:67:89:AB:CD:EF:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef";
        let add_out = evaluate_add_client_response("add", valid, "pc", 3);
        match add_out {
            AddClientDialogOutput::PairRequested(cfg) => {
                assert_eq!(cfg.pos, Position::Bottom);
                assert_eq!(
                    cfg.fingerprint,
                    Some("01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef".to_string())
                );
            }
            AddClientDialogOutput::Cancelled => panic!("expected PairRequested"),
        }

        assert_eq!(
            evaluate_add_client_response("add", "invalid", "pc", 0),
            AddClientDialogOutput::Cancelled
        );
        assert_eq!(
            evaluate_add_client_response("cancel", valid, "pc", 0),
            AddClientDialogOutput::Cancelled
        );
        assert_eq!(
            evaluate_add_client_response("close", valid, "pc", 0),
            AddClientDialogOutput::Cancelled
        );
    }
}
