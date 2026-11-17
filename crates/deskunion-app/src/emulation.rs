use crate::connect::{ConnectionEvent, DeskunionConnection, ServerTarget};
use deskunion_proto::{Position, ProtoEvent};
use input_emulation::{EmulationHandle, InputEmulation, InputEmulationError};
use input_event::Event;
use local_channel::mpsc::{Receiver, Sender, channel};
use std::{
    cell::Cell,
    collections::HashMap,
    net::SocketAddr,
    rc::Rc,
    time::{Duration, Instant},
};
use tokio::{
    select,
    task::{JoinHandle, spawn_local},
};

/// emulation handling events received over the dialed server connection
pub(crate) struct Emulation {
    task: JoinHandle<()>,
    request_tx: Sender<EmulationRequest>,
    event_rx: Receiver<EmulationEvent>,
}

pub(crate) enum EmulationEvent {
    /// the session to the configured server is established
    Connected {
        addr: SocketAddr,
        fingerprint: String,
    },
    /// the server's pointer entered this device
    Entered {
        /// address of the connection
        addr: SocketAddr,
        /// position of the connection
        pos: deskunion_ipc::Position,
        /// certificate fingerprint of the connection
        fingerprint: String,
    },
    /// connection to the server closed
    Disconnected { addr: SocketAddr },
    /// a dial failed; the next attempt is scheduled
    Retrying(crate::retry::RetryNotice),
    #[cfg(feature = "audio")]
    AudioError { addr: SocketAddr, error: String },
    /// result of a dial-out test requested by the frontend
    ConnectionTested {
        request_id: u64,
        error: Option<String>,
    },
    /// emulation was disabled
    EmulationDisabled,
    /// the emulation session failed and starts again after `delay`; input
    /// is dropped meanwhile, but the server connection stays up
    EmulationRestarting { delay: Duration },
    /// emulation was enabled
    EmulationEnabled,
    /// capture should be released
    ReleaseNotify,
    /// local clipboard text changed while this peer owns pointer focus
    ClipboardText(String),
}

enum EmulationRequest {
    Reenable,
    SetEnabled(bool),
    Release(SocketAddr),
    /// set (or clear) the server endpoint to dial
    SetServer(Option<ServerTarget>),
    /// dial again now, ignoring the retry delay
    Reconnect,
    TestConnection {
        request_id: u64,
        hostname: String,
        port: u16,
    },
    Terminate,
}

impl Emulation {
    pub(crate) fn new(
        backend: Option<input_emulation::Backend>,
        conn: DeskunionConnection,
        enabled: bool,
        clipboard_enabled: bool,
    ) -> Self {
        let emulation_proxy = EmulationProxy::new(backend, enabled, clipboard_enabled);
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_task = DialTask {
            conn,
            emulation_proxy,
            request_rx,
            event_tx,
            clipboard_enabled,
        };
        let task = spawn_local(emulation_task.run());
        Self {
            task,
            request_tx,
            event_rx,
        }
    }

    pub(crate) fn send_leave_event(&self, addr: SocketAddr) {
        self.request_tx
            .send(EmulationRequest::Release(addr))
            .expect("channel closed");
    }

    pub(crate) fn reenable(&self) {
        self.request_tx
            .send(EmulationRequest::Reenable)
            .expect("channel closed");
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.request_tx
            .send(EmulationRequest::SetEnabled(enabled))
            .expect("channel closed");
    }

    pub(crate) fn set_server(&self, target: Option<ServerTarget>) {
        self.request_tx
            .send(EmulationRequest::SetServer(target))
            .expect("channel closed");
    }

    pub(crate) fn reconnect_now(&self) {
        self.request_tx
            .send(EmulationRequest::Reconnect)
            .expect("channel closed");
    }

    pub(crate) fn test_connection(&self, request_id: u64, hostname: String, port: u16) {
        self.request_tx
            .send(EmulationRequest::TestConnection {
                request_id,
                hostname,
                port,
            })
            .expect("channel closed");
    }

    pub(crate) async fn event(&mut self) -> EmulationEvent {
        self.event_rx.recv().await.expect("channel closed")
    }

