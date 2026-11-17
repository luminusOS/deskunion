use std::collections::HashMap;
use std::net::SocketAddr;

use adw::prelude::*;
use relm4::factory::{DynamicIndex, FactoryVecDeque};
use relm4::prelude::*;

use deskunion_ipc::{AudioDeviceInfo, FrontendEvent, FrontendRequest};

use super::PageOutput;
use super::audio_devices::{
    AUDIO_BITRATES, audio_bitrate_index, audio_device_index, audio_device_model,
    selected_audio_device,
};
use crate::app::copy;
use crate::factories::{AudioStreamRowInit, AudioStreamRowInput, AudioStreamRowModel};

#[derive(Debug)]
pub enum AudioMsg {
    Frontend(Box<FrontendEvent>),
    SendToggled(bool),
    ReceiveToggled(bool),
    CaptureDeviceChanged(u32),
    PlaybackDeviceChanged(u32),
    BitrateChanged(u32),
    BufferChanged(u32),
}

pub struct AudioPage {
    send: bool,
    receive: bool,
    bitrate: u32,
    buffer_ms: u32,
    loopback_supported: bool,
    capture_device: Option<String>,
    playback_device: Option<String>,
    capture_devices: Vec<AudioDeviceInfo>,
    playback_devices: Vec<AudioDeviceInfo>,
    stream_rows: FactoryVecDeque<AudioStreamRowModel>,
    streams_by_addr: HashMap<SocketAddr, DynamicIndex>,
}

impl AudioPage {
    fn handle_frontend_event(&mut self, event: FrontendEvent) {
        match event {
            FrontendEvent::AudioStream {
                addr,
                active,
                latency_ms,
                packets_lost,
                level,
            } => {
                if !active {
                    if let Some(index) = self.streams_by_addr.remove(&addr) {
                        self.stream_rows.guard().remove(index.current_index());
                    }
                } else if let Some(index) = self.streams_by_addr.get(&addr) {
                    self.stream_rows.send(
                        index.current_index(),
                        AudioStreamRowInput::UpdateStats {
                            latency_ms,
                            packets_lost,
                            level,
                        },
                    );
                } else {
                    let index = self.stream_rows.guard().push_back(AudioStreamRowInit {
                        addr: addr.to_string(),
                        latency_ms,
                        packets_lost,
                        level,
                    });
                    self.streams_by_addr.insert(addr, index);
                }
            }
            FrontendEvent::AudioStatus {
                send,
                receive,
                bitrate,
                buffer_ms,
                capture_device,
                playback_device,
                loopback_supported,
            } => {
                self.send = send;
                self.receive = receive;
                self.bitrate = bitrate;
                self.buffer_ms = buffer_ms;
                self.capture_device = capture_device;
                self.playback_device = playback_device;
                self.loopback_supported = loopback_supported;
            }
            FrontendEvent::AudioDevices { capture, playback } => {
                self.capture_devices = capture;
                self.playback_devices = playback;
            }
            _ => {}
        }
    }

    fn request(sender: &ComponentSender<Self>, request: FrontendRequest) {
        let _ = sender.output(PageOutput::Request(request));
    }
}

#[relm4::component(pub)]
impl SimpleComponent for AudioPage {
    type Init = ();
    type Input = AudioMsg;
    type Output = PageOutput;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {},

            add_top_bar = &adw::Banner {
                set_title: "System audio needs macOS 14.6",
                #[watch]
                set_visible: !model.loopback_supported,
                #[watch]
                set_revealed: !model.loopback_supported,
            },

            #[wrap(Some)]
            set_content = &adw::PreferencesPage {
                adw::PreferencesGroup {
                    set_title: "Receiving",

                    adw::SwitchRow {
                        set_title: "Play audio from clients",
                        #[watch]
                        #[block_signal(receive_handler)]
                        set_active: model.receive,
                        connect_active_notify[sender] => move |row| {
                            sender.input(AudioMsg::ReceiveToggled(row.is_active()));
                        } @receive_handler,
                    },

                    adw::ComboRow {
                        set_title: "Output device",
                        #[watch]
                        #[block_signal(playback_handler)]
                        set_model: Some(&audio_device_model(&model.playback_devices, model.playback_device.as_deref())),
                        #[watch]
                        #[block_signal(playback_handler)]
                        set_selected: audio_device_index(&model.playback_devices, model.playback_device.as_deref()),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(AudioMsg::PlaybackDeviceChanged(row.selected()));
                        } @playback_handler,
                    },
                },

