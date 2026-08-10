use adw::prelude::*;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::prelude::*;

use deskunion_ipc::Position;

fn position_from_selected(selected: u32) -> Position {
    match selected {
        1 => Position::Right,
        2 => Position::Top,
        3 => Position::Bottom,
        _ => Position::Left,
    }
}

pub struct ParkedDeviceRowInit {
    pub addr: String,
    pub fingerprint: String,
}

/// an authorized device that connected to this server but is not paired
/// to a client entry yet ("parked") — the row offers a position picker
/// whose Assign action pairs it via `FrontendRequest::AssignPosition`.
///
/// The daemon auto-pairs on the first free edge, so this row only shows
/// up in the fallback case where all four edges are already configured.
pub struct ParkedDeviceRowModel {
    addr: String,
    fingerprint: String,
    position: Position,
}

#[derive(Debug)]
pub enum ParkedDeviceRowInput {
    SetPosition(Position),
}

#[derive(Debug)]
pub enum ParkedDeviceRowOutput {
    Assign(DynamicIndex),
}

impl ParkedDeviceRowModel {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn position(&self) -> Position {
        self.position
    }
}

#[relm4::factory(pub)]
impl FactoryComponent for ParkedDeviceRowModel {
    type Init = ParkedDeviceRowInit;
    type Input = ParkedDeviceRowInput;
    type Output = ParkedDeviceRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        root = adw::ActionRow {
            set_title: &self.addr,
            set_subtitle: &format!("{} · waiting for a screen position", self.fingerprint),
            add_prefix = &gtk::Image {
                set_icon_name: Some("computer-symbolic"),
                set_pixel_size: 24,
            },

            add_suffix = &gtk::Button {
                set_label: "Assign",
                set_valign: gtk::Align::Center,
                add_css_class: "pill",
                add_css_class: "suggested-action",
                connect_clicked[sender, index] => move |_| {
                    sender.output(ParkedDeviceRowOutput::Assign(index.clone())).unwrap();
                },
            },

            add_suffix = &gtk::DropDown {
                set_valign: gtk::Align::Center,
                set_model: Some(&gtk::StringList::new(&["Left", "Right", "Top", "Bottom"])),
                // right is the pairing preference everywhere else
                set_selected: 1,
                set_tooltip_text: Some("screen position for this device"),
                connect_selected_notify[sender] => move |dropdown| {
                    sender.input(ParkedDeviceRowInput::SetPosition(
                        position_from_selected(dropdown.selected()),
                    ));
                },
            },
        }
    }

    fn init_model(init: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        let ParkedDeviceRowInit { addr, fingerprint } = init;
        Self {
            addr,
            fingerprint,
            position: Position::Right,
        }
    }

    fn update(&mut self, message: Self::Input, _sender: FactorySender<Self>) {
        match message {
            ParkedDeviceRowInput::SetPosition(position) => self.position = position,
        }
    }
}