    /// wait for termination
    pub(crate) async fn terminate(&mut self) {
        log::debug!("terminating emulation");
        self.request_tx
            .send(EmulationRequest::Terminate)
            .expect("channel closed");
        if let Err(e) = (&mut self.task).await {
            log::warn!("{e}");
        }
    }
}

struct DialTask {
    conn: DeskunionConnection,
    emulation_proxy: EmulationProxy,
    request_rx: Receiver<EmulationRequest>,
    event_tx: Sender<EmulationEvent>,
    clipboard_enabled: bool,
}

impl DialTask {
    async fn run(mut self) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        let mut clipboard_text = crate::clipboard::ClipboardTextAssembler::default();
        let mut active_addr = None;
        let mut clipboard_transfer_id = 0u32;
        let mut pending_local_clipboard: Option<String> = None;
        let mut last_sent_local_clipboard: Option<String> = None;
        // Keep this above the listener's DTLS liveness interval. Audio and
        // control share one association, so a transiently slow audio send
        // must not make the UI declare the peer dead.
        const PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
        let mut last_response = HashMap::new();
        loop {
            select! {
                e = self.conn.recv() => {match e {
                    ConnectionEvent::Message { event, addr } => {
                        log::trace!("{event} <-<-<-<-<- {addr}");
                        last_response.insert(addr, Instant::now());
                        match event {
                            ProtoEvent::Enter(pos) => {
                                if let Some(fingerprint) = self.conn.get_certificate_fingerprint(addr).await {
                                    log::info!("releasing capture: {addr} entered this device");
                                    if active_addr != Some(addr) {
                                        clipboard_text.reset();
                                    }
                                    active_addr = Some(addr);
                                    if self.clipboard_enabled
                                        && let Some(text) = pending_local_clipboard.take() {
                                            last_sent_local_clipboard = Some(text.clone());
                                            self.conn.send_clipboard_text(addr, clipboard_transfer_id, &text);
                                            clipboard_transfer_id = clipboard_transfer_id.wrapping_add(1);
                                        }
                                    self.event_tx.send(EmulationEvent::ReleaseNotify).expect("channel closed");
                                    self.conn.reply(addr, ProtoEvent::Ack(0)).await;
                                    self.event_tx.send(EmulationEvent::Entered{addr, pos: to_ipc_pos(pos), fingerprint}).expect("channel closed");
                                }
                            }
                            ProtoEvent::Leave(_) => {
                                if active_addr == Some(addr) {
                                    active_addr = None;
                                    clipboard_text.reset();
                                    pending_local_clipboard = None;
                                    last_sent_local_clipboard = None;
                                    self.conn.clear_clipboard_text();
                                }
                                self.emulation_proxy.remove(addr);
                                self.conn.reply(addr, ProtoEvent::Ack(0)).await;
                            }
                            ProtoEvent::Input(event) => self.emulation_proxy.consume(event, addr),
                            // the listener (capture side) pings; only the
                            // emulation side knows whether it can receive
                            ProtoEvent::Ping => self.conn.reply(addr, ProtoEvent::Pong(self.emulation_proxy.emulation_active.get())).await,
                            _ => {}
                        }
                    }
                    ConnectionEvent::PeerAlive { addr } => {
                        // any received datagram (forward-compat unknowns)
                        // proves the peer is alive, not just application
                        // events
                        last_response.insert(addr, Instant::now());
                    }
                    ConnectionEvent::ClipboardText { addr, fragment, payload } => {
                        last_response.insert(addr, Instant::now());
                        if self.clipboard_enabled && active_addr == Some(addr) {
                            let transfer_id = fragment.transfer_id;
                            let is_last_fragment = fragment.index + 1 == fragment.count;
                            if let Some(text) = clipboard_text.push(&fragment, &payload) {
                                log::info!("clipboard: applying text from {addr} ({} bytes)", text.len());
                                self.emulation_proxy.set_clipboard_text(addr, text);
                            }
                            if is_last_fragment && clipboard_text.is_completed(transfer_id) {
                                self.conn.send_clipboard_ack(addr, transfer_id);
                            }
                        }
                    }
                    ConnectionEvent::ClipboardAck { addr, transfer_id } => {
                        last_response.insert(addr, Instant::now());
                        self.conn.acknowledge_clipboard_text(addr, transfer_id);
                        }
                    ConnectionEvent::Retrying(notice) => {
                        self.event_tx.send(EmulationEvent::Retrying(notice)).expect("channel closed");
                    }
                    ConnectionEvent::Connected { addr, fingerprint } => {
                        last_response.insert(addr, Instant::now());
                        self.event_tx.send(EmulationEvent::Connected { addr, fingerprint }).expect("channel closed");
                    }
                    ConnectionEvent::Disconnected { addr } => {
                        last_response.remove(&addr);
                        self.emulation_proxy.remove(addr);
                        if active_addr == Some(addr) {
                            active_addr = None;
                            clipboard_text.reset();
                            pending_local_clipboard = None;
                            last_sent_local_clipboard = None;
                            self.conn.clear_clipboard_text();
                        }
                        self.event_tx.send(EmulationEvent::Disconnected { addr }).expect("channel closed");
                    }
                    #[cfg(feature = "audio")]
                    ConnectionEvent::AudioError { addr, error } => {
                        let _ = self.event_tx.send(audio_error_event(addr, error));
                    }
                }},
                event = self.emulation_proxy.event() => match event {
                    EmulationEvent::ClipboardText(text) => {
                        log::info!("clipboard: local text ready ({} bytes), active={active_addr:?}", text.len());
                        if self.clipboard_enabled {
                            if let Some(addr) = active_addr {
                                if last_sent_local_clipboard.as_deref() != Some(text.as_str()) {
                                    last_sent_local_clipboard = Some(text.clone());
                                    self.conn.send_clipboard_text(addr, clipboard_transfer_id, &text);
                                    clipboard_transfer_id = clipboard_transfer_id.wrapping_add(1);
                                }
                            } else {
                                pending_local_clipboard = Some(text);
                            }
                        }
                    }
                    event @ (EmulationEvent::EmulationDisabled
                    | EmulationEvent::EmulationRestarting { .. }) => {
                        // input is dropped from now on: hand the pointer
                        // back instead of leaving the server grabbing it
                        if let Some(addr) = active_addr {
                            self.conn.reply(addr, ProtoEvent::Leave(0)).await;
                        }
                        pending_local_clipboard = None;
                        active_addr = None;
                        last_sent_local_clipboard = None;
                        self.conn.clear_clipboard_text();
                        self.event_tx.send(event).expect("channel closed");
                    }
                    event => self.event_tx.send(event).expect("channel closed"),
                },
                request = self.request_rx.recv() => match request.expect("channel closed") {
                    // reenable emulation
                    EmulationRequest::Reenable => self.emulation_proxy.reenable(),
                    EmulationRequest::SetEnabled(enabled) => self.emulation_proxy.set_enabled(enabled),
                    // notify the other end that we hit a barrier (should release capture)
                    EmulationRequest::Release(addr) => self.conn.reply(addr, ProtoEvent::Leave(0)).await,
                    EmulationRequest::SetServer(target) => {
                        pending_local_clipboard = None;
                        active_addr = None;
                        last_sent_local_clipboard = None;
                        self.conn.clear_clipboard_text();
                        self.conn.set_server(target).await;
                    }
                    EmulationRequest::Reconnect => self.conn.reconnect_now().await,
                    EmulationRequest::TestConnection { request_id, hostname, port } => {
                        let test = self.conn.test_connection(hostname, port);
                        let event_tx = self.event_tx.clone();
                        spawn_local(async move {
                            let error = test.await.err().map(|error| error.to_string());
                            event_tx
                                .send(EmulationEvent::ConnectionTested { request_id, error })
                                .expect("channel closed");
                        });
                    }
                    EmulationRequest::Terminate => break,
                },
                _ = interval.tick() => {
                    let mut timed_out = Vec::new();
                    last_response.retain(|&addr, instant| {
                        if instant.elapsed() > PEER_IDLE_TIMEOUT {
                            timed_out.push(addr);
                            false
                        } else {
                            true
                        }
                    });
                    for addr in timed_out {
                        log::warn!("releasing keys: {addr} not responding!");
                        self.emulation_proxy.remove(addr);
                        // tear the DTLS connection down deterministically
                        // (sending `AudioControl::Stop` first) so our own
                        // audio sender/capture doesn't linger as an orphan;
                        // the reconnect loop redials the server
                        self.conn.close(addr).await;
                        self.event_tx.send(EmulationEvent::Disconnected { addr }).expect("channel closed");
                    }
                }
            }
        }
        self.conn.terminate().await;
        self.emulation_proxy.terminate().await;
    }
}

