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

#[derive(Debug)]
pub enum FingerprintDialogOutput {
    Confirmed(String, String),
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
                set_spacing: 18,
                set_width_request: 360,

                adw::PreferencesGroup {
                    #[name(description)]
                    add = &adw::EntryRow {
                        set_title: "Description",
                        set_enable_undo: true,
                    },

                    #[name(fingerprint)]
                    add = &adw::EntryRow {
                        set_title: "SHA-256 fingerprint",
                        set_enable_undo: true,
                        set_text: model.prefilled_fingerprint.as_deref().unwrap_or(""),
                        set_editable: model.prefilled_fingerprint.is_none(),
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
        let model = FingerprintDialogModel {
            prefilled_fingerprint: init.fingerprint,
        };
        let widgets = view_output!();

        let sync_confirm = glib::clone!(
            #[weak(rename_to = dialog)]
            widgets.root,
            move |entry: &adw::EntryRow| {
                dialog.set_response_enabled("confirm", !entry.text().trim().is_empty());
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
                    if response == "confirm" {
                        let desc = description_widget.text().as_str().trim().to_owned();
                        let fp = fingerprint_widget.text().as_str().trim().to_owned();
                        sender
                            .output(FingerprintDialogOutput::Confirmed(desc, fp))
                            .unwrap();
                    }
                }
            ),
        );

        widgets.root.present(Some(&init.parent));

        ComponentParts { model, widgets }
    }
}
