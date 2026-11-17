//! What the service is doing, as the window shows it. Every page reads it;
//! only the window changes it, from `FrontendEvent`s and the user's intent.

use std::net::SocketAddr;

use deskunion_ipc::{ClientHandle, DEFAULT_PORT, FrontendEvent, OperationMode, Status};

use crate::server_link::ServerRetry;

/// What the input emulation of a client is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientEmulation {
    Running,
    /// ended on its own and starts again shortly
    Restarting,
    /// meant to run but off: needs the user
    Disabled,
    /// stopped by the user
    Stopped,
}

pub fn client_emulation(active: bool, restarting: bool, wanted_running: bool) -> ClientEmulation {
    if active {
        ClientEmulation::Running
    } else if !wanted_running {
        ClientEmulation::Stopped
    } else if restarting {
        ClientEmulation::Restarting
    } else {
        ClientEmulation::Disabled
    }
}

/// A part of the input pipeline that is off although sharing should run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attention {
    Capture,
    Emulation,
}

/// A configured client of a server, for the header and the tray.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientSummary {
    pub handle: ClientHandle,
    pub name: String,
    pub shared: bool,
    pub connected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub mode: OperationMode,
    pub port: u16,
    pub capture_active: bool,
    pub emulation_active: bool,
    pub emulation_restarting: bool,
    /// whether the user wants sharing to run (pressed Start, or it came up
    /// by itself at service boot); an intentional Stop is not a failure
    pub wanted_running: bool,
    pub server_configured: bool,
    /// the server this client is connected to
    pub server_connected: Option<SocketAddr>,
    /// the service is waiting before dialing the server again
    pub server_retry: Option<ServerRetry>,
    pub clients: Vec<ClientSummary>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            mode: OperationMode::default(),
            port: DEFAULT_PORT,
            capture_active: false,
            emulation_active: false,
            emulation_restarting: false,
            wanted_running: false,
            server_configured: false,
            server_connected: None,
            server_retry: None,
            clients: Vec::new(),
        }
    }
}

impl Session {
    pub fn apply(&mut self, event: &FrontendEvent) {
        match event {
            FrontendEvent::OperationMode(mode) => {
                if self.mode != *mode {
                    self.server_retry = None;
                }
                self.mode = *mode;
            }
            FrontendEvent::PortChanged(port, _) => self.port = *port,
            FrontendEvent::CaptureStatus(status) => {
                self.capture_active = *status == Status::Enabled;
                if self.capture_active {
                    self.wanted_running = true;
                }
            }
            FrontendEvent::EmulationRestarting { .. } => {
                self.emulation_active = false;
                self.emulation_restarting = true;
            }
            FrontendEvent::EmulationStatus(status) => {
                self.emulation_restarting = false;
                self.emulation_active = *status == Status::Enabled;
                if self.emulation_active {
                    self.wanted_running = true;
                } else {
                    self.server_connected = None;
                    self.server_retry = None;
                }
            }
            FrontendEvent::ServerRetry {
                retry_in_secs,
                awaiting_authorization,
            } => {
                self.server_retry = Some(ServerRetry {
                    in_secs: *retry_in_secs,
                    awaiting_authorization: *awaiting_authorization,
                });
            }
            // in client mode this is the dial to the configured server; a
            // server lists the device for a position instead
            FrontendEvent::DeviceConnected { addr, .. } if self.mode != OperationMode::Server => {
                self.server_connected = Some(*addr);
                self.server_retry = None;
            }
            FrontendEvent::IncomingDisconnected(addr) => {
                if self.server_connected == Some(*addr) {
                    self.server_connected = None;
                }
            }
            FrontendEvent::ServerEndpoint { hostname, ips, .. } => {
                self.server_configured = hostname.is_some() || !ips.is_empty();
                self.server_retry = None;
            }
            _ => {}
        }
    }

    pub fn client_emulation(&self) -> ClientEmulation {
        client_emulation(
            self.emulation_active,
            self.emulation_restarting,
            self.wanted_running,
        )
    }

    /// whether the role's input pipeline is up; a client restarting its
    /// emulation still runs and offers Stop, not Start
    pub fn service_running(&self) -> bool {
        match self.mode {
            OperationMode::Unconfigured => false,
            OperationMode::Server => self.capture_active,
            OperationMode::Client => matches!(
                self.client_emulation(),
                ClientEmulation::Running | ClientEmulation::Restarting
            ),
        }
    }

