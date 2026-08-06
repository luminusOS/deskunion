use deskunion_proto::{Datagram, MAX_DATAGRAM_SIZE, MAX_EVENT_SIZE, ProtoEvent, decode};
use futures::{Stream, StreamExt};
use local_channel::mpsc::{Receiver, Sender, channel};
use rustls::pki_types::CertificateDer;
use std::{
    collections::{HashMap, VecDeque},
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
    conn::DTLSConn,
    crypto::Certificate,
    listener::listen,
};
use webrtc_util::{Conn, Error, conn::Listener};

use crate::crypto;

#[derive(Error, Debug)]
pub enum ListenerCreationError {
    #[error(transparent)]
    WebrtcUtil(#[from] webrtc_util::Error),
    #[error(transparent)]
    WebrtcDtls(#[from] webrtc_dtls::Error),
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
}

pub(crate) struct DeskunionListener {
    listen_rx: Receiver<ListenEvent>,
    listen_tx: Sender<ListenEvent>,
    listen_task: JoinHandle<()>,
    conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>>,
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

        let conns_clone = conns.clone();
        let listen_task: JoinHandle<()> = {
            let listen_tx = listen_tx.clone();
            let connection_attempts = connection_attempts.clone();
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
                                let dtls_conn: &DTLSConn = conn.as_any().downcast_ref().expect("dtls conn");
                                let certs = dtls_conn.connection_state().await.peer_certificates;
                                let cert = certs.first().expect("cert");
                                let fingerprint = crypto::generate_fingerprint(cert);
                                listen_tx.send(ListenEvent::Accept { addr, fingerprint }).expect("channel closed");
                                spawn_local(read_loop(conns_clone.clone(), addr, conn, listen_tx.clone(), audio.clone()));
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

    pub(crate) async fn get_certificate_fingerprint(&self, addr: SocketAddr) -> Option<String> {
        if let Some(conn) = self
            .conns
            .lock()
            .await
            .iter()
            .find(|(a, _)| *a == addr)
            .map(|(_, c)| c.clone())
        {
            let conn: &DTLSConn = conn.as_any().downcast_ref().expect("dtls conn");
            let certs = conn.connection_state().await.peer_certificates;
            let cert = certs.first()?;
            let fingerprint = crypto::generate_fingerprint(cert);
            Some(fingerprint)
        } else {
            None
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

/// awaits the next queued audio frame, or never resolves if there's no
/// sender (audio disabled/unavailable) — lets `read_loop`'s `select!`
/// stay a single unconditional shape regardless of the `audio` feature
/// or runtime `audio.send` setting.
async fn recv_audio_frame(
    rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<(u32, u32, Vec<u8>)>>,
) -> Option<(u32, u32, Vec<u8>)> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn read_loop(
    conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>>,
    addr: SocketAddr,
    conn: ArcConn,
    dtls_tx: Sender<ListenEvent>,
    #[cfg_attr(not(feature = "audio"), allow(unused_variables))]
    audio: crate::config::AudioSettings,
) -> Result<(), Error> {
    let mut buf = [0u8; MAX_DATAGRAM_SIZE];

    // Audio flows emulation -> capture (§3.1 of the audio plan): this
    // is the emulation side, so it's the sender. `_audio_sender` is
    // held only to keep the stream alive for the loop's lifetime —
    // dropping it stops capture.
    #[cfg(feature = "audio")]
    let (_audio_sender, mut audio_rx) = match crate::audio::start_sender(&audio) {
        Some((sender, rx)) => {
            let mut out = [0u8; MAX_DATAGRAM_SIZE];
            if let Ok(len) = deskunion_proto::encode_into(
                deskunion_proto::DatagramRef::AudioControl(
                    deskunion_proto::AudioControlCmd::Start {
                        sample_rate: deskunion_audio::codec::SAMPLE_RATE,
                        channels: crate::audio::WIRE_CHANNELS as u8,
                    },
                ),
                &mut out,
            ) {
                let _ = conn.send(&out[..len]).await;
            }
            (Some(sender), Some(rx))
        }
        None => (None, None),
    };
    #[cfg(not(feature = "audio"))]
    let mut audio_rx: Option<tokio::sync::mpsc::UnboundedReceiver<(u32, u32, Vec<u8>)>> = None;

    loop {
        tokio::select! {
            result = conn.recv(&mut buf) => {
                let Ok(n) = result else { break };
                match decode(&buf[..n]) {
                    Ok(Datagram::Event(event)) => dtls_tx
                        .send(ListenEvent::Msg { event, addr })
                        .expect("channel closed"),
                    Ok(Datagram::Audio { .. } | Datagram::AudioControl(_)) => {
                        // audio is half-duplex in V1 (§9.7): the capture
                        // side never sends audio to the emulation side,
                        // so receiving one here means a peer running a
                        // future bidirectional-audio version — skip it,
                        // same forward-compat handling as an unknown
                        // event type.
                        log::debug!("ignoring unexpected audio datagram from {addr}");
                    }
                    Err(e) => {
                        // Skip the malformed/unknown datagram and keep
                        // listening. Each DTLS recv returns one full
                        // datagram, so a parse error here can't desync a
                        // stream; the next call gets a fresh, framed
                        // message. This makes the protocol forward-
                        // compatible: a peer running a newer Deskunion
                        // version can introduce additional event types
                        // and old peers will simply ignore them rather
                        // than dropping the connection.
                        log::debug!("ignoring undecodable event from {addr}: {e}");
                    }
                }
            }
            Some((seq, ts_ms, payload)) = recv_audio_frame(&mut audio_rx) => {
                let mut out = [0u8; MAX_DATAGRAM_SIZE];
                let dg = deskunion_proto::DatagramRef::Audio { seq, ts_ms, payload: &payload };
                match deskunion_proto::encode_into(dg, &mut out) {
                    Ok(len) => { let _ = conn.send(&out[..len]).await; }
                    Err(e) => log::debug!("failed to encode audio frame for {addr}: {e}"),
                }
            }
        }
    }
    log::info!("dtls client disconnected {addr:?}");
    let mut conns = conns.lock().await;
    let index = conns
        .iter()
        .position(|(a, _)| *a == addr)
        .expect("connection not found");
    conns.remove(index);
    Ok(())
}