                adw::PreferencesGroup {
                    set_title: "Sending",

                    adw::SwitchRow {
                        set_title: "Send this computer's audio",
                        #[watch]
                        #[block_signal(send_handler)]
                        set_active: model.send,
                        connect_active_notify[sender] => move |row| {
                            sender.input(AudioMsg::SendToggled(row.is_active()));
                        } @send_handler,
                    },

                    adw::ComboRow {
                        set_title: "Capture source",
                        #[watch]
                        #[block_signal(capture_handler)]
                        set_model: Some(&audio_device_model(&model.capture_devices, model.capture_device.as_deref())),
                        #[watch]
                        #[block_signal(capture_handler)]
                        set_selected: audio_device_index(&model.capture_devices, model.capture_device.as_deref()),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(AudioMsg::CaptureDeviceChanged(row.selected()));
                        } @capture_handler,
                    },

                    adw::ComboRow {
                        set_title: "Bitrate",
                        set_model: Some(&gtk::StringList::new(&[
                            "64 kbps", "96 kbps", "128 kbps", "192 kbps", "256 kbps",
                        ])),
                        #[watch]
                        #[block_signal(bitrate_handler)]
                        set_selected: audio_bitrate_index(model.bitrate),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(AudioMsg::BitrateChanged(row.selected()));
                        } @bitrate_handler,
                    },

                    adw::SpinRow {
                        set_title: "Jitter buffer",
                        set_subtitle: "Milliseconds",
                        set_adjustment: Some(&gtk::Adjustment::new(80.0, 20.0, 200.0, 10.0, 10.0, 0.0)),
                        #[watch]
                        #[block_signal(buffer_handler)]
                        set_value: model.buffer_ms as f64,
                        connect_value_notify[sender] => move |row| {
                            sender.input(AudioMsg::BufferChanged(row.value() as u32));
                        } @buffer_handler,
                    },
                },

                adw::PreferencesGroup {
                    set_title: "Active streams",
                    #[local_ref]
                    stream_list -> gtk::ListBox {
                        set_selection_mode: gtk::SelectionMode::None,
                        add_css_class: "boxed-list",
                    },
                },
            },
        }
    }

    fn init(
        _: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let stream_list = gtk::ListBox::new();
        stream_list.set_placeholder(Some(
            &adw::ActionRow::builder()
                .title(copy::NO_AUDIO_STREAMS)
                .build(),
        ));
        let stream_rows = FactoryVecDeque::<AudioStreamRowModel>::builder()
            .launch(stream_list.clone())
            .detach();
        let model = AudioPage {
            send: false,
            receive: false,
            bitrate: 96_000,
            buffer_ms: 80,
            loopback_supported: true,
            capture_device: None,
            playback_device: None,
            capture_devices: Vec::new(),
            playback_devices: Vec::new(),
            stream_rows,
            streams_by_addr: HashMap::new(),
        };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            AudioMsg::Frontend(event) => self.handle_frontend_event(*event),
            AudioMsg::SendToggled(active) => {
                Self::request(&sender, FrontendRequest::SetAudioSend(active));
            }
            AudioMsg::ReceiveToggled(active) => {
                Self::request(&sender, FrontendRequest::SetAudioReceive(active));
            }
            AudioMsg::CaptureDeviceChanged(selected) => {
                let device = selected_audio_device(
                    &self.capture_devices,
                    selected,
                    self.capture_device.as_deref(),
                );
                Self::request(&sender, FrontendRequest::SetAudioCaptureDevice(device));
            }
            AudioMsg::PlaybackDeviceChanged(selected) => {
                let device = selected_audio_device(
                    &self.playback_devices,
                    selected,
                    self.playback_device.as_deref(),
                );
                Self::request(&sender, FrontendRequest::SetAudioPlaybackDevice(device));
            }
            AudioMsg::BitrateChanged(selected) => {
                self.bitrate = AUDIO_BITRATES
                    .get(selected as usize)
                    .copied()
                    .unwrap_or(96_000);
                Self::request(
                    &sender,
                    FrontendRequest::UpdateAudioSettings {
                        bitrate: self.bitrate,
                        buffer_ms: self.buffer_ms,
                    },
                );
            }
            AudioMsg::BufferChanged(buffer_ms) => {
                self.buffer_ms = buffer_ms;
                Self::request(
                    &sender,
                    FrontendRequest::UpdateAudioSettings {
                        bitrate: self.bitrate,
                        buffer_ms,
                    },
                );
            }
        }
    }
}
