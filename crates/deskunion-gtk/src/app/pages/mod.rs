//! The window's pages. Each one owns its widgets and the state only it
//! shows; it reads the shared `Session` and reaches the service and the
//! window through `PageOutput`.

pub mod audio;
mod audio_devices;
pub mod connections;
mod log_state;
pub mod logs;
pub mod settings;

use deskunion_ipc::{FrontendRequest, OperationMode};

use super::session::ClientSummary;

#[derive(Debug)]
pub enum PageOutput {
    Request(FrontendRequest),
    Toast(String),
    /// stays until dismissed
    Error(String),
    SetMode(OperationMode),
    /// Start or Stop, depending on whether sharing runs
    ToggleSharing,
    /// the user verified a server: save it and start sharing
    StartSharing,
    OpenFingerprintDialog(Option<String>),
    AddClient,
    Clients(Vec<ClientSummary>),
}