#[cfg(feature = "audio")]
fn audio_error_event(addr: SocketAddr, error: String) -> EmulationEvent {
    EmulationEvent::AudioError { addr, error }
}

#[cfg(all(test, feature = "audio"))]
mod audio_error_tests {
    use super::*;

    #[test]
    fn sender_start_failure_is_forwarded_as_emulation_event() {
        let addr = "127.0.0.1:4242".parse().unwrap();
        let event = audio_error_event(addr, "capture unavailable".to_owned());

        assert!(
            matches!(event, EmulationEvent::AudioError { addr: event_addr, error } if event_addr == addr && error == "capture unavailable")
        );
    }
}

/// proxy handling the actual input emulation,
/// discarding events when it is disabled
pub(crate) struct EmulationProxy {
    emulation_active: Rc<Cell<bool>>,
    exit_requested: Rc<Cell<bool>>,
    request_tx: Sender<ProxyRequest>,
    event_rx: Receiver<EmulationEvent>,
    task: JoinHandle<()>,
}

enum ProxyRequest {
    Input(Event, SocketAddr),
    ClipboardText(String, SocketAddr),
    Remove(SocketAddr),
    Terminate,
    Reenable,
    SetEnabled(bool),
}

impl EmulationProxy {
    fn new(
        backend: Option<input_emulation::Backend>,
        enabled: bool,
        clipboard_enabled: bool,
    ) -> Self {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_active = Rc::new(Cell::new(false));
        let exit_requested = Rc::new(Cell::new(false));
        let emulation_task = EmulationTask {
            backend,
            enabled,
            clipboard_enabled,
            exit_requested: exit_requested.clone(),
            request_rx,
            event_tx,
            handles: Default::default(),
            next_id: 0,
            announced_enabled: false,
            session_backend: None,
            session_started: None,
        };
        let task = spawn_local(emulation_task.run());
        Self {
            emulation_active,
            exit_requested,
            request_tx,
            task,
            event_rx,
        }
    }

