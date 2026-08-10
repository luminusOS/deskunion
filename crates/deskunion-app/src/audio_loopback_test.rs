//! Loopback harness for the audio pipeline: a real [`AudioSender`]
//! (dummy capture) feeds the real batching logic from `connect.rs`
//! over an in-process datagram link into the real receive path from
//! `listen.rs` ([`AudioRxState`]) and a real [`AudioReceiver`] whose
//! dummy playback sink simulates an output device clock.
//!
//! The whole pipeline minus the OS capture/playback backends runs here:
//! encode thread -> bounded queue -> batching -> wire encode -> wire
//! decode -> jitter buffer -> playback callback. The dummy playback's
//! virtual device clock pulls the jitter buffer exactly like a real
//! output device does, which is what lets the harness see the "played
//! the first seconds, then went silent" field failure: a device pulling
//! faster than frames arrive drains the buffer and starts concealing
//! frames that were never lost.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use deskunion_audio::codec::{DEFAULT_BITRATE, SAMPLE_RATE};
use deskunion_audio::{AudioReceiver, AudioSender, capture, playback};
use deskunion_proto::{Datagram, MAX_DATAGRAM_SIZE, decode};
use tokio::sync::{Mutex, mpsc};
use tokio::task::spawn_local;
use webrtc_util::{Conn, Error, Result};

use crate::audio::EncodedFrame;
use crate::connect::{recv_audio_frame, send_audio_batch};
use crate::listen::AudioRxState;

/// how long the harness streams before asserting
const RUN_DURATION: Duration = Duration::from_secs(10);
/// same bound as `audio::AUDIO_QUEUE_FRAMES`
const QUEUE_FRAMES: usize = 8;

/// message-boundary-preserving in-process datagram link — stands in
/// for the DTLS connection (UDP also preserves datagram boundaries,
/// which the wire format relies on)
struct MockConn {
    rx: Mutex<mpsc::Receiver<Vec<u8>>>,
    tx: mpsc::Sender<Vec<u8>>,
    local: SocketAddr,
    remote: SocketAddr,
}

fn mock_conn_pair() -> (Arc<MockConn>, Arc<MockConn>) {
    let a_addr: SocketAddr = "127.0.0.1:41001".parse().expect("addr");
    let b_addr: SocketAddr = "127.0.0.1:41002".parse().expect("addr");
    let (a_tx, b_rx) = mpsc::channel(1024);
    let (b_tx, a_rx) = mpsc::channel(1024);
    (
        Arc::new(MockConn {
            rx: Mutex::new(a_rx),
            tx: a_tx,
            local: a_addr,
            remote: b_addr,
        }),
        Arc::new(MockConn {
            rx: Mutex::new(b_rx),
            tx: b_tx,
            local: b_addr,
            remote: a_addr,
        }),
    )
}

#[async_trait]
impl Conn for MockConn {
    async fn connect(&self, _addr: SocketAddr) -> Result<()> {
        Ok(())
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        let mut rx = self.rx.lock().await;
        match rx.recv().await {
            Some(data) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                Ok(n)
            }
            None => Err(Error::ErrUseClosedNetworkConn),
        }
    }

    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let n = self.recv(buf).await?;
        Ok((n, self.remote))
    }

    async fn send(&self, buf: &[u8]) -> Result<usize> {
        self.tx
            .send(buf.to_vec())
            .await
            .map_err(|_| Error::ErrUseClosedNetworkConn)?;
        Ok(buf.len())
    }

    async fn send_to(&self, buf: &[u8], _target: SocketAddr) -> Result<usize> {
        self.send(buf).await
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local)
    }

    fn remote_addr(&self) -> Option<SocketAddr> {
        Some(self.remote)
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }

    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }
}

fn test_audio_settings() -> crate::config::AudioSettings {
    crate::config::AudioSettings {
        send: true,
        receive: true,
        bitrate: DEFAULT_BITRATE as u32,
        buffer_ms: 80,
        capture_device: None,
        playback_device: None,
    }
}

