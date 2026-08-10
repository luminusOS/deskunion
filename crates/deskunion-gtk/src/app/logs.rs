use std::collections::VecDeque;

use gtk::glib;
use gtk::prelude::*;

use deskunion_ipc::FrontendEvent;

/// coarse bucket for the Logs page's filter chips — `None` filter means
/// "All".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCategory {
    Connections,
    Audio,
    Errors,
}

pub struct LogEntry {
    category: LogCategory,
    label: gtk::Label,
}

const MAX_LOG_ENTRIES: usize = 500;

/// Logs page state — owns the ring buffer and the `gtk::Box` its entries
/// are appended to directly (not a `FactoryVecDeque`: entries are a plain
/// append-only display log, not an editable/interactive collection, so a
/// factory would add ceremony without eliminating any GObject boilerplate
/// — there was never any here).
pub struct LogState {
    entries: VecDeque<LogEntry>,
    filter: Option<LogCategory>,
    list_box: gtk::Box,
}

impl LogState {
    pub fn new(list_box: gtk::Box) -> Self {
        Self {
            entries: VecDeque::new(),
            filter: None,
            list_box,
        }
    }

    /// one-line summary of an incoming `FrontendEvent`, or `None` for
    /// events too frequent/uninteresting to log (`State` fires on every
    /// per-client tick; `PublicKeyFingerprint` has no user-facing signal).
    fn describe(event: &FrontendEvent) -> Option<(LogCategory, String)> {
        let described = match event {
            FrontendEvent::OperationMode(mode) => (
                LogCategory::Connections,
                format!("operation mode: {mode:?}"),
            ),
            FrontendEvent::Error(e) => (LogCategory::Errors, format!("error: {e}")),
            FrontendEvent::AudioError { addr, message } => (
                LogCategory::Errors,
                match addr {
                    Some(addr) => format!("audio error ({addr}): {message}"),
                    None => format!("audio error: {message}"),
                },
            ),
            FrontendEvent::AudioStatus {
                send,
                receive,
                bitrate,
                buffer_ms,
                ..
            } => (
                LogCategory::Audio,
                format!(
                    "audio status: send={send} receive={receive} bitrate={bitrate} buffer={buffer_ms}ms"
                ),
            ),
            FrontendEvent::AudioDevices { capture, playback } => (
                LogCategory::Audio,
                format!(
                    "audio devices: {} capture, {} playback",
                    capture.len(),
                    playback.len()
                ),
            ),
            FrontendEvent::AudioStream { addr, active, .. } => (
                LogCategory::Audio,
                format!(
                    "audio stream {addr}: {}",
                    if *active { "active" } else { "inactive" }
                ),
            ),
            FrontendEvent::Created(_, config, _) => (
                LogCategory::Connections,
                format!(
                    "client created: {}",
                    config.hostname.as_deref().unwrap_or("?")
                ),
            ),
            FrontendEvent::ConnectionTested { error, .. } => match error {
                Some(error) => (
                    LogCategory::Errors,
                    format!("connection test failed: {error}"),
                ),
                None => (
                    LogCategory::Connections,
                    "connection test succeeded".to_owned(),
                ),
            },
            FrontendEvent::Deleted(handle) => (
                LogCategory::Connections,
                format!("client deleted: handle {handle}"),
            ),
            FrontendEvent::State(..) => return None,
            FrontendEvent::NoSuchClient(handle) => (
                LogCategory::Errors,
                format!("no such client: handle {handle}"),
            ),
            FrontendEvent::PortChanged(port, msg) => (
                LogCategory::Connections,
                match msg {
                    Some(m) => format!("port change to {port} failed: {m}"),
                    None => format!("listening on port {port}"),
                },
            ),
            FrontendEvent::Enumerate(clients) => (
                LogCategory::Connections,
                format!("enumerated {} client(s)", clients.len()),
            ),
            FrontendEvent::CaptureStatus(status) => (
                LogCategory::Connections,
                format!("capture status: {status:?}"),
            ),
            FrontendEvent::EmulationStatus(status) => (
                LogCategory::Connections,
                format!("emulation status: {status:?}"),
            ),
            FrontendEvent::AuthorizedUpdated(keys) => (
                LogCategory::Connections,
                format!("authorized keys updated: {} key(s)", keys.len()),
            ),
            FrontendEvent::PublicKeyFingerprint(_) => return None,
            FrontendEvent::DeviceConnected { addr, .. } => (
                LogCategory::Connections,
                format!("device connected: {addr}"),
            ),
            FrontendEvent::DeviceEntered { addr, pos, .. } => (
                LogCategory::Connections,
                format!("device entered ({pos}): {addr}"),
            ),
            FrontendEvent::IncomingDisconnected(addr) => (
                LogCategory::Connections,
                format!("incoming disconnected: {addr}"),
            ),
            FrontendEvent::ConnectionAttempt { fingerprint } => (
                LogCategory::Connections,
                format!("connection attempt, needs authorization: {fingerprint}"),
            ),
            FrontendEvent::ServerEndpoint { hostname, port, .. } => (
                LogCategory::Connections,
                format!(
                    "server endpoint: {}:{port}",
                    hostname.as_deref().unwrap_or("-")
                ),
            ),
        };
        Some(described)
    }

    pub fn log_event(&mut self, event: &FrontendEvent) {
        if let Some((category, message)) = Self::describe(event) {
            self.append(category, &message);
        }
    }

    fn append(&mut self, category: LogCategory, message: &str) {
        let ts = glib::DateTime::now_local()
            .and_then(|d| d.format("%H:%M:%S"))
            .map(|s| s.to_string())
            .unwrap_or_default();
        let tag = match category {
            LogCategory::Connections => "conn",
            LogCategory::Audio => "audio",
            LogCategory::Errors => "error",
        };
        let label = gtk::Label::builder()
            .label(format!("{ts}  [{tag}]  {message}"))
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        label.add_css_class("monospace");
        label.add_css_class("caption");
        if category == LogCategory::Errors {
            label.add_css_class("error");
        }
        let visible = match self.filter {
            None => true,
            Some(c) => c == category,
        };
        label.set_visible(visible);

        self.list_box.append(&label);
        self.entries.push_back(LogEntry { category, label });

        if self.entries.len() > MAX_LOG_ENTRIES {
            if let Some(old) = self.entries.pop_front() {
                self.list_box.remove(&old.label);
            }
        }
    }

    pub fn apply_filter(&mut self, filter: Option<LogCategory>) {
        self.filter = filter;
        for entry in &self.entries {
            let visible = match filter {
                None => true,
                Some(f) => f == entry.category,
            };
            entry.label.set_visible(visible);
        }
    }

    pub fn copy_visible(&self) -> String {
        self.entries
            .iter()
            .filter(|e| e.label.is_visible())
            .map(|e| e.label.label().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn clear(&mut self) {
        for entry in self.entries.drain(..) {
            self.list_box.remove(&entry.label);
        }
    }
}