    async fn event(&mut self) -> EmulationEvent {
        let event = self.event_rx.recv().await.expect("channel closed");
        if matches!(&event, EmulationEvent::EmulationEnabled) {
            self.emulation_active.replace(true);
        }
        if matches!(
            &event,
            EmulationEvent::EmulationDisabled | EmulationEvent::EmulationRestarting { .. }
        ) {
            self.emulation_active.replace(false);
        }
        event
    }

    fn consume(&self, event: Event, addr: SocketAddr) {
        // ignore events if emulation is currently disabled
        if self.emulation_active.get() {
            self.request_tx
                .send(ProxyRequest::Input(event, addr))
                .expect("channel closed");
        }
    }

    fn remove(&self, addr: SocketAddr) {
        self.request_tx
            .send(ProxyRequest::Remove(addr))
            .expect("channel closed");
    }

    fn set_clipboard_text(&self, addr: SocketAddr, text: String) {
        if self.emulation_active.get() {
            self.request_tx
                .send(ProxyRequest::ClipboardText(text, addr))
                .expect("channel closed");
        }
    }

    fn reenable(&self) {
        self.request_tx
            .send(ProxyRequest::Reenable)
            .expect("channel closed");
    }

    fn set_enabled(&self, enabled: bool) {
        self.request_tx
            .send(ProxyRequest::SetEnabled(enabled))
            .expect("channel closed");
    }

    async fn terminate(&mut self) {
        self.exit_requested.replace(true);
        self.request_tx
            .send(ProxyRequest::Terminate)
            .expect("channel closed");
        let _ = (&mut self.task).await;
    }
}

