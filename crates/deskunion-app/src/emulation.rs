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
    /// result of a dial-out test requested by the frontend
    ConnectionTested {
        request_id: u64,
        error: Option<String>,
    },
    /// emulation was disabled
    EmulationDisabled,
    /// emulation was enabled
    EmulationEnabled,
    /// capture should be released
    ReleaseNotify,
}

enum EmulationRequest {
    Reenable,
    SetEnabled(bool),
    Release(SocketAddr),
    /// set (or clear) the server endpoint to dial
    SetServer(Option<ServerTarget>),
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
    ) -> Self {
        let emulation_proxy = EmulationProxy::new(backend, enabled);
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_task = DialTask {
            conn,
            emulation_proxy,
            request_rx,
            event_tx,
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
}

impl DialTask {
    async fn run(mut self) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
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
                                    self.event_tx.send(EmulationEvent::ReleaseNotify).expect("channel closed");
                                    self.conn.reply(addr, ProtoEvent::Ack(0)).await;
                                    self.event_tx.send(EmulationEvent::Entered{addr, pos: to_ipc_pos(pos), fingerprint}).expect("channel closed");
                                }
                            }
                            ProtoEvent::Leave(_) => {
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
                    ConnectionEvent::Connected { addr, fingerprint } => {
                        last_response.insert(addr, Instant::now());
                        self.event_tx.send(EmulationEvent::Connected { addr, fingerprint }).expect("channel closed");
                    }
                    ConnectionEvent::Disconnected { addr } => {
                        last_response.remove(&addr);
                        self.emulation_proxy.remove(addr);
                        self.event_tx.send(EmulationEvent::Disconnected { addr }).expect("channel closed");
                    }
                }}
                event = self.emulation_proxy.event() => {
                    self.event_tx.send(event).expect("channel closed");
                }
                request = self.request_rx.recv() => match request.expect("channel closed") {
                    // reenable emulation
                    EmulationRequest::Reenable => self.emulation_proxy.reenable(),
                    EmulationRequest::SetEnabled(enabled) => self.emulation_proxy.set_enabled(enabled),
                    // notify the other end that we hit a barrier (should release capture)
                    EmulationRequest::Release(addr) => self.conn.reply(addr, ProtoEvent::Leave(0)).await,
                    EmulationRequest::SetServer(target) => self.conn.set_server(target).await,
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
    Remove(SocketAddr),
    Terminate,
    Reenable,
    SetEnabled(bool),
}

impl EmulationProxy {
    fn new(backend: Option<input_emulation::Backend>, enabled: bool) -> Self {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_active = Rc::new(Cell::new(false));
        let exit_requested = Rc::new(Cell::new(false));
        let emulation_task = EmulationTask {
            backend,
            enabled,
            exit_requested: exit_requested.clone(),
            request_rx,
            event_tx,
            handles: Default::default(),
            next_id: 0,
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
        if let EmulationEvent::EmulationEnabled = event {
            self.emulation_active.replace(true);
        }
        if let EmulationEvent::EmulationDisabled = event {
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
    exit_requested: Rc<Cell<bool>>,
    request_rx: Receiver<ProxyRequest>,
    event_tx: Sender<EmulationEvent>,
    handles: HashMap<SocketAddr, EmulationHandle>,
    next_id: EmulationHandle,
}

impl EmulationTask {
    async fn run(mut self) {
        loop {
            if self.enabled {
                if let Err(e) = self.do_emulation().await {
                    log::warn!("input emulation exited: {e}");
                }
                self.enabled = false;
            }
            if self.exit_requested.get() {
                break;
            }
            // wait for reenable request
            loop {
                match self.request_rx.recv().await.expect("channel closed") {
                    ProxyRequest::Reenable | ProxyRequest::SetEnabled(true) => {
                        self.enabled = true;
                        break;
                    }
                    ProxyRequest::SetEnabled(false) => {}
                    ProxyRequest::Terminate => return,
                    ProxyRequest::Input(..) => { /* emulation inactive => ignore */ }
                    ProxyRequest::Remove(..) => { /* emulation inactive => ignore */ }
                }
            }
        }
    }

    async fn do_emulation(&mut self) -> Result<(), InputEmulationError> {
        log::info!("creating input emulation ...");
        let mut emulation = tokio::select! {
            r = InputEmulation::new(self.backend) => r?,
            // allow termination event while requesting input emulation
            _ = wait_for_termination(&mut self.request_rx) => return Ok(()),
        };

        // used to send enabled and disabled events
        let _emulation_guard = DropGuard::new(
            self.event_tx.clone(),
            EmulationEvent::EmulationEnabled,
            EmulationEvent::EmulationDisabled,
        );

        // create active handles
        if let Err(e) = self.create_clients(&mut emulation).await {
            emulation.terminate().await;
            return Err(e);
        }

        let res = self.do_emulation_session(&mut emulation).await;
        // FIXME replace with async drop when stabilized
        emulation.terminate().await;
        res
    }

    async fn create_clients(
        &mut self,
        emulation: &mut InputEmulation,
    ) -> Result<(), InputEmulationError> {
        for handle in self.handles.values() {
            tokio::select! {
                _ = emulation.create(*handle) => {},
                _ = wait_for_termination(&mut self.request_rx) => return Ok(()),
            }
        }
        Ok(())
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

async fn wait_for_termination(rx: &mut Receiver<ProxyRequest>) {
    loop {
        match rx.recv().await.expect("channel closed") {
            ProxyRequest::Terminate => return,
            ProxyRequest::Input(_, _) => continue,
            ProxyRequest::Remove(_) => continue,
            ProxyRequest::Reenable => continue,
            ProxyRequest::SetEnabled(_) => continue,
        }
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
