use std::{
    collections::{HashMap, HashSet},
    env::VarError,
    fmt::Display,
    io,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};
use thiserror::Error;

#[cfg(unix)]
use std::{
    env,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

mod connect;
mod connect_async;
mod listen;

pub use connect::{FrontendEventReader, FrontendRequestWriter, connect};
pub use connect_async::{AsyncFrontendEventReader, AsyncFrontendRequestWriter, connect_async};
pub use listen::AsyncFrontendListener;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error(transparent)]
    SocketPath(#[from] SocketPathError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("connection timed out")]
    Timeout,
}

#[derive(Debug, Error)]
pub enum IpcListenerCreationError {
    #[error("could not determine socket-path: `{0}`")]
    SocketPath(#[from] SocketPathError),
    #[error("service already running!")]
    AlreadyRunning,
    #[error("failed to bind deskunion socket: `{0}`")]
    Bind(io::Error),
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("io error occured: `{0}`")]
    Io(#[from] io::Error),
    #[error("invalid json: `{0}`")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Listen(#[from] IpcListenerCreationError),
}

pub const DEFAULT_PORT: u16 = 4242;

/// Determines which side of the software KVM this machine operates as.
///
/// A server captures local input and sends it to configured clients. A
/// client accepts remote input and emulates it locally. Keeping this in the
/// service/frontend IPC lets the UI change roles without restarting and,
/// importantly, lets the service request only the OS permissions required by
/// the selected role.
#[derive(Debug, Default, Eq, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationMode {
    #[default]
    Unconfigured,
    Server,
    Client,
}

#[derive(Debug, Default, Eq, Hash, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    #[default]
    Left,
    Right,
    Top,
    Bottom,
}

impl Position {
    pub fn opposite(&self) -> Self {
        match self {
            Position::Left => Position::Right,
            Position::Right => Position::Left,
            Position::Top => Position::Bottom,
            Position::Bottom => Position::Top,
        }
    }
}

#[derive(Debug, Error)]
#[error("not a valid position: {pos}")]
pub struct PositionParseError {
    pos: String,
}

impl FromStr for Position {
    type Err = PositionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "top" => Ok(Self::Top),
            "bottom" => Ok(Self::Bottom),
            _ => Err(PositionParseError { pos: s.into() }),
        }
    }
}

impl Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Position::Left => "left",
                Position::Right => "right",
                Position::Top => "top",
                Position::Bottom => "bottom",
            }
        )
    }
}

impl TryFrom<&str> for Position {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "left" => Ok(Position::Left),
            "right" => Ok(Position::Right),
            "top" => Ok(Position::Top),
            "bottom" => Ok(Position::Bottom),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// display label of this client (historically the hostname used for
    /// dialing; the server no longer dials, so this is informational)
    pub hostname: Option<String>,
    /// fix ips, determined by the user (legacy, unused for dialing)
    pub fix_ips: Vec<IpAddr>,
    /// legacy peer port (unused; kept for config round-trips)
    pub port: u16,
    /// position of a client on screen
    pub pos: Position,
    /// enter hook
    pub cmd: Option<String>,
    /// sha256 certificate fingerprint of the paired device. This is the
    /// stable identity of a client: incoming connections are matched to
    /// a client entry by fingerprint, not by source IP (which breaks
    /// behind NAT). `None` = not paired yet.
    #[serde(default)]
    pub fingerprint: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            hostname: Default::default(),
            fix_ips: Default::default(),
            pos: Default::default(),
            cmd: None,
            fingerprint: None,
        }
    }
}