struct EmulationTask {
    backend: Option<input_emulation::Backend>,
    enabled: bool,
    clipboard_enabled: bool,
    exit_requested: Rc<Cell<bool>>,
    request_rx: Receiver<ProxyRequest>,
    event_tx: Sender<EmulationEvent>,
    handles: HashMap<SocketAddr, EmulationHandle>,
    next_id: EmulationHandle,
    /// `EmulationEnabled` was sent and not yet followed by `EmulationDisabled`
    announced_enabled: bool,
    /// the backend the last session ran on; a restart must not fall back
    /// to another one (the dummy backend would swallow all input)
    session_backend: Option<input_emulation::Backend>,
    /// when the current session became usable, for [`STABLE_SESSION`]
    session_started: Option<Instant>,
}

/// Delays before input emulation is started again after it failed (a
/// compositor ending the portal session on screen lock, a reset EIS
/// socket). Once they are used up, emulation stays off until it is
/// enabled again by hand.
const RESTART_DELAYS: [Duration; 7] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// A session that ran at least this long starts the restart delays over.
const STABLE_SESSION: Duration = Duration::from_secs(10);

/// How many times input emulation was restarted in a row.
#[derive(Debug, Default)]
struct Restarts {
    attempts: usize,
}

impl Restarts {
    /// the delay before the next restart, or `None` once they are used up
    fn next_delay(&mut self) -> Option<Duration> {
        let delay = RESTART_DELAYS.get(self.attempts).copied()?;
        self.attempts += 1;
        Some(delay)
    }

    fn reset(&mut self) {
        self.attempts = 0;
    }
}

/// what ended the wait before input emulation starts again
#[derive(Debug, PartialEq, Eq)]
enum Wait {
    Restart,
    /// the user turned emulation off; only an explicit enable restarts it
    Disabled,
    Terminate,
}

/// A denied permission prompt would come back on every retry, so that one
/// waits for the user.
fn should_restart(result: &Result<(), InputEmulationError>) -> bool {
    matches!(result, Err(e) if !e.cancelled_by_user())
}

/// The delay before the next restart after a session ended with `result`,
/// having been usable for `ran` (`None` when it never got that far, e.g. the
/// portal failed). Only a session that really ran for [`STABLE_SESSION`]
/// starts the delays over, so a slow failing portal cannot restart forever.
fn next_restart(
    restarts: &mut Restarts,
    result: &Result<(), InputEmulationError>,
    ran: Option<Duration>,
) -> Option<Duration> {
    if ran.is_some_and(|ran| ran >= STABLE_SESSION) {
        restarts.reset();
    }
    if should_restart(result) {
        restarts.next_delay()
    } else {
        None
    }
}

/// A request that ended the setup of a session.
#[derive(Debug, PartialEq, Eq)]
enum Interrupt {
    Disabled,
    Terminate,
}

impl EmulationTask {
    async fn run(mut self) {
        let mut restarts = Restarts::default();
        loop {
            if self.enabled {
                let result = self.do_emulation().await;
                if let Err(e) = &result {
                    log::warn!("input emulation exited: {e}");
                }
                if self.exit_requested.get() {
                    break;
                }
                let ran = self.session_started.take().map(|at| at.elapsed());
                let delay = if self.enabled {
                    next_restart(&mut restarts, &result, ran)
                } else {
                    None
                };
                if let Some(delay) = delay {
                    log::info!("restarting input emulation in {delay:?}");
                    self.event_tx
                        .send(EmulationEvent::EmulationRestarting { delay })
                        .expect("channel closed");
                    match self.wait_to_restart(Some(delay)).await {
                        Wait::Restart => continue,
                        Wait::Disabled => {}
                        Wait::Terminate => break,
                    }
                }
                self.enabled = false;
                self.announce_disabled();
            }
            if self.exit_requested.get() {
                break;
            }
            match self.wait_to_restart(None).await {
                Wait::Restart => {
                    restarts.reset();
                    // enabled by hand: any backend may be picked again
                    self.session_backend = None;
                    self.enabled = true;
                }
                Wait::Disabled => {}
                Wait::Terminate => break,
            }
        }
        self.announce_disabled();
    }

