use adw::prelude::*;
use relm4::prelude::*;

use deskunion_ipc::FrontendEvent;

use super::PageOutput;
use super::log_state::{LogCategory, LogState};

#[derive(Debug)]
pub enum LogsMsg {
    Frontend(Box<FrontendEvent>),
    FilterChanged(Option<LogCategory>),
    Copy,
    Clear,
}

pub struct LogsPage {
    log: LogState,
}

impl LogsPage {
    #[cfg(all(test, feature = "ui-tests"))]
    pub fn has_visible_entries(&self) -> bool {
        self.log.has_visible()
    }
}

#[relm4::component(pub)]
impl SimpleComponent for LogsPage {
    type Init = ();
    type Input = LogsMsg;
    type Output = PageOutput;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                // the filter names the page, so it replaces the title
                #[wrap(Some)]
                set_title_widget = &gtk::DropDown {
                    set_model: Some(&gtk::StringList::new(&["All events", "Connections", "Audio", "Errors"])),
                    set_tooltip_text: Some("Filter events"),
                    update_property: &[gtk::accessible::Property::Label("Filter events")],
                    connect_selected_notify[sender] => move |dropdown| {
                        let filter = match dropdown.selected() {
                            1 => Some(LogCategory::Connections),
                            2 => Some(LogCategory::Audio),
                            3 => Some(LogCategory::Errors),
                            _ => None,
                        };
                        sender.input(LogsMsg::FilterChanged(filter));
                    },
                },
                pack_end = &gtk::Button {
                    set_icon_name: "user-trash-symbolic",
                    set_tooltip_text: Some("Clear"),
                    update_property: &[gtk::accessible::Property::Label("Clear")],
                    #[watch]
                    set_sensitive: model.log.has_visible(),
                    connect_clicked => LogsMsg::Clear,
                },
                pack_end = &gtk::Button {
                    set_icon_name: "edit-copy-symbolic",
                    set_tooltip_text: Some("Copy"),
                    update_property: &[gtk::accessible::Property::Label("Copy")],
                    #[watch]
                    set_sensitive: model.log.has_visible(),
                    connect_clicked => LogsMsg::Copy,
                },
            },

            #[wrap(Some)]
            set_content = &gtk::Overlay {
                gtk::ScrolledWindow {
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    set_child: Some(&list_box),
                },

                add_overlay = &adw::StatusPage {
                    set_icon_name: Some("view-list-symbolic"),
                    set_title: "No events",
                    #[watch]
                    set_visible: !model.log.has_visible(),
                },
            },
        }
    }

    fn init(
        _: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let list_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .build();
        let model = LogsPage {
            log: LogState::new(list_box.clone()),
        };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            LogsMsg::Frontend(event) => self.log.log_event(&event),
            LogsMsg::FilterChanged(filter) => self.log.apply_filter(filter),
            LogsMsg::Copy => {
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&self.log.copy_visible());
                    let _ = sender.output(PageOutput::Toast("Copied".to_string()));
                }
            }
            LogsMsg::Clear => self.log.clear(),
        }
    }
}