pub type ClientHandle = u64;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ClientState {
    /// events should be sent to and received from the client
    pub active: bool,
    /// `active` address of the client, used to send data to.
    /// This should generally be the socket address where data
    /// was last received from.
    pub active_addr: Option<SocketAddr>,
    /// tracks whether or not the client is available for emulation
    pub alive: bool,
    /// ips from dns
    pub dns_ips: Vec<IpAddr>,
    /// all ip addresses associated with a particular client
    /// e.g. Laptops usually have at least an ethernet and a wifi port
    /// which have different ip addresses
    pub ips: HashSet<IpAddr>,
    /// client has pressed keys
    pub has_pressed_keys: bool,
    /// dns resolving in progress
    pub resolving: bool,
    /// Peer's build short commit hash from the [`Hello`] proto
    /// event. `None` means we haven't received a Hello yet — either
    /// the connection is fresh, or the peer is on an older build
    /// that predates the Hello event. The frontend uses this to
    /// soft-warn on version mismatch.
    pub peer_commit: Option<[u8; 8]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrontendEvent {
    /// current software-KVM role
    OperationMode(OperationMode),
    /// a client was created
    Created(ClientHandle, ClientConfig, ClientState),
    /// result of a real DTLS handshake requested by the add-client dialog
    ConnectionTested {
        request_id: u64,
        error: Option<String>,
    },
    /// no such client
    NoSuchClient(ClientHandle),
    /// state changed
    State(ClientHandle, ClientConfig, ClientState),
    /// the client was deleted
    Deleted(ClientHandle),
    /// new port, reason of failure (if failed)
    PortChanged(u16, Option<String>),
    /// list of all clients, used for initial state synchronization
    Enumerate(Vec<(ClientHandle, ClientConfig, ClientState)>),
    /// an error occured
    Error(String),
    /// capture status
    CaptureStatus(Status),
    /// emulation status
    EmulationStatus(Status),
    /// authorized public key fingerprints have been updated
    AuthorizedUpdated(HashMap<String, String>),
    /// public key fingerprint of this device
    PublicKeyFingerprint(String),
    /// new device connected
    DeviceConnected {
        addr: SocketAddr,
        fingerprint: String,
    },
    /// incoming device entered the screen
    DeviceEntered {
        fingerprint: String,
        addr: SocketAddr,
        pos: Position,
    },
    /// incoming disconnected
    IncomingDisconnected(SocketAddr),
    /// failed connection attempt (approval for fingerprint required)
    ConnectionAttempt { fingerprint: String },
    /// the server endpoint this device dials in client mode
    /// (emulation side connects out to the capture side)
    ServerEndpoint {
        hostname: Option<String>,
        ips: Vec<IpAddr>,
        port: u16,
    },
    /// current audio settings/capability
    AudioStatus {
        send: bool,
        receive: bool,
        bitrate: u32,
        buffer_ms: u32,
        /// false when the OS backend can't do system-output loopback
        /// (e.g. macOS < 14.6) — the UI shows a warning banner
        loopback_supported: bool,
    },
    /// available audio devices, for the capture/playback pickers
    AudioDevices {
        capture: Vec<AudioDeviceInfo>,
        playback: Vec<AudioDeviceInfo>,
    },
    /// per-peer audio stream status, for the "active streams" list
    AudioStream {
        addr: SocketAddr,
        active: bool,
        latency_ms: u32,
        packets_lost: u64,
        /// normalized 0.0..=1.0 for the VU meter
        level: f32,
    },
    /// an audio-specific error, optionally scoped to one peer
    AudioError {
        addr: Option<SocketAddr>,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDeviceInfo {
    pub id: String,
    pub name: String,
    /// true if this is a loopback/monitor (system-output) source
    pub is_monitor: bool,
    pub is_default: bool,
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub enum FrontendRequest {
    /// change the software-KVM role
    SetOperationMode(OperationMode),
    /// activate/deactivate client
    Activate(ClientHandle, bool),
    /// add a new client
    Create,
    /// add a fully configured client after its connection has been verified
    CreateConfigured { config: ClientConfig, active: bool },
    /// verify DNS, UDP forwarding, DTLS and protocol response without saving a client
    TestConnection {
        request_id: u64,
        hostname: String,
        port: u16,
    },
    /// change the listen port (recreate udp listener)
    ChangePort(u16),
    /// remove a client
    Delete(ClientHandle),
    /// request an enumeration of all clients
    Enumerate(),
    /// resolve dns
    ResolveDns(ClientHandle),
    /// update hostname
    UpdateHostname(ClientHandle, Option<String>),
    /// update port
    UpdatePort(ClientHandle, u16),
    /// update position
    UpdatePosition(ClientHandle, Position),
    /// assign a screen position to a connected (authorized but not yet
    /// paired) device, identified by its certificate fingerprint. Creates
    /// the client entry if none exists for this fingerprint yet and binds
    /// the parked connection to it.
    AssignPosition { fingerprint: String, pos: Position },
    /// update fix-ips
    UpdateFixIps(ClientHandle, Vec<IpAddr>),
    /// set the server endpoint this device dials in client mode
    /// (emulation side connecting out to the capture side)
    SetServer {
        hostname: Option<String>,
        ips: Vec<IpAddr>,
        port: u16,
    },
    /// request reenabling input capture
    EnableCapture,
    /// request reenabling input emulation
    EnableEmulation,
    /// start/stop the active role's pipeline (server: capture,
    /// client: emulation + edge-handoff capture)
    SetServiceRunning(bool),
    /// synchronize all state
    Sync,
    /// authorize fingerprint (description, fingerprint)
    AuthorizeKey(String, String),
    /// remove fingerprint (fingerprint)
    RemoveAuthorizedKey(String),
    /// change the hook command
    UpdateEnterHook(u64, Option<String>),
    /// save config file
    SaveConfiguration,
    /// enable/disable sending this machine's audio to peers
    SetAudioSend(bool),
    /// enable/disable playing audio received from peers
    SetAudioReceive(bool),
    /// update codec/buffer parameters
    UpdateAudioSettings { bitrate: u32, buffer_ms: u32 },
    /// select the audio capture device (`None` = system default)
    SetAudioCaptureDevice(Option<String>),
    /// select the audio playback device (`None` = system default)
    SetAudioPlaybackDevice(Option<String>),
    /// request enumeration of available audio devices
    EnumerateAudioDevices,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Status {
    #[default]
    Disabled,
    Enabled,
}

impl From<Status> for bool {
    fn from(status: Status) -> Self {
        match status {
            Status::Enabled => true,
            Status::Disabled => false,
        }
    }
}

#[cfg(unix)]
const DESKUNION_SOCKET_NAME: &str = "deskunion-socket.sock";

#[derive(Debug, Error)]
pub enum SocketPathError {
    #[error("could not determine $XDG_RUNTIME_DIR: `{0}`")]
    XdgRuntimeDirNotFound(VarError),
    #[error("could not determine $HOME: `{0}`")]
    HomeDirNotFound(VarError),
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn default_socket_path() -> Result<PathBuf, SocketPathError> {
    let xdg_runtime_dir =
        env::var("XDG_RUNTIME_DIR").map_err(SocketPathError::XdgRuntimeDirNotFound)?;
    Ok(Path::new(xdg_runtime_dir.as_str()).join(DESKUNION_SOCKET_NAME))
}

#[cfg(all(unix, target_os = "macos"))]
pub fn default_socket_path() -> Result<PathBuf, SocketPathError> {
    let home = env::var("HOME").map_err(SocketPathError::HomeDirNotFound)?;
    Ok(Path::new(home.as_str())
        .join("Library")
        .join("Caches")
        .join(DESKUNION_SOCKET_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_mode_defaults_to_unconfigured() {
        assert_eq!(OperationMode::default(), OperationMode::Unconfigured);
    }

    #[test]
    fn operation_mode_request_round_trips_through_frontend_ipc() {
        let request = FrontendRequest::SetOperationMode(OperationMode::Client);
        let json = serde_json::to_string(&request).expect("serialize request");
        assert_eq!(
            serde_json::from_str::<FrontendRequest>(&json).expect("deserialize request"),
            request
        );
    }

    #[test]
    fn configured_client_request_round_trips_through_frontend_ipc() {
        let request = FrontendRequest::CreateConfigured {
            config: ClientConfig {
                hostname: Some("127.0.0.1".to_owned()),
                port: 4243,
                pos: Position::Right,
                ..ClientConfig::default()
            },
            active: true,
        };
        let json = serde_json::to_string(&request).expect("serialize request");
        assert_eq!(
            serde_json::from_str::<FrontendRequest>(&json).expect("deserialize request"),
            request
        );
    }

    #[test]
    fn client_config_without_fingerprint_deserializes() {
        // configs written before fingerprint pairing was introduced
        // must keep loading
        let json = r#"{"hostname":null,"fix_ips":[],"port":4242,"pos":"left","cmd":null}"#;
        let config: ClientConfig = serde_json::from_str(json).expect("deserialize client config");
        assert_eq!(config.fingerprint, None);
    }

    #[test]
    fn assign_position_round_trips_through_frontend_ipc() {
        let request = FrontendRequest::AssignPosition {
            fingerprint: "ab:cd".to_owned(),
            pos: Position::Right,
        };
        let json = serde_json::to_string(&request).expect("serialize request");
        assert_eq!(
            serde_json::from_str::<FrontendRequest>(&json).expect("deserialize request"),
            request
        );
    }

    #[test]
    fn set_server_round_trips_through_frontend_ipc() {
        let request = FrontendRequest::SetServer {
            hostname: Some("10.0.2.2".to_owned()),
            ips: vec!["10.0.2.2".parse().expect("ip")],
            port: DEFAULT_PORT,
        };
        let json = serde_json::to_string(&request).expect("serialize request");
        assert_eq!(
            serde_json::from_str::<FrontendRequest>(&json).expect("deserialize request"),
            request
        );
    }
}
