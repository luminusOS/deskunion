use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    net::SocketAddr,
    rc::Rc,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use deskunion_ipc::ClientHandle;
use deskunion_proto::ProtoEvent;
use futures::StreamExt;
use input_capture::{
    CaptureError, CaptureEvent, CaptureHandle, InputCapture, InputCaptureError, Position,
};
use input_event::{Event, KeyboardEvent, scancode};
use local_channel::mpsc::{Receiver, Sender, channel};
use tokio::task::{JoinHandle, spawn_local};
use tokio_util::sync::CancellationToken;
use webrtc_dtls::crypto::Certificate;

use crate::client::ClientManager;
use crate::config::local_commit;
use crate::listen::{DeskunionListener, ListenEvent, ListenerCreationError, SendError};

pub(crate) struct Capture {
    cancellation_token: CancellationToken,
    request_tx: Sender<CaptureRequest>,
    task: JoinHandle<()>,
    event_rx: Receiver<ICaptureEvent>,
}

pub(crate) enum ICaptureEvent {
    /// a client was entered
    CaptureBegin(CaptureHandle),
    /// capture disabled
    CaptureDisabled,
    /// capture disabled
    CaptureEnabled,
    /// A (new) client was entered.
    /// In contrast to [`ICaptureEvent::CaptureBegin`] this
    /// event is only triggered when the capture was
    /// explicitly released in the meantime by
    /// either the remote client leaving its device region,
    /// a new device entering the screen or the release bind.
    ClientEntered(u64),
    ClientStateChanged(ClientHandle),
    /// an authorized device connected in but is not paired to a
    /// client entry yet (parked) — the frontend can offer to assign
    /// it a screen position
    DeviceConnected {
        addr: SocketAddr,
        fingerprint: String,
    },
    /// a parked (unpaired) device disconnected
    DeviceDisconnected {
        addr: SocketAddr,
    },
    /// failed connection attempt (approval for fingerprint required)
    ConnectionAttempt {
        fingerprint: String,
    },
    /// the port of the listener has changed
    PortChanged(Result<u16, ListenerCreationError>),
    AudioStream {
        addr: SocketAddr,
        active: bool,
        latency_ms: u32,
        packets_lost: u64,
        level: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureType {
    /// a normal input capture
    Default,
    /// A capture only interested in [`CaptureEvent::Begin`] events.
    /// The capture is released immediately, if there is no
    /// Default capture at the same position.
    EnterOnly,
}

#[derive(Clone)]
enum CaptureRequest {
    /// capture must release the mouse
    Release,
    /// add a capture client
    Create(CaptureHandle, Position, CaptureType),
    /// destory a capture client
    Destroy(CaptureHandle),
    /// reenable input capture
    Reenable,
    /// enable or disable the backend for the selected operation mode
    SetEnabled(bool),
    /// set release bind
    SetReleaseBind(Vec<scancode::Linux>),
    /// change the listen port (recreate udp listener)
    ChangePort(u16),
    /// create the DTLS listener (entering server mode at runtime; at
    /// startup the listener is created upfront in `Service::new`)
    StartListening {
        port: u16,
        // boxed: a Certificate dwarfs every other variant of this enum
        cert: Box<Certificate>,
        authorized_keys: Arc<RwLock<HashMap<String, String>>>,
        audio: crate::config::AudioSettings,
    },
    /// tear down the DTLS listener (leaving server mode) — in client
    /// mode nothing accepts connections, so no socket stays open
    StopListening,
}

impl Capture {
    pub(crate) fn new(
        backend: Option<input_capture::Backend>,
        listener: Option<DeskunionListener>,
        client_manager: ClientManager,
        release_bind: Vec<scancode::Linux>,
        enabled: bool,
    ) -> Self {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let cancellation_token = CancellationToken::new();
        let capture_task = CaptureTask {
            active_client: None,
            backend,
            cancellation_token: cancellation_token.clone(),
            captures: Default::default(),
            listener,
            client_manager,
            event_tx,
            enabled,
            rejected_connections: Default::default(),
            request_rx,
            release_bind: Rc::new(RefCell::new(release_bind)),
            state: Default::default(),
        };
        let task = spawn_local(capture_task.run());
        Self {
            cancellation_token,
            request_tx,
            task,
            event_rx,
        }
    }

    pub(crate) fn reenable(&self) {
        self.request_tx
            .send(CaptureRequest::Reenable)
            .expect("channel closed");
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.request_tx
            .send(CaptureRequest::SetEnabled(enabled))
            .expect("channel closed");
    }

    pub(crate) async fn terminate(&mut self) {
        self.cancellation_token.cancel();
        log::debug!("terminating capture");
        if let Err(e) = (&mut self.task).await {
            log::warn!("{e}");
        }
    }

    pub(crate) fn create(
        &self,
        handle: CaptureHandle,
        pos: deskunion_ipc::Position,
        capture_type: CaptureType,
    ) {
        let pos = to_capture_pos(pos);
        self.request_tx
            .send(CaptureRequest::Create(handle, pos, capture_type))
            .expect("channel closed");
    }

    pub(crate) fn destroy(&self, handle: CaptureHandle) {
        self.request_tx
            .send(CaptureRequest::Destroy(handle))
            .expect("channel closed");
    }

    pub(crate) fn release(&self) {
        self.request_tx
            .send(CaptureRequest::Release)
            .expect("channel closed");
    }

    pub(crate) async fn event(&mut self) -> ICaptureEvent {
        self.event_rx.recv().await.expect("channel closed")
    }

    pub(crate) fn set_release_bind(&mut self, bind: Vec<scancode::Linux>) {
        let _ = self.request_tx.send(CaptureRequest::SetReleaseBind(bind));
    }

    pub(crate) fn request_port_change(&self, port: u16) {
        self.request_tx
            .send(CaptureRequest::ChangePort(port))
            .expect("channel closed");
    }

    /// create the DTLS listener (server mode). No-op if already
    /// listening; the result surfaces as `ICaptureEvent::PortChanged`.
    pub(crate) fn start_listening(
        &self,
        port: u16,
        cert: Certificate,
        authorized_keys: Arc<RwLock<HashMap<String, String>>>,
        audio: crate::config::AudioSettings,
    ) {
        self.request_tx
            .send(CaptureRequest::StartListening {
                port,
                cert: Box::new(cert),
                authorized_keys,
                audio,
            })
            .expect("channel closed");
    }

    /// tear down the DTLS listener, if any (client mode accepts no
    /// connections, so it must not keep a socket open)
    pub(crate) fn stop_listening(&self) {
        self.request_tx
            .send(CaptureRequest::StopListening)
            .expect("channel closed");
    }
}

/// debounce a statement `$st`, i.e. the statement is executed only if the
/// time since the previous execution is at least `$dur`.
/// `$prev` is used to keep track of this timestamp
macro_rules! debounce {
    ($prev:ident, $dur:expr, $st:stmt) => {
        let exec = match $prev.get() {
            None => true,
            Some(instant) if instant.elapsed() > $dur => true,
            _ => false,
        };
        if exec {
            $prev.replace(Some(Instant::now()));
            $st
        }
    };
}

struct CaptureTask {
    active_client: Option<CaptureHandle>,
    backend: Option<input_capture::Backend>,
    cancellation_token: CancellationToken,
    captures: Vec<(CaptureHandle, Position, CaptureType)>,
    /// the capture side's transport: accepted DTLS connections from
    /// dialed-in emulation devices. `None` outside server mode: a
    /// client-mode device accepts no connections, so it must not keep
    /// a UDP socket open (an always-on listener also triggered a
    /// Windows firewall prompt for no reason). Created lazily via
    /// [`CaptureRequest::StartListening`].
    listener: Option<DeskunionListener>,
    client_manager: ClientManager,
    event_tx: Sender<ICaptureEvent>,
    enabled: bool,
    /// debounce for unauthorized connection attempts
    rejected_connections: HashMap<String, Instant>,
    release_bind: Rc<RefCell<Vec<scancode::Linux>>>,
    request_rx: Receiver<CaptureRequest>,
    state: State,
}

impl CaptureTask {
    /// connection-bookkeeping events that are handled the same whether
    /// or not a capture session is active: pairing on accept, liveness
    /// (Pong) and version metadata (Hello), audio stream stats and
    /// unauthorized connection attempts.
    async fn handle_listen_event(&mut self, event: ListenEvent) {
        match event {
            ListenEvent::Accept { addr, fingerprint } => {
                match self.client_manager.get_client_by_fingerprint(&fingerprint) {
                    Some(handle) => {
                        log::info!(
                            "paired device {fingerprint} connected from {addr} (client {handle})"
                        );
                        self.client_manager.set_active_addr(handle, Some(addr));
                        // the first Pong(emulation_active) turns this on
                        self.client_manager.set_alive(handle, false);
                        self.event_tx
                            .send(ICaptureEvent::ClientStateChanged(handle))
                            .expect("channel closed");
                    }
                    None => {
                        log::info!(
                            "authorized device {fingerprint} connected from {addr}, not paired yet"
                        );
                        self.client_manager.park(fingerprint.clone(), addr);
                        self.event_tx
                            .send(ICaptureEvent::DeviceConnected { addr, fingerprint })
                            .expect("channel closed");
                    }
                }
            }
            ListenEvent::Rejected { fingerprint } => {
                if self
                    .rejected_connections
                    .insert(fingerprint.clone(), Instant::now())
                    .is_none_or(|i| i.elapsed() >= Duration::from_secs(2))
                {
                    self.event_tx
                        .send(ICaptureEvent::ConnectionAttempt { fingerprint })
                        .expect("channel closed");
                }
            }
            ListenEvent::Disconnected { addr } => {
                if let Some(handle) = self.client_manager.get_client_by_active_addr(addr) {
                    log::info!("client {handle} disconnected ({addr})");
                    self.client_manager.set_active_addr(handle, None);
                    self.client_manager.set_alive(handle, false);
                    self.client_manager.set_peer_commit(handle, None);
                    self.event_tx
                        .send(ICaptureEvent::ClientStateChanged(handle))
                        .expect("channel closed");
                } else if self.client_manager.unpark_addr(addr).is_some() {
                    self.event_tx
                        .send(ICaptureEvent::DeviceDisconnected { addr })
                        .expect("channel closed");
                }
            }
            ListenEvent::Msg { event, addr } => match event {
                ProtoEvent::Pong(emulation_active) => {
                    if let Some(handle) = self.client_manager.get_client_by_active_addr(addr) {
                        let changed = self.client_manager.alive(handle) != emulation_active;
                        self.client_manager.set_alive(handle, emulation_active);
                        if changed {
                            self.event_tx
                                .send(ICaptureEvent::ClientStateChanged(handle))
                                .expect("channel closed");
                        }
                    }
                }
                ProtoEvent::Hello { commit } => {
                    // echo our own commit back so the dialed emulation
                    // side can display its server's version
                    if let Some(listener) = &self.listener {
                        listener
                            .reply(
                                addr,
                                ProtoEvent::Hello {
                                    commit: local_commit(),
                                },
                            )
                            .await;
                    }
                    if let Some(handle) = self.client_manager.get_client_by_active_addr(addr) {
                        self.client_manager.set_peer_commit(handle, Some(commit));
                        self.event_tx
                            .send(ICaptureEvent::ClientStateChanged(handle))
                            .expect("channel closed");
                    }
                }
                // `Ack`/`Leave` are consumed by the capture session;
                // `Enter`/`Input`/`Ping` flow in the other direction
                // and are never expected on the listener
                event => log::trace!("ignoring unexpected {event} from {addr}"),
            },
            ListenEvent::AudioStream {
                addr,
                active,
                latency_ms,
                packets_lost,
                level,
            } => self
                .event_tx
                .send(ICaptureEvent::AudioStream {
                    addr,
                    active,
                    latency_ms,
                    packets_lost,
                    level,
                })
                .expect("channel closed"),
        }
    }

    async fn request_port_change(&mut self, port: u16) {
        let result = match &mut self.listener {
            Some(listener) => {
                listener.request_port_change(port);
                listener.port_changed().await
            }
            // not listening (client mode): nothing to rebind — the new
            // port applies when the listener is (re)created
            None => Ok(port),
        };
        self.event_tx
            .send(ICaptureEvent::PortChanged(result))
            .expect("channel closed");
    }

    async fn start_listening(
        &mut self,
        port: u16,
        cert: Certificate,
        authorized_keys: Arc<RwLock<HashMap<String, String>>>,
        audio: crate::config::AudioSettings,
    ) {
        if self.listener.is_some() {
            return;
        }
        let result = DeskunionListener::new(
            port,
            cert,
            authorized_keys,
            self.client_manager.clone(),
            audio,
        )
        .await;
        match result {
            Ok(listener) => {
                log::info!("listening for devices on port {port}");
                self.listener = Some(listener);
                self.event_tx
                    .send(ICaptureEvent::PortChanged(Ok(port)))
                    .expect("channel closed");
            }
            Err(e) => {
                log::warn!("failed to start listening on port {port}: {e}");
                self.event_tx
                    .send(ICaptureEvent::PortChanged(Err(e)))
                    .expect("channel closed");
            }
        }
    }

    async fn stop_listening(&mut self) {
        if let Some(mut listener) = self.listener.take() {
            log::debug!("closing the device listener");
            listener.terminate().await;
        }
    }

    /// send an event to a connected client; without a listener (client
    /// mode) no device can be connected, so this always fails
    async fn send_to_client(
        &self,
        event: ProtoEvent,
        handle: CaptureHandle,
    ) -> Result<(), SendError> {
        match &self.listener {
            Some(listener) => listener.send(event, handle).await,
            None => Err(SendError::NotConnected(handle)),
        }
    }

    fn add_capture(&mut self, handle: CaptureHandle, pos: Position, capture_type: CaptureType) {
        self.captures.push((handle, pos, capture_type));
    }

    fn remove_capture(&mut self, handle: CaptureHandle) {
        self.captures.retain(|&(h, ..)| handle != h);
    }

    fn is_default_capture_at(&self, pos: Position) -> bool {
        self.captures
            .iter()
            .any(|&(_, p, t)| p == pos && t == CaptureType::Default)
    }

    fn get_pos(&self, handle: CaptureHandle) -> Position {
        self.captures
            .iter()
            .find(|(h, ..)| *h == handle)
            .expect("no such capture")
            .1
    }

    fn get_type(&self, handle: CaptureHandle) -> CaptureType {
        self.captures
            .iter()
            .find(|(h, ..)| *h == handle)
            .expect("no such capture")
            .2
    }

    async fn run(mut self) {
        self.run_loop().await;
        log::debug!("terminating listener");
        self.stop_listening().await;
    }

    async fn run_loop(&mut self) {
        loop {
            if self.enabled {
                if let Err(e) = self.do_capture().await {
                    log::warn!("input capture exited: {e}");
                }
                self.enabled = false;
                if self.cancellation_token.is_cancelled() {
                    return;
                }
            }
            loop {
                tokio::select! {
                    r = self.request_rx.recv() => match r.expect("channel closed") {
                        CaptureRequest::Reenable | CaptureRequest::SetEnabled(true) => {
                            self.enabled = true;
                            break;
                        }
                        CaptureRequest::SetEnabled(false) => {},
                        CaptureRequest::Create(h, p, t) => self.add_capture(h, p, t),
                        CaptureRequest::Destroy(h) => self.remove_capture(h),
                        CaptureRequest::Release => { /* nothing to do */ }
                        CaptureRequest::SetReleaseBind(bind) => {
                            self.release_bind.borrow_mut().clone_from(&bind);
                        }
                        CaptureRequest::ChangePort(port) => {
                            self.request_port_change(port).await;
                        }
                        CaptureRequest::StartListening { port, cert, authorized_keys, audio } => {
                            self.start_listening(port, *cert, authorized_keys, audio).await;
                        }
                        CaptureRequest::StopListening => self.stop_listening().await,
                    },
                    event = next_listen_event(&mut self.listener) => match event {
                        Some(event) => self.handle_listen_event(event).await,
                        /* listener terminated */
                        None => return,
                    },
                    _ = self.cancellation_token.cancelled() => return,
                }
            }
        }
    }

    async fn do_capture(&mut self) -> Result<(), InputCaptureError> {
        /* allow cancelling capture request */
        let mut capture = tokio::select! {
            r = InputCapture::new(self.backend) => r?,
            _ = self.cancellation_token.cancelled() => return Ok(()),
        };

        let _capture_guard = DropGuard::new(
            self.event_tx.clone(),
            ICaptureEvent::CaptureEnabled,
            ICaptureEvent::CaptureDisabled,
        );

        /* create barriers for active clients */
        let r = self.create_captures(&mut capture).await;
        if let Err(e) = r {
            capture.terminate().await?;
            return Err(e.into());
        }

        let r = self.do_capture_session(&mut capture).await;

        // FIXME replace with async drop when stabilized
        capture.terminate().await?;

        r
    }

    async fn create_captures(&mut self, capture: &mut InputCapture) -> Result<(), CaptureError> {
        let captures = self.captures.clone();
        for (handle, pos, _type) in captures {
            tokio::select! {
                r = capture.create(handle, pos) => r?,
                _ = self.cancellation_token.cancelled() => return Ok(()),
            }
        }
        Ok(())
    }

    async fn do_capture_session(
        &mut self,
        capture: &mut InputCapture,
    ) -> Result<(), InputCaptureError> {
        loop {
            tokio::select! {
                event = capture.next() => match event {
                    Some(event) => self.handle_capture_event(capture, event?).await?,
                    None => return Ok(()),
                },
                listen_event = next_listen_event(&mut self.listener) => {
                    let Some(listen_event) = listen_event else {
                        /* listener terminated */
                        return Ok(());
                    };
                    match listen_event {
                        // connection acknowlegded => set state to Sending
                        ListenEvent::Msg { event: ProtoEvent::Ack(_), addr } => {
                            if self.active_client.is_some_and(|active| {
                                self.client_manager.get_client_by_active_addr(addr) == Some(active)
                            }) {
                                log::info!("client @ {addr} acknowledged the connection!");
                                self.state = State::Sending;
                            }
                        }
                        // client left its device region
                        ListenEvent::Msg { event: ProtoEvent::Leave(_), addr } => {
                            if self.active_client.is_some_and(|active| {
                                self.client_manager.get_client_by_active_addr(addr) == Some(active)
                            }) {
                                log::info!("releasing capture: left remote client device region");
                                self.release_capture(capture).await?;
                            }
                        }
                        event => self.handle_listen_event(event).await,
                    }
                },
                e = self.request_rx.recv() => match e.expect("channel closed") {
                    CaptureRequest::Reenable => { /* already active */ },
                    CaptureRequest::SetEnabled(true) => { /* already active */ },
                    CaptureRequest::SetEnabled(false) => {
                        self.enabled = false;
                        self.release_capture(capture).await?;
                        return Ok(());
                    },
                    CaptureRequest::Release => self.release_capture(capture).await?,
                    CaptureRequest::Create(h, p, t) => {
                        self.add_capture(h, p, t);
                        capture.create(h, p).await?;
                    }
                    CaptureRequest::Destroy(h) => {
                        self.remove_capture(h);
                        capture.destroy(h).await?;
                    }
                    CaptureRequest::SetReleaseBind(bind) => {
                        self.release_bind.borrow_mut().clone_from(&bind);
                    }
                    CaptureRequest::ChangePort(port) => {
                        self.request_port_change(port).await;
                    }
                    CaptureRequest::StartListening { port, cert, authorized_keys, audio } => {
                        self.start_listening(port, *cert, authorized_keys, audio).await;
                    }
                    CaptureRequest::StopListening => self.stop_listening().await,
                },
                _ = self.cancellation_token.cancelled() => break,
            }
        }
        Ok(())
    }

    async fn handle_capture_event(
        &mut self,
        capture: &mut InputCapture,
        event: (CaptureHandle, CaptureEvent),
    ) -> Result<(), CaptureError> {
        let (handle, event) = event;
        log::trace!("({handle}): {event:?}");

        if capture.keys_pressed(&self.release_bind.borrow()) {
            log::info!("releasing capture: release-bind pressed");
            return self.release_capture(capture).await;
        }

        if event == CaptureEvent::Begin {
            self.event_tx
                .send(ICaptureEvent::CaptureBegin(handle))
                .expect("channel closed");
        }

        // enter only capture (for incoming connections)
        if self.get_type(handle) == CaptureType::EnterOnly {
            // if there is no active outgoing connection at the current capture,
            // we release the capture
            if !self.is_default_capture_at(self.get_pos(handle)) {
                log::info!("releasing capture: no active client at this position");
                capture.release().await?;
            }
            // we dont care about events from incoming handles except for releasing the capture
            return Ok(());
        }

        // activated a new client
        if event == CaptureEvent::Begin && Some(handle) != self.active_client {
            self.state = State::WaitingForAck;
            self.active_client.replace(handle);
            self.event_tx
                .send(ICaptureEvent::ClientEntered(handle))
                .expect("channel closed");
        }

        let opposite_pos = to_proto_pos(self.get_pos(handle).opposite());

        let event = match event {
            CaptureEvent::Begin => ProtoEvent::Enter(opposite_pos),
            CaptureEvent::Input(e) => match self.state {
                // connection not acknowledged, repeat `Enter` event
                State::WaitingForAck => ProtoEvent::Enter(opposite_pos),
                State::Sending => ProtoEvent::Input(e),
            },
        };

        if let Err(e) = self.send_to_client(event, handle).await {
            const DUR: Duration = Duration::from_millis(500);
            debounce!(PREV_LOG, DUR, log::warn!("releasing capture: {e}"));
            capture.release().await?;
        }
        Ok(())
    }

    async fn release_capture(&mut self, capture: &mut InputCapture) -> Result<(), CaptureError> {
        // If we have an active client, notify them we're leaving
        if let Some(handle) = self.active_client.take() {
            // Synthesize key-up events for every key still held in the
            // capture's pressed_keys set BEFORE sending Leave. Without
            // this, pressing the release-bind chord (typically all four
            // modifiers) leaves the peer with phantom held modifiers:
            // the down events were forwarded while capture was active,
            // but the matching up events arrive after the local tap
            // flips to passthrough and never reach the peer. The peer
            // then runs every subsequent keystroke through those held
            // mods until its watchdog times out (1+ s) or our Leave
            // arrives — and Leave can be lost over UDP/DTLS.
            for key in capture.take_pressed_keys() {
                let key_up = ProtoEvent::Input(Event::Keyboard(KeyboardEvent::Key {
                    time: 0,
                    key: key as u32,
                    state: 0,
                }));
                if let Err(e) = self.send_to_client(key_up, handle).await {
                    log::warn!("failed to send key-up to client {handle}: {e}");
                }
            }
            // Reset the modifier mask too. The peer's input-emulation
            // layer keeps a separate XKB-style modifier state that's
            // updated by KeyboardEvent::Modifiers, distinct from the
            // pressed_keys set drained above. Without this, an
            // already-locked CapsLock would survive the release.
            let mods_zero = ProtoEvent::Input(Event::Keyboard(KeyboardEvent::Modifiers {
                depressed: 0,
                latched: 0,
                locked: 0,
                group: 0,
            }));
            if let Err(e) = self.send_to_client(mods_zero, handle).await {
                log::warn!("failed to reset modifiers on client {handle}: {e}");
            }

            log::info!("sending Leave event to client {handle}");
            if let Err(e) = self.send_to_client(ProtoEvent::Leave(0), handle).await {
                log::warn!("failed to send Leave to client {handle}: {e}");
            }
        }
        capture.release().await
    }
}

/// next listener event; pends forever when there is no listener
/// (client mode), keeping the capture task's `select!` arms a single
/// unconditional shape
async fn next_listen_event(listener: &mut Option<DeskunionListener>) -> Option<ListenEvent> {
    match listener {
        Some(listener) => listener.next().await,
        None => std::future::pending().await,
    }
}

thread_local! {
    static PREV_LOG: Cell<Option<Instant>> = const { Cell::new(None) };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    WaitingForAck,
    Sending,
}

fn to_capture_pos(pos: deskunion_ipc::Position) -> input_capture::Position {
    match pos {
        deskunion_ipc::Position::Left => input_capture::Position::Left,
        deskunion_ipc::Position::Right => input_capture::Position::Right,
        deskunion_ipc::Position::Top => input_capture::Position::Top,
        deskunion_ipc::Position::Bottom => input_capture::Position::Bottom,
    }
}

fn to_proto_pos(pos: input_capture::Position) -> deskunion_proto::Position {
    match pos {
        input_capture::Position::Left => deskunion_proto::Position::Left,
        input_capture::Position::Right => deskunion_proto::Position::Right,
        input_capture::Position::Top => deskunion_proto::Position::Top,
        input_capture::Position::Bottom => deskunion_proto::Position::Bottom,
    }
}

struct DropGuard<T> {
    tx: Sender<T>,
    on_drop: Option<T>,
}

impl<T> DropGuard<T> {
    fn new(tx: Sender<T>, on_new: T, on_drop: T) -> Self {
        tx.send(on_new).expect("channel closed");
        let on_drop = Some(on_drop);
        Self { tx, on_drop }
    }
}

impl<T> Drop for DropGuard<T> {
    fn drop(&mut self) {
        self.tx
            .send(self.on_drop.take().expect("item"))
            .expect("channel closed");
    }
}
