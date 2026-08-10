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
            set_heading: Some("Add Certificate Fingerprint"),
            set_body: "The certificate fingerprint serves as a unique identifier for your device.\nYou can find it under the `General` section of the device you want to connect",
            add_response: ("confirm", "Confirm"),
            set_response_appearance: ("confirm", adw::ResponseAppearance::Suggested),
            set_default_response: Some("confirm"),

            #[wrap(Some)]
            set_extra_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 18,

                adw::PreferencesGroup {
                    set_title: "description",

                    add = &adw::ActionRow {
                        #[wrap(Some)]
                        #[name(description)]
                        set_child = &gtk::Text {
                            set_margin_all: 10,
                            set_enable_undo: true,
                            set_hexpand: true,
                        },
                    },
                },

                adw::PreferencesGroup {
                    set_title: "sha256 fingerprint",

                    add = &adw::ActionRow {
                        #[wrap(Some)]
                        #[name(fingerprint)]
                        set_child = &gtk::Text {
                            set_margin_all: 10,
                            set_enable_undo: true,
                            set_hexpand: true,
                            set_text: model.prefilled_fingerprint.as_deref().unwrap_or(""),
                            set_editable: model.prefilled_fingerprint.is_none(),
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
        let model = FingerprintDialogModel {
            prefilled_fingerprint: init.fingerprint,
        };
        let widgets = view_output!();

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
