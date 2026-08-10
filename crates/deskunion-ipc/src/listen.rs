use futures::{Stream, StreamExt, stream::SelectAll};
#[cfg(unix)]
use std::path::PathBuf;
use std::{
    io::ErrorKind,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio_stream::wrappers::LinesStream;

#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::{FrontendEvent, FrontendRequest, IpcError, IpcListenerCreationError};

/// wait for a frontend on `server` and hand back the connected instance
/// together with a fresh one for the next connection. A named pipe has
/// no poll-based accept, so the listener keeps this future around and
/// polls it from its `Stream` impl instead of running an accept task.
#[cfg(windows)]
async fn accept_pipe(
    server: NamedPipeServer,
) -> std::io::Result<(NamedPipeServer, NamedPipeServer)> {
    server.connect().await?;
    let next = ServerOptions::new().create(crate::DESKUNION_PIPE_NAME)?;
    Ok((server, next))
}

#[cfg(windows)]
type PipeAccept =
    Pin<Box<dyn std::future::Future<Output = std::io::Result<(NamedPipeServer, NamedPipeServer)>>>>;

pub struct AsyncFrontendListener {
    #[cfg(windows)]
    accept: PipeAccept,
    #[cfg(unix)]
    listener: UnixListener,
    #[cfg(unix)]
    socket_path: PathBuf,
    #[cfg(unix)]
    line_streams: SelectAll<LinesStream<BufReader<ReadHalf<UnixStream>>>>,
    #[cfg(windows)]
    line_streams: SelectAll<LinesStream<BufReader<ReadHalf<NamedPipeServer>>>>,
    #[cfg(unix)]
    tx_streams: Vec<WriteHalf<UnixStream>>,
    #[cfg(windows)]
    tx_streams: Vec<WriteHalf<NamedPipeServer>>,
}

impl AsyncFrontendListener {
    pub async fn new() -> Result<Self, IpcListenerCreationError> {
        #[cfg(unix)]
        let (socket_path, listener) = {
            let socket_path = crate::default_socket_path()?;

            log::debug!("remove socket: {socket_path:?}");
            if socket_path.exists() {
                // try to connect to see if some other instance
                // of deskunion is already running
                match UnixStream::connect(&socket_path).await {
                    // connected -> deskunion is already running
                    Ok(_) => return Err(IpcListenerCreationError::AlreadyRunning),
                    // deskunion is not running but a socket was left behind
                    Err(e) => {
                        log::debug!("{socket_path:?}: {e} - removing left behind socket");
                        let _ = std::fs::remove_file(&socket_path);
                    }
                }
            }
            let listener = match UnixListener::bind(&socket_path) {
                Ok(ls) => ls,
                // some other deskunion instance has bound the socket in the meantime
                Err(e) if e.kind() == ErrorKind::AddrInUse => {
                    return Err(IpcListenerCreationError::AlreadyRunning);
                }
                Err(e) => return Err(IpcListenerCreationError::Bind(e)),
            };
            (socket_path, listener)
        };

        // a named pipe serves one client per instance: creating the
        // first one reserves the name for this process, and every
        // accepted connection is replaced by a fresh instance for the
        // next frontend
        #[cfg(windows)]
        let accept = {
            let server = match ServerOptions::new()
                .first_pipe_instance(true)
                .create(crate::DESKUNION_PIPE_NAME)
            {
                Ok(server) => server,
                // another deskunion instance owns the pipe name
                Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                    return Err(IpcListenerCreationError::AlreadyRunning);
                }
                Err(e) => return Err(IpcListenerCreationError::Bind(e)),
            };
            Box::pin(accept_pipe(server))
        };

        let adapter = Self {
            #[cfg(unix)]
            listener,
            #[cfg(windows)]
            accept,
            #[cfg(unix)]
            socket_path,
            line_streams: SelectAll::new(),
            tx_streams: vec![],
        };

        Ok(adapter)
    }

    pub async fn broadcast(&mut self, notify: FrontendEvent) {
        // encode event
        let mut json = serde_json::to_string(&notify).unwrap();
        json.push('\n');

        let mut keep = vec![];
        // TODO do simultaneously
        for tx in self.tx_streams.iter_mut() {
            // write len + payload
            if tx.write(json.as_bytes()).await.is_err() {
                keep.push(false);
                continue;
            }
            keep.push(true);
        }

        // could not find a better solution because async
        let mut keep = keep.into_iter();
        self.tx_streams.retain(|_| keep.next().unwrap());
    }
}

#[cfg(unix)]
impl Drop for AsyncFrontendListener {
    fn drop(&mut self) {
        log::debug!("remove socket: {:?}", self.socket_path);
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Stream for AsyncFrontendListener {
    type Item = Result<FrontendRequest, IpcError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Poll::Ready(Some(Ok(l))) = self.line_streams.poll_next_unpin(cx) {
            let request = serde_json::from_str(l.as_str()).map_err(|e| e.into());
            return Poll::Ready(Some(request));
        }
        let mut sync = false;
        #[cfg(unix)]
        while let Poll::Ready(Ok((stream, _))) = self.listener.poll_accept(cx) {
            let (rx, tx) = tokio::io::split(stream);
            let buf_reader = BufReader::new(rx);
            let lines = buf_reader.lines();
            let lines = LinesStream::new(lines);
            self.line_streams.push(lines);
            self.tx_streams.push(tx);
            sync = true;
        }
        #[cfg(windows)]
        while let Poll::Ready(accepted) = self.accept.as_mut().poll(cx) {
            let (stream, next) = match accepted {
                Ok(accepted) => accepted,
                Err(e) => {
                    // the pipe name is gone; park the slot on a future
                    // that never resolves so this arm stops polling a
                    // finished future
                    log::warn!("frontend pipe accept failed: {e}");
                    self.accept = Box::pin(std::future::pending());
                    break;
                }
            };
            self.accept = Box::pin(accept_pipe(next));
            let (rx, tx) = tokio::io::split(stream);
            let buf_reader = BufReader::new(rx);
            let lines = buf_reader.lines();
            let lines = LinesStream::new(lines);
            self.line_streams.push(lines);
            self.tx_streams.push(tx);
            sync = true;
        }
        if sync {
            Poll::Ready(Some(Ok(FrontendRequest::Sync)))
        } else {
            Poll::Pending
        }
    }
}
