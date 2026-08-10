use adw::prelude::*;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::prelude::*;

/// (description, fingerprint) — mirrors the old `KeyObject::new` signature.
pub type KeyRowInit = (String, String);

pub struct KeyRowModel {
    description: String,
    fingerprint: String,
}

#[derive(Debug)]
pub enum KeyRowOutput {
    Delete(DynamicIndex),
}

impl KeyRowModel {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

#[relm4::factory(pub)]
impl FactoryComponent for KeyRowModel {
    type Init = KeyRowInit;
    type Input = ();
    type Output = KeyRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        root = adw::ActionRow {
            set_title: &self.description,
            set_subtitle: &self.fingerprint,

            add_prefix = &gtk::Button {
                set_valign: gtk::Align::Center,
                set_halign: gtk::Align::End,
                set_tooltip_text: Some("revoke authorization"),
                set_icon_name: "edit-delete-symbolic",
                add_css_class: "flat",
                connect_clicked[sender, index] => move |_| {
                    sender.output(KeyRowOutput::Delete(index.clone())).unwrap();
                },
            },
        }
    }

    fn init_model(init: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        let (description, fingerprint) = init;
        Self {
            description,
            fingerprint,
        }
    }
}
