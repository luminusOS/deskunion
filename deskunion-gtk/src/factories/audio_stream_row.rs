use adw::prelude::*;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::prelude::*;

pub struct AudioStreamRowInit {
    pub addr: String,
    pub latency_ms: u32,
    pub packets_lost: u64,
    pub level: f32,
}

pub struct AudioStreamRowModel {
    addr: String,
    latency_ms: u32,
    packets_lost: u64,
    level: f32,
}

#[derive(Debug)]
pub enum AudioStreamRowInput {
    UpdateStats {
        latency_ms: u32,
        packets_lost: u64,
        level: f32,
    },
}

impl AudioStreamRowModel {
    fn subtitle(&self) -> String {
        format!("{} ms · {} lost", self.latency_ms, self.packets_lost)
    }
}

#[relm4::factory(pub)]
impl FactoryComponent for AudioStreamRowModel {
    type Init = AudioStreamRowInit;
    type Input = AudioStreamRowInput;
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        root = adw::ActionRow {
            set_title: &self.addr,
            #[watch]
            set_subtitle: &self.subtitle(),

            add_suffix = &gtk::LevelBar {
                set_min_value: 0.0,
                set_max_value: 1.0,
                set_valign: gtk::Align::Center,
                set_width_request: 80,
                #[watch]
                set_value: self.level as f64,
            },
        }
    }

    fn init_model(init: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        let AudioStreamRowInit {
            addr,
            latency_ms,
            packets_lost,
            level,
        } = init;
        Self {
            addr,
            latency_ms,
            packets_lost,
            level,
        }
    }

    fn update(&mut self, message: Self::Input, _sender: FactorySender<Self>) {
        match message {
            AudioStreamRowInput::UpdateStats {
                latency_ms,
                packets_lost,
                level,
            } => {
                self.latency_ms = latency_ms;
                self.packets_lost = packets_lost;
                self.level = level;
            }
        }
    }
}
