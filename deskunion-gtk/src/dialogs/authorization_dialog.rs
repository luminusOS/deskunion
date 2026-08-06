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

#[derive(Debug)]
pub enum AuthorizationDialogOutput {
    Confirmed(String),
    Cancelled,
}

#[relm4::component(pub)]
impl SimpleComponent for AuthorizationDialogModel {
    type Init = AuthorizationDialogInit;
    type Input = ();
    type Output = AuthorizationDialogOutput;

    view! {
        #[name(root)]
        adw::AlertDialog {
            set_heading: Some("Unauthorized Device"),
            set_body: "An unauthorized Device is trying to connect. Do you want to authorize this Device?",
            add_response: ("cancel", "Cancel"),
            add_response: ("authorize", "Authorize"),
            set_response_appearance: ("authorize", adw::ResponseAppearance::Destructive),
            set_close_response: "cancel",

            #[wrap(Some)]
            set_extra_child = &adw::PreferencesGroup {
                set_title: "sha256 fingerprint",

                add = &adw::ActionRow {
                    #[wrap(Some)]
                    set_child = &gtk::Label {
                        set_label: &model.fingerprint,
                        set_wrap: true,
                        set_wrap_mode: gtk::pango::WrapMode::WordChar,
                        set_justify: gtk::Justification::Center,
                        set_property: ("xalign", 0.5f32),
                        set_margin_all: 10,
                        set_width_chars: 64,
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
        let model = AuthorizationDialogModel {
            fingerprint: init.fingerprint.clone(),
        };
        let widgets = view_output!();

        // `AlertDialog::connect_response`'s real signature takes an extra
        // leading `detail: Option<&str>` filter arg before the closure —
        // the `view!{}` macro's single-arg `connect_NAME => closure` sugar
        // isn't built for that, so wire it manually.
        let fingerprint = init.fingerprint;
        widgets.root.connect_response(
            None,
            glib::clone!(
                #[strong]
                sender,
                move |_dialog, response| {
                    let output = match response {
                        "authorize" => AuthorizationDialogOutput::Confirmed(fingerprint.clone()),
                        _ => AuthorizationDialogOutput::Cancelled,
                    };
                    sender.output(output).unwrap();
                }
            ),
        );

        widgets.root.present(Some(&init.parent));

        ComponentParts { model, widgets }
    }
}
