use ashpd::{
    desktop::{
        Session,
        clipboard::{Clipboard, RequestClipboardOptions, SetSelectionOptions},
        input_capture::{
            Activated, ActivatedBarrier, Barrier, BarrierID, Capabilities, CreateSession2Options,
            CreateSessionOptions, InputCapture, Region, ReleaseOptions, StartOptions, Zones,
        },
    },
    enumflags2::BitFlags,
};
use async_trait::async_trait;
use futures::{FutureExt, StreamExt};
use reis::{
    ei::{self, handshake::ContextType},
    event::{Connection, DeviceCapability, EiEvent},
    tokio::EiConvertEventStream,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io,
    num::NonZeroU32,
    os::unix::net::UnixStream,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    fs::File as AsyncFile,
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{
        Notify, Semaphore,
        mpsc::{self, Receiver, Sender},
    },
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use futures_core::Stream;

use input_event::Event;

use crate::CaptureEvent;

const MAX_CLIPBOARD_TEXT_BYTES: usize = 64 * 1024;

use super::{
    Capture as DeskunionInputCapture, Position,
    error::{CaptureError, LibeiCaptureCreationError},
};

/* there is a bug in xdg-remote-desktop-portal-gnome / mutter that
 * prevents receiving further events after a session has been disabled once.
 * Therefore the session needs to be recreated when the barriers are updated */

/// events that necessitate restarting the capture session
#[derive(Clone, Copy, Debug)]
enum LibeiNotifyEvent {
    Create(Position),
    Destroy(Position),
}

struct CaptureSessionSetup {
    session: Session<InputCapture>,
    clipboard_enabled: bool,
}

#[allow(dead_code)]
pub struct LibeiInputCapture {
    input_capture: Pin<Box<InputCapture>>,
    capture_task: JoinHandle<Result<(), CaptureError>>,
    event_rx: Receiver<(Position, CaptureEvent)>,
    clipboard_event_rx: Option<Receiver<(Position, String)>>,
    notify_capture: Sender<LibeiNotifyEvent>,
    notify_release: Arc<Notify>,
    clipboard_tx: Sender<ClipboardCommand>,
    cancellation_token: CancellationToken,
    terminated: bool,
}

enum ClipboardCommand {
    SetText(String),
}

struct ClipboardCapture {
    commands: Receiver<ClipboardCommand>,
    events: Sender<(Position, String)>,
    requested: bool,
    enabled: bool,
}

struct CaptureSessionEvents<'a> {
    input: &'a Sender<(Position, CaptureEvent)>,
    clipboard: &'a mut ClipboardCapture,
}

/// returns (start pos, end pos), inclusive
fn pos_to_barrier(r: &Region, pos: Position) -> (i32, i32, i32, i32) {
    let (x, y) = (r.x_offset(), r.y_offset());
    let (w, h) = (r.width() as i32, r.height() as i32);
    match pos {
        Position::Left => (x, y, x, y + h - 1),
        Position::Right => (x + w, y, x + w, y + h - 1),
        Position::Top => (x, y, x + w - 1, y),
        Position::Bottom => (x, y + h, x + w - 1, y + h),
    }
}

/// Ashpd does not expose fields
#[derive(Clone, Copy, Debug)]
struct ICBarrier {
    barrier_id: BarrierID,
    position: (i32, i32, i32, i32),
}

impl ICBarrier {
    fn new(barrier_id: BarrierID, position: (i32, i32, i32, i32)) -> Self {
        Self {
            barrier_id,
            position,
        }
    }
}

impl From<ICBarrier> for Barrier {
    fn from(barrier: ICBarrier) -> Self {
        Barrier::new(barrier.barrier_id, barrier.position)
    }
}

fn select_barriers(
    zones: &Zones,
    clients: &[Position],
    next_barrier_id: &mut NonZeroU32,
) -> (Vec<ICBarrier>, HashMap<BarrierID, Position>) {
    let mut pos_for_barrier = HashMap::new();
    let mut barriers: Vec<ICBarrier> = vec![];

    for pos in clients {
        let mut client_barriers = zones
            .regions()
            .iter()
            .map(|r| {
                let id = *next_barrier_id;
                *next_barrier_id = next_barrier_id
                    .checked_add(1)
                    .expect("barrier id out of range");
                let position = pos_to_barrier(r, *pos);
                pos_for_barrier.insert(id, *pos);
                ICBarrier::new(id, position)
            })
            .collect();
        barriers.append(&mut client_barriers);
    }
    (barriers, pos_for_barrier)
}

