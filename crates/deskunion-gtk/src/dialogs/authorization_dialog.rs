use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

/// fingerprint to display + the window to present the dialog onto.
/// `adw::AlertDialog` isn't a `gtk::Window` — it's a widget presented via
/// `.present(Some(parent))` — so unlike a `gtk::Window`-based dialog it
/// can't use relm4's `ComponentBuilder::transient_for` (that requires
/// `Root: AsRef<gtk::Window>`); the parent is threaded through `Init`
/// instead and used directly in `init()`.
pub struct AuthorizationDialogInit {
    pub fingerprint: String,
    pub parent: adw::ApplicationWindow,
}

pub struct AuthorizationDialogModel {
    fingerprint: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthorizationDialogOutput {
    Confirmed(String),
    Cancelled,
}

pub(crate) fn evaluate_authorization_response(
    response: &str,
    fingerprint: &str,
) -> AuthorizationDialogOutput {
    match response {
        "authorize" => AuthorizationDialogOutput::Confirmed(fingerprint.to_string()),
        _ => AuthorizationDialogOutput::Cancelled,
    }
}

#[relm4::component(pub)]
impl SimpleComponent for AuthorizationDialogModel {
    type Init = AuthorizationDialogInit;
    type Input = ();
    type Output = AuthorizationDialogOutput;

    view! {
        #[name(root)]
        adw::AlertDialog {
            set_heading: Some("Allow this device to connect?"),
            set_body: "Another computer wants to share this computer's keyboard and mouse. Allow it only if the fingerprint matches the one shown on that computer.",
            add_response: ("cancel", "_Cancel"),
            add_response: ("authorize", "_Allow"),
            set_default_response: Some("cancel"),
            set_close_response: "cancel",

            #[wrap(Some)]
            set_extra_child = &adw::PreferencesGroup {
                set_title: "SHA-256 fingerprint",

                add = &adw::ActionRow {
                    #[wrap(Some)]
                    set_child = &gtk::Label {
                        set_label: &model.fingerprint,
                        set_selectable: true,
                        add_css_class: "monospace",
                        set_wrap: true,
                        set_wrap_mode: gtk::pango::WrapMode::WordChar,
                        set_justify: gtk::Justification::Center,
                        set_property: ("xalign", 0.5f32),
                        set_margin_all: 10,
                        set_max_width_chars: 64,
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
        let fingerprint = deskunion_ipc::normalize_fingerprint(&init.fingerprint)
            .unwrap_or_else(|_| init.fingerprint.clone());
        let model = AuthorizationDialogModel {
            fingerprint: fingerprint.clone(),
        };
        let widgets = view_output!();

        // `AlertDialog::connect_response`'s real signature takes an extra
        // leading `detail: Option<&str>` filter arg before the closure —
        // the `view!{}` macro's single-arg `connect_NAME => closure` sugar
        // isn't built for that, so wire it manually.
        widgets.root.connect_response(
            None,
            glib::clone!(
                #[strong]
                sender,
                move |_dialog, response| {
                    let output = evaluate_authorization_response(response, &fingerprint);
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
    fn evaluate_authorization_response_confirmed_on_authorize() {
        let fp = "01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef";
        assert_eq!(
            evaluate_authorization_response("authorize", fp),
            AuthorizationDialogOutput::Confirmed(fp.to_string())
        );
    }

    #[test]
    fn evaluate_authorization_response_cancelled_on_other_responses() {
        let fp = "01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89:ab:cd:ef";
        assert_eq!(
            evaluate_authorization_response("cancel", fp),
            AuthorizationDialogOutput::Cancelled
        );
        assert_eq!(
            evaluate_authorization_response("close", fp),
            AuthorizationDialogOutput::Cancelled
        );
        assert_eq!(
            evaluate_authorization_response("", fp),
            AuthorizationDialogOutput::Cancelled
        );
    }
}
