use crate::config::local_commit;
use deskunion_proto::{Datagram, MAX_DATAGRAM_SIZE, MAX_EVENT_SIZE, ProtoEvent, decode};
use local_channel::mpsc::{Receiver, Sender, channel};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::{
    net::{UdpSocket, lookup_host},
    sync::Mutex,
    task::{JoinSet, spawn_local},
};
use webrtc_dtls::{
    config::{Config, ExtendedMasterSecretType},
    conn::DTLSConn,
    crypto::Certificate,
};
use webrtc_util::Conn;

#[derive(Debug, Error)]
pub(crate) enum DeskunionConnectionError {
    #[error(transparent)]
    Bind(#[from] std::io::Error),
    #[error(transparent)]
    Dtls(#[from] webrtc_dtls::Error),
    #[error(transparent)]
    Webrtc(#[from] webrtc_util::Error),
    #[error("not connected")]
    NotConnected,
    #[error("Connection timed out")]
    Timeout,
    #[error(
        "the remote server rejected this certificate; authorize this computer in DeskUnion on the remote machine, then try again"
    )]
    AuthorizationRequired,
}

pub(crate) enum ConnectionEvent {
    /// a protocol event received from the server
    Message { addr: SocketAddr, event: ProtoEvent },
    /// any non-event datagram arrived from the server (e.g. audio from a
    /// future bidirectional-audio peer, or an unknown datagram) —
    /// proves liveness for the emulation-side idle watchdog
    PeerAlive { addr: SocketAddr },
    /// the DTLS session to the server is established
    Connected {
        addr: SocketAddr,
        fingerprint: String,
    },
    /// the DTLS session to the server ended; the reconnect loop already
    /// redials while a server target is configured
    Disconnected { addr: SocketAddr },
}

/// the capture-side endpoint the emulation side dials
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServerTarget {
    pub ips: Vec<IpAddr>,
    pub port: u16,
}

impl ServerTarget {
    fn addrs(&self) -> Vec<SocketAddr> {
        self.ips
            .iter()
            .map(|ip| SocketAddr::new(*ip, self.port))
            .collect()
    }
}

const DEFAULT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const AUTHORIZATION_WAIT_TIMEOUT: Duration = Duration::from_secs(45);
// Give the user enough time to complete the authorization form before
// reconnecting. Older peers reopen their dialog for every rejected handshake.
const AUTHORIZATION_RETRY_INTERVAL: Duration = Duration::from_secs(15);
/// delay between connection attempts while the server is unreachable
const RECONNECT_INTERVAL: Duration = Duration::from_secs(3);

type ArcConn = Arc<dyn Conn + Send + Sync>;

/// local address to bind the dialing socket to: the address of the
/// interface the OS would route to `addr`, not the wildcard.
///
/// A wildcard-bound UDP socket can receive from anyone, so Windows
/// Defender treats it as a server and raises the "allow network access"
/// prompt even though DeskUnion only dials out in client mode. Binding
/// the outgoing interface address keeps the socket to what it is. It
/// also picks the right address family — the previous hardcoded
/// `0.0.0.0:0` could not reach an IPv6 peer at all.
///
/// The probe socket never sends a packet; `connect` only makes the
/// kernel run the route lookup so `local_addr` reveals the interface.
fn local_bind_addr(addr: SocketAddr) -> SocketAddr {
    let unspecified = match addr {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let probe = std::net::UdpSocket::bind(unspecified)
        .and_then(|socket| socket.connect(addr).and_then(|()| socket.local_addr()));
    match probe {
        Ok(local) => SocketAddr::new(local.ip(), 0),
        Err(e) => {
            log::debug!("no route to {addr} yet ({e}); binding the wildcard instead");
            unspecified
        }
    }
}

pub(crate) async fn connect(
    addr: SocketAddr,
    cert: Certificate,
) -> Result<(ArcConn, SocketAddr), (SocketAddr, DeskunionConnectionError)> {
    log::info!("connecting to {addr} ...");
    let conn = Arc::new(
        UdpSocket::bind(local_bind_addr(addr))
            .await
            .map_err(|e| (addr, e.into()))?,
    );
    conn.connect(addr).await.map_err(|e| (addr, e.into()))?;
    let config = Config {
        certificates: vec![cert],
        server_name: "ignored".to_owned(),
        insecure_skip_verify: true,
        extended_master_secret: ExtendedMasterSecretType::Require,
        ..Default::default()
    };
    let timeout = tokio::time::sleep(DEFAULT_CONNECTION_TIMEOUT);
    tokio::select! {
        _ = timeout => Err((addr, DeskunionConnectionError::Timeout)),
        result = DTLSConn::new(conn, config, true, None) => match result {
            Ok(dtls_conn) => Ok((Arc::new(dtls_conn), addr)),
            Err(webrtc_dtls::Error::ErrAlertFatalOrClose) => {
                Err((addr, DeskunionConnectionError::AuthorizationRequired))
            }
            Err(e) => Err((addr, e.into())),
        }
    }
}

async fn connect_any(
    addrs: &[SocketAddr],
    cert: Certificate,
) -> Result<(ArcConn, SocketAddr), DeskunionConnectionError> {
    let mut joinset = JoinSet::new();
    let mut best_error = None;
    for &addr in addrs {
        joinset.spawn_local(connect(addr, cert.clone()));
    }
    loop {
        match joinset.join_next().await {
            None => {
                return Err(best_error.unwrap_or(DeskunionConnectionError::NotConnected));
            }
            Some(r) => match r.expect("join error") {
                Ok(conn) => return Ok(conn),
                Err((a, e)) => {
                    log::warn!("failed to connect to {a}: `{e}`");
                    // An IPv6 resolution can fail locally while IPv4 reaches
                    // the peer and is rejected only because authorization is
                    // pending. Preserve the actionable remote error instead
                    // of replacing it with a generic/local socket failure.
                    if matches!(e, DeskunionConnectionError::AuthorizationRequired)
                        || !matches!(
                            best_error.as_ref(),
                            Some(DeskunionConnectionError::AuthorizationRequired)
                        )
                    {
                        best_error = Some(e);
                    }
                }
            },
        };
    }
}

/// connection state shared between the handle, the connect loop and the
/// per-connection server loop
struct Shared {
    conn: Mutex<Option<(SocketAddr, ArcConn)>>,
    target: Mutex<Option<ServerTarget>>,
    connecting: Mutex<bool>,
}

/// the emulation side's transport: a persistent DTLS session dialed out
/// to the configured server (capture side). The server pings; this side
/// answers `Pong` from the emulation task. Audio capture/encoding runs
/// here (emulation -> capture direction).
pub(crate) struct DeskunionConnection {
    cert: Certificate,
    shared: Rc<Shared>,
    recv_rx: Receiver<ConnectionEvent>,
    recv_tx: Sender<ConnectionEvent>,
    audio: crate::config::AudioSettings,
}

impl DeskunionConnection {
    pub(crate) fn new(cert: Certificate, audio: crate::config::AudioSettings) -> Self {
        let (recv_tx, recv_rx) = channel();
        Self {
            cert,
            shared: Rc::new(Shared {
                conn: Default::default(),
                target: Default::default(),
                connecting: Default::default(),
            }),
            recv_rx,
            recv_tx,
            audio,
        }
    }

    pub(crate) async fn recv(&mut self) -> ConnectionEvent {
        self.recv_rx.recv().await.expect("channel closed")
    }

    /// fire-and-forget event send to the server (`Ack` / `Leave` /
    /// `Pong` / `Hello` replies)
    pub(crate) async fn reply(&self, addr: SocketAddr, event: ProtoEvent) {
        log::trace!("reply {event} >->->->->- {addr}");
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        let conn = self.shared.conn.lock().await;
        if let Some((a, conn)) = conn.as_ref() {
            if *a == addr {
                let _ = conn.send(&buf[..len]).await;
            }
        }
    }

    /// set the server endpoint to dial. `Some` starts (or retargets) the
    /// reconnect loop; `None` tears the current connection down and
    /// stops redialing.
    pub(crate) async fn set_server(&self, target: Option<ServerTarget>) {
        let changed = {
            let mut current = self.shared.target.lock().await;
            if *current == target {
                false
            } else {
                *current = target;
                true
            }
        };
        if !changed {
            return;
        }
        // retarget or shutdown: drop the current session; the server
        // loop's exit path redials the new target, if any
        let conn = self.shared.conn.lock().await.take();
        if let Some((addr, conn)) = conn {
            #[cfg(feature = "audio")]
            send_audio_stop(&conn, addr).await;
            #[cfg(not(feature = "audio"))]
            let _ = addr;
            let _ = conn.close().await;
        }
        if self.shared.target.lock().await.is_some() {
            self.ensure_connecting();
        }
    }

    /// start dialing the configured server, unless a session or a
    /// connect loop already exists
    fn ensure_connecting(&self) {
        ensure_connecting(
            self.shared.clone(),
            self.cert.clone(),
            self.recv_tx.clone(),
            self.audio.clone(),
        );
    }

    /// close the session to the server: the emulation-side watchdog
    /// uses this on idle timeout. The peer is notified with
    /// `AudioControl::Stop` first so it can tear down its receiver
    /// instead of waiting for its own keepalive timeout. The reconnect
    /// loop redials while a target is configured.
    pub(crate) async fn close(&self, addr: SocketAddr) {
        let conn = self.shared.conn.lock().await;
        if let Some((a, conn)) = conn.as_ref() {
            if *a == addr {
                #[cfg(feature = "audio")]
                send_audio_stop(conn, addr).await;
                log::info!("closing connection to {addr}");
                let _ = conn.close().await;
                // the entry is removed by `server_loop` when its recv errors
            }
        }
    }

    pub(crate) async fn terminate(&self) {
        self.set_server(None).await;
    }

    pub(crate) async fn get_certificate_fingerprint(&self, addr: SocketAddr) -> Option<String> {
        let conns = self.shared.conn.lock().await;
        let (_, conn) = conns.as_ref().filter(|(a, _)| *a == addr)?;
        let conn: &DTLSConn = conn.as_any().downcast_ref().expect("dtls conn");
        let certs = conn.connection_state().await.peer_certificates;
        let cert = certs.first()?;
        Some(crate::crypto::generate_fingerprint(cert))
    }

    /// Verify the UDP/DTLS path to a server before its endpoint is
    /// saved. Succeeds once the server proves the session is alive by
    /// pinging us (the listener is the pinger) or echoing our Hello;
    /// the temporary connection is closed afterwards.
    pub(crate) fn test_connection(
        &self,
        hostname: String,
        port: u16,
    ) -> impl std::future::Future<Output = Result<(), DeskunionConnectionError>> + use<> {
        let cert = self.cert.clone();
        async move {
            let addrs = lookup_host((hostname.as_str(), port))
                .await
                .map_err(DeskunionConnectionError::Bind)?
                .collect::<Vec<_>>();
            if addrs.is_empty() {
                return Err(DeskunionConnectionError::NotConnected);
            }

            let authorization_deadline = Instant::now() + AUTHORIZATION_WAIT_TIMEOUT;
            let (conn, _addr) = loop {
                match connect_any(&addrs, cert.clone()).await {
                    Ok(connection) => break connection,
                    Err(DeskunionConnectionError::AuthorizationRequired)
                        if Instant::now() < authorization_deadline =>
                    {
                        tokio::time::sleep(AUTHORIZATION_RETRY_INTERVAL).await;
                    }
                    Err(error) => return Err(error),
                }
            };
            let (hello, len): ([u8; MAX_EVENT_SIZE], usize) = ProtoEvent::Hello {
                commit: local_commit(),
            }
            .into();
            conn.send(&hello[..len]).await?;

            let mut buf = [0u8; MAX_DATAGRAM_SIZE];
            tokio::time::timeout(DEFAULT_CONNECTION_TIMEOUT, async {
                loop {
                    let len = conn.recv(&mut buf).await?;
                    // the listener pings once the session is up and echoes
                    // our Hello — either proves a live, authorized peer
                    if let Ok(Datagram::Event(ProtoEvent::Ping | ProtoEvent::Hello { .. })) =
                        decode(&buf[..len])
                    {
                        return Ok::<_, webrtc_util::Error>(());
                    }
                }
            })
            .await
            .map_err(|_| DeskunionConnectionError::Timeout)?
            .map_err(DeskunionConnectionError::Webrtc)?;
            let _ = conn.close().await;
            Ok(())
        }
    }
}

fn ensure_connecting(
    shared: Rc<Shared>,
    cert: Certificate,
    tx: Sender<ConnectionEvent>,
    audio: crate::config::AudioSettings,
) {
    spawn_local(async move {
        {
            let mut connecting = shared.connecting.lock().await;
            if *connecting || shared.conn.lock().await.is_some() {
                return;
            }
            *connecting = true;
        }
        connect_loop(shared.clone(), cert, tx, audio).await;
        *shared.connecting.lock().await = false;
    });
}

/// dial the configured server until a session is established or the
/// target is cleared. Reconnection after an established session drops
/// is triggered by `server_loop`'s exit path.
async fn connect_loop(
    shared: Rc<Shared>,
    cert: Certificate,
    tx: Sender<ConnectionEvent>,
    audio: crate::config::AudioSettings,
) {
    loop {
        let target = shared.target.lock().await.clone();
        let Some(target) = target else { return };
        let addrs = target.addrs();
        if addrs.is_empty() {
            return;
        }
        log::info!("connecting to server ... (ips: {addrs:?})");
        let (conn, addr) = match connect_any(&addrs, cert.clone()).await {
            Ok(c) => c,
            Err(e) => {
                log::debug!("reconnecting after: {e}");
                tokio::time::sleep(RECONNECT_INTERVAL).await;
                continue;
            }
        };
        // the target may have changed while the handshake was in flight
        if shared.target.lock().await.as_ref() != Some(&target) {
            let _ = conn.close().await;
            continue;
        }
        let dtls_conn: &DTLSConn = conn.as_any().downcast_ref().expect("dtls conn");
        let certs = dtls_conn.connection_state().await.peer_certificates;
        let fingerprint = certs
            .first()
            .map(|c| crate::crypto::generate_fingerprint(c))
            .unwrap_or_default();
        log::info!("connected to server @ {addr}");
        shared.conn.lock().await.replace((addr, conn.clone()));
        tx.send(ConnectionEvent::Connected { addr, fingerprint })
            .expect("channel closed");

        // Best-effort compatibility metadata. Send our commit hash once
        // immediately after the DTLS handshake; the listen side mirrors
        // a Hello back. Old peers will silently skip this event per the
        // forward-compat handler in [`server_loop`].
        let (buf, len) = ProtoEvent::Hello {
            commit: local_commit(),
        }
        .into();
        if let Err(e) = conn.send(&buf[..len]).await {
            log::debug!("hello send to {addr} failed: {e}");
        }

        spawn_local(server_loop(shared, addr, conn, cert, tx, audio));
        return;
    }
}

/// announce the stream format to the peer. `Start` travels over
/// unreliable UDP, so `server_loop` retransmits it periodically while
/// the stream is active; the receive side treats repeats as idempotent.
#[cfg(feature = "audio")]
async fn send_audio_start(conn: &ArcConn, addr: SocketAddr) {
    let mut out = [0u8; MAX_DATAGRAM_SIZE];
    match deskunion_proto::encode_into(
        deskunion_proto::DatagramRef::AudioControl(deskunion_proto::AudioControlCmd::Start {
            sample_rate: deskunion_audio::codec::SAMPLE_RATE,
            channels: crate::audio::WIRE_CHANNELS as u8,
        }),
        &mut out,
    ) {
        Ok(len) => {
            let _ = conn.send(&out[..len]).await;
        }
        Err(e) => log::warn!("failed to encode AudioControl::Start for {addr}: {e}"),
    }
}

/// tell the peer to tear down its audio receiver. Best-effort over
/// unreliable UDP — the peer's own disconnect handling is the fallback.
#[cfg(feature = "audio")]
async fn send_audio_stop(conn: &ArcConn, addr: SocketAddr) {
    let mut out = [0u8; MAX_DATAGRAM_SIZE];
    match deskunion_proto::encode_into(
        deskunion_proto::DatagramRef::AudioControl(deskunion_proto::AudioControlCmd::Stop),
        &mut out,
    ) {
        Ok(len) => {
            let _ = conn.send(&out[..len]).await;
        }
        Err(e) => log::warn!("failed to encode AudioControl::Stop for {addr}: {e}"),
    }
}

/// awaits the next queued audio frame, or never resolves if there's no
/// sender (audio disabled/unavailable) — lets `server_loop`'s `select!`
/// stay a single unconditional shape regardless of the `audio` feature
/// or runtime `audio.send` setting.
pub(crate) async fn recv_audio_frame(
    rx: &mut Option<tokio::sync::mpsc::Receiver<(u32, u32, Vec<u8>)>>,
) -> Option<(u32, u32, Vec<u8>)> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// frames per audio datagram — batching amortizes the relatively
/// expensive DTLS send without changing the codec's 20 ms frame size
pub(crate) const AUDIO_BATCH_FRAMES: usize = 3;

/// send one batch of audio frames to the peer: takes the frame that
/// just arrived, drains up to [`AUDIO_BATCH_FRAMES`] - 1 follow-ups that
/// are *already queued*, and encodes them as one `AudioBatch`, falling
/// back to single-frame datagrams when the batch overflows the datagram
/// budget. Extracted from `server_loop` so the loopback harness can
/// exercise the real batching logic.
///
/// Draining, never waiting: this runs inside `server_loop`'s `select!`,
/// so every millisecond spent here is a millisecond `conn.recv()` is not
/// polled and an incoming mouse/keyboard event sits unhandled. Waiting
/// for follow-ups cost up to 50 ms of cursor lag on every batch — and
/// delayed the audio just as much, since the frame that triggered the
/// batch was held back for the same wait. The queue is empty at rest and
/// only fills when sends fall behind, which is exactly when amortizing
/// the DTLS send is worth anything.
pub(crate) async fn send_audio_batch(
    conn: &ArcConn,
    addr: SocketAddr,
    audio_rx: &mut Option<tokio::sync::mpsc::Receiver<(u32, u32, Vec<u8>)>>,
    first: (u32, u32, Vec<u8>),
    sent_audio_frames: &mut u64,
) {
    let mut batch = vec![first];
    if let Some(rx) = audio_rx {
        while batch.len() < AUDIO_BATCH_FRAMES {
            match rx.try_recv() {
                Ok(frame) => batch.push(frame),
                Err(_) => break,
            }
        }
    }
    let mut out = [0u8; MAX_DATAGRAM_SIZE];
    let refs = batch
        .iter()
        .map(|(seq, ts_ms, payload)| deskunion_proto::AudioFrameRef {
            seq: *seq,
            ts_ms: *ts_ms,
            payload,
        })
        .collect::<Vec<_>>();
    let dg = deskunion_proto::DatagramRef::AudioBatch(&refs);
    match deskunion_proto::encode_into(dg, &mut out) {
        Ok(len) => match conn.send(&out[..len]).await {
            Ok(_) => {
                let first = *sent_audio_frames == 0;
                *sent_audio_frames += batch.len() as u64;
                if first {
                    log::info!(
                        "audio stream started sending to {addr} in batches of up to {AUDIO_BATCH_FRAMES} frames"
                    );
                }
            }
            Err(error) => log::warn!("failed to send audio batch to {addr}: {error}"),
        },
        Err(deskunion_proto::ProtocolError::BufferTooSmall { .. }) => {
            // a full batch can overflow the datagram budget;
            // fall back to single-frame datagrams rather
            // than dropping ~60 ms of audio
            log::debug!(
                "audio batch of {} frames too large for {addr}; sending frames individually",
                batch.len()
            );
            for (seq, ts_ms, payload) in &batch {
                let dg = deskunion_proto::DatagramRef::Audio {
                    seq: *seq,
                    ts_ms: *ts_ms,
                    payload,
                };
                match deskunion_proto::encode_into(dg, &mut out) {
                    Ok(len) => match conn.send(&out[..len]).await {
                        Ok(_) => *sent_audio_frames += 1,
                        Err(error) => {
                            log::warn!("failed to send audio frame to {addr}: {error}")
                        }
                    },
                    Err(e) => {
                        log::warn!("failed to encode audio frame for {addr}: {e}")
                    }
                }
            }
        }
        Err(e) => log::warn!("failed to encode audio batch for {addr}: {e}"),
    }
}

/// the session loop of the dialed server connection. Audio flows
/// emulation -> capture (§3.1 of the audio plan): this is the emulation
/// side, so it's the sender. `audio_sender` is held only to keep the
/// stream alive for the loop's lifetime — dropping it stops capture.
async fn server_loop(
    shared: Rc<Shared>,
    addr: SocketAddr,
    conn: ArcConn,
    cert: Certificate,
    tx: Sender<ConnectionEvent>,
    #[cfg_attr(not(feature = "audio"), allow(unused_variables))]
    audio: crate::config::AudioSettings,
) {
    let mut buf = [0u8; MAX_DATAGRAM_SIZE];

    #[cfg(feature = "audio")]
    let (audio_sender, mut audio_rx) = match crate::audio::start_sender(&audio) {
        Some((sender, rx)) => {
            send_audio_start(&conn, addr).await;
            (Some(sender), Some(rx))
        }
        None => (None, None),
    };
    #[cfg(not(feature = "audio"))]
    let mut audio_rx: Option<tokio::sync::mpsc::Receiver<(u32, u32, Vec<u8>)>> = None;

    // retransmit `AudioControl::Start` while the stream is active —
    // the first one may have been lost in flight (unreliable UDP)
    let mut start_retransmit = tokio::time::interval(Duration::from_secs(2));
    start_retransmit.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    start_retransmit.tick().await; // consume the immediate first tick

    let mut sent_audio_frames = 0u64;

    loop {
        tokio::select! {
            result = conn.recv(&mut buf) => {
                let Ok(n) = result else { break };
                match decode(&buf[..n]) {
                    Ok(Datagram::Event(ProtoEvent::Hello { commit })) => {
                        // the server's Hello echo: version metadata only,
                        // consume it here so the emulation task never
                        // replies (which would ping-pong Hello forever)
                        log::debug!("server {addr} runs commit {}", String::from_utf8_lossy(&commit));
                    }
                    Ok(Datagram::Event(event)) => tx
                        .send(ConnectionEvent::Message { addr, event })
                        .expect("channel closed"),
                    Ok(Datagram::Audio { .. } | Datagram::AudioBatch(_) | Datagram::AudioControl(_)) => {
                        // audio is half-duplex in V1 (§9.7): the capture
                        // side never sends audio to the emulation side,
                        // so receiving one here means a peer running a
                        // future bidirectional-audio version — skip it,
                        // same forward-compat handling as an unknown
                        // event type. It still proves liveness.
                        tx.send(ConnectionEvent::PeerAlive { addr }).expect("channel closed");
                        log::debug!("ignoring unexpected audio datagram from {addr}");
                    }
                    Err(e) => {
                        // Skip the malformed/unknown datagram and keep
                        // the session. Each DTLS recv returns one full
                        // datagram, so a parse error here can't desync a
                        // stream; the next call gets a fresh, framed
                        // message. This makes the protocol forward-
                        // compatible: a peer running a newer Deskunion
                        // version can introduce additional event types
                        // and old peers will simply ignore them rather
                        // than dropping the connection.
                        tx.send(ConnectionEvent::PeerAlive { addr }).expect("channel closed");
                        log::debug!("ignoring undecodable event from {addr}: {e}");
                    }
                }
            }
            _ = start_retransmit.tick() => {
                #[cfg(feature = "audio")]
                if audio_sender.is_some() {
                    send_audio_start(&conn, addr).await;
                }
            }
            // `frame = ...` and not `Some(frame) = ...`: on `None` a
            // pattern-guarded arm is disabled for the rest of the loop,
            // so a dead encode thread would leave the session up and
            // permanently silent with nothing in the log
            frame = recv_audio_frame(&mut audio_rx) => match frame {
                Some(frame) => {
                    send_audio_batch(&conn, addr, &mut audio_rx, frame, &mut sent_audio_frames).await;
                }
                None => {
                    log::error!(
                        "audio capture for {addr} ended after {sent_audio_frames} frames; no more audio will be sent on this connection"
                    );
                    audio_rx = None;
                    #[cfg(feature = "audio")]
                    send_audio_stop(&conn, addr).await;
                }
            },
        }
    }
    log::info!("server disconnected {addr} after sending {sent_audio_frames} audio frames");
    // best-effort: tell the peer to tear its receiver down; dropping
    // `audio_sender` at the end of this scope stops the capture
    // backend and encoder thread with it
    #[cfg(feature = "audio")]
    if audio_sender.is_some() {
        send_audio_stop(&conn, addr).await;
    }
    {
        let mut current = shared.conn.lock().await;
        if matches!(current.as_ref(), Some((a, _)) if *a == addr) {
            current.take();
        }
    }
    tx.send(ConnectionEvent::Disconnected { addr })
        .expect("channel closed");
    // the reconnect loop redials while a server target is configured
    if shared.target.lock().await.is_some() {
        log::debug!("redialing server ...");
        ensure_connecting(shared, cert, tx, audio);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// the dialing socket must not sit on the wildcard: that is what
    /// makes Windows Defender ask for network permission on a
    /// client-mode machine that only dials out
    #[test]
    fn dial_socket_binds_the_routed_interface() {
        let v4 = local_bind_addr("127.0.0.1:4242".parse().expect("addr"));
        assert_eq!(v4.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(v4.port(), 0);
        assert!(!v4.ip().is_unspecified());
    }

    /// ... and it must match the peer's address family, which the old
    /// hardcoded `0.0.0.0:0` did not
    #[test]
    fn dial_socket_matches_the_peer_address_family() {
        let v6 = local_bind_addr("[::1]:4242".parse().expect("addr"));
        assert!(v6.is_ipv6(), "{v6} should be IPv6");
        assert_eq!(v6.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    }

    /// a connection that accepts every send and never receives
    #[derive(Default)]
    struct SinkConn;

    #[async_trait::async_trait]
    impl Conn for SinkConn {
        async fn connect(&self, _addr: SocketAddr) -> webrtc_util::Result<()> {
            Ok(())
        }
        async fn recv(&self, _buf: &mut [u8]) -> webrtc_util::Result<usize> {
            Err(webrtc_util::Error::ErrUseClosedNetworkConn)
        }
        async fn recv_from(&self, _buf: &mut [u8]) -> webrtc_util::Result<(usize, SocketAddr)> {
            Err(webrtc_util::Error::ErrUseClosedNetworkConn)
        }
        async fn send(&self, buf: &[u8]) -> webrtc_util::Result<usize> {
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

    /// batching runs inside `server_loop`'s `select!`, so it must never
    /// wait: while it does, `conn.recv()` is not polled and remote input
    /// events sit unhandled. Gathering used to block on two 25 ms
    /// timeouts, adding up to 50 ms of cursor lag to every batch.
    #[tokio::test]
    async fn batching_drains_the_queue_without_waiting_for_more() {
        let addr: SocketAddr = "127.0.0.1:4242".parse().expect("addr");
        let conn: ArcConn = Arc::new(SinkConn);
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        // one follow-up already queued, so a full batch of
        // AUDIO_BATCH_FRAMES is never reachable without waiting
        tx.send((1, 20, vec![0u8; 40])).await.expect("queue frame");
        let mut audio_rx = Some(rx);
        let mut sent = 0u64;

        let started = std::time::Instant::now();
        send_audio_batch(&conn, addr, &mut audio_rx, (0, 0, vec![0u8; 40]), &mut sent).await;
        let elapsed = started.elapsed();

        assert_eq!(sent, 2, "the queued follow-up should have been drained");
        assert!(
            elapsed < Duration::from_millis(10),
            "batching waited {elapsed:?} for frames that had not arrived"
        );
    }
}
