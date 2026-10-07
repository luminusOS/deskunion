use crate::{ConnectionError, FrontendEvent, FrontendRequest, IpcError};
use std::io;

#[cfg(unix)]
use std::{
    cmp::min,
    io::{BufReader, LineWriter, Lines, prelude::*},
    os::unix::net::UnixStream,
    thread,
    time::Duration,
};

pub struct FrontendEventReader {
    #[cfg(unix)]
    lines: Lines<BufReader<UnixStream>>,
    #[cfg(windows)]
    events: std::sync::mpsc::Receiver<Result<FrontendEvent, IpcError>>,
}

pub struct FrontendRequestWriter {
    #[cfg(unix)]
    line_writer: LineWriter<UnixStream>,
    #[cfg(windows)]
    requests: tokio::sync::mpsc::UnboundedSender<FrontendRequest>,
}

impl FrontendEventReader {
    #[cfg(unix)]
    pub fn next_event(&mut self) -> Option<Result<FrontendEvent, IpcError>> {
        match self.lines.next()? {
            Err(e) => Some(Err(e.into())),
            Ok(l) => Some(serde_json::from_str(l.as_str()).map_err(|e| e.into())),
        }
    }

    #[cfg(windows)]
    pub fn next_event(&mut self) -> Option<Result<FrontendEvent, IpcError>> {
        self.events.recv().ok()
    }
}

impl FrontendRequestWriter {
    #[cfg(unix)]
    pub fn request(&mut self, request: FrontendRequest) -> Result<(), io::Error> {
        let mut json = serde_json::to_string(&request).unwrap();
        log::debug!("requesting: {json}");
        json.push('\n');
        self.line_writer.write_all(json.as_bytes())?;
        Ok(())
    }

    /// queues the request for the pipe thread; never blocks the caller
    #[cfg(windows)]
    pub fn request(&mut self, request: FrontendRequest) -> Result<(), io::Error> {
        self.requests
            .send(request)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ipc thread has exited"))
    }
}

#[cfg(unix)]
pub fn connect() -> Result<(FrontendEventReader, FrontendRequestWriter), ConnectionError> {
    let rx = wait_for_service()?;
    let tx = rx.try_clone()?;
    let buf_reader = BufReader::new(rx);
    let lines = buf_reader.lines();
    let line_writer = LineWriter::new(tx);
    let reader = FrontendEventReader { lines };
    let writer = FrontendRequestWriter { line_writer };
    Ok((reader, writer))
}

/// A synchronous pipe handle serializes I/O per file object, so a pending
/// blocking read stalls every write on the duplicated handle. That froze
/// the GTK thread whenever the service had nothing to say. The pipe is
/// therefore driven by an overlapped (tokio) client on its own thread;
/// callers only touch channels and are never blocked by pipe I/O.
///
/// Returns immediately: requests queue until the service is online.
#[cfg(windows)]
pub fn connect() -> Result<(FrontendEventReader, FrontendRequestWriter), ConnectionError> {
    use futures::StreamExt;

    let (event_tx, events) = std::sync::mpsc::channel();
    let (requests, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    std::thread::Builder::new()
        .name("deskunion-ipc".into())
        .spawn(move || {
            runtime.block_on(async move {
                let Ok((mut reader, mut writer)) = crate::connect_async(None).await else {
                    return;
                };
                loop {
                    tokio::select! {
                        event = reader.next() => match event {
                            Some(event) => {
                                if event_tx.send(event).is_err() {
                                    break;
                                }
                            }
                            None => break,
                        },
                        request = request_rx.recv() => match request {
                            Some(request) => {
                                if let Err(e) = writer.request(request).await {
                                    log::error!("error sending message: {e}");
                                }
                            }
                            None => break,
                        },
                    }
                }
            })
        })?;
    Ok((
        FrontendEventReader { events },
        FrontendRequestWriter { requests },
    ))
}

/// wait for the deskunion socket to come online
#[cfg(unix)]
fn wait_for_service() -> Result<UnixStream, ConnectionError> {
    let socket_path = crate::default_socket_path()?;
    let mut duration = Duration::from_millis(10);
    loop {
        if let Ok(stream) = UnixStream::connect(&socket_path) {
            break Ok(stream);
        }
        // a signaling mechanism or inotify could be used to
        // improve this
        thread::sleep(exponential_back_off(&mut duration));
    }
}

#[cfg(unix)]
fn exponential_back_off(duration: &mut Duration) -> Duration {
    let new = duration.saturating_mul(2);
    *duration = min(new, Duration::from_secs(1));
    *duration
}
