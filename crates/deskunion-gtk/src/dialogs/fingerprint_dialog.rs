use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

/// `Some(fingerprint)` prefills and locks the fingerprint field (the
/// "confirm a pending connection attempt" entry point); `None` leaves both
/// fields blank and editable (the manual "Authorize" button entry point).
/// Also carries the window to present the dialog onto — see
/// `AuthorizationDialogInit`'s doc comment for why this can't use
/// `ComponentBuilder::transient_for`.
pub struct FingerprintDialogInit {
    pub fingerprint: Option<String>,
    pub parent: adw::ApplicationWindow,
}

pub struct FingerprintDialogModel {
    prefilled_fingerprint: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FingerprintDialogOutput {
    Confirmed(String, String),
    Cancelled,
}

pub(crate) fn evaluate_fingerprint_response(
    response: &str,
    description: &str,
    raw_fingerprint: &str,
) -> FingerprintDialogOutput {
    if response == "confirm" {
        let desc = description.trim().to_owned();
        let fp = raw_fingerprint.trim();
        match deskunion_ipc::normalize_fingerprint(fp) {
            Ok(normalized_fp) => FingerprintDialogOutput::Confirmed(desc, normalized_fp),
            Err(_) => FingerprintDialogOutput::Cancelled,
        }
    } else {
        FingerprintDialogOutput::Cancelled
    }
}

#[relm4::component(pub)]
impl SimpleComponent for FingerprintDialogModel {
    type Init = FingerprintDialogInit;
    type Input = ();
    type Output = FingerprintDialogOutput;

    view! {
        #[name(root)]
        adw::AlertDialog {
            set_heading: Some("Add authorized device"),
            set_body: "Enter the certificate fingerprint shown on the other device.",
            add_response: ("cancel", "_Cancel"),
            add_response: ("confirm", "C_onfirm"),
            set_response_appearance: ("confirm", adw::ResponseAppearance::Suggested),
            set_default_response: Some("confirm"),
            set_close_response: "cancel",

            #[wrap(Some)]
            set_extra_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 10,
                set_width_request: 360,

                adw::PreferencesGroup {
                    #[name(description)]
                    add = &adw::EntryRow {
                        set_title: "Description",
                        set_activates_default: true,
                        set_enable_undo: true,
                    },

                    #[name(fingerprint)]
                    add = &adw::EntryRow {
                        set_title: "SHA-256 fingerprint",
                        set_activates_default: true,
                        set_enable_undo: true,
                        set_text: model.prefilled_fingerprint.as_deref().unwrap_or(""),
                        set_editable: model.prefilled_fingerprint.is_none(),
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
        let model = FingerprintDialogModel {
            prefilled_fingerprint: init.fingerprint,
        };
        let widgets = view_output!();

        let sync_confirm = glib::clone!(
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
                    dialog.set_response_enabled("confirm", false);
                } else {
                    match deskunion_ipc::normalize_fingerprint(trimmed) {
                        Ok(_) => {
                            entry.remove_css_class("error");
                            error_label.set_visible(false);
                            hint_label.set_visible(false);
                            dialog.set_response_enabled("confirm", true);
                        }
                        Err(err) => {
                            entry.add_css_class("error");
                            error_label.set_text(&err.to_string());
                            error_label.set_visible(true);
                            hint_label.set_visible(true);
                            dialog.set_response_enabled("confirm", false);
                        }
                    }
                }
            }
        );
        sync_confirm(&widgets.fingerprint);
        widgets.fingerprint.connect_changed(sync_confirm);

        widgets.root.connect_response(
            None,
            glib::clone!(
                #[strong]
                sender,
                #[strong(rename_to = description_widget)]
                widgets.description,
                #[strong(rename_to = fingerprint_widget)]
                widgets.fingerprint,
                move |_dialog, response| {
                    let output = evaluate_fingerprint_response(
                        response,
                        description_widget.text().as_str(),
                        fingerprint_widget.text().as_str(),
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
    fn confirm_with_valid_colon_hex_emits_confirmed_with_normalized_fingerprint() {
        let raw = "01:23:45:67:89:AB:CD:EF:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef";
        let output = evaluate_fingerprint_response("confirm", "workstation", raw);
        assert_eq!(
            output,
            FingerprintDialogOutput::Confirmed(
                "workstation".to_string(),
                "01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef".to_string()
            )
        );
    }

    #[test]
    fn confirm_with_valid_plain_hex_emits_confirmed_with_normalized_fingerprint() {
        let plain = "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef";
        let output = evaluate_fingerprint_response("confirm", "  desk  ", plain);
        assert_eq!(
            output,
            FingerprintDialogOutput::Confirmed(
                "desk".to_string(),
                "01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef".to_string()
            )
        );
    }

    #[test]
    fn confirm_with_invalid_fingerprint_emits_cancelled() {
        let output = evaluate_fingerprint_response("confirm", "workstation", "invalid-fingerprint");
        assert_eq!(output, FingerprintDialogOutput::Cancelled);
    }

    #[test]
    fn cancel_response_emits_cancelled() {
        let valid = "01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef";
        let output = evaluate_fingerprint_response("cancel", "workstation", valid);
        assert_eq!(output, FingerprintDialogOutput::Cancelled);
    }

    #[test]
    fn window_close_or_escape_emits_cancelled() {
        let valid = "01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef";
        let output = evaluate_fingerprint_response("close", "workstation", valid);
        assert_eq!(output, FingerprintDialogOutput::Cancelled);

        let output_none = evaluate_fingerprint_response("", "workstation", valid);
        assert_eq!(output_none, FingerprintDialogOutput::Cancelled);
    }
}
