use adw::prelude::*;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::prelude::*;

use deskunion_ipc::Position;

pub struct IncomingDeviceRowInit {
    pub addr: String,
    pub position: Position,
    pub fingerprint: String,
}

pub struct IncomingDeviceRowModel {
    addr: String,
    position: Position,
    fingerprint: String,
}

#[relm4::factory(pub)]
impl FactoryComponent for IncomingDeviceRowModel {
    type Init = IncomingDeviceRowInit;
    type Input = ();
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        root = adw::ActionRow {
            set_title: &self.addr,
            set_subtitle: &format!("{} · {}", self.position, self.fingerprint),
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
}