    fn announce_disabled(&mut self) {
        if std::mem::take(&mut self.announced_enabled) {
            self.event_tx
                .send(EmulationEvent::EmulationDisabled)
                .expect("channel closed");
        }
    }

    /// Waits for `delay` (forever with `None`), or until emulation is
    /// enabled or disabled by hand.
    async fn wait_to_restart(&mut self, delay: Option<Duration>) -> Wait {
        let sleep = async {
            match delay {
                Some(delay) => tokio::time::sleep(delay).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(sleep);
        loop {
            select! {
                _ = &mut sleep => return Wait::Restart,
                request = self.request_rx.recv() => match request.expect("channel closed") {
                    ProxyRequest::Reenable | ProxyRequest::SetEnabled(true) => return Wait::Restart,
                    ProxyRequest::SetEnabled(false) => {
                        self.enabled = false;
                        return Wait::Disabled;
                    }
                    ProxyRequest::Terminate => return Wait::Terminate,
                    // no session to destroy it in: the next one must not
                    // recreate a device for a peer that is gone
                    ProxyRequest::Remove(addr) => {
                        self.handles.remove(&addr);
                    }
                    ProxyRequest::Input(..) | ProxyRequest::ClipboardText(..) => {
                        /* emulation inactive => ignore */
                    }
                },
            }
        }
    }

    async fn do_emulation(&mut self) -> Result<(), InputEmulationError> {
        log::info!("creating input emulation ...");
        let backend = self.backend.or(self.session_backend);
        let mut emulation = tokio::select! {
            r = InputEmulation::new_with_clipboard(backend, self.clipboard_enabled) => r?,
            // a portal prompt can take long: stay responsive meanwhile
            _ = self.interrupted() => return Ok(()),
        };
        self.session_backend = Some(emulation.backend());

        // create active handles
        let created = self.create_clients(&mut emulation).await;
        if created.is_err() || self.exit_requested.get() || !self.enabled {
            emulation.terminate().await;
            return created;
        }

        // sent again after every restart: it is what lets the proxy forward
        // input once more. `run` reports how the session ended.
        self.announced_enabled = true;
        self.session_started = Some(Instant::now());
        self.event_tx
            .send(EmulationEvent::EmulationEnabled)
            .expect("channel closed");

        let res = self.do_emulation_session(&mut emulation).await;
        // FIXME replace with async drop when stabilized
        emulation.terminate().await;
        res
    }

    async fn create_clients(
        &mut self,
        emulation: &mut InputEmulation,
    ) -> Result<(), InputEmulationError> {
        let handles: Vec<_> = self.handles.values().copied().collect();
        for handle in handles {
            tokio::select! {
                _ = emulation.create(handle) => {},
                _ = self.interrupted() => return Ok(()),
            }
        }
        Ok(())
    }

    /// Serves requests while a session is set up, until one of them ends
    /// the setup. A disable or terminate must not be lost while a portal
    /// prompt is pending, nor a peer removal: the new session would
    /// recreate a device for a peer that is gone.
    async fn interrupted(&mut self) -> Interrupt {
        loop {
            match self.request_rx.recv().await.expect("channel closed") {
                ProxyRequest::Terminate => return Interrupt::Terminate,
                ProxyRequest::SetEnabled(false) => {
                    self.enabled = false;
                    return Interrupt::Disabled;
                }
                ProxyRequest::Remove(addr) => {
                    self.handles.remove(&addr);
                }
                ProxyRequest::Input(..)
                | ProxyRequest::ClipboardText(..)
                | ProxyRequest::Reenable
                | ProxyRequest::SetEnabled(true) => {}
            }
        }
    }

    async fn do_emulation_session(
        &mut self,
        emulation: &mut InputEmulation,
    ) -> Result<(), InputEmulationError> {
        loop {
            tokio::select! {
                e = self.request_rx.recv() => match e.expect("channel closed") {
                    ProxyRequest::Input(event, addr) => {
                        let handle = match self.handles.get(&addr) {
                            Some(&handle) => handle,
                            None => {
                                let handle = self.next_id;
                                self.next_id += 1;
                                emulation.create(handle).await;
                                self.handles.insert(addr, handle);
                                handle
                            }
                        };
                        emulation.consume(event, handle).await?;
                    },
                    ProxyRequest::ClipboardText(text, _addr) => {
                        if self.clipboard_enabled {
                            emulation.set_clipboard_text(text).await?;
                        }
                    }
                    ProxyRequest::Remove(addr) => {
                        if let Some(handle) = self.handles.remove(&addr) {
                            emulation.destroy(handle).await;
                        }
                    }
                    ProxyRequest::Terminate => break Ok(()),
                    ProxyRequest::Reenable => continue,
                    ProxyRequest::SetEnabled(true) => continue,
                    ProxyRequest::SetEnabled(false) => {
                        self.enabled = false;
                        break Ok(());
                    },
                },
                text = emulation.next_clipboard_text(), if self.clipboard_enabled => {
                    if let Some(text) = text {
                        self.event_tx
                            .send(EmulationEvent::ClipboardText(text))
                            .expect("channel closed");
                    }
                }
            }
        }
    }
}

fn to_ipc_pos(pos: Position) -> deskunion_ipc::Position {
    match pos {
        Position::Left => deskunion_ipc::Position::Left,
        Position::Right => deskunion_ipc::Position::Right,
        Position::Top => deskunion_ipc::Position::Top,
        Position::Bottom => deskunion_ipc::Position::Bottom,
    }
}

#[cfg(test)]
mod restart_tests {
    use super::*;

    fn task() -> (EmulationTask, Sender<ProxyRequest>) {
        let (request_tx, request_rx) = channel();
        let (event_tx, _event_rx) = channel();
        let task = EmulationTask {
            backend: Some(input_emulation::Backend::Dummy),
            enabled: true,
            clipboard_enabled: false,
            exit_requested: Default::default(),
            request_rx,
            event_tx,
            handles: Default::default(),
            next_id: 0,
            announced_enabled: false,
            session_backend: None,
            session_started: None,
        };
        (task, request_tx)
    }

    #[test]
    fn restart_delays_grow_and_run_out() {
        let mut restarts = Restarts::default();
        let delays: Vec<_> = std::iter::from_fn(|| restarts.next_delay()).collect();
        assert_eq!(delays, RESTART_DELAYS);
        assert!(delays.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(restarts.next_delay(), None);
    }

    #[test]
    fn restart_delays_start_over_after_reset() {
        let mut restarts = Restarts::default();
        restarts.next_delay();
        restarts.next_delay();
        restarts.reset();
        assert_eq!(restarts.next_delay(), Some(RESTART_DELAYS[0]));
    }

    #[test]
    fn a_failed_session_is_restarted_but_a_clean_exit_is_not() {
        let failed: Result<(), InputEmulationError> =
            Err(input_emulation::EmulationError::EndOfStream.into());
        assert!(should_restart(&failed));
        assert!(!should_restart(&Ok(())));
    }

    /// a portal that takes longer than [`STABLE_SESSION`] to fail must not
    /// start the delays over every time: the restarts have to run out
    #[test]
    fn only_a_session_that_really_ran_starts_the_delays_over() {
        let failed: Result<(), InputEmulationError> =
            Err(input_emulation::EmulationError::EndOfStream.into());
        let mut restarts = Restarts::default();
        let delays: Vec<_> =
            std::iter::from_fn(|| next_restart(&mut restarts, &failed, None)).collect();
        assert_eq!(delays, RESTART_DELAYS);

        assert_eq!(
            next_restart(&mut restarts, &failed, Some(STABLE_SESSION)),
            Some(RESTART_DELAYS[0])
        );
        assert_eq!(
            next_restart(&mut restarts, &failed, Some(STABLE_SESSION / 2)),
            Some(RESTART_DELAYS[1])
        );
        assert_eq!(next_restart(&mut restarts, &Ok(()), None), None);
    }

    /// a disable or a peer removal that arrives while a portal prompt is
    /// pending must not be lost
    #[tokio::test]
    async fn a_disable_during_setup_ends_it_and_removals_are_kept() {
        let (mut task, tx) = task();
        let addr: SocketAddr = "127.0.0.1:4242".parse().expect("addr");
        task.handles.insert(addr, 0);
        tx.send(ProxyRequest::Remove(addr)).expect("send");
        tx.send(ProxyRequest::Reenable).expect("send");
        tx.send(ProxyRequest::SetEnabled(false)).expect("send");
        assert_eq!(task.interrupted().await, Interrupt::Disabled);
        assert!(!task.enabled);
        assert!(task.handles.is_empty());
        tx.send(ProxyRequest::Terminate).expect("send");
        assert_eq!(task.interrupted().await, Interrupt::Terminate);
    }

    /// a restart must reuse the backend that worked: picking again could
    /// land on the dummy backend, which swallows all input without failing
    #[tokio::test]
    async fn a_session_remembers_its_backend_and_announces_itself_once_usable() {
        let (request_tx, request_rx) = channel();
        let (event_tx, mut event_rx) = channel();
        let mut task = EmulationTask {
            backend: Some(input_emulation::Backend::Dummy),
            enabled: true,
            clipboard_enabled: false,
            exit_requested: Default::default(),
            request_rx,
            event_tx,
            handles: Default::default(),
            next_id: 0,
            announced_enabled: false,
            session_backend: None,
            session_started: None,
        };
        // the session is ended only after it announced itself, so the
        // terminate cannot race the creation of the session
        async fn one_session(
            task: &mut EmulationTask,
            requests: &Sender<ProxyRequest>,
            events: &mut Receiver<EmulationEvent>,
        ) -> Option<EmulationEvent> {
            let stop = async {
                let event = events.recv().await;
                requests.send(ProxyRequest::Terminate).expect("send");
                event
            };
            let (session, event) = tokio::join!(task.do_emulation(), stop);
            session.expect("session");
            event
        }

        let event = one_session(&mut task, &request_tx, &mut event_rx).await;
        assert!(matches!(event, Some(EmulationEvent::EmulationEnabled)));
        assert_eq!(task.session_backend, Some(input_emulation::Backend::Dummy));
        assert!(task.session_started.is_some());

        // a restart without a configured backend reuses the one that worked
        task.backend = None;
        let event = one_session(&mut task, &request_tx, &mut event_rx).await;
        assert!(matches!(event, Some(EmulationEvent::EmulationEnabled)));
        assert_eq!(task.session_backend, Some(input_emulation::Backend::Dummy));
    }

    #[tokio::test(start_paused = true)]
    async fn the_wait_ends_after_the_delay() {
        let (mut task, _tx) = task();
        assert_eq!(
            task.wait_to_restart(Some(Duration::from_millis(250))).await,
            Wait::Restart
        );
    }

    #[tokio::test(start_paused = true)]
    async fn enabling_by_hand_cuts_the_wait_short() {
        let (mut task, tx) = task();
        tx.send(ProxyRequest::Reenable).expect("send");
        let started = tokio::time::Instant::now();
        assert_eq!(
            task.wait_to_restart(Some(Duration::from_secs(30))).await,
            Wait::Restart
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn disabling_or_terminating_ends_the_wait_without_a_restart() {
        let (mut task, tx) = task();
        tx.send(ProxyRequest::SetEnabled(false)).expect("send");
        assert_eq!(
            task.wait_to_restart(Some(Duration::from_secs(30))).await,
            Wait::Disabled
        );
        tx.send(ProxyRequest::Terminate).expect("send");
        assert_eq!(task.wait_to_restart(None).await, Wait::Terminate);
    }

    /// a peer that left while no session existed must not get its device
    /// recreated by the next session
    #[tokio::test(start_paused = true)]
    async fn a_peer_removed_during_the_wait_is_forgotten() {
        let (mut task, tx) = task();
        let addr: SocketAddr = "127.0.0.1:4242".parse().expect("addr");
        task.handles.insert(addr, 0);
        tx.send(ProxyRequest::Remove(addr)).expect("send");
        tx.send(ProxyRequest::Input(
            Event::Pointer(input_event::PointerEvent::Motion {
                time: 0,
                dx: 1.0,
                dy: 1.0,
            }),
            addr,
        ))
        .expect("send");
        tx.send(ProxyRequest::Reenable).expect("send");
        assert_eq!(task.wait_to_restart(None).await, Wait::Restart);
        assert!(task.handles.is_empty());
    }
}
