use adw::prelude::*;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::prelude::*;

use deskunion_ipc::Position;

pub struct IncomingDeviceRowInit {
    pub addr: String,
    pub position: Option<Position>,
    pub fingerprint: String,
}

pub struct IncomingDeviceRowModel {
    addr: String,
    position: Option<Position>,
    fingerprint: String,
}

#[derive(Debug)]
pub enum IncomingDeviceRowInput {
    Entered {
        position: Position,
        fingerprint: String,
    },
}

impl IncomingDeviceRowModel {
    fn subtitle(&self) -> String {
        match self.position {
            Some(position) => format!("Connected · {position} · {}", self.fingerprint),
            None => format!("Connected · {}", self.fingerprint),
        }
    }
}

#[relm4::factory(pub)]
impl FactoryComponent for IncomingDeviceRowModel {
    type Init = IncomingDeviceRowInit;
    type Input = IncomingDeviceRowInput;
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        root = adw::ActionRow {
            set_title: &self.addr,
            #[watch]
            set_subtitle: &self.subtitle(),

            add_prefix = &gtk::Image {
                set_icon_name: Some("computer-symbolic"),
            },

            add_suffix = &gtk::Label {
                set_valign: gtk::Align::Center,
                set_label: "Connected",
                add_css_class: "status-pill",
                add_css_class: "connected",
            },
        }
    }

    fn init_model(init: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        let IncomingDeviceRowInit {
            addr,
            position,
            fingerprint,
        } = init;
        Self {
            addr,
            position,
            fingerprint,
        }
    }

    fn update(&mut self, message: Self::Input, _sender: FactorySender<Self>) {
        match message {
            IncomingDeviceRowInput::Entered {
                position,
                fingerprint,
            } => {
                self.position = Some(position);
                self.fingerprint = fingerprint;
            }
        }
    }
}