async fn update_barriers(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
    active_clients: &[Position],
    next_barrier_id: &mut NonZeroU32,
) -> Result<(Vec<ICBarrier>, HashMap<BarrierID, Position>), ashpd::Error> {
    let zones = input_capture
        .zones(session, Default::default())
        .await?
        .response()?;
    log::debug!("zones: {zones:?}");

    let (barriers, id_map) = select_barriers(&zones, active_clients, next_barrier_id);
    log::debug!("barriers: {barriers:?}");
    log::debug!("client for barrier id: {id_map:?}");

    let ashpd_barriers: Vec<Barrier> = barriers.iter().copied().map(|b| b.into()).collect();
    let response = input_capture
        .set_pointer_barriers(
            session,
            &ashpd_barriers,
            zones.zone_set(),
            Default::default(),
        )
        .await?;
    let response = response.response()?;
    log::debug!("{response:?}");
    Ok((barriers, id_map))
}

async fn create_session(
    input_capture: &InputCapture,
    clipboard_requested: bool,
) -> std::result::Result<CaptureSessionSetup, ashpd::Error> {
    log::debug!("creating input capture session");
    if !supports_create_session2(input_capture.version()) {
        return create_legacy_session(input_capture, clipboard_requested).await;
    }
    match input_capture
        .create_session2(CreateSession2Options::default())
        .await
    {
        Ok(session) => {
            // Portal v2 requires clipboard authorization before Start.
            let clipboard_requested = if clipboard_requested {
                request_clipboard(&session).await
            } else {
                false
            };
            let start_options = StartOptions::default().set_capabilities(requested_capabilities());
            let response = input_capture
                .start(&session, None, start_options)
                .await?
                .response()?;
            let clipboard_enabled =
                clipboard_available(clipboard_requested, response.is_clipboard_enabled());
            if clipboard_requested && !clipboard_enabled {
                log::info!("input capture portal did not grant clipboard access");
            }
            Ok(CaptureSessionSetup {
                session,
                clipboard_enabled,
            })
        }
        Err(ashpd::Error::RequiresVersion(_, _)) => {
            create_legacy_session(input_capture, clipboard_requested).await
        }
        Err(error) => Err(error),
    }
}

fn supports_create_session2(version: u32) -> bool {
    version >= 2
}

fn clipboard_available(requested: bool, granted: bool) -> bool {
    requested && granted
}

async fn create_legacy_session(
    input_capture: &InputCapture,
    clipboard_requested: bool,
) -> std::result::Result<CaptureSessionSetup, ashpd::Error> {
    if clipboard_requested {
        log::info!("input capture portal v1 does not support clipboard; disabling clipboard sync");
    }
    let options = CreateSessionOptions::default().set_capabilities(requested_capabilities());
    let (session, _capabilities) = input_capture.create_session(None, options).await?;
    Ok(CaptureSessionSetup {
        session,
        clipboard_enabled: false,
    })
}

fn requested_capabilities() -> BitFlags<Capabilities> {
    Capabilities::Keyboard | Capabilities::Pointer | Capabilities::Touchscreen
}

async fn request_clipboard(session: &Session<InputCapture>) -> bool {
    let clipboard = match Clipboard::new().await {
        Ok(clipboard) => clipboard,
        Err(error) => {
            log::warn!("portal clipboard is unavailable: {error}");
            return false;
        }
    };
    match clipboard
        .request(session, RequestClipboardOptions::default())
        .await
    {
        Ok(()) => true,
        Err(error) => {
            log::warn!("clipboard access request failed; continuing input capture: {error}");
            false
        }
    }
}

async fn connect_to_eis(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
) -> Result<(ei::Context, Connection, EiConvertEventStream), CaptureError> {
    log::debug!("connect_to_eis");
    let fd = input_capture
        .connect_to_eis(session, Default::default())
        .await?;

    // create unix stream from fd
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(true)?;

    // create ei context
    let context = ei::Context::new(stream)?;
    let (conn, event_stream) = context
        .handshake_tokio("io.github.luminusos.DeskUnion", ContextType::Receiver)
        .await?;

    Ok((context, conn, event_stream))
}

async fn libei_event_handler(
    mut ei_event_stream: EiConvertEventStream,
    context: ei::Context,
    event_tx: Sender<(Position, CaptureEvent)>,
    release_session: Arc<Notify>,
    current_pos: Rc<Cell<Option<Position>>>,
) -> Result<(), CaptureError> {
    loop {
        let ei_event = ei_event_stream
            .next()
            .await
            .ok_or(CaptureError::EndOfStream)??;
        log::trace!("from ei: {ei_event:?}");
        let client = current_pos.get();
        handle_ei_event(ei_event, client, &context, &event_tx, &release_session).await?;
    }
}