    pub fn attention(&self) -> Option<Attention> {
        // macOS: the capture warning doubles as the Accessibility grant
        // flow, so it shows regardless of the Start/Stop intent
        let wanted = cfg!(target_os = "macos") || self.wanted_running;
        // client-side capture is armed only after the server sends Enter,
        // so it being off while idle is not a failure
        if self.mode == OperationMode::Server && !self.capture_active && wanted {
            Some(Attention::Capture)
        } else if self.mode == OperationMode::Client
            && self.client_emulation() == ClientEmulation::Disabled
        {
            Some(Attention::Emulation)
        } else {
            None
        }
    }

    pub fn connected_clients(&self) -> usize {
        self.clients.iter().filter(|c| c.connected).count()
    }

    /// one status line: the Connections header subtitle and the tray
    pub fn status(&self) -> String {
        match self.mode {
            OperationMode::Unconfigured => "Not configured".to_string(),
            OperationMode::Server if self.capture_active => {
                let n = self.connected_clients();
                format!(
                    "Server · {n} client{} connected",
                    if n == 1 { "" } else { "s" }
                )
            }
            OperationMode::Server => "Server · stopped".to_string(),
            OperationMode::Client if self.server_connected.is_some() => {
                "Client · connected".to_string()
            }
            OperationMode::Client if self.emulation_active && self.server_configured => {
                match self.server_retry {
                    Some(retry) => format!("Client · {}", retry.short_text()),
                    None => "Client · connecting…".to_string(),
                }
            }
            OperationMode::Client if self.emulation_active => "Client · running".to_string(),
            OperationMode::Client if self.client_emulation() == ClientEmulation::Restarting => {
                "Client · restarting input emulation…".to_string()
            }
            OperationMode::Client => "Client · stopped".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// While input emulation restarts on its own (screen lock, a reset EIS
    /// socket) the client must say so, not claim emulation is disabled and
    /// ask the user to re-enable it.
    #[test]
    fn a_restarting_emulation_is_not_reported_as_disabled() {
        assert_eq!(
            client_emulation(true, false, true),
            ClientEmulation::Running
        );
        assert_eq!(
            client_emulation(false, true, true),
            ClientEmulation::Restarting
        );
        assert_eq!(
            client_emulation(false, false, true),
            ClientEmulation::Disabled
        );
        assert_eq!(
            client_emulation(false, false, false),
            ClientEmulation::Stopped
        );
        assert_eq!(
            client_emulation(false, true, false),
            ClientEmulation::Stopped
        );
    }

    fn client() -> Session {
        let mut session = Session::default();
        session.apply(&FrontendEvent::OperationMode(OperationMode::Client));
        session
    }

    #[test]
    fn a_restarting_client_still_runs_and_needs_nothing_from_the_user() {
        let mut session = client();
        session.apply(&FrontendEvent::EmulationStatus(Status::Enabled));
        session.apply(&FrontendEvent::EmulationRestarting { retry_in_ms: 250 });
        assert!(session.service_running());
        assert_eq!(session.attention(), None);
        assert_eq!(session.status(), "Client · restarting input emulation…");
        session.apply(&FrontendEvent::EmulationStatus(Status::Disabled));
        assert_eq!(session.attention(), Some(Attention::Emulation));
    }

    #[test]
    fn a_stopped_server_is_not_a_failure() {
        let mut session = Session::default();
        session.apply(&FrontendEvent::OperationMode(OperationMode::Server));
        assert_eq!(session.status(), "Server · stopped");
        if !cfg!(target_os = "macos") {
            assert_eq!(session.attention(), None);
        }
        session.apply(&FrontendEvent::CaptureStatus(Status::Enabled));
        assert_eq!(session.status(), "Server · 0 clients connected");
        session.apply(&FrontendEvent::CaptureStatus(Status::Disabled));
        assert_eq!(session.attention(), Some(Attention::Capture));
    }

    #[test]
    fn the_server_link_follows_dials_and_endpoint_changes() {
        let mut session = client();
        let addr: SocketAddr = "192.0.2.1:4242".parse().unwrap();
        session.apply(&FrontendEvent::EmulationStatus(Status::Enabled));
        session.apply(&FrontendEvent::ServerEndpoint {
            hostname: None,
            ips: vec![addr.ip()],
            port: 4242,
        });
        assert_eq!(session.status(), "Client · connecting…");
        session.apply(&FrontendEvent::ServerRetry {
            retry_in_secs: 5,
            awaiting_authorization: false,
        });
        assert_eq!(session.status(), "Client · retrying in 5 s");
        session.apply(&FrontendEvent::DeviceConnected {
            fingerprint: String::new(),
            addr,
        });
        assert_eq!(session.server_retry, None);
        assert_eq!(session.status(), "Client · connected");
        session.apply(&FrontendEvent::IncomingDisconnected(addr));
        assert_eq!(session.server_connected, None);
    }
}
