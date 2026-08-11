use deskunion_ipc::ClientHandle;
use deskunion_proto::{Datagram, MAX_DATAGRAM_SIZE, MAX_EVENT_SIZE, ProtoEvent, decode};
use futures::{Stream, StreamExt};
use local_channel::mpsc::{Receiver, Sender, channel};
use rustls::pki_types::CertificateDer;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    net::SocketAddr,
    rc::Rc,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    sync::Mutex as AsyncMutex,
    task::{JoinHandle, spawn_local},
};
use webrtc_dtls::{
    config::{ClientAuthType::RequireAnyClientCert, Config, ExtendedMasterSecretType},
    crypto::Certificate,
    listener::listen,
};
use webrtc_util::{Conn, Error, conn::Listener};

use crate::client::ClientManager;
use crate::crypto;

#[derive(Error, Debug)]
pub enum ListenerCreationError {
    #[error(transparent)]
    WebrtcUtil(#[from] webrtc_util::Error),
    #[error(transparent)]
    WebrtcDtls(#[from] webrtc_dtls::Error),
}

#[derive(Debug, Error)]
pub(crate) enum SendError {
    #[error("client {0} is not connected")]
    NotConnected(ClientHandle),
    #[error("emulation is disabled on client {0}")]
    EmulationDisabled(ClientHandle),
}

type ArcConn = Arc<dyn Conn + Send + Sync>;

pub(crate) enum ListenEvent {
    Msg {
        event: ProtoEvent,
        addr: SocketAddr,
    },
    Accept {
        addr: SocketAddr,
        fingerprint: String,
    },
    Rejected {
        fingerprint: String,
    },
    /// the DTLS association to this peer ended (recv error or explicit
    /// close) and its `read_loop` exited
    Disconnected {
        addr: SocketAddr,
    },
    /// per-peer audio stream status, for the "active streams" list
    #[cfg_attr(not(feature = "audio"), allow(dead_code))]
    AudioStream {
        addr: SocketAddr,
        active: bool,
        latency_ms: u32,
        packets_lost: u64,
        level: f32,
    },
}

pub(crate) struct DeskunionListener {
    listen_rx: Receiver<ListenEvent>,
    listen_tx: Sender<ListenEvent>,
    listen_task: JoinHandle<()>,
    conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>>,
    client_manager: ClientManager,
    request_port_change: Sender<u16>,
    port_changed: Receiver<Result<u16, ListenerCreationError>>,
}

type VerifyPeerCertificateFn = Arc<
    dyn (Fn(&[Vec<u8>], &[CertificateDer<'static>]) -> Result<(), webrtc_dtls::Error>)
        + Send
        + Sync,
>;

impl DeskunionListener {
    pub(crate) async fn new(
        port: u16,
        cert: Certificate,
        authorized_keys: Arc<RwLock<HashMap<String, String>>>,
        client_manager: ClientManager,
        audio: crate::config::AudioSettings,
    ) -> Result<Self, ListenerCreationError> {
        let (listen_tx, listen_rx) = channel();
        let (request_port_change, mut request_port_change_rx) = channel();
        let (port_changed_tx, port_changed) = channel();
        let connection_attempts: Arc<Mutex<VecDeque<String>>> = Default::default();

        let authorized = authorized_keys.clone();
        let verify_peer_certificate: Option<VerifyPeerCertificateFn> = {
            let connection_attempts = connection_attempts.clone();
            Some(Arc::new(
                move |certs: &[Vec<u8>], _chains: &[CertificateDer<'static>]| {
                    assert!(certs.len() == 1);
                    let fingerprints = certs
                        .iter()
                        .map(|c| crypto::generate_fingerprint(c))
                        .collect::<Vec<_>>();
                    if authorized
                        .read()
                        .expect("lock")
                        .contains_key(&fingerprints[0])
                    {
                        Ok(())
                    } else {
                        let fingerprint = fingerprints.into_iter().next().expect("fingerprint");
                        connection_attempts
                            .lock()
                            .expect("lock")
                            .push_back(fingerprint);
                        Err(webrtc_dtls::Error::ErrVerifyDataMismatch)
                    }
                },
            ))
        };
        let cfg = Config {
            certificates: vec![cert.clone()],
            extended_master_secret: ExtendedMasterSecretType::Require,
            client_auth: RequireAnyClientCert,
            verify_peer_certificate,
            ..Default::default()
        };

        let listen_addr = SocketAddr::new("0.0.0.0".parse().expect("invalid ip"), port);
        let mut listener = listen(listen_addr, cfg.clone()).await?;

        let conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>> =
            Rc::new(AsyncMutex::new(Vec::new()));
        let ping_response: Rc<RefCell<HashSet<SocketAddr>>> = Default::default();

        let conns_clone = conns.clone();
        let listen_task: JoinHandle<()> = {
            let listen_tx = listen_tx.clone();
            let connection_attempts = connection_attempts.clone();
            let ping_response = ping_response.clone();
            let audio = audio.clone();
            spawn_local(async move {
                loop {
                    let sleep = tokio::time::sleep(Duration::from_secs(2));
                    tokio::select! {
                        /* workaround for https://github.com/webrtc-rs/webrtc/issues/614 */
                        _ = sleep => continue,
                        c = listener.accept() => match c {
                            Ok((conn, addr)) => {
                                log::info!("dtls client connected, ip: {addr}");
                                let mut conns = conns_clone.lock().await;
                                conns.push((addr, conn.clone()));
                                drop(conns);
                                let dtls_conn: &webrtc_dtls::conn::DTLSConn = conn.as_any().downcast_ref().expect("dtls conn");
                                let certs = dtls_conn.connection_state().await.peer_certificates;
                                let cert = certs.first().expect("cert");
                                let fingerprint = crypto::generate_fingerprint(cert);
                                listen_tx.send(ListenEvent::Accept { addr, fingerprint }).expect("channel closed");
                                // the listener (capture side) is the pinger:
                                // the dialed emulation side answers with Pong.
                                // `read_loop` owns the pinger's handle and
                                // aborts it when the connection dies.
                                let pinger = spawn_local(ping_pong(addr, conn.clone(), ping_response.clone()));
                                spawn_local(read_loop(conns_clone.clone(), addr, conn, listen_tx.clone(), ping_response.clone(), pinger, audio.clone()));
                            },
                            Err(e) => {
                                if let Error::Std(ref e) = e {
                                    if let Some(e) = e.0.downcast_ref::<webrtc_dtls::Error>() {
                                        match e {
                                            webrtc_dtls::Error::ErrVerifyDataMismatch => {
                                                if let Some(fingerprint) = connection_attempts.lock().expect("lock").pop_front() {
                                                    listen_tx.send(ListenEvent::Rejected { fingerprint }).expect("channel closed");
                                                }
                                            }
                                            _ => log::warn!("accept: {e}"),
                                        }
                                    } else {
                                        log::warn!("accept: {e:?}");
                                    }
                                } else {
                                    log::warn!("accept: {e:?}");
                                }
                            }
                        },
                        port = request_port_change_rx.recv() => {
                            let port = port.expect("channel closed");
                            let listen_addr = SocketAddr::new("0.0.0.0".parse().expect("invalid ip"), port);
                            match listen(listen_addr, cfg.clone()).await {
                                Ok(new_listener) => {
                                    let _ = listener.close().await;
                                    listener = new_listener;
                                    port_changed_tx.send(Ok(port)).expect("channel closed");
                                }
                                Err(e) => {
                                    log::warn!("unable to change port: {e}");
                                    port_changed_tx.send(Err(e.into())).expect("channel closed");
                                }
                            };
                        },
                    };
                }
            })
        };

        Ok(Self {
            conns,
            listen_rx,
            listen_tx,
            listen_task,
            client_manager,
            port_changed,
            request_port_change,
        })
    }

    pub(crate) fn request_port_change(&mut self, port: u16) {
        self.request_port_change.send(port).expect("channel closed");
    }

    pub(crate) async fn port_changed(&mut self) -> Result<u16, ListenerCreationError> {
        self.port_changed.recv().await.expect("channel closed")
    }

    pub(crate) async fn terminate(&mut self) {
        self.listen_task.abort();
        let conns = self.conns.lock().await;
        for (_, conn) in conns.iter() {
            let _ = conn.close().await;
        }
        self.listen_tx.close();
    }

    /// send an event to the accepted connection paired with `handle`.
    /// Parked (connected but unpaired) devices have no client entry and
    /// therefore no `active_addr`, so they can never be addressed here —
    /// they cannot be entered until the user assigns them a position.
    pub(crate) async fn send(
        &self,
        event: ProtoEvent,
        handle: ClientHandle,
    ) -> Result<(), SendError> {
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        let buf = &buf[..len];
        let Some(addr) = self.client_manager.active_addr(handle) else {
            return Err(SendError::NotConnected(handle));
        };
        if !self.client_manager.alive(handle) {
            return Err(SendError::EmulationDisabled(handle));
        }
        let conn = {
            let conns = self.conns.lock().await;
            conns
                .iter()
                .find(|(a, _)| *a == addr)
                .map(|(_, c)| c.clone())
        };
        let Some(conn) = conn else {
            return Err(SendError::NotConnected(handle));
        };
        if let Err(e) = conn.send(buf).await {
            log::warn!("client {handle} failed to send: {e}");
            // deterministic teardown: `read_loop` exits on the recv
            // error and emits `Disconnected`
            let _ = conn.close().await;
            return Err(SendError::NotConnected(handle));
        }
        log::trace!("{event} >=>=>=>=>=> {addr}");
        Ok(())
    }

    pub(crate) async fn reply(&self, addr: SocketAddr, event: ProtoEvent) {
        log::trace!("reply {event} >=>=>=>=>=> {addr}");
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        let conns = self.conns.lock().await;
        for (a, conn) in conns.iter() {
            if *a == addr {
                let _ = conn.send(&buf[..len]).await;
            }
        }
    }
}

impl Stream for DeskunionListener {
    type Item = ListenEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.listen_rx.poll_next_unpin(cx)
    }
}

/// how often to probe the peer — short enough that a few lost UDP
/// datagrams can't trip the keepalive on their own
const LIVENESS_INTERVAL: Duration = Duration::from_secs(5);
/// consecutive probes without any inbound datagram before the peer is
/// declared dead (6 × 5 s = 30 s liveness budget)
const LIVENESS_MAX_MISSES: u32 = 6;

/// consecutive-miss keepalive counter: a single lost ping/pong must
/// not kill the connection; only a sustained silence does.
#[derive(Default)]
struct MissCounter {
    misses: u32,
}

impl MissCounter {
    /// record an unanswered probe; returns true once the peer should
    /// be considered dead
    fn miss(&mut self) -> bool {
        self.misses += 1;
        self.misses >= LIVENESS_MAX_MISSES
    }

    /// any received datagram proves the peer is alive
    fn reset(&mut self) {
        self.misses = 0;
    }
}

/// the listener (capture side) pings; the dialed emulation side answers
/// with `Pong(emulation_active)`. Any received datagram — Pong, audio or
/// input control — counts as liveness, so a busy real-time audio stream
/// can't falsely fail the keepalive.
async fn ping_pong(
    addr: SocketAddr,
    conn: ArcConn,
    ping_response: Rc<RefCell<HashSet<SocketAddr>>>,
) {
    let mut keepalive = MissCounter::default();
    loop {
        let (buf, len) = ProtoEvent::Ping.into();
        // Clear the preceding interval before issuing this probe. Any
        // successfully received datagram proves the DTLS peer is alive.
        ping_response.borrow_mut().remove(&addr);
        if let Err(e) = conn.send(&buf[..len]).await {
            log::warn!("{addr}: send error `{e}`, closing connection");
            let _ = conn.close().await;
            return;
        }
        log::trace!("PING >=>=>=>=>=> {addr}");

        tokio::time::sleep(LIVENESS_INTERVAL).await;

        if ping_response.borrow_mut().remove(&addr) {
            keepalive.reset();
        } else if keepalive.miss() {
            log::warn!(
                "{addr} did not respond to {LIVENESS_MAX_MISSES} consecutive pings, closing connection"
            );
            let _ = conn.close().await;
            return;
        } else {
            log::debug!(
                "{addr} missed ping {}/{LIVENESS_MAX_MISSES}",
                keepalive.misses
            );
        }
    }
}

/// the format announced by the most recent `AudioControl::Start`, or
/// the wire default (48 kHz stereo) when frames arrive before (or
/// without) a `Start` — that datagram travels over unreliable UDP.
#[cfg(feature = "audio")]
fn announced_or_default_format(last_start_format: Option<(u32, u8)>) -> (u32, u8) {
    last_start_format.unwrap_or((
        deskunion_audio::codec::SAMPLE_RATE,
        crate::audio::WIRE_CHANNELS as u8,
    ))
}

/// per-peer audio receive state of a `read_loop`: the receiver
/// (created on `AudioControl::Start` or lazily on the first frame),
/// the most recently announced format, and frame counters for
/// logging/diagnostics. Extracted so the loopback harness can exercise
/// the real receive path.
#[cfg(feature = "audio")]
#[derive(Default)]
pub(crate) struct AudioRxState {
    pub(crate) receiver: Option<deskunion_audio::AudioReceiver>,
    /// format announced by the most recent `AudioControl::Start`; used
    /// to lazily create the receiver if frames arrive before (or
    /// without) a `Start`
    last_start_format: Option<(u32, u8)>,
    /// lazy receiver creation already failed once; don't retry (and
    /// re-log) for every single incoming frame
    lazy_failed: bool,
    received: u64,
    dropped: u64,
}

#[cfg(feature = "audio")]
impl AudioRxState {
    /// push every frame of a decoded `Audio`/`AudioBatch` datagram
    /// (payload ranges into `buf`) into the receiver
    pub(crate) fn on_frames(
        &mut self,
        frames: impl IntoIterator<Item = (u32, std::ops::Range<usize>)>,
        buf: &[u8],
        audio: &crate::config::AudioSettings,
        tx: &Sender<ListenEvent>,
        addr: SocketAddr,
    ) {
        self.ensure_receiver(audio, tx, addr);
        for (seq, payload_range) in frames {
            if let Some(receiver) = &self.receiver {
                receiver.push_frame(seq, &buf[payload_range]);
                self.received += 1;
                if self.received == 1 {
                    log::info!("audio stream started receiving from {addr}");
                }
            } else {
                self.dropped += 1;
                if self.dropped == 1 || self.dropped.is_multiple_of(1000) {
                    log::debug!(
                        "dropping audio frame from {addr}: no receiver ({} dropped so far)",
                        self.dropped
                    );
                }
            }
        }
    }

    /// handle an `AudioControl` datagram. `Start` is retransmitted
    /// while the stream is active (it travels over unreliable UDP), so
    /// it is idempotent: only create the receiver when there isn't one
    /// already. `Stop` tears the receiver down.
    pub(crate) fn on_control(
        &mut self,
        cmd: deskunion_proto::AudioControlCmd,
        audio: &crate::config::AudioSettings,
        tx: &Sender<ListenEvent>,
        addr: SocketAddr,
    ) {
        match cmd {
            deskunion_proto::AudioControlCmd::Start {
                sample_rate,
                channels,
            } => {
                self.last_start_format = Some((sample_rate, channels));
                self.lazy_failed = false;
                if self.receiver.is_none() {
                    self.receiver = crate::audio::start_receiver(audio, sample_rate, channels);
                    if self.receiver.is_some() {
                        tx.send(ListenEvent::AudioStream {
                            addr,
                            active: true,
                            latency_ms: 0,
                            packets_lost: 0,
                            level: 0.0,
                        })
                        .expect("channel closed");
                    }
                }
            }
            deskunion_proto::AudioControlCmd::Stop => {
                self.receiver = None;
                self.last_start_format = None;
                self.lazy_failed = false;
                tx.send(ListenEvent::AudioStream {
                    addr,
                    active: false,
                    latency_ms: 0,
                    packets_lost: 0,
                    level: 0.0,
                })
                .expect("channel closed");
            }
        }
    }

    /// lazily create the audio receiver when frames arrive before (or
    /// without) an `AudioControl::Start` — that datagram is sent over
    /// unreliable UDP and can be lost.
    fn ensure_receiver(
        &mut self,
        audio: &crate::config::AudioSettings,
        tx: &Sender<ListenEvent>,
        addr: SocketAddr,
    ) {
        if self.receiver.is_some() || self.lazy_failed {
            return;
        }
        let (sample_rate, channels) = announced_or_default_format(self.last_start_format);
        log::debug!(
            "audio frames from {addr} arrived without an active receiver; starting one lazily ({sample_rate} Hz, {channels} ch)"
        );
        self.receiver = crate::audio::start_receiver(audio, sample_rate, channels);
        if self.receiver.is_some() {
            tx.send(ListenEvent::AudioStream {
                addr,
                active: true,
                latency_ms: 0,
                packets_lost: 0,
                level: 0.0,
            })
            .expect("channel closed");
        } else {
            self.lazy_failed = true;
        }
    }

    /// tear down a playback stream the backend reported as failed, so a
    /// later `Start` or frame rebuilds it. `lazy_failed` is cleared too:
    /// this is a new failure, not the one that latched it.
    pub(crate) fn drop_if_failed(&mut self, tx: &Sender<ListenEvent>, addr: SocketAddr) {
        let failed = self
            .receiver
            .as_ref()
            .is_some_and(|receiver| !receiver.is_healthy());
        if failed {
            log::error!(
                "audio playback from {addr} failed; dropping the stream so it can be restarted"
            );
            self.receiver = None;
            self.lazy_failed = false;
            // without this the stats block below stops emitting and the
            // frontend's stream row stays frozen on the last active
            // reading until a later `Start` happens to rebuild a receiver
            let _ = tx.send(ListenEvent::AudioStream {
                addr,
                active: false,
                latency_ms: 0,
                packets_lost: 0,
                level: 0.0,
            });
        }
    }

    pub(crate) fn received(&self) -> u64 {
        self.received
    }
}

/// read loop of an accepted connection. Audio flows emulation ->
/// capture (§3.1 of the audio plan): the listener is the capture side,
/// so this is the receiving end — one `AudioReceiver` per peer, created
/// on `AudioControl::Start` (or lazily on the first frame) and torn
/// down on `Stop` or disconnect. Owns the connection's `pinger` task:
/// when this loop exits, the peer is gone and pinging it (and logging
/// keepalive timeouts for a dead connection) is pointless.
#[allow(clippy::too_many_arguments)]
async fn read_loop(
    conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>>,
    addr: SocketAddr,
    conn: ArcConn,
    dtls_tx: Sender<ListenEvent>,
    ping_response: Rc<RefCell<HashSet<SocketAddr>>>,
    pinger: JoinHandle<()>,
    #[cfg_attr(not(feature = "audio"), allow(unused_variables))]
    audio: crate::config::AudioSettings,
) {
    let mut buf = [0u8; MAX_DATAGRAM_SIZE];
    #[cfg(feature = "audio")]
    let mut audio_rx = AudioRxState::default();
    #[cfg(feature = "audio")]
    let mut last_audio_stats = std::time::Instant::now();

    while let Ok(n) = conn.recv(&mut buf).await {
        // Receiving anything from this DTLS association is stronger proof of
        // liveness than waiting for a Pong queued behind real-time audio.
        ping_response.borrow_mut().insert(addr);
        match decode(&buf[..n]) {
            Ok(Datagram::Event(event)) => {
                log::trace!("{addr} <=<=<=<=<= {event}");
                dtls_tx
                    .send(ListenEvent::Msg { event, addr })
                    .expect("channel closed");
            }
            #[cfg_attr(not(feature = "audio"), allow(unused_variables))]
            Ok(Datagram::Audio {
                seq,
                ts_ms: _,
                payload_range,
            }) => {
                #[cfg(feature = "audio")]
                audio_rx.on_frames(
                    std::iter::once((seq, payload_range)),
                    &buf,
                    &audio,
                    &dtls_tx,
                    addr,
                );
            }
            #[cfg_attr(not(feature = "audio"), allow(unused_variables))]
            Ok(Datagram::AudioBatch(frames)) => {
                #[cfg(feature = "audio")]
                audio_rx.on_frames(
                    frames
                        .iter()
                        .map(|frame| (frame.seq, frame.payload_range.clone())),
                    &buf,
                    &audio,
                    &dtls_tx,
                    addr,
                );
            }
            #[cfg_attr(not(feature = "audio"), allow(unused_variables))]
            Ok(Datagram::AudioControl(cmd)) => {
                #[cfg(feature = "audio")]
                {
                    if matches!(cmd, deskunion_proto::AudioControlCmd::Start { .. }) {
                        last_audio_stats = std::time::Instant::now();
                    }
                    audio_rx.on_control(cmd, &audio, &dtls_tx, addr);
                }
            }
            // Skip undecodable datagrams without dropping the
            // connection. Each DTLS recv is one framed message, so
            // skipping is safe and keeps us forward-compatible with
            // peers that send event types we don't yet know about.
            Err(e) => log::debug!("ignoring undecodable event from {addr}: {e}"),
        }
        #[cfg(feature = "audio")]
        if last_audio_stats.elapsed() >= Duration::from_secs(1) {
            // a cpal stream that errored never calls its data callback
            // again: drop the receiver so the next `AudioControl::Start`
            // retransmit (every 2s while a stream is active) or the next
            // frame builds a fresh one. Without this the connection,
            // the jitter buffer and the stats all stay healthy while
            // nothing is audible.
            audio_rx.drop_if_failed(&dtls_tx, addr);
            if let Some(receiver) = &audio_rx.receiver {
                let stats = receiver.stats();
                dtls_tx
                    .send(ListenEvent::AudioStream {
                        addr,
                        active: true,
                        latency_ms: stats.latency_ms,
                        packets_lost: stats.packets_lost,
                        level: stats.level,
                    })
                    .expect("channel closed");
            }
            last_audio_stats = std::time::Instant::now();
        }
    }
    #[cfg(feature = "audio")]
    {
        if audio_rx.receiver.is_some() {
            // teardown path: `terminate()` closes the connections and then
            // the channel, so this loop can legitimately outlive the
            // receiver. Panicking here aborts the process (release builds
            // are `panic = "abort"`) on an ordinary Server -> Client switch.
            let _ = dtls_tx.send(ListenEvent::AudioStream {
                addr,
                active: false,
                latency_ms: 0,
                packets_lost: 0,
                level: 0.0,
            });
        }
        log::info!(
            "dtls client disconnected {addr} after {} audio frames",
            audio_rx.received()
        );
    }
    #[cfg(not(feature = "audio"))]
    log::info!("dtls client disconnected {addr}");
    // the peer is gone: stop pinging it (otherwise the pinger keeps
    // logging keepalive timeouts for a dead connection) and drop its
    // liveness bookkeeping
    pinger.abort();
    ping_response.borrow_mut().remove(&addr);
    let mut conns = conns.lock().await;
    if let Some(index) = conns.iter().position(|(a, _)| *a == addr) {
        conns.remove(index);
    }
    drop(conns);
    let _ = dtls_tx.send(ListenEvent::Disconnected { addr });
}

#[cfg(test)]
mod test {
    use super::*;

    /// a connection whose reads fail (so `read_loop` exits) while
    /// sends keep succeeding into the void — the field condition that
    /// left a pinger logging keepalive timeouts 35 s after its
    /// connection died
    #[derive(Default)]
    struct DeadReadConn {
        sends: std::sync::atomic::AtomicU64,
    }

    #[async_trait::async_trait]
    impl Conn for DeadReadConn {
        async fn connect(&self, _addr: SocketAddr) -> webrtc_util::Result<()> {
            Ok(())
        }
        async fn recv(&self, _buf: &mut [u8]) -> webrtc_util::Result<usize> {
            Err(Error::ErrUseClosedNetworkConn)
        }
        async fn recv_from(&self, _buf: &mut [u8]) -> webrtc_util::Result<(usize, SocketAddr)> {
            Err(Error::ErrUseClosedNetworkConn)
        }
        async fn send(&self, buf: &[u8]) -> webrtc_util::Result<usize> {
            self.sends
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(buf.len())
        }
        async fn send_to(&self, buf: &[u8], _target: SocketAddr) -> webrtc_util::Result<usize> {
            self.send(buf).await
        }
        fn local_addr(&self) -> webrtc_util::Result<SocketAddr> {
            Ok("127.0.0.1:9".parse().expect("addr"))
        }
        fn remote_addr(&self) -> Option<SocketAddr> {
            None
        }
        async fn close(&self) -> webrtc_util::Result<()> {
            Ok(())
        }
        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
        }
    }

    /// when a connection's read_loop exits, its pinger task must be
    /// cancelled — regression test for pings (and keepalive timeout
    /// warnings) continuing after the peer was already gone
    #[tokio::test]
    async fn read_loop_cancels_pinger_on_exit() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let addr: SocketAddr = "127.0.0.1:4242".parse().expect("addr");
                let conn: ArcConn = Arc::new(DeadReadConn::default());
                let conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>> = Default::default();
                conns.lock().await.push((addr, conn.clone()));
                let (tx, mut rx) = channel();
                let ping_response: Rc<RefCell<HashSet<SocketAddr>>> = Default::default();

                let sends = conn.clone();
                let pinger = spawn_local(ping_pong(addr, conn.clone(), ping_response.clone()));
                let read = spawn_local(read_loop(
                    conns,
                    addr,
                    conn,
                    tx,
                    ping_response,
                    pinger,
                    test_audio_settings(),
                ));
                read.await.expect("read_loop");

                // the disconnect surfaced ...
                assert!(matches!(
                    rx.recv().await,
                    Some(ListenEvent::Disconnected { .. })
                ));
                // ... and the pinger is dead: no further pings, even
                // after a full keepalive interval
                let sends_at_exit = sends
                    .as_any()
                    .downcast_ref::<DeadReadConn>()
                    .expect("dead read conn")
                    .sends
                    .load(std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(LIVENESS_INTERVAL + Duration::from_secs(2)).await;
                let sends_later = sends
                    .as_any()
                    .downcast_ref::<DeadReadConn>()
                    .expect("dead read conn")
                    .sends
                    .load(std::sync::atomic::Ordering::Relaxed);
                assert_eq!(
                    sends_at_exit, sends_later,
                    "pinger kept pinging after its connection died"
                );
            })
            .await;
    }

    /// `terminate()` closes the connections and then the event channel,
    /// but never aborts the per-connection `read_loop` tasks. Those loops
    /// then run their teardown sends into an already-closed channel — a
    /// panic, and with `panic = "abort"` in the release profile a process
    /// abort, on an ordinary Server -> Client switch in the UI.
    #[tokio::test]
    async fn read_loop_survives_a_channel_closed_by_terminate() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let addr: SocketAddr = "127.0.0.1:4242".parse().expect("addr");
                let conn: ArcConn = Arc::new(DeadReadConn::default());
                let conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>> = Default::default();
                conns.lock().await.push((addr, conn.clone()));
                let (mut tx, rx) = channel();
                let ping_response: Rc<RefCell<HashSet<SocketAddr>>> = Default::default();

                // what `terminate()` does to the channel, before the loop
                // reaches its teardown sends
                tx.close();
                drop(rx);

                let pinger = spawn_local(ping_pong(addr, conn.clone(), ping_response.clone()));
                let read = spawn_local(read_loop(
                    conns,
                    addr,
                    conn,
                    tx,
                    ping_response,
                    pinger,
                    test_audio_settings(),
                ));
                read.await.expect("read_loop panicked on a closed channel");
            })
            .await;
    }

    #[test]
    fn keepalive_tolerates_scattered_losses() {
        let mut keepalive = MissCounter::default();
        // scattered single misses never reach the limit
        for _ in 0..10 {
            assert!(!keepalive.miss());
            keepalive.reset();
        }
        // LIVENESS_MAX_MISSES - 1 consecutive misses still survive
        let mut keepalive = MissCounter::default();
        for _ in 0..LIVENESS_MAX_MISSES - 1 {
            assert!(!keepalive.miss());
        }
        // the next consecutive miss declares the peer dead
        assert!(keepalive.miss());
    }

    #[test]
    fn keepalive_resets_on_any_response() {
        let mut keepalive = MissCounter::default();
        for _ in 0..LIVENESS_MAX_MISSES - 1 {
            keepalive.miss();
        }
        keepalive.reset();
        // a full new budget of misses is required after a response
        for _ in 0..LIVENESS_MAX_MISSES - 1 {
            assert!(!keepalive.miss());
        }
        assert!(keepalive.miss());
    }

    #[cfg(feature = "audio")]
    #[test]
    fn lazy_receiver_format_defaults_to_wire_format() {
        // without an announced `Start`, the lazily created receiver on
        // the listener side must use the wire format (48 kHz stereo)
        assert_eq!(
            announced_or_default_format(None),
            (deskunion_audio::codec::SAMPLE_RATE, 2)
        );
        // an announced `Start` format is honored instead
        assert_eq!(announced_or_default_format(Some((44_100, 1))), (44_100, 1));
    }

    fn test_audio_settings() -> crate::config::AudioSettings {
        crate::config::AudioSettings {
            send: false,
            receive: false,
            bitrate: 96_000,
            buffer_ms: 80,
            capture_device: None,
            playback_device: None,
        }
    }

    fn ephemeral_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .map(|s| s.local_addr().expect("addr").port())
            .expect("bind")
    }

    /// a connection whose fingerprint is authorized but not paired to a
    /// client entry has no `active_addr`, so the capture side cannot
    /// route `Enter` (or any other event) to it — it stays parked until
    /// the user assigns a position
    #[tokio::test]
    async fn unpaired_device_cannot_be_addressed() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let cert =
                    Certificate::generate_self_signed(["ignored".to_owned()]).expect("certificate");
                let listener = DeskunionListener::new(
                    ephemeral_port(),
                    cert,
                    Default::default(),
                    ClientManager::default(),
                    test_audio_settings(),
                )
                .await
                .expect("listener");
                let result = listener
                    .send(ProtoEvent::Enter(deskunion_proto::Position::Left), 0)
                    .await;
                assert!(matches!(result, Err(SendError::NotConnected(0))));
            })
            .await;
    }

    /// a dead connection's ping task must be cancelled: after the
    /// peer disconnects, no more pings may be sent to its old address
    /// (the pinger would otherwise keep logging keepalive timeouts
    /// for a connection that is already gone)
    #[tokio::test]
    async fn pinger_stops_when_peer_disconnects() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let server_cert =
                    Certificate::generate_self_signed(["ignored".to_owned()]).expect("certificate");
                let client_cert =
                    Certificate::generate_self_signed(["ignored".to_owned()]).expect("certificate");
                let client_fingerprint = crypto::certificate_fingerprint(&client_cert);
                let authorized = Arc::new(RwLock::new(HashMap::from([(
                    client_fingerprint.clone(),
                    "test".to_owned(),
                )])));
                let port = ephemeral_port();
                let mut listener = DeskunionListener::new(
                    port,
                    server_cert,
                    authorized,
                    ClientManager::default(),
                    test_audio_settings(),
                )
                .await
                .expect("listener");
                let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");
                let (conn, _) = crate::connect::connect(addr, client_cert)
                    .await
                    .map_err(|(_, e)| e)
                    .expect("connect");
                let client_port = conn.local_addr().expect("local addr").port();

                let accept = tokio::time::timeout(Duration::from_secs(5), listener.next())
                    .await
                    .expect("accept timeout")
                    .expect("accept event");
                assert!(matches!(accept, ListenEvent::Accept { .. }));

                // close the session; the server's read_loop exits and
                // must cancel the pinger
                conn.close().await.expect("close");
                drop(conn);
                let disconnected = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let ListenEvent::Disconnected { .. } =
                            listener.next().await.expect("event")
                        {
                            break;
                        }
                    }
                })
                .await;
                disconnected.expect("disconnect timeout");

                // listen on the dead peer's old port: one ping may
                // still be in flight from before the abort, but after
                // that the pinger must be gone — no ping at the next
                // 5 s keepalive slot
                let mut socket = None;
                for _ in 0..30 {
                    if let Ok(s) = std::net::UdpSocket::bind(("0.0.0.0", client_port)) {
                        socket = Some(s);
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                let socket = socket.expect("rebind the peer's old port");
                socket
                    .set_read_timeout(Some(Duration::from_millis(1500)))
                    .expect("timeout");
                let mut buf = [0u8; MAX_DATAGRAM_SIZE];
                // drain a possibly in-flight ping
                while socket.recv_from(&mut buf).is_ok() {}
                socket
                    .set_read_timeout(Some(LIVENESS_INTERVAL + Duration::from_secs(2)))
                    .expect("timeout");
                let result = socket.recv_from(&mut buf);
                assert!(
                    result.is_err(),
                    "pinger kept pinging a dead connection: {result:?}"
                );

                listener.terminate().await;
            })
            .await;
    }

    /// ping/pong direction: the listener (capture side) pings, the
    /// dialed emulation side answers `Pong(emulation_active)`. Uses a
    /// real DTLS session over loopback.
    #[tokio::test]
    async fn listener_pings_dialer_pongs() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let server_cert =
                    Certificate::generate_self_signed(["ignored".to_owned()]).expect("certificate");
                let client_cert =
                    Certificate::generate_self_signed(["ignored".to_owned()]).expect("certificate");
                let client_fingerprint = crypto::certificate_fingerprint(&client_cert);
                let authorized = Arc::new(RwLock::new(HashMap::from([(
                    client_fingerprint.clone(),
                    "test".to_owned(),
                )])));
                let port = ephemeral_port();
                let mut listener = DeskunionListener::new(
                    port,
                    server_cert,
                    authorized,
                    ClientManager::default(),
                    test_audio_settings(),
                )
                .await
                .expect("listener");
                let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");
                let (conn, _) = crate::connect::connect(addr, client_cert)
                    .await
                    .map_err(|(_, e)| e)
                    .expect("connect");

                // the accept surfaces the dialer's certificate
                // fingerprint (the pairing key)
                let accept = tokio::time::timeout(Duration::from_secs(5), listener.next())
                    .await
                    .expect("accept timeout")
                    .expect("accept event");
                match accept {
                    ListenEvent::Accept { fingerprint, .. } => {
                        assert_eq!(fingerprint, client_fingerprint)
                    }
                    _ => panic!("expected Accept"),
                }

                // the first datagram from the listener must be a Ping —
                // the dialed side never pings
                let mut buf = [0u8; MAX_DATAGRAM_SIZE];
                let n = tokio::time::timeout(LIVENESS_INTERVAL * 2, conn.recv(&mut buf))
                    .await
                    .expect("ping timeout")
                    .expect("recv");
                assert!(
                    matches!(decode(&buf[..n]), Ok(Datagram::Event(ProtoEvent::Ping))),
                    "first datagram from the listener must be a Ping"
                );

                // the dialer answers with Pong(emulation_active) ...
                let (pong, len): ([u8; MAX_EVENT_SIZE], usize) = ProtoEvent::Pong(true).into();
                conn.send(&pong[..len]).await.expect("pong");

                // ... which the listener surfaces as a peer message
                let msg = tokio::time::timeout(Duration::from_secs(5), listener.next())
                    .await
                    .expect("pong timeout")
                    .expect("pong event");
                assert!(matches!(
                    msg,
                    ListenEvent::Msg {
                        event: ProtoEvent::Pong(true),
                        ..
                    }
                ));

                listener.terminate().await;
            })
            .await;
    }
}