impl LibeiInputCapture {
    pub async fn new(
        clipboard_enabled: bool,
    ) -> std::result::Result<Self, LibeiCaptureCreationError> {
        let input_capture = Box::pin(InputCapture::new().await?);
        let input_capture_ptr = input_capture.as_ref().get_ref() as *const InputCapture;
        let first_session =
            Some(create_session(unsafe { &*input_capture_ptr }, clipboard_enabled).await?);

        let (event_tx, event_rx) = mpsc::channel(1);
        let (clipboard_event_tx, clipboard_event_rx) = mpsc::channel(1);
        let (notify_capture, notify_rx) = mpsc::channel(1);
        let (clipboard_tx, clipboard_rx) = mpsc::channel(4);
        let notify_release = Arc::new(Notify::new());

        let cancellation_token = CancellationToken::new();

        let capture = do_capture(
            input_capture_ptr,
            notify_rx,
            notify_release.clone(),
            first_session,
            event_tx,
            ClipboardCapture {
                commands: clipboard_rx,
                events: clipboard_event_tx,
                requested: clipboard_enabled,
                enabled: clipboard_enabled,
            },
            cancellation_token.clone(),
        );
        let capture_task = tokio::task::spawn_local(capture);

        let producer = Self {
            input_capture,
            event_rx,
            clipboard_event_rx: Some(clipboard_event_rx),
            capture_task,
            notify_capture,
            notify_release,
            clipboard_tx,
            cancellation_token,
            terminated: false,
        };

        Ok(producer)
    }
}

async fn do_capture(
    input_capture: *const InputCapture,
    mut capture_event: Receiver<LibeiNotifyEvent>,
    notify_release: Arc<Notify>,
    mut session: Option<CaptureSessionSetup>,
    event_tx: Sender<(Position, CaptureEvent)>,
    mut clipboard: ClipboardCapture,
    cancellation_token: CancellationToken,
) -> Result<(), CaptureError> {
    /* safety: libei_task does not outlive Self */
    let input_capture = unsafe { &*input_capture };
    let mut active_clients: Vec<Position> = vec![];
    let mut next_barrier_id = NonZeroU32::new(1).expect("id must be non-zero");

    let mut zones_changed = input_capture.receive_zones_changed().await?;

    loop {
        // do capture session
        let cancel_session = CancellationToken::new();
        let cancel_update = CancellationToken::new();

        let mut capture_event_occured: Option<LibeiNotifyEvent> = None;
        let mut zones_have_changed = false;

        // kill session if clients need to be updated
        let handle_session_update_request = async {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    log::debug!("cancelled")
                }, /* exit requested */
                _ = cancel_update.cancelled() => {
                    log::debug!("update task cancelled");
                }, /* session exited */
                _ = zones_changed.next() => {
                    log::debug!("zones changed!");
                    zones_have_changed = true
                }, /* zones have changed */
                e = capture_event.recv() => if let Some(e) = e { /* clients changed */
                    log::debug!("capture event: {e:?}");
                    capture_event_occured.replace(e);
                },
            }
            // kill session (might already be dead!)
            log::debug!("=> cancelling session");
            cancel_session.cancel();
        };

        if !active_clients.is_empty() {
            // create session
            let setup = match session.take() {
                Some(setup) => setup,
                None => create_session(input_capture, clipboard.requested).await?,
            };
            clipboard.enabled = setup.clipboard_enabled;
            let mut session = setup.session;

            let mut events = CaptureSessionEvents {
                input: &event_tx,
                clipboard: &mut clipboard,
            };
            let capture_session = do_capture_session(
                input_capture,
                &mut session,
                &mut events,
                &active_clients,
                &mut next_barrier_id,
                &notify_release,
                (cancel_session.clone(), cancel_update.clone()),
            );

            let (capture_result, ()) = tokio::join!(capture_session, handle_session_update_request);
            log::debug!("capture session + session_update task done!");

            // disable capture
            log::debug!("disabling input capture");
            if let Err(e) = input_capture.disable(&session, Default::default()).await {
                log::warn!("input_capture.disable(&session) {e}");
            }
            if let Err(e) = session.close().await {
                log::warn!("session.close(): {e}");
            }

            // propagate error from capture session
            capture_result?;
        } else {
            handle_session_update_request.await;
        }

        // update clients if requested
        if let Some(event) = capture_event_occured.take() {
            match event {
                LibeiNotifyEvent::Create(p) => active_clients.push(p),
                LibeiNotifyEvent::Destroy(p) => active_clients.retain(|&pos| pos != p),
            }
        }

        // break
        if cancellation_token.is_cancelled() {
            break Ok(());
        }
    }
}

