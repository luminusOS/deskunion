use crate::error::EmulationError;

use super::{Emulation, error::WlrootsEmulationCreationError};
use async_trait::async_trait;
use bitflags::bitflags;
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wayland_client::WEnum;
use wayland_client::backend::WaylandError;

use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{Axis, AxisSource, ButtonState};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 as VpManager,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as Vp,
};

use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1 as VkManager,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1 as Vk,
};

use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry, wl_seat},
};

use input_event::{Event, KeyboardEvent, PointerEvent, scancode};

use super::EmulationHandle;
use super::error::WaylandBindError;

struct State {
    keymap: Option<(u32, OwnedFd, u32)>,
    keyboard: Option<Vk>,
    /// the seat keyboard the keymap comes from; events are dispatched for the
    /// whole session now, and a repeated `Capabilities` must not add another
    seat_keyboard: Option<wl_keyboard::WlKeyboard>,
    input_for_client: HashMap<EmulationHandle, VirtualInput>,
    seat: wl_seat::WlSeat,
    qh: QueueHandle<Self>,
    vpm: VpManager,
    vkm: VkManager,
}

// App State, implements Dispatch event handlers
pub(crate) struct WlrootsEmulation {
    state: State,
    queue: EventQueue<State>,
}

/// How long the compositor may leave our requests unread before the session is
/// given up; the emulation task then starts a new one.
const OUTPUT_STALL_TIMEOUT: Duration = Duration::from_millis(500);

