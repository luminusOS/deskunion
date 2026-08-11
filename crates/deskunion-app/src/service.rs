use crate::{
    capture::{Capture, CaptureType, ICaptureEvent},
    client::ClientManager,
    config::{Config, ConfigClient, ServerEndpoint},
    connect::{DeskunionConnection, ServerTarget},
    crypto,
    dns::{DnsEvent, DnsResolver},
    emulation::{Emulation, EmulationEvent},
    listen::{DeskunionListener, ListenerCreationError},
};
use deskunion_ipc::{
    AsyncFrontendListener, ClientConfig, ClientHandle, FrontendEvent, FrontendRequest, IpcError,
    IpcListenerCreationError, OperationMode, Position, Status,
};
use futures::StreamExt;
use log;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    net::{IpAddr, SocketAddr},
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::{process::Command, signal, sync::Notify};

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    IpcListen(#[from] IpcListenerCreationError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    ListenError(#[from] ListenerCreationError),
    #[error("failed to load certificate: `{0}`")]
    Certificate(#[from] crypto::Error),
}

pub struct Service {
    /// configuration
    config: Config,
    /// whether this machine controls peers or accepts remote control
    operation_mode: OperationMode,
    /// the (D)TLS certificate, kept to (re)create the listener when
    /// entering server mode at runtime
    cert: webrtc_dtls::crypto::Certificate,
    /// input capture
    capture: Capture,
    /// input emulation
    emulation: Emulation,
    /// dns resolver
    resolver: DnsResolver,
    /// frontend listener
    frontend_listener: AsyncFrontendListener,
    /// authorized public key sha256 fingerprints
    authorized_keys: Arc<RwLock<HashMap<String, String>>>,
    /// (outgoing) client information
    client_manager: ClientManager,
    /// current port
    port: u16,
    /// the public key fingerprint for (D)TLS
    public_key_fingerprint: String,
    /// notify for pending frontend events
    frontend_event_pending: Notify,
    /// frontend events queued for sending
    pending_frontend_events: VecDeque<FrontendEvent>,
    /// status of input capture (enabled / disabled)
    capture_status: Status,
    /// status of input emulation (enabled / disabled)
    emulation_status: Status,
    /// keep track of registered connections to avoid duplicate barriers
    incoming_conns: HashSet<SocketAddr>,
    /// map from capture handle to connection info
    incoming_conn_info: HashMap<ClientHandle, Incoming>,
    /// resolved IPs of the configured server endpoint (client mode)
    server_dns_ips: Vec<IpAddr>,
    next_trigger_handle: u64,
}

#[derive(Debug)]
struct Incoming {
    fingerprint: String,
    addr: SocketAddr,
    pos: Position,
}

impl Service {
    pub async fn new(config: Config) -> Result<Self, ServiceError> {
        let client_manager = ClientManager::default();
        for client in config.clients() {
            client_manager.add_with_config(client);
        }

        // load certificate
        let cert = crypto::load_or_generate_key_and_cert(config.cert_path())?;
        let public_key_fingerprint = crypto::certificate_fingerprint(&cert);

        // create frontend communication adapter, exit if already running
        let frontend_listener = AsyncFrontendListener::new().await?;

        let authorized_keys = Arc::new(RwLock::new(config.authorized_fingerprints()));
        // The connection direction is inverted relative to historical
        // lan-mouse: the capture side (server) listens and authorizes
        // incoming devices; the emulation side (client) dials out to the
        // configured server. Only the server role accepts connections,
        // so the DTLS listener is created upfront only in server mode —
        // in client mode nothing uses it, and an always-on listening
        // socket triggered a Windows firewall prompt for no reason.
        // Entering server mode at runtime creates it lazily
        // (`apply_operation_mode`).
        let operation_mode = config.operation_mode();
        let listener = match operation_mode {
            OperationMode::Server => Some(
                DeskunionListener::new(
                    config.port(),
                    cert.clone(),
                    authorized_keys.clone(),
                    client_manager.clone(),
                    config.audio_settings(),
                )
                .await?,
            ),
            OperationMode::Client | OperationMode::Unconfigured => None,
        };
        let conn = DeskunionConnection::new(cert.clone(), config.audio_settings());

        // Input permissions are requested lazily according to the selected
        // role. Historically both tasks started here, which caused two OS
        // permission prompts before the user had chosen how to use DeskUnion.
        let capture_backend = config.capture_backend().map(|b| b.into());
        let capture = Capture::new(
            capture_backend,
            listener,
            client_manager.clone(),
            config.release_bind(),
            operation_mode == OperationMode::Server,
        );
        let emulation_backend = config.emulation_backend().map(|b| b.into());
        let emulation = Emulation::new(
            emulation_backend,
            conn,
            operation_mode == OperationMode::Client,
        );

        // create dns resolver
        let resolver = DnsResolver::new()?;

        let port = config.port();
        let service = Self {
            config,
            operation_mode,
            cert,
            capture,
            emulation,
            frontend_listener,
            resolver,
            authorized_keys,
            public_key_fingerprint,
            client_manager,
            frontend_event_pending: Default::default(),
            port,
            pending_frontend_events: Default::default(),
            capture_status: Default::default(),
            emulation_status: Default::default(),
            incoming_conn_info: Default::default(),
            incoming_conns: Default::default(),
            server_dns_ips: Default::default(),
            next_trigger_handle: 0,
        };
        Ok(service)
    }

    pub async fn run(&mut self) -> Result<(), ServiceError> {
        let active = self.client_manager.active_clients();
        for handle in active.iter() {
            // small hack: `activate_client()` checks, if the client
            // is already active in client_manager and does not create a
            // capture barrier in that case so we have to deactivate it first
            self.client_manager.deactivate_client(*handle);
        }

        for handle in active {
            self.activate_client(handle);
        }

        // a client-mode daemon reconnects to its configured server
        // without waiting for a frontend
        if self.operation_mode == OperationMode::Client {
            self.resolve_server();
        }

        loop {
            tokio::select! {
                request = self.frontend_listener.next() => self.handle_frontend_request(request),
                _ = self.frontend_event_pending.notified() => self.handle_frontend_pending().await,
                event = self.emulation.event() => self.handle_emulation_event(event),
                event = self.capture.event() => self.handle_capture_event(event),
                event = self.resolver.event() => self.handle_resolver_event(event),
                _ = self.config.changed() => self.handle_config_change(),
                r = signal::ctrl_c() => break r.expect("failed to wait for CTRL+C"),
            }
        }

        log::info!("terminating service ...");
        log::debug!("terminating capture ...");
        self.capture.terminate().await;
        log::debug!("terminating emulation ...");
        self.emulation.terminate().await;
        log::debug!("terminating dns resolver ...");
        self.resolver.terminate().await;

        Ok(())
    }

    fn handle_frontend_request(&mut self, request: Option<Result<FrontendRequest, IpcError>>) {
        let request = match request.expect("frontend listener closed") {
            Ok(r) => r,
            Err(e) => return log::error!("error receiving request: {e}"),
        };
        match request {
            FrontendRequest::Activate(handle, active) => {
                self.set_client_active(handle, active);
                self.save_config();
            }
            FrontendRequest::AuthorizeKey(desc, fp) => {
                self.add_authorized_key(desc, fp);
                self.save_config();
            }
            FrontendRequest::ChangePort(port) => self.change_port(port),
            FrontendRequest::Create => {
                self.add_client();
                self.save_config();
            }
            FrontendRequest::CreateConfigured { config, active } => {
                self.add_configured_client(config, active);
                self.save_config();
            }
            FrontendRequest::TestConnection {
                request_id,
                hostname,
                port,
            } => self.emulation.test_connection(request_id, hostname, port),
            FrontendRequest::Delete(handle) => {
                self.remove_client(handle);
                self.save_config();
            }
            FrontendRequest::EnableCapture => {
                if self.operation_mode == OperationMode::Server
                    || !self.incoming_conn_info.is_empty()
                {
                    self.capture.reenable();
                }
            }
            FrontendRequest::EnableEmulation => {
                if self.operation_mode == OperationMode::Client {
                    self.emulation.reenable();
                }
            }
            FrontendRequest::SetServiceRunning(running) => self.set_service_running(running),
            FrontendRequest::SetOperationMode(mode) => self.set_operation_mode(mode),
            FrontendRequest::Enumerate() => self.enumerate(),
            FrontendRequest::UpdateFixIps(handle, fix_ips) => {
                self.update_fix_ips(handle, fix_ips);
                self.save_config();
            }
            FrontendRequest::UpdateHostname(handle, host) => {
                self.update_hostname(handle, host);
                self.save_config();
            }
            FrontendRequest::UpdatePort(handle, port) => {
                self.update_port(handle, port);
                self.save_config();
            }
            FrontendRequest::UpdatePosition(handle, pos) => {
                self.update_pos(handle, pos);
                self.save_config();
            }
            FrontendRequest::AssignPosition { fingerprint, pos } => {
                self.assign_position(fingerprint, pos);
                self.save_config();
            }
            FrontendRequest::SetServer {
                hostname,
                ips,
                port,
            } => {
                self.set_server_endpoint(hostname, ips, port);
                self.save_config();
            }
            FrontendRequest::ResolveDns(handle) => self.resolve(handle),
            FrontendRequest::Sync => self.sync_frontend(),
            FrontendRequest::RemoveAuthorizedKey(key) => {
                self.remove_authorized_key(key);
                self.save_config();
            }
            FrontendRequest::UpdateEnterHook(handle, enter_hook) => {
                self.update_enter_hook(handle, enter_hook)
            }
            FrontendRequest::SaveConfiguration => self.save_config(),
            FrontendRequest::SetAudioSend(enabled) => {
                let mut audio = self.config.audio_settings();
                audio.send = enabled;
                self.config.set_audio_settings(audio);
                self.save_config();
                self.notify_audio_status();
            }
            FrontendRequest::SetAudioReceive(enabled) => {
                let mut audio = self.config.audio_settings();
                audio.receive = enabled;
                self.config.set_audio_settings(audio);
                self.save_config();
                self.notify_audio_status();
            }
            FrontendRequest::UpdateAudioSettings { bitrate, buffer_ms } => {
                let mut audio = self.config.audio_settings();
                audio.bitrate = bitrate;
                audio.buffer_ms = buffer_ms;
                self.config.set_audio_settings(audio);
                self.save_config();
                self.notify_audio_status();
            }
            FrontendRequest::SetAudioCaptureDevice(device) => {
                let mut audio = self.config.audio_settings();
                audio.capture_device = device;
                self.config.set_audio_settings(audio);
                self.save_config();
                self.notify_audio_status();
            }
            FrontendRequest::SetAudioPlaybackDevice(device) => {
                let mut audio = self.config.audio_settings();
                audio.playback_device = device;
                self.config.set_audio_settings(audio);
                self.save_config();
                self.notify_audio_status();
            }
            FrontendRequest::EnumerateAudioDevices => self.enumerate_audio_devices(),
        }
    }

    /// broadcast current audio settings. Note: these settings are read
    /// fresh by each new connection (`listen.rs`/`connect.rs`, Fase 4)
    /// but not yet live-pushed into already-running `AudioSender`/
    /// `AudioReceiver` instances — same "takes effect on
    /// reconnect/restart" behavior as `capture_backend`/
    /// `emulation_backend`. Live-reload is a follow-up, not a
    /// regression from any prior working behavior.
    fn notify_audio_status(&mut self) {
        let audio = self.config.audio_settings();
        self.notify_frontend(FrontendEvent::AudioStatus {
            send: audio.send,
            receive: audio.receive,
            bitrate: audio.bitrate,
            buffer_ms: audio.buffer_ms,
            // cpal covers loopback capture natively on Linux (PipeWire
            // host) and macOS (CoreAudio, on OS versions new enough to
            // support it) — see the audio plan's §3.5 spike. We don't
            // currently detect the macOS-version cutoff at runtime, so
            // this is optimistic there; not load-bearing beyond an
            // advisory UI banner.
            loopback_supported: true,
        });
    }

    #[cfg(feature = "audio")]
    fn enumerate_audio_devices(&mut self) {
        let (capture, playback) = crate::audio::enumerate_devices();
        self.notify_frontend(FrontendEvent::AudioDevices { capture, playback });
    }

    #[cfg(not(feature = "audio"))]
    fn enumerate_audio_devices(&mut self) {
        self.notify_frontend(FrontendEvent::AudioDevices {
            capture: Vec::new(),
            playback: Vec::new(),
        });
    }

    fn save_config(&mut self) {
        let clients = self.client_manager.clients();
        let clients = clients
            .into_iter()
            .map(|(c, s)| ConfigClient {
                ips: HashSet::from_iter(c.fix_ips),
                hostname: c.hostname,
                port: c.port,
                pos: c.pos,
                active: s.active,
                enter_hook: c.cmd,
                fingerprint: c.fingerprint,
            })
            .collect();
        self.config.set_clients(clients);
        let authorized_keys = self.authorized_keys.read().expect("lock").clone();
        self.config.set_authorized_keys(authorized_keys);
        if let Err(e) = self.config.write_back() {
            log::warn!("failed to write config: {e}");
        }
    }

    fn handle_config_change(&mut self) {
        let configured_mode = self.config.operation_mode();
        if configured_mode != self.operation_mode {
            self.apply_operation_mode(configured_mode);
            self.notify_frontend(FrontendEvent::OperationMode(configured_mode));
        }
        for h in self.client_manager.registered_clients() {
            self.remove_client(h);
        }
        for c in self.config.clients() {
            let handle = self.client_manager.add_with_config(c);
            log::info!("added client {handle}");
            let (c, s) = self.client_manager.get_state(handle).unwrap();
            if s.active {
                self.client_manager.deactivate_client(handle);
                self.activate_client(handle);
            }
            self.notify_frontend(FrontendEvent::Created(handle, c, s));
        }
        let release_bind = self.config.release_bind();
        self.capture.set_release_bind(release_bind);
        let authorized_keys = self.config.authorized_fingerprints();
        self.authorized_keys
            .write()
            .unwrap()
            .clone_from(&authorized_keys);
        if self.operation_mode == OperationMode::Client {
            self.server_dns_ips.clear();
            self.resolve_server();
            self.push_server_target();
        }
        self.sync_frontend();
    }

    async fn handle_frontend_pending(&mut self) {
        while let Some(event) = self.pending_frontend_events.pop_front() {
            self.frontend_listener.broadcast(event).await;
        }
    }

    fn handle_emulation_event(&mut self, event: EmulationEvent) {
        match event {
            EmulationEvent::Entered {
                addr,
                pos,
                fingerprint,
            } => {
                if self.operation_mode != OperationMode::Client {
                    return;
                }
                // check if already registered
                if !self.incoming_conns.contains(&addr) {
                    self.add_incoming(addr, pos, fingerprint.clone());
                    self.notify_frontend(FrontendEvent::DeviceEntered {
                        fingerprint,
                        addr,
                        pos,
                    });
                } else {
                    self.update_incoming(addr, pos, fingerprint);
                }
            }
            EmulationEvent::Disconnected { addr } => {
                // `remove_incoming` only knows devices whose pointer
                // actually entered (`add_incoming` runs from `Entered`),
                // but the frontend's "Connected to <server>" comes from
                // `DeviceConnected`, which fires on the dial alone. Gating
                // the notification on the removal left a client that never
                // got entered showing a live connection after the session
                // dropped. Both events track the same dial, so both are
                // reported unconditionally.
                self.remove_incoming(addr);
                self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
            }
            EmulationEvent::ConnectionTested { request_id, error } => {
                self.notify_frontend(FrontendEvent::ConnectionTested { request_id, error });
            }
            EmulationEvent::EmulationDisabled => {
                self.emulation_status = Status::Disabled;
                self.emulation.set_server(None);
                self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
            }
            EmulationEvent::EmulationEnabled => {
                self.emulation_status = Status::Enabled;
                self.push_server_target();
                self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
            }
            EmulationEvent::ReleaseNotify => self.capture.release(),
            EmulationEvent::Connected { addr, fingerprint } => {
                if self.operation_mode == OperationMode::Client {
                    self.notify_frontend(FrontendEvent::DeviceConnected { addr, fingerprint });
                }
            }
        }
    }

    fn handle_capture_event(&mut self, event: ICaptureEvent) {
        match event {
            ICaptureEvent::CaptureBegin(handle) => {
                // we entered the capture zone for an incoming connection
                // => notify it that its capture should be released
                if let Some(incoming) = self.incoming_conn_info.get(&handle) {
                    self.emulation.send_leave_event(incoming.addr);
                }
            }
            ICaptureEvent::CaptureDisabled => {
                self.capture_status = Status::Disabled;
                self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
            }
            ICaptureEvent::CaptureEnabled => {
                self.capture_status = Status::Enabled;
                self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
            }
            ICaptureEvent::ClientEntered(handle) => {
                log::info!("entering client {handle} ...");
                self.spawn_hook_command(handle);
            }
            ICaptureEvent::ConnectionAttempt { fingerprint } => {
                // the listener authorizes incoming devices — with the
                // inverted connection direction that is the server role
                if self.operation_mode == OperationMode::Server {
                    self.notify_frontend(FrontendEvent::ConnectionAttempt { fingerprint });
                }
            }
            ICaptureEvent::DeviceConnected { addr, fingerprint } => {
                // an authorized but unpaired device parked its
                // connection; pair it right away on the first free
                // screen edge (right first) instead of asking — only
                // fall back to the manual prompt when every edge is
                // already taken
                if self.operation_mode == OperationMode::Server {
                    match self.client_manager.first_free_position() {
                        Some(pos) => {
                            log::info!(
                                "auto-pairing device {fingerprint} from {addr} at the {pos} screen edge"
                            );
                            self.assign_position(fingerprint, pos);
                            self.save_config();
                        }
                        None => {
                            log::info!(
                                "every screen edge is taken: device {fingerprint} from {addr} is waiting for a position assignment"
                            );
                            self.notify_frontend(FrontendEvent::DeviceConnected {
                                addr,
                                fingerprint,
                            });
                        }
                    }
                }
            }
            ICaptureEvent::DeviceDisconnected { addr } => {
                if self.operation_mode == OperationMode::Server {
                    self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
                }
            }
            ICaptureEvent::PortChanged(port) => match port {
                Ok(port) => {
                    self.port = port;
                    self.notify_frontend(FrontendEvent::PortChanged(port, None));
                }
                Err(e) => self
                    .notify_frontend(FrontendEvent::PortChanged(self.port, Some(format!("{e}")))),
            },
            ICaptureEvent::ClientStateChanged(handle) => {
                self.broadcast_client(handle);
            }
            ICaptureEvent::AudioStream {
                addr,
                active,
                latency_ms,
                packets_lost,
                level,
            } => {
                self.notify_frontend(FrontendEvent::AudioStream {
                    addr,
                    active,
                    latency_ms,
                    packets_lost,
                    level,
                });
            }
        }
    }

    fn handle_resolver_event(&mut self, event: DnsEvent) {
        const SERVER_HANDLE: ClientHandle = u64::MAX;
        let handle = match event {
            DnsEvent::Resolving(SERVER_HANDLE) => return,
            DnsEvent::Resolving(handle) => {
                self.client_manager.set_resolving(handle, true);
                handle
            }
            DnsEvent::Resolved(SERVER_HANDLE, hostname, ips) => {
                if let Err(e) = &ips {
                    log::warn!("could not resolve server {hostname}: {e}");
                }
                self.server_dns_ips = ips.unwrap_or_default();
                self.push_server_target();
                return;
            }
            DnsEvent::Resolved(handle, hostname, ips) => {
                self.client_manager.set_resolving(handle, false);
                if let Err(e) = &ips {
                    log::warn!("could not resolve {hostname}: {e}");
                }
                let ips = ips.unwrap_or_default();
                self.client_manager.set_dns_ips(handle, ips);
                handle
            }
        };
        self.broadcast_client(handle);
    }

    fn resolve(&self, handle: ClientHandle) {
        if let Some(hostname) = self.client_manager.get_hostname(handle) {
            self.resolver.resolve(handle, hostname);
        }
    }

    /// resolve the configured server hostname (client mode). Uses a
    /// dedicated resolver handle that can never collide with a client.
    fn resolve_server(&self) {
        if let Some(hostname) = self.config.server_endpoint().hostname {
            self.resolver.resolve(u64::MAX, hostname);
        }
    }

    /// (re)dial the configured server endpoint when this device is an
    /// enabled client. The target persists in the connection and its
    /// reconnect loop redials after drops, so this only needs to run
    /// when the endpoint or the emulation state changes.
    fn push_server_target(&mut self) {
        if self.operation_mode != OperationMode::Client || self.emulation_status != Status::Enabled
        {
            return;
        }
        let endpoint = self.config.server_endpoint();
        let mut ips = endpoint.ips.clone();
        ips.extend(self.server_dns_ips.iter().copied());
        ips.sort();
        ips.dedup();
        if ips.is_empty() {
            // no usable address yet — a pending DNS resolution pushes
            // the target again once it completes
            return;
        }
        self.emulation.set_server(Some(ServerTarget {
            ips,
            port: endpoint.port,
        }));
    }

    fn set_server_endpoint(&mut self, hostname: Option<String>, ips: Vec<IpAddr>, port: u16) {
        self.config.set_server_endpoint(ServerEndpoint {
            hostname,
            ips,
            port,
        });
        // stale resolutions belong to the previous hostname
        self.server_dns_ips.clear();
        self.resolve_server();
        self.push_server_target();
        let endpoint = self.config.server_endpoint();
        self.notify_frontend(FrontendEvent::ServerEndpoint {
            hostname: endpoint.hostname,
            ips: endpoint.ips,
            port: endpoint.port,
        });
    }

    /// pair a connected (authorized but unpaired) device: bind its
    /// fingerprint to a client entry at the given position, creating
    /// the entry if needed, and adopt its parked connection as the
    /// client's active address so it becomes enterable.
    fn assign_position(&mut self, fingerprint: String, pos: Position) {
        let handle = match self.client_manager.get_client_by_fingerprint(&fingerprint) {
            Some(handle) => {
                self.update_pos(handle, pos);
                handle
            }
            None => {
                let handle = self.client_manager.add_client();
                self.client_manager.set_config(
                    handle,
                    ClientConfig {
                        fingerprint: Some(fingerprint.clone()),
                        pos,
                        ..Default::default()
                    },
                );
                let (config, state) = self.client_manager.get_state(handle).unwrap();
                self.notify_frontend(FrontendEvent::Created(handle, config, state));
                handle
            }
        };
        if let Some(addr) = self.client_manager.unpark(&fingerprint) {
            self.client_manager.set_active_addr(handle, Some(addr));
        }
        self.activate_client(handle);
        self.broadcast_client(handle);
    }

    fn sync_frontend(&mut self) {
        self.enumerate();
        self.notify_frontend(FrontendEvent::OperationMode(self.operation_mode));
        self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
        self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
        self.notify_frontend(FrontendEvent::PortChanged(self.port, None));
        self.notify_frontend(FrontendEvent::PublicKeyFingerprint(
            self.public_key_fingerprint.clone(),
        ));
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
        let endpoint = self.config.server_endpoint();
        self.notify_frontend(FrontendEvent::ServerEndpoint {
            hostname: endpoint.hostname,
            ips: endpoint.ips,
            port: endpoint.port,
        });
        self.notify_audio_status();
    }

    const ENTER_HANDLE_BEGIN: u64 = u64::MAX / 2 + 1;

    fn add_incoming(&mut self, addr: SocketAddr, pos: Position, fingerprint: String) {
        if self.operation_mode != OperationMode::Client {
            return;
        }
        // A client only needs capture after a remote pointer actually enters:
        // this small enter-only barrier detects the handoff back to the server.
        // Delaying it avoids a capture permission prompt merely for opening the app.
        self.capture.set_enabled(true);
        let handle = Self::ENTER_HANDLE_BEGIN + self.next_trigger_handle;
        self.next_trigger_handle += 1;
        self.capture.create(handle, pos, CaptureType::EnterOnly);
        self.incoming_conns.insert(addr);
        self.incoming_conn_info.insert(
            handle,
            Incoming {
                fingerprint,
                addr,
                pos,
            },
        );
    }

    fn update_incoming(&mut self, addr: SocketAddr, pos: Position, fingerprint: String) {
        let incoming = self
            .incoming_conn_info
            .iter_mut()
            .find(|(_, i)| i.addr == addr)
            .map(|(_, i)| i)
            .expect("no such client");
        let mut changed = false;
        if incoming.fingerprint != fingerprint {
            incoming.fingerprint = fingerprint.clone();
            changed = true;
        }
        if incoming.pos != pos {
            incoming.pos = pos;
            changed = true;
        }
        if changed {
            self.remove_incoming(addr);
            self.add_incoming(addr, pos, fingerprint.clone());
            self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
            self.notify_frontend(FrontendEvent::DeviceEntered {
                fingerprint,
                addr,
                pos,
            });
        }
    }

    fn remove_incoming(&mut self, addr: SocketAddr) -> Option<SocketAddr> {
        let handle = self
            .incoming_conn_info
            .iter()
            .find(|(_, incoming)| incoming.addr == addr)
            .map(|(k, _)| *k)?;
        self.capture.destroy(handle);
        self.incoming_conns.remove(&addr);
        let removed = self
            .incoming_conn_info
            .remove(&handle)
            .map(|incoming| incoming.addr);
        if self.operation_mode == OperationMode::Client && self.incoming_conn_info.is_empty() {
            self.capture.set_enabled(false);
        }
        removed
    }

    fn set_operation_mode(&mut self, mode: OperationMode) {
        if self.operation_mode == mode {
            self.notify_frontend(FrontendEvent::OperationMode(mode));
            return;
        }

        self.apply_operation_mode(mode);
        self.config.set_operation_mode(mode);
        self.save_config();
        self.notify_frontend(FrontendEvent::OperationMode(mode));
    }

    /// stop/start the active role's input pipeline without touching the
    /// configured operation mode — a later `SetServiceRunning(true)` (or
    /// mode re-select) restores exactly what the mode requires.
    fn set_service_running(&mut self, running: bool) {
        match self.operation_mode {
            OperationMode::Unconfigured => {}
            OperationMode::Server => {
                if running {
                    self.capture.reenable();
                } else {
                    self.capture.set_enabled(false);
                }
            }
            OperationMode::Client => {
                if running {
                    self.emulation.reenable();
                    self.push_server_target();
                    if !self.incoming_conn_info.is_empty() {
                        self.capture.reenable();
                    }
                } else {
                    self.emulation.set_enabled(false);
                    self.emulation.set_server(None);
                    self.capture.set_enabled(false);
                }
            }
        }
    }

    /// Switching modes only tears down the other role's pipeline — it
    /// never *starts* the new one. Starting is an explicit user action:
    /// `SetServiceRunning(true)` (the frontend's Start button). The one
    /// exception is service startup, where `Capture::new`/`Emulation::new`
    /// get `enabled` from the persisted mode so headless daemons keep
    /// working without a frontend to press Start.
    fn apply_operation_mode(&mut self, mode: OperationMode) {
        self.operation_mode = mode;
        match mode {
            OperationMode::Unconfigured => {
                self.capture.stop_listening();
                self.emulation.set_server(None);
                self.capture.set_enabled(false);
                self.emulation.set_enabled(false);
            }
            OperationMode::Server => {
                let incoming_addrs = self
                    .incoming_conn_info
                    .values()
                    .map(|incoming| incoming.addr)
                    .collect::<Vec<_>>();
                for addr in incoming_addrs {
                    self.remove_incoming(addr);
                    self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
                }
                self.emulation.set_server(None);
                self.emulation.set_enabled(false);
                // the server role accepts incoming devices: make sure
                // the listener exists (it is only created upfront when
                // starting in server mode)
                self.capture.start_listening(
                    self.port,
                    self.cert.clone(),
                    self.authorized_keys.clone(),
                    self.config.audio_settings(),
                );
                for handle in self.client_manager.active_clients() {
                    if let Some(pos) = self.client_manager.get_pos(handle) {
                        self.capture.create(handle, pos, CaptureType::Default);
                    }
                }
            }
            OperationMode::Client => {
                // a client accepts no connections: close the listener
                // so no UDP port stays open
                self.capture.stop_listening();
                for handle in self.client_manager.active_clients() {
                    self.capture.destroy(handle);
                }
                self.capture.set_enabled(false);
                // warm the server address; the actual dial happens when
                // emulation is (re)enabled
                self.resolve_server();
            }
        }
    }

    fn notify_frontend(&mut self, event: FrontendEvent) {
        self.pending_frontend_events.push_back(event);
        self.frontend_event_pending.notify_one();
    }

    fn add_authorized_key(&mut self, desc: String, fp: String) {
        self.authorized_keys.write().expect("lock").insert(fp, desc);
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
    }

    fn remove_authorized_key(&mut self, fp: String) {
        self.authorized_keys.write().expect("lock").remove(&fp);
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
    }

    fn enumerate(&mut self) {
        let clients = self.client_manager.get_client_states();
        self.notify_frontend(FrontendEvent::Enumerate(clients));
    }

    fn add_client(&mut self) {
        let handle = self.client_manager.add_client();
        log::info!("added client {handle}");
        let (c, s) = self.client_manager.get_state(handle).unwrap();
        self.notify_frontend(FrontendEvent::Created(handle, c, s));
    }

    fn add_configured_client(&mut self, config: deskunion_ipc::ClientConfig, active: bool) {
        let handle = self.client_manager.add_client();
        self.client_manager.set_config(handle, config);
        log::info!("added configured client {handle}");
        if active {
            self.activate_client(handle);
        }
        let (config, state) = self.client_manager.get_state(handle).unwrap();
        self.notify_frontend(FrontendEvent::Created(handle, config, state));
    }

    fn set_client_active(&mut self, handle: ClientHandle, active: bool) {
        if active {
            self.activate_client(handle);
        } else {
            self.deactivate_client(handle);
        }
    }

    fn deactivate_client(&mut self, handle: ClientHandle) {
        log::debug!("deactivating client {handle}");
        if self.client_manager.deactivate_client(handle) {
            self.capture.destroy(handle);
            self.broadcast_client(handle);
            log::info!("deactivated client {handle}");
        }
    }

    fn activate_client(&mut self, handle: ClientHandle) {
        log::debug!("activating client {handle}");

        /* resolve dns on activate */
        self.resolve(handle);

        /* deactivate potential other client at this position */
        let Some(pos) = self.client_manager.get_pos(handle) else {
            return;
        };

        if let Some(other) = self.client_manager.client_at(pos) {
            if other != handle {
                self.deactivate_client(other);
            }
        }

        /* activate the client */
        if self.client_manager.activate_client(handle) {
            /* notify capture and frontends */
            if self.operation_mode == OperationMode::Server {
                self.capture.create(handle, pos, CaptureType::Default);
            }
            self.broadcast_client(handle);
            log::info!("activated client {handle} ({pos})");
        }
    }

    fn change_port(&mut self, port: u16) {
        if self.port != port {
            self.capture.request_port_change(port);
        } else {
            self.notify_frontend(FrontendEvent::PortChanged(self.port, None));
        }
    }

    fn remove_client(&mut self, handle: ClientHandle) {
        if self
            .client_manager
            .remove_client(handle)
            .map(|(_, s)| s.active)
            .unwrap_or(false)
        {
            self.capture.destroy(handle);
        }
        self.notify_frontend(FrontendEvent::Deleted(handle));
    }

    fn update_fix_ips(&mut self, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
        self.client_manager.set_fix_ips(handle, fix_ips);
        self.broadcast_client(handle);
    }

    fn update_hostname(&mut self, handle: ClientHandle, hostname: Option<String>) {
        log::info!("hostname changed: {hostname:?}");
        if self.client_manager.set_hostname(handle, hostname.clone()) {
            self.resolve(handle);
        }
        self.broadcast_client(handle);
    }

    fn update_port(&mut self, handle: ClientHandle, port: u16) {
        self.client_manager.set_port(handle, port);
        self.broadcast_client(handle);
    }

    fn update_pos(&mut self, handle: ClientHandle, pos: Position) {
        // update state in event input emulator & input capture
        if self.client_manager.set_pos(handle, pos) {
            self.deactivate_client(handle);
            self.activate_client(handle);
        }
        self.broadcast_client(handle);
    }

    fn update_enter_hook(&mut self, handle: ClientHandle, enter_hook: Option<String>) {
        self.client_manager.set_enter_hook(handle, enter_hook);
        self.broadcast_client(handle);
    }

    fn broadcast_client(&mut self, handle: ClientHandle) {
        let event = self
            .client_manager
            .get_state(handle)
            .map(|(c, s)| FrontendEvent::State(handle, c, s))
            .unwrap_or(FrontendEvent::NoSuchClient(handle));
        self.notify_frontend(event);
    }

    fn spawn_hook_command(&self, handle: ClientHandle) {
        let Some(cmd) = self.client_manager.get_enter_cmd(handle) else {
            return;
        };
        tokio::task::spawn_local(async move {
            log::info!("spawning command!");
            let mut child = match Command::new("sh").arg("-c").arg(cmd.as_str()).spawn() {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("could not execute cmd: {e}");
                    return;
                }
            };
            match child.wait().await {
                Ok(s) => {
                    if s.success() {
                        log::info!("{cmd} exited successfully");
                    } else {
                        log::warn!("{cmd} exited with {s}");
                    }
                }
                Err(e) => log::warn!("{cmd}: {e}"),
            }
        });
    }
}