async fn do_capture_session(
    input_capture: &InputCapture,
    session: &mut Session<InputCapture>,
    events: &mut CaptureSessionEvents<'_>,
    active_clients: &[Position],
    next_barrier_id: &mut NonZeroU32,
    notify_release: &Notify,
    cancel: (CancellationToken, CancellationToken),
) -> Result<(), CaptureError> {
    let (cancel_session, cancel_update) = cancel;
    let event_tx = events.input;
    let clipboard_enabled = events.clipboard.enabled;
    let clipboard_rx = &mut events.clipboard.commands;
    // current client
    let current_pos = Rc::new(Cell::new(None));

    // connect to eis server
    let (context, _conn, ei_event_stream) = connect_to_eis(input_capture, session).await?;

    // set barriers
    let (barriers, pos_for_barrier_id) =
        update_barriers(input_capture, session, active_clients, next_barrier_id).await?;

    log::debug!("enabling session");
    input_capture.enable(session, Default::default()).await?;

    let clipboard = if clipboard_enabled {
        match Clipboard::new().await {
            Ok(clipboard) => Some(Arc::new(clipboard)),
            Err(error) => {
                log::warn!("portal clipboard is unavailable; continuing input capture: {error}");
                None
            }
        }
    } else {
        None
    };

    // cancellation token to release session
    let release_session = Arc::new(Notify::new());
    let clipboard_cancellation = CancellationToken::new();

    // async event task
    let cancel_ei_handler = CancellationToken::new();
    let event_chan = (*event_tx).clone();
    let pos = current_pos.clone();
    let cancel_session_clone = cancel_session.clone();
    let release_session_clone = release_session.clone();
    let cancel_ei_handler_clone = cancel_ei_handler.clone();
    let ei_task = async move {
        tokio::select! {
            r = libei_event_handler(
                ei_event_stream,
                context,
                event_chan,
                release_session_clone,
                pos,
            ) => {
                log::debug!("libei exited: {r:?} cancelling session task");
                cancel_session_clone.cancel();
            }
            _ = cancel_ei_handler_clone.cancelled() => {},
        }
        Ok::<(), CaptureError>(())
    };

    let capture_session_task = async {
        // receiver for activation tokens
        let mut activated = input_capture.receive_activated().await?;
        let mut owner_changed = match clipboard.as_ref() {
            Some(clipboard) => match clipboard
                .receive_selection_owner_changed::<InputCapture>()
                .await
            {
                Ok(stream) => Some(Box::pin(stream)),
                Err(error) => {
                    log::warn!("portal clipboard owner notifications unavailable: {error}");
                    None
                }
            },
            None => None,
        };
        let mut selection_transfer = match clipboard.as_ref() {
            Some(clipboard) => match clipboard.receive_selection_transfer::<InputCapture>().await {
                Ok(stream) => Some(Box::pin(stream)),
                Err(error) => {
                    log::warn!("portal clipboard transfer notifications unavailable: {error}");
                    None
                }
            },
            None => None,
        };
        let mut clipboard_active = owner_changed.is_some() && selection_transfer.is_some();
        let remote_text = Rc::new(RefCell::new(None::<String>));
        let local_text = Rc::new(RefCell::new(None::<String>));
        let clipboard_revision = Rc::new(Cell::new(0u64));
        let local_read_pending =
            Rc::new(RefCell::new(None::<(Session<InputCapture>, String, u64)>));
        let local_read_notify = Arc::new(Notify::new());
        let transfer_slots = Arc::new(Semaphore::new(4));
        let rejection_slots = Arc::new(Semaphore::new(4));
        if clipboard_active {
            if let Some(clipboard) = clipboard.clone() {
                let pending = local_read_pending.clone();
                let notify = local_read_notify.clone();
                let local_text = local_text.clone();
                let clipboard_revision = clipboard_revision.clone();
                let current_pos = current_pos.clone();
                let clipboard_event_tx = events.clipboard.events.clone();
                let cancellation = clipboard_cancellation.child_token();
                tokio::task::spawn_local(async move {
                    loop {
                        tokio::select! {
                            _ = cancellation.cancelled() => break,
                            _ = notify.notified() => {},
                        }
                        loop {
                            let request = pending.borrow_mut().take();
                            let Some((owner_session, mime_type, revision)) = request else {
                                break;
                            };
                            let Some(text) = read_portal_text(
                                &clipboard,
                                &owner_session,
                                &mime_type,
                                &cancellation,
                            )
                            .await
                            else {
                                continue;
                            };
                            if clipboard_revision.get() != revision {
                                continue;
                            }
                            *local_text.borrow_mut() = Some(text.clone());
                            if let Some(pos) = current_pos.get() {
                                tokio::select! {
                                    _ = cancellation.cancelled() => break,
                                    result = clipboard_event_tx.send((pos, text)) => {
                                        if result.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            }
        }
        let set_selection_worker = async {
            loop {
                let Some(ClipboardCommand::SetText(text)) = clipboard_rx.recv().await else {
                    return;
                };
                if text.len() > MAX_CLIPBOARD_TEXT_BYTES || text.contains('\0') {
                    continue;
                }
                let _ = local_text.borrow_mut().take();
                let _ = local_read_pending.borrow_mut().take();
                clipboard_revision.set(clipboard_revision.get().wrapping_add(1));
                *remote_text.borrow_mut() = Some(text);
                if let Some(clipboard) = clipboard.as_ref() {
                    let options = SetSelectionOptions::default()
                        .set_mime_types(&["text/plain;charset=utf-8", "text/plain"]);
                    let result = tokio::select! {
                        _ = clipboard_cancellation.cancelled() => return,
                        result = clipboard.set_selection(session, options) => result,
                    };
                    if let Err(error) = result {
                        log::debug!("failed to publish remote clipboard text: {error}");
                    }
                }
            }
        };
        tokio::pin!(set_selection_worker);
        let mut ei_devices_changed = false;
        loop {
            tokio::select! {
                changed = async {
                    match owner_changed.as_mut() {
                        Some(stream) => stream.next().await,
                        None => futures::future::pending().await,
                    }
                } => {
                    if let Some((owner_session, details)) = changed {
                        let matching_session = format!("{owner_session:?}") == format!("{session:?}");
                        if clipboard_active && matching_session {
                            let revision = clipboard_revision.get().wrapping_add(1);
                            clipboard_revision.set(revision);
                            let _ = local_text.borrow_mut().take();
                            if details.session_is_owner() != Some(true) {
                                if let Some(mime_type) = select_text_mime(details.mime_types()) {
                                    *local_read_pending.borrow_mut() = Some((
                                        owner_session,
                                        mime_type.to_owned(),
                                        revision,
                                    ));
                                    local_read_notify.notify_one();
                                }
                            }
                        }
                    } else if clipboard_active {
                        log::warn!("clipboard owner stream closed; disabling clipboard sync");
                        clipboard_active = false;
                        owner_changed = None;
                        selection_transfer = None;
                        local_text.borrow_mut().take();
                        let _ = local_read_pending.borrow_mut().take();
                        clipboard_cancellation.cancel();
                    }
                }
                transfer = async {
                    match selection_transfer.as_mut() {
                        Some(stream) => stream.next().await,
                        None => futures::future::pending().await,
                    }
                } => {
                    if let Some((transfer_session, mime_type, serial)) = transfer {
                        let matching_session = format!("{transfer_session:?}") == format!("{session:?}");
                        if clipboard_active && matching_session {
                            let Ok(permit) = transfer_slots.clone().try_acquire_owned() else {
                                if let (Some(clipboard), Ok(permit)) = (
                                    clipboard.as_ref().cloned(),
                                    rejection_slots.clone().try_acquire_owned(),
                                ) {
                                    let cancellation = clipboard_cancellation.child_token();
                                    tokio::task::spawn_local(async move {
                                        let _permit = permit;
                                        let result = tokio::select! {
                                            _ = cancellation.cancelled() => return,
                                            result = tokio::time::timeout(
                                                std::time::Duration::from_secs(1),
                                                clipboard.selection_write_done(
                                                    &transfer_session,
                                                    serial,
                                                    false,
                                                ),
                                            ) => result,
                                        };
                                        if let Ok(Err(error)) = result {
                                            log::debug!(
                                                "portal clipboard rejection completion failed: {error}"
                                            );
                                        }
                                    });
                                } else {
                                    log::warn!(
                                        "dropping overloaded portal clipboard request without blocking input capture"
                                    );
                                }
                                continue;
                            };
                            let supported = matches!(mime_type.as_str(), "text/plain" | "text/plain;charset=utf-8" | "UTF8_STRING");
                            let text = remote_text.borrow().clone().filter(|_| supported);
                            let clipboard = clipboard.as_ref().expect("clipboard active").clone();
                            let cancellation = clipboard_cancellation.child_token();
                            tokio::task::spawn_local(async move {
                                let _permit = permit;
                                if cancellation.is_cancelled() {
                                    return;
                                }
                                let result = if let Some(text) = text {
                                    write_portal_text(
                                        &clipboard,
                                        &transfer_session,
                                        serial,
                                        text.as_bytes(),
                                        &cancellation,
                                    )
                                    .await
                                } else {
                                    false
                                };
                                let completion = tokio::select! {
                                    _ = cancellation.cancelled() => return,
                                    result = clipboard.selection_write_done(&transfer_session, serial, result) => result,
                                };
                                if let Err(error) = completion {
                                    log::debug!("portal clipboard completion failed: {error}");
                                }
                            });
                        }
                    } else if clipboard_active {
                        log::warn!("clipboard transfer stream closed; disabling clipboard sync");
                        clipboard_active = false;
                        owner_changed = None;
                        selection_transfer = None;
                        local_text.borrow_mut().take();
                        let _ = local_read_pending.borrow_mut().take();
                        clipboard_cancellation.cancel();
                    }
                }
                _ = &mut set_selection_worker, if clipboard_active => {
                    clipboard_active = false;
                    log::warn!("clipboard selection worker ended; disabling clipboard sync");
                }
                activated = activated.next() => {
                    let activated = activated.ok_or(CaptureError::ActivationClosed)?;
                    log::debug!("activated: {activated:?}");

                    // get barrier id from activation
                    let barrier_id = match activated.barrier_id() {
                        Some(ActivatedBarrier::Barrier(id)) => id,
                        // workaround for KDE plasma not reporting barrier ids
                        Some(ActivatedBarrier::UnknownBarrier) | None => find_corresponding_client(&barriers, activated.cursor_position().expect("no cursor position reported by compositor")),
                    };

                    // find client corresponding to barrier
                    let pos = match pos_for_barrier_id.get(&barrier_id) {
                        Some(id) => *id,
                        None => {
                            log::warn!("INVALID BARRIER ID: Id {barrier_id} does not exist!");
                            let id = find_corresponding_client(&barriers, activated.cursor_position().expect("no cursor position reported by compositor"));
                            let pos = *pos_for_barrier_id.get(&id).expect("invalid barrier id");
                            pos
                        },
                    };
                    current_pos.replace(Some(pos));

                    // client entered => send event
                    event_tx.send((pos, CaptureEvent::Begin)).await.expect("no channel");
                    if let Some(text) = local_text.borrow().clone() {
                        let _ = events.clipboard.events.try_send((pos, text));
                    }

                    tokio::select! {
                        _ = notify_release.notified() => { /* capture release */
                            log::debug!("release session requested");
                        },
                        _ = release_session.notified() => { /* release session */
                            log::debug!("ei devices changed");
                            ei_devices_changed = true;
                        },
                        _ = cancel_session.cancelled() => { /* kill session notify */
                            log::debug!("session cancel requested");
                            break
                        },
                    }

                    release_capture(input_capture, session, activated, pos).await?;

                }
                _ = notify_release.notified() => { /* capture release -> we are not capturing anyway, so ignore */
                    log::debug!("release session requested");
                },
                _ = release_session.notified() => { /* release session */
                    log::debug!("ei devices changed");
                    ei_devices_changed = true;
                },
                _ = cancel_session.cancelled() => { /* kill session notify */
                    log::debug!("session cancel requested");
                    break
                },
            }
            if ei_devices_changed {
                /* for whatever reason, GNOME seems to kill the session
                 * as soon as devices are added or removed, so we need
                 * to cancel */
                break;
            }
        }
        // cancel libei task
        log::debug!("session exited: killing libei task");
        cancel_ei_handler.cancel();
        Ok::<(), CaptureError>(())
    };

    let (a, b) = tokio::join!(ei_task, capture_session_task);

    clipboard_cancellation.cancel();
    cancel_update.cancel();

    log::debug!("both session and ei task finished!");
    a?;
    b?;

    Ok(())
}

async fn release_capture(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
    activated: Activated,
    current_pos: Position,
) -> Result<(), CaptureError> {
    if let Some(activation_id) = activated.activation_id() {
        log::debug!("releasing input capture {activation_id}");
    }
    let (x, y) = activated
        .cursor_position()
        .expect("compositor did not report cursor position!");
    log::debug!("client entered @ ({x}, {y})");
    let (dx, dy) = match current_pos {
        // offset cursor position to not enter again immediately
        Position::Left => (1., 0.),
        Position::Right => (-1., 0.),
        Position::Top => (0., 1.),
        Position::Bottom => (0., -1.),
    };
    // release 1px to the right of the entered zone
    let cursor_position = (x as f64 + dx, y as f64 + dy);
    let release_options = ReleaseOptions::default()
        .set_activation_id(activated.activation_id())
        .set_cursor_position(Some(cursor_position));
    input_capture.release(session, release_options).await?;
    Ok(())
}

fn find_corresponding_client(barriers: &[ICBarrier], pos: (f32, f32)) -> BarrierID {
    barriers
        .iter()
        .copied()
        .min_by_key(|b| {
            let (x1, y1, x2, y2) = b.position;
            let (x1, y1, x2, y2) = (x1 as f32, y1 as f32, x2 as f32, y2 as f32);
            distance_to_line(((x1, y1), (x2, y2)), pos) as i32
        })
        .expect("could not find barrier corresponding to client")
        .barrier_id
}

fn select_text_mime(mime_types: &[String]) -> Option<&'static str> {
    ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"]
        .into_iter()
        .find(|mime_type| mime_types.iter().any(|advertised| advertised == mime_type))
}

async fn read_portal_text(
    clipboard: &Clipboard,
    session: &Session<InputCapture>,
    mime_type: &str,
    cancellation: &CancellationToken,
) -> Option<String> {
    let fd = tokio::select! {
        _ = cancellation.cancelled() => return None,
        result = clipboard.selection_read(session, mime_type) => match result {
            Ok(fd) => fd,
            Err(error) => {
                log::debug!("portal clipboard read failed: {error}");
                return None;
            }
        },
    };
    let file = AsyncFile::from_std(std::fs::File::from(std::os::fd::OwnedFd::from(fd)));
    let mut file = file.take((MAX_CLIPBOARD_TEXT_BYTES + 2) as u64);
    let mut bytes = Vec::new();
    let result = tokio::select! {
        _ = cancellation.cancelled() => return None,
        result = file.read_to_end(&mut bytes) => result,
    };
    if let Err(error) = result {
        log::debug!("portal clipboard stream read failed: {error}");
        return None;
    }
    if bytes.len() > MAX_CLIPBOARD_TEXT_BYTES + 1 {
        log::warn!("ignoring oversized portal clipboard text");
        return None;
    }
    if bytes.last() == Some(&0) {
        bytes.pop();
    }
    if bytes.len() > MAX_CLIPBOARD_TEXT_BYTES {
        log::warn!("ignoring oversized portal clipboard text");
        return None;
    }
    if bytes.contains(&0) {
        log::debug!("ignoring portal clipboard text with embedded NUL");
        return None;
    }
    String::from_utf8(bytes)
        .ok()
        .filter(|text| !text.contains('\0'))
}

async fn write_portal_text(
    clipboard: &Clipboard,
    session: &Session<InputCapture>,
    serial: u32,
    text: &[u8],
    cancellation: &CancellationToken,
) -> bool {
    let fd = tokio::select! {
        _ = cancellation.cancelled() => return false,
        result = clipboard.selection_write(session, serial) => match result {
            Ok(fd) => fd,
            Err(error) => {
                log::debug!("portal clipboard write failed: {error}");
                return false;
            }
        },
    };
    let mut file = AsyncFile::from_std(std::fs::File::from(std::os::fd::OwnedFd::from(fd)));
    tokio::select! {
        _ = cancellation.cancelled() => false,
        result = file.write_all(text) => result.is_ok(),
    }
}

fn distance_to_line(line: ((f32, f32), (f32, f32)), p: (f32, f32)) -> f32 {
    let ((x1, y1), (x2, y2)) = line;
    let (x0, y0) = p;
    /*
     * we use the fact that for the triangle spanned by the line and p,
     * the height of the triangle is the desired distance and can be calculated by
     * h = 2A / b with b being the line_length and
     */
    let double_triangle_area = ((y2 - y1) * x0 - (x2 - x1) * y0 + x2 * y1 - y2 * x1).abs();
    let line_length = ((y2 - y1).powf(2.0) + (x2 - x1).powf(2.0)).sqrt();
    let distance = double_triangle_area / line_length;
    log::debug!("distance to line({line:?}, {p:?}) = {distance}");
    distance
}

async fn handle_ei_event(
    ei_event: EiEvent,
    current_client: Option<Position>,
    context: &ei::Context,
    event_tx: &Sender<(Position, CaptureEvent)>,
    release_session: &Notify,
) -> Result<(), CaptureError> {
    let all_capabilities = DeviceCapability::Pointer
        | DeviceCapability::PointerAbsolute
        | DeviceCapability::Keyboard
        | DeviceCapability::Touch
        | DeviceCapability::Scroll
        | DeviceCapability::Button;
    match ei_event {
        EiEvent::SeatAdded(s) => {
            s.seat.bind_capabilities(all_capabilities);
            context.flush().map_err(|e| io::Error::new(e.kind(), e))?;
        }
        EiEvent::SeatRemoved(_) | /* EiEvent::DeviceAdded(_) | */ EiEvent::DeviceRemoved(_) => {
            log::debug!("releasing session: {ei_event:?}");
            release_session.notify_waiters();
        }
        EiEvent::DevicePaused(_) | EiEvent::DeviceResumed(_) => {}
        EiEvent::DeviceStartEmulating(_) => log::debug!("START EMULATING"),
        EiEvent::DeviceStopEmulating(_) => log::debug!("STOP EMULATING"),
        EiEvent::Disconnected(d) => {
            return Err(CaptureError::Disconnected(format!("{:?}", d.reason)))
        }
        _ => {
            if let Some(pos) = current_client {
                for event in Event::from_ei_event(ei_event) {
                    event_tx.send((pos, CaptureEvent::Input(event))).await.expect("no channel");
                }
            }
        }
    }
    Ok(())
}

#[async_trait]
impl DeskunionInputCapture for LibeiInputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        let _ = self
            .notify_capture
            .send(LibeiNotifyEvent::Create(pos))
            .await;
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        let _ = self
            .notify_capture
            .send(LibeiNotifyEvent::Destroy(pos))
            .await;
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        self.notify_release.notify_waiters();
        Ok(())
    }

    fn take_clipboard_events(&mut self) -> Option<Receiver<(Position, String)>> {
        self.clipboard_event_rx.take()
    }

    fn set_clipboard_text(&mut self, text: String) -> Result<(), CaptureError> {
        if text.len() <= MAX_CLIPBOARD_TEXT_BYTES {
            match self.clipboard_tx.try_send(ClipboardCommand::SetText(text)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    log::warn!("dropping clipboard update: portal queue full");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(CaptureError::EndOfStream);
                }
            }
        }
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        self.cancellation_token.cancel();
        let task = &mut self.capture_task;
        log::debug!("waiting for capture to terminate...");
        let res = if !task.is_finished() {
            task.await.expect("libei task panic")
        } else {
            Ok(())
        };
        self.terminated = true;
        log::debug!("done!");
        res
    }
}

impl Drop for LibeiInputCapture {
    fn drop(&mut self) {
        if !self.terminated {
            /* this workaround is needed until async drop is stabilized */
            panic!("LibeiInputCapture dropped without being terminated!");
        }
    }
}

impl Stream for LibeiInputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match self.capture_task.poll_unpin(cx) {
            Poll::Ready(r) => match r.expect("failed to join") {
                Ok(()) => Poll::Ready(None),
                Err(e) => Poll::Ready(Some(Err(e))),
            },
            Poll::Pending => self.event_rx.poll_recv(cx).map(|e| e.map(Result::Ok)),
        }
    }
}

#[cfg(test)]
mod clipboard_tests {
    use super::{clipboard_available, select_text_mime, supports_create_session2};

    #[test]
    fn selects_only_supported_plain_text_formats() {
        assert_eq!(select_text_mime(&["image/png".into()]), None);
        assert_eq!(
            select_text_mime(&["image/png".into(), "text/plain".into()]),
            Some("text/plain")
        );
    }

    #[test]
    fn create_session2_is_used_only_for_portal_v2_or_newer() {
        assert!(!supports_create_session2(1));
        assert!(supports_create_session2(2));
        assert!(supports_create_session2(3));
    }

    #[test]
    fn clipboard_streams_require_config_opt_in_and_start_grant() {
        assert!(!clipboard_available(false, true));
        assert!(!clipboard_available(true, false));
        assert!(clipboard_available(true, true));
    }
}