/// Reads and dispatches the compositor's replies. Left unread they fill the
/// socket in the other direction until the compositor stops reading our
/// requests too. This backend is the connection's only reader.
fn dispatch_incoming(
    queue: &mut EventQueue<State>,
    state: &mut State,
) -> Result<(), EmulationError> {
    queue.dispatch_pending(state).map_err(io::Error::other)?;
    if let Some(guard) = queue.prepare_read() {
        match guard.read() {
            Ok(_) => {}
            Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    }
    queue.dispatch_pending(state).map_err(io::Error::other)?;
    Ok(())
}

/// Calls `flush` until it reports everything sent (`Ok(true)`), waiting a
/// moment while the socket is full (`Ok(false)`). Requests are never dropped:
/// dropping a key-up leaves a key stuck. A socket that stays full for
/// `timeout` fails the session instead.
async fn flush_within(
    timeout: Duration,
    mut flush: impl FnMut() -> Result<bool, EmulationError>,
) -> Result<(), EmulationError> {
    let started = tokio::time::Instant::now();
    let mut retried = false;
    loop {
        if flush()? {
            if retried {
                log::debug!("wayland output recovered after {:?}", started.elapsed());
            }
            return Ok(());
        }
        retried = true;
        if started.elapsed() >= timeout {
            return Err(EmulationError::WaylandOutputBlocked(timeout));
        }
        // yield to the runtime while the compositor drains the socket
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

impl WlrootsEmulation {
    async fn flush_pending(&mut self) -> Result<(), EmulationError> {
        let (queue, state) = (&mut self.queue, &mut self.state);
        flush_within(OUTPUT_STALL_TIMEOUT, || {
            dispatch_incoming(queue, state)?;
            match queue.flush() {
                Ok(()) => Ok(true),
                Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => Ok(false),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub(crate) fn new() -> Result<Self, WlrootsEmulationCreationError> {
        let conn = Connection::connect_to_env()?;
        let (globals, queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();

        let seat: wl_seat::WlSeat = globals
            .bind(&qh, 7..=8, ())
            .map_err(|e| WaylandBindError::new(e, "wl_seat 7..=8"))?;

        let vpm: VpManager = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| WaylandBindError::new(e, "wlr-virtual-pointer-unstable-v1"))?;
        let vkm: VkManager = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| WaylandBindError::new(e, "virtual-keyboard-unstable-v1"))?;

        let input_for_client: HashMap<EmulationHandle, VirtualInput> = HashMap::new();

        let mut emulate = WlrootsEmulation {
            state: State {
                keymap: None,
                keyboard: None,
                seat_keyboard: None,
                input_for_client,
                seat,
                vpm,
                vkm,
                qh,
            },
            queue,
        };
        while emulate.state.keymap.is_none() {
            emulate.queue.blocking_dispatch(&mut emulate.state)?;
        }
        // let fd = unsafe { &File::from_raw_fd(emulate.state.keymap.unwrap().1.as_raw_fd()) };
        // let mmap = unsafe { MmapOptions::new().map_copy(fd).unwrap() };
        // log::debug!("{:?}", &mmap[..100]);
        Ok(emulate)
    }
}

impl State {
    /// One keyboard serves every client: each keymap send queues a file
    /// descriptor, and reconnecting clients used to exhaust the connection's
    /// descriptor budget (ETOOMANYREFS).
    fn shared_keyboard(&mut self) -> Vk {
        if let Some(keyboard) = &self.keyboard {
            return keyboard.clone();
        }
        let keyboard: Vk = self.vkm.create_virtual_keyboard(&self.seat, &self.qh, ());
        // TODO: use server side keymap
        if let Some((format, fd, size)) = self.keymap.as_ref() {
            keyboard.keymap(*format, fd.as_fd(), *size);
        } else {
            panic!("no keymap");
        }
        self.keyboard = Some(keyboard.clone());
        keyboard
    }

    fn add_client(&mut self, client: EmulationHandle) {
        let pointer: Vp = self.vpm.create_virtual_pointer(None, &self.qh, ());
        let keyboard = self.shared_keyboard();

        let vinput = VirtualInput {
            pointer,
            keyboard,
            modifiers: Arc::new(Mutex::new(XMods::empty())),
        };

        self.input_for_client.insert(client, vinput);
    }

    fn destroy_client(&mut self, handle: EmulationHandle) {
        if let Some(input) = self.input_for_client.remove(&handle) {
            input.pointer.destroy();
        }
    }
}

#[async_trait]
impl Emulation for WlrootsEmulation {
    async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        dispatch_incoming(&mut self.queue, &mut self.state)?;
        if let Some(virtual_input) = self.state.input_for_client.get(&handle) {
            virtual_input
                .consume_event(event)
                .unwrap_or_else(|_| panic!("failed to convert event: {event:?}"));
            self.flush_pending().await?;
        }
        Ok(())
    }

    async fn create(&mut self, handle: EmulationHandle) {
        self.state.add_client(handle);
        if let Err(e) = self.flush_pending().await {
            log::error!("{e}");
        }
    }
    async fn destroy(&mut self, handle: EmulationHandle) {
        self.state.destroy_client(handle);
        if let Err(e) = self.flush_pending().await {
            log::error!("{e}");
        }
    }
    async fn terminate(&mut self) {
        /* nothing to do */
    }
}

struct VirtualInput {
    pointer: Vp,
    keyboard: Vk,
    modifiers: Arc<Mutex<XMods>>,
}

impl VirtualInput {
    fn consume_event(&self, event: Event) -> Result<(), ()> {
        // the peer's timestamps use its own clock and unit (libei sends microseconds,
        // windows and macos send 0); clients compare them to detect double clicks
        let now: u32 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u32;

        match event {
            Event::Pointer(e) => {
                match e {
                    PointerEvent::Motion { dx, dy, .. } => self.pointer.motion(now, dx, dy),
                    PointerEvent::Button { button, state, .. } => {
                        let state: ButtonState = state.try_into()?;
                        self.pointer.button(now, button, state);
                    }
                    PointerEvent::Axis { axis, value, .. } => {
                        let axis: Axis = (axis as u32).try_into()?;
                        self.pointer.axis(now, axis, value);
                        // without a source wlroots reports wheel input, which makes
                        // touchpad scrolling stutter
                        self.pointer.axis_source(AxisSource::Continuous);
                        self.pointer.frame();
                    }
                    PointerEvent::AxisDiscrete120 { axis, value } => {
                        let axis: Axis = (axis as u32).try_into()?;
                        self.pointer
                            .axis_discrete(now, axis, value as f64 / 8., value / 120);
                        self.pointer.axis_source(AxisSource::Wheel);
                        self.pointer.frame();
                    }
                }
                self.pointer.frame();
            }
            Event::Keyboard(e) => match e {
                KeyboardEvent::Key { key, state, .. } => {
                    self.keyboard.key(now, key, state as u32);
                    if let Ok(mut mods) = self.modifiers.lock()
                        && mods.update_by_key_event(key, state)
                    {
                        log::trace!("Key triggers modifier change: {mods:?}");
                        self.keyboard.modifiers(
                            mods.mask_pressed().bits(),
                            0,
                            mods.mask_locks().bits(),
                            0,
                        );
                    }
                }
                KeyboardEvent::Modifiers {
                    depressed: mods_depressed,
                    latched: mods_latched,
                    locked: mods_locked,
                    group,
                } => {
                    // Synchronize internal modifier state, assuming server is authoritative
                    if let Ok(mut mods) = self.modifiers.lock() {
                        mods.update_by_mods_event(e);
                    }
                    self.keyboard
                        .modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            },
        }
        Ok(())
    }
}

delegate_noop!(State: Vp);
delegate_noop!(State: Vk);
delegate_noop!(State: VpManager);
delegate_noop!(State: VkManager);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut State,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: <WlKeyboard as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            state.keymap = Some((u32::from(format), fd, size));
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: <WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Keyboard)
            && state.seat_keyboard.is_none()
        {
            state.seat_keyboard = Some(seat.get_keyboard(qhandle, ()));
        }
    }
}

// From X11/X.h
bitflags! {
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
    struct XMods: u32 {
        const ShiftMask = (1<<0);
        const LockMask = (1<<1);
        const ControlMask = (1<<2);
        const Mod1Mask = (1<<3);
        const Mod2Mask = (1<<4);
        const Mod3Mask = (1<<5);
        const Mod4Mask = (1<<6);
        const Mod5Mask = (1<<7);
    }
}

impl XMods {
    fn update_by_mods_event(&mut self, evt: KeyboardEvent) {
        if let KeyboardEvent::Modifiers {
            depressed, locked, ..
        } = evt
        {
            *self = XMods::from_bits_truncate(depressed) | XMods::from_bits_truncate(locked);
        }
    }

    fn update_by_key_event(&mut self, key: u32, state: u8) -> bool {
        if let Ok(key) = scancode::Linux::try_from(key) {
            log::trace!("Attempting to process modifier from: {key:#?}");
            let pressed_mask = match key {
                scancode::Linux::KeyLeftShift | scancode::Linux::KeyRightShift => XMods::ShiftMask,
                scancode::Linux::KeyLeftCtrl | scancode::Linux::KeyRightCtrl => XMods::ControlMask,
                scancode::Linux::KeyLeftAlt | scancode::Linux::KeyRightalt => XMods::Mod1Mask,
                scancode::Linux::KeyLeftMeta | scancode::Linux::KeyRightmeta => XMods::Mod4Mask,
                _ => XMods::empty(),
            };

            let locked_mask = match key {
                scancode::Linux::KeyCapsLock => XMods::LockMask,
                scancode::Linux::KeyNumlock => XMods::Mod2Mask,
                scancode::Linux::KeyScrollLock => XMods::Mod3Mask,
                _ => XMods::empty(),
            };

            // unchanged
            if pressed_mask.is_empty() && locked_mask.is_empty() {
                log::trace!("{key:#?} is not a modifier key");
                return false;
            }
            match state {
                1 => self.insert(pressed_mask),
                _ => {
                    self.remove(pressed_mask);
                    self.toggle(locked_mask);
                }
            }
            true
        } else {
            false
        }
    }

    fn mask_locks(&self) -> XMods {
        *self & (XMods::LockMask | XMods::Mod2Mask | XMods::Mod3Mask)
    }

    fn mask_pressed(&self) -> XMods {
        *self & (XMods::ShiftMask | XMods::ControlMask | XMods::Mod1Mask | XMods::Mod4Mask)
    }
}

#[cfg(test)]
mod flush_tests {
    use super::*;

    /// a compositor that stops reading must fail the session within the
    /// timeout, so the emulation task can start a new one
    #[tokio::test(start_paused = true)]
    async fn a_socket_that_stays_full_fails_the_session() {
        let mut calls = 0;
        let started = tokio::time::Instant::now();
        let result = flush_within(OUTPUT_STALL_TIMEOUT, || {
            calls += 1;
            Ok(false)
        })
        .await;
        assert!(matches!(
            result,
            Err(EmulationError::WaylandOutputBlocked(_))
        ));
        assert!(started.elapsed() >= OUTPUT_STALL_TIMEOUT);
        assert!(started.elapsed() < OUTPUT_STALL_TIMEOUT * 2);
        assert!(calls > 1, "retried while waiting");
    }

    /// a short stall is waited out: nothing is dropped and no error is raised
    #[tokio::test(start_paused = true)]
    async fn a_short_stall_is_waited_out() {
        let mut full = 3;
        let result = flush_within(OUTPUT_STALL_TIMEOUT, || {
            full -= 1;
            Ok(full < 0)
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(full, -1, "flushed again until everything was sent");
    }

    #[tokio::test(start_paused = true)]
    async fn a_broken_connection_is_reported_at_once() {
        let started = tokio::time::Instant::now();
        let result = flush_within(OUTPUT_STALL_TIMEOUT, || Err(EmulationError::EndOfStream)).await;
        assert!(matches!(result, Err(EmulationError::EndOfStream)));
        assert_eq!(started.elapsed(), Duration::ZERO);
    }
}
