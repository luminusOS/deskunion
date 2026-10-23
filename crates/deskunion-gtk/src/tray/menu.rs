//! What the tray menu contains, independent of the platform that draws it.
//! Linux (ksni) and Windows (`tray-icon`) both render [`menu_model`], so the
//! wording and the rules for when an item appears are written and tested once.

use deskunion_ipc::{ClientHandle, OperationMode};

use super::TrayAction;

/// What the application tells the tray about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayInfo {
    pub mode: OperationMode,
    /// the role's input pipeline is up
    pub running: bool,
    /// one line for the tooltip, the same as the window's subtitle
    pub status: String,
    pub clients: Vec<TrayClient>,
}

impl Default for TrayInfo {
    fn default() -> Self {
        Self {
            mode: OperationMode::Unconfigured,
            running: false,
            status: String::new(),
            clients: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayClient {
    pub handle: ClientHandle,
    pub name: String,
    /// screen sharing with this computer is on
    pub shared: bool,
    pub connected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Item {
        label: String,
        icon: &'static str,
        action: TrayAction,
        enabled: bool,
    },
    Check {
        label: String,
        checked: bool,
        action: TrayAction,
    },
    Separator,
    Submenu {
        label: String,
        icon: &'static str,
        entries: Vec<Entry>,
    },
}

fn item(label: &str, icon: &'static str, action: TrayAction) -> Entry {
    Entry::Item {
        label: label.to_string(),
        icon,
        action,
        enabled: true,
    }
}

fn window_entry(window_visible: bool) -> Entry {
    if window_visible {
        item(
            "Hide window",
            "view-conceal-symbolic",
            TrayAction::HideWindow,
        )
    } else {
        item(
            "Show window",
            "view-reveal-symbolic",
            TrayAction::ShowWindow,
        )
    }
}

fn client_label(client: &TrayClient) -> String {
    let name = if client.name.trim().is_empty() {
        "Unnamed computer"
    } else {
        client.name.trim()
    };
    if client.connected {
        format!("{name} · connected")
    } else {
        name.to_string()
    }
}

pub fn menu_model(info: &TrayInfo, window_visible: bool) -> Vec<Entry> {
    let mut entries = vec![window_entry(window_visible), Entry::Separator];
    match info.mode {
        OperationMode::Unconfigured => {}
        OperationMode::Server => {
            entries.push(if info.running {
                item(
                    "Stop sharing",
                    "media-playback-stop-symbolic",
                    TrayAction::SetRunning(false),
                )
            } else {
                item(
                    "Start sharing",
                    "media-playback-start-symbolic",
                    TrayAction::SetRunning(true),
                )
            });
            if !info.clients.is_empty() {
                entries.push(Entry::Submenu {
                    label: "Computers".to_string(),
                    icon: "video-display-symbolic",
                    entries: info
                        .clients
                        .iter()
                        .map(|client| Entry::Check {
                            label: client_label(client),
                            checked: client.shared,
                            action: TrayAction::SetClientShared(client.handle, !client.shared),
                        })
                        .collect(),
                });
            }
            entries.push(Entry::Separator);
        }
        OperationMode::Client => {
            entries.push(if info.running {
                item(
                    "Disconnect",
                    "network-offline-symbolic",
                    TrayAction::SetRunning(false),
                )
            } else {
                item(
                    "Connect",
                    "network-wired-symbolic",
                    TrayAction::SetRunning(true),
                )
            });
            if info.running {
                entries.push(item(
                    "Reconnect now",
                    "view-refresh-symbolic",
                    TrayAction::ReconnectNow,
                ));
            }
            entries.push(Entry::Separator);
        }
    }
    entries.push(item("Quit", "application-exit-symbolic", TrayAction::Quit));
    entries
}

/// ksni (dbusmenu) reads `_` in a label as the access key marker.
#[cfg(any(unix, test))]
pub fn escape_underscores(label: &str) -> String {
    label.replace('_', "__")
}

/// Win32 menus read `&` in a label as the access key marker.
#[cfg(any(windows, test))]
pub fn escape_ampersands(label: &str) -> String {
    label.replace('&', "&&")
}

/// Menu action <-> string, for backends that identify items by text.
#[cfg(any(windows, test))]
pub fn action_id(action: TrayAction) -> Option<String> {
    Some(match action {
        TrayAction::ToggleWindow => "window:toggle".to_string(),
        TrayAction::ShowWindow => "window:show".to_string(),
        TrayAction::HideWindow => "window:hide".to_string(),
        TrayAction::Quit => "quit".to_string(),
        TrayAction::SetRunning(on) => format!("running:{}", u8::from(on)),
        TrayAction::ReconnectNow => "reconnect".to_string(),
        TrayAction::SetClientShared(handle, on) => format!("client:{handle}:{}", u8::from(on)),
        #[cfg(unix)]
        TrayAction::Shown(_) => return None,
    })
}

#[cfg(any(windows, test))]
pub fn parse_action(id: &str) -> Option<TrayAction> {
    let flag = |value: &str| match value {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    };
    let mut parts = id.split(':');
    match (parts.next()?, parts.next(), parts.next(), parts.next()) {
        ("window", Some("toggle"), None, None) => Some(TrayAction::ToggleWindow),
        ("window", Some("show"), None, None) => Some(TrayAction::ShowWindow),
        ("window", Some("hide"), None, None) => Some(TrayAction::HideWindow),
        ("quit", None, None, None) => Some(TrayAction::Quit),
        ("running", Some(on), None, None) => Some(TrayAction::SetRunning(flag(on)?)),
        ("reconnect", None, None, None) => Some(TrayAction::ReconnectNow),
        ("client", Some(handle), Some(on), None) => {
            Some(TrayAction::SetClientShared(handle.parse().ok()?, flag(on)?))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item { label, .. } | Entry::Check { label, .. } => Some(label.clone()),
                Entry::Submenu { label, .. } => Some(label.clone()),
                Entry::Separator => None,
            })
            .collect()
    }

    fn all_labels(entries: &[Entry], out: &mut Vec<String>) {
        for entry in entries {
            match entry {
                Entry::Item { label, .. } | Entry::Check { label, .. } => out.push(label.clone()),
                Entry::Submenu { label, entries, .. } => {
                    out.push(label.clone());
                    all_labels(entries, out);
                }
                Entry::Separator => {}
            }
        }
    }

    fn server(running: bool, clients: Vec<TrayClient>) -> TrayInfo {
        TrayInfo {
            mode: OperationMode::Server,
            running,
            clients,
            ..Default::default()
        }
    }

    fn client(running: bool) -> TrayInfo {
        TrayInfo {
            mode: OperationMode::Client,
            running,
            ..Default::default()
        }
    }

    fn computer(handle: ClientHandle, name: &str, shared: bool, connected: bool) -> TrayClient {
        TrayClient {
            handle,
            name: name.to_string(),
            shared,
            connected,
        }
    }

    #[test]
    fn no_label_repeats_the_application_name() {
        let infos = [
            TrayInfo::default(),
            server(true, vec![computer(1, "Desk", true, true)]),
            server(false, Vec::new()),
            client(true),
            client(false),
        ];
        for info in infos {
            for visible in [true, false] {
                let mut all = Vec::new();
                all_labels(&menu_model(&info, visible), &mut all);
                assert!(
                    all.iter().all(|label| !label.contains("DeskUnion")),
                    "{all:?}"
                );
            }
        }
    }

    #[test]
    fn the_window_item_follows_the_window() {
        let shown = menu_model(&TrayInfo::default(), true);
        let hidden = menu_model(&TrayInfo::default(), false);
        assert_eq!(labels(&shown)[0], "Hide window");
        assert_eq!(labels(&hidden)[0], "Show window");
    }

    #[test]
    fn an_unconfigured_device_only_offers_the_window_and_quit() {
        assert_eq!(
            labels(&menu_model(&TrayInfo::default(), false)),
            ["Show window", "Quit"]
        );
    }

    #[test]
    fn a_server_starts_and_stops_sharing() {
        assert_eq!(
            labels(&menu_model(&server(true, Vec::new()), true)),
            ["Hide window", "Stop sharing", "Quit"]
        );
        assert_eq!(
            labels(&menu_model(&server(false, Vec::new()), true)),
            ["Hide window", "Start sharing", "Quit"]
        );
    }

    #[test]
    fn a_server_lists_its_computers_with_their_state() {
        let model = menu_model(
            &server(
                true,
                vec![
                    computer(7, "Desk", true, true),
                    computer(9, "  ", false, false),
                ],
            ),
            true,
        );
        let Some(Entry::Submenu { label, entries, .. }) =
            model.iter().find(|e| matches!(e, Entry::Submenu { .. }))
        else {
            panic!("no computers submenu in {model:?}");
        };
        assert_eq!(label, "Computers");
        assert_eq!(
            entries,
            &[
                Entry::Check {
                    label: "Desk · connected".to_string(),
                    checked: true,
                    action: TrayAction::SetClientShared(7, false),
                },
                Entry::Check {
                    label: "Unnamed computer".to_string(),
                    checked: false,
                    action: TrayAction::SetClientShared(9, true),
                },
            ]
        );
    }

    #[test]
    fn a_server_without_computers_has_no_submenu() {
        let model = menu_model(&server(true, Vec::new()), true);
        assert!(!model.iter().any(|e| matches!(e, Entry::Submenu { .. })));
    }

    #[test]
    fn a_running_client_can_disconnect_and_reconnect() {
        assert_eq!(
            labels(&menu_model(&client(true), true)),
            ["Hide window", "Disconnect", "Reconnect now", "Quit"]
        );
    }

    #[test]
    fn a_stopped_client_can_only_connect() {
        assert_eq!(
            labels(&menu_model(&client(false), true)),
            ["Hide window", "Connect", "Quit"]
        );
    }

    #[test]
    fn separators_never_lead_trail_or_double_up() {
        let infos = [
            TrayInfo::default(),
            server(true, vec![computer(1, "Desk", true, false)]),
            server(false, Vec::new()),
            client(true),
            client(false),
        ];
        for info in infos {
            let model = menu_model(&info, true);
            assert_ne!(model.first(), Some(&Entry::Separator));
            assert_ne!(model.last(), Some(&Entry::Separator));
            assert!(
                model
                    .windows(2)
                    .all(|pair| pair != [Entry::Separator, Entry::Separator]),
                "{model:?}"
            );
        }
    }

    #[test]
    fn quit_is_always_last() {
        for info in [TrayInfo::default(), server(true, Vec::new()), client(true)] {
            let model = menu_model(&info, true);
            assert!(matches!(
                model.last(),
                Some(Entry::Item {
                    action: TrayAction::Quit,
                    ..
                })
            ));
        }
    }

    #[test]
    fn every_menu_action_survives_the_string_round_trip() {
        let mut actions = Vec::new();
        for info in [
            TrayInfo::default(),
            server(true, vec![computer(42, "Desk", true, true)]),
            client(true),
            client(false),
        ] {
            for visible in [true, false] {
                collect_actions(&menu_model(&info, visible), &mut actions);
            }
        }
        assert!(actions.len() > 6);
        for action in actions {
            let id = action_id(action).expect("menu actions have ids");
            assert_eq!(parse_action(&id), Some(action), "{id}");
        }
        assert_eq!(
            parse_action(&action_id(TrayAction::ToggleWindow).expect("id")),
            Some(TrayAction::ToggleWindow)
        );
    }

    fn collect_actions(entries: &[Entry], out: &mut Vec<TrayAction>) {
        for entry in entries {
            match entry {
                Entry::Item { action, .. } | Entry::Check { action, .. } => out.push(*action),
                Entry::Submenu { entries, .. } => collect_actions(entries, out),
                Entry::Separator => {}
            }
        }
    }

    #[test]
    fn computer_names_cannot_turn_into_access_keys() {
        assert_eq!(escape_underscores("PC_Sala"), "PC__Sala");
        assert_eq!(escape_underscores("Desk"), "Desk");
        assert_eq!(escape_ampersands("A&B"), "A&&B");
        assert_eq!(escape_ampersands("Desk"), "Desk");
    }

    #[test]
    fn unknown_or_malformed_ids_are_ignored() {
        for id in [
            "",
            "nope",
            "running",
            "running:2",
            "client:abc:1",
            "client:1",
            "client:1:1:1",
            "quit:now",
            "window:minimize",
        ] {
            assert_eq!(parse_action(id), None, "{id:?}");
        }
    }
}