#[tokio::test]
async fn audio_keeps_flowing_through_the_whole_pipeline() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let audio_settings = test_audio_settings();

            // sender: real AudioSender on the dummy capture backend,
            // feeding the bounded queue the same way
            // `audio::start_sender` does. Mono: the dummy capture's
            // channel count (the frame-rate mechanics under test are
            // channel-independent).
            let (frame_tx, frame_rx) = mpsc::channel::<EncodedFrame>(QUEUE_FRAMES);
            let mut sender = AudioSender::start(
                capture::Backend::Dummy,
                None,
                1,
                DEFAULT_BITRATE,
                Box::new(move |seq, ts_ms, payload: &[u8]| {
                    let _ = frame_tx.try_send((seq, ts_ms, payload.to_vec()));
                }),
            )
            .expect("start sender");

            let (client, server) = mock_conn_pair();
            let server_addr = client.local;
            let client: Arc<dyn Conn + Send + Sync> = client;
            let server: Arc<dyn Conn + Send + Sync> = server;

            // send task: the real batching logic from connect.rs
            let send_task = {
                let client = client.clone();
                spawn_local(async move {
                    let mut rx = Some(frame_rx);
                    let mut sent = 0u64;
                    while let Some(first) = recv_audio_frame(&mut rx).await {
                        send_audio_batch(&client, server_addr, &mut rx, first, &mut sent).await;
                    }
                    sent
                })
            };

            // receiver: real AudioReceiver over an inspectable dummy sink
            let dummy = playback::DummyPlayback::new();
            let metrics = dummy.metrics();
            let receiver = AudioReceiver::start_with(Box::new(dummy), None, SAMPLE_RATE, 1, 80)
                .expect("start receiver");
            let mut rx_state = AudioRxState::default();
            rx_state.receiver = Some(receiver);
            let (event_tx, _event_rx) = local_channel::mpsc::channel();

            // receive loop: real wire decode + the real listen.rs
            // receive path, with periodic stats sampling
            let started = Instant::now();
            let deadline = tokio::time::sleep(RUN_DURATION);
            tokio::pin!(deadline);
            let mut buf = [0u8; MAX_DATAGRAM_SIZE];
            let mut sample_tick = tokio::time::interval(Duration::from_millis(250));
            sample_tick.tick().await; // consume the immediate first tick
            let mut occupancy_samples = Vec::new();
            let mut mid_lost = None;
            let mut mid_ticks = None;
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    result = server.recv(&mut buf) => {
                        let n = result.expect("recv");
                        match decode(&buf[..n]).expect("decode") {
                            Datagram::Audio { seq, payload_range, .. } => rx_state.on_frames(
                                std::iter::once((seq, payload_range)),
                                &buf,
                                &audio_settings,
                                &event_tx,
                                server_addr,
                            ),
                            Datagram::AudioBatch(frames) => rx_state.on_frames(
                                frames.iter().map(|frame| (frame.seq, frame.payload_range.clone())),
                                &buf,
                                &audio_settings,
                                &event_tx,
                                server_addr,
                            ),
                            Datagram::AudioControl(cmd) => {
                                rx_state.on_control(cmd, &audio_settings, &event_tx, server_addr)
                            }
                            Datagram::Event(_) => {}
                        }
                    }
                    _ = sample_tick.tick() => {
                        let receiver = rx_state.receiver.as_ref().expect("receiver");
                        occupancy_samples.push(receiver.occupancy());
                        if mid_lost.is_none() && started.elapsed() >= RUN_DURATION / 2 {
                            mid_lost = Some(receiver.stats().packets_lost);
                            mid_ticks = Some(metrics.clock_ticks());
                        }
                    }
                }
            }

            send_task.abort();
            sender.stop();

            let receiver = rx_state.receiver.as_ref().expect("receiver");
            let stats = receiver.stats();

            // ~50 frames/s over the whole run must have arrived (the
            // in-process link has no loss and the queue must not drop)
            let expected_frames = RUN_DURATION.as_secs() * 1000 / 20;
            assert!(
                rx_state.received() >= expected_frames * 9 / 10,
                "expected ~{expected_frames} frames, receiver saw {}",
                rx_state.received()
            );

            // no frame may be concealed as lost on a lossless link
            assert_eq!(stats.packets_lost, 0, "no packet loss on a lossless link");

            // drift correction must keep the jitter buffer near its
            // target instead of ratcheting to the window cap
            let late = &occupancy_samples[occupancy_samples.len() / 2..];
            let max_late = late.iter().copied().max().unwrap_or(0);
            assert!(
                max_late <= 8,
                "occupancy must stay bounded near the 4-frame target, peaked at {max_late}"
            );

            // the actual field failure: playback going quiet while the
            // link stays healthy. With the device pulling, that shows up
            // as concealment of frames that were never lost — measured
            // over the second half so startup/prebuffer is excluded.
            let ticks = metrics.clock_ticks() - mid_ticks.expect("mid sample");
            assert!(ticks > 0, "the virtual device must keep pulling");
            assert_eq!(
                stats.packets_lost,
                mid_lost.expect("mid sample"),
                "playback starved: frames were concealed in the second half of the run"
            );

            // and the device must have pulled real audio for the whole
            // run, not stopped part-way
            let pulled = *metrics.samples_received().lock().expect("lock");
            let expected_samples = metrics.clock_ticks() as usize * 480;
            assert_eq!(
                pulled,
                expected_samples,
                "the device pulled {pulled} samples over {} ticks",
                metrics.clock_ticks()
            );
        })
        .await;
}
