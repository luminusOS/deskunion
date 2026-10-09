use super::error::{EmulationError, WindowsEmulationCreationError};
use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent,
    scancode,
};

use async_trait::async_trait;
use std::ops::BitOrAssign;
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::Notify;
use tokio::task::AbortHandle;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT_0, KEYEVENTF_EXTENDEDKEY, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE, XBUTTON1, XBUTTON2,
};
use windows::Win32::{
    Foundation::{GlobalFree, HANDLE},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
            IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
        Ole::CF_UNICODETEXT,
    },
};
use windows::core::w;

use super::{Emulation, EmulationHandle};

const DEFAULT_REPEAT_DELAY: Duration = Duration::from_millis(500);
const DEFAULT_REPEAT_INTERVAL: Duration = Duration::from_millis(32);

pub(crate) struct WindowsEmulation {
    repeat_task: Option<AbortHandle>,
    latest_clipboard: Arc<Mutex<Option<String>>>,
    clipboard_notify: Arc<Notify>,
    clipboard_stop: Arc<AtomicBool>,
    clipboard_thread: Option<JoinHandle<()>>,
    remote_text: Arc<Mutex<Option<String>>>,
}

impl WindowsEmulation {
    pub(crate) fn new(clipboard_enabled: bool) -> Result<Self, WindowsEmulationCreationError> {
        let clipboard_stop = Arc::new(AtomicBool::new(false));
        let remote_text = Arc::new(Mutex::new(None));
        let latest_clipboard = Arc::new(Mutex::new(None));
        let clipboard_notify = Arc::new(Notify::new());
        let clipboard_thread = clipboard_enabled.then(|| {
            let stop = clipboard_stop.clone();
            let suppressed = remote_text.clone();
            let latest = latest_clipboard.clone();
            let notify = clipboard_notify.clone();
            thread::spawn(move || {
                let mut last_sequence = 0;
                let mut failed_reads = 0u8;
                while !stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(150));
                    // SAFETY: `GetClipboardSequenceNumber` takes no arguments and has no
                    // preconditions.
                    let sequence = unsafe { GetClipboardSequenceNumber() };
                    if sequence == last_sequence {
                        continue;
                    }
                    let Some(text) = read_clipboard_text() else {
                        // the owner may still hold the clipboard right after a
                        // copy: retry on the next tick instead of losing it
                        failed_reads += 1;
                        if failed_reads >= 10 {
                            last_sequence = sequence;
                            failed_reads = 0;
                        }
                        continue;
                    };
                    last_sequence = sequence;
                    failed_reads = 0;
                    {
                        log::debug!("local clipboard changed ({} bytes)", text.len());
                        let is_remote_echo = suppressed
                            .lock()
                            .ok()
                            .map(|mut value| {
                                if value.as_deref() == Some(text.as_str()) {
                                    value.take();
                                    true
                                } else {
                                    false
                                }
                            })
                            .unwrap_or(false);
                        if !is_remote_echo {
                            *latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
                            notify.notify_one();
                        }
                    }
                }
            })
        });
        Ok(Self {
            repeat_task: None,
            latest_clipboard,
            clipboard_notify,
            clipboard_stop,
            clipboard_thread,
            remote_text,
        })
    }
}

#[async_trait]
impl Emulation for WindowsEmulation {
    async fn consume(&mut self, event: Event, _: EmulationHandle) -> Result<(), EmulationError> {
        match event {
            Event::Pointer(pointer_event) => match pointer_event {
                PointerEvent::Motion { time: _, dx, dy } => {
                    rel_mouse(dx as i32, dy as i32);
                }
                PointerEvent::Button {
                    time: _,
                    button,
                    state,
                } => mouse_button(button, state),
                PointerEvent::Axis {
                    time: _,
                    axis,
                    value,
                } => scroll(axis, value as i32),
                PointerEvent::AxisDiscrete120 { axis, value } => scroll(axis, value),
            },
            Event::Keyboard(keyboard_event) => match keyboard_event {
                KeyboardEvent::Key {
                    time: _,
                    key,
                    state,
                } => {
                    match state {
                        // pressed
                        0 => self.kill_repeat_task(),
                        1 => self.spawn_repeat_task(key).await,
                        _ => {}
                    }
                    key_event(key, state)
                }
                KeyboardEvent::Modifiers { .. } => {}
            },
        }
        // FIXME
        Ok(())
    }

    async fn set_clipboard_text(&mut self, text: String) -> Result<(), EmulationError> {
        const MAX_CLIPBOARD_TEXT_BYTES: usize = 64 * 1024;
        if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
            return Ok(());
        }
        *self.remote_text.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.clone());
        match tokio::task::spawn_blocking(move || write_clipboard_text(&text)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.remote_text
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                log::warn!("failed to apply remote clipboard text: {error}");
            }
            Err(error) => {
                self.remote_text
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                log::warn!("Windows clipboard worker failed: {error}");
            }
        }
        Ok(())
    }

    async fn next_clipboard_text(&mut self) -> Option<String> {
        loop {
            let notified = self.clipboard_notify.notified();
            if let Some(text) = self
                .latest_clipboard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                return Some(text);
            }
            notified.await;
        }
    }

    async fn create(&mut self, _handle: EmulationHandle) {}

    async fn destroy(&mut self, _handle: EmulationHandle) {}

    async fn terminate(&mut self) {
        self.clipboard_stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.clipboard_thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_clipboard_text() -> Option<String> {
    const MAX_CLIPBOARD_TEXT_BYTES: usize = 64 * 1024;
    // SAFETY: the clipboard is opened for the whole block and closed on every path. The
    // `GlobalLock` pointer is non-null, the slice is capped at `GlobalSize` bytes and read
    // only while the lock is held, and every exit after locking unlocks it.
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }
        let result = (|| {
            IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).ok()?;
            let handle = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
            let global = windows::Win32::Foundation::HGLOBAL(handle.0);
            let size = GlobalSize(global).min(MAX_CLIPBOARD_TEXT_BYTES * 2 + 2);
            if size < 2 {
                return None;
            }
            let pointer = GlobalLock(global);
            if pointer.is_null() {
                return None;
            }
            let units = std::slice::from_raw_parts(pointer.cast::<u16>(), size / 2);
            let Some(length) = units.iter().position(|&unit| unit == 0) else {
                let _ = GlobalUnlock(global);
                return None;
            };
            let text = String::from_utf16(&units[..length]).ok();
            let _ = GlobalUnlock(global);
            text.filter(|text| text.len() <= MAX_CLIPBOARD_TEXT_BYTES)
        })();
        let _ = CloseClipboard();
        result
    }
}

fn write_clipboard_text(text: &str) -> io::Result<()> {
    let mut wide = text.encode_utf16().collect::<Vec<_>>();
    wide.push(0);
    let bytes = wide.len() * std::mem::size_of::<u16>();
    // SAFETY: the clipboard stays open until `close` drops. The allocation holds `bytes`
    // bytes, so copying `wide.len()` u16s into the locked pointer is in bounds, and it is
    // freed only on paths where `SetClipboardData` did not take ownership.
    unsafe {
        // `OpenClipboard(NULL)` makes `EmptyClipboard` leave the clipboard
        // ownerless, after which `SetClipboardData` fails: own it through a
        // throwaway message-only window
        let owner = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
        let result = write_clipboard_text_owned(owner, &wide, bytes);
        let _ = DestroyWindow(owner);
        result
    }
}

/// # Safety
/// `owner` must be a valid window handle created on the calling thread.
unsafe fn write_clipboard_text_owned(
    owner: windows::Win32::Foundation::HWND,
    wide: &[u16],
    bytes: usize,
) -> io::Result<()> {
    // SAFETY: see `write_clipboard_text`.
    unsafe {
        let mut opened = OpenClipboard(Some(owner));
        for _ in 0..10 {
            if opened.is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
            opened = OpenClipboard(Some(owner));
        }
        opened.map_err(|error| io::Error::other(error.to_string()))?;
        let close = ClipboardCloseGuard;
        EmptyClipboard().map_err(|error| io::Error::other(error.to_string()))?;
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let pointer = GlobalLock(memory);
        if pointer.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err(io::Error::last_os_error());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), pointer.cast::<u16>(), wide.len());
        let _ = GlobalUnlock(memory);
        if let Err(error) = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(memory.0))) {
            let _ = GlobalFree(Some(memory));
            return Err(io::Error::other(error.to_string()));
        }
        drop(close);
    }
    Ok(())
}

struct ClipboardCloseGuard;

impl Drop for ClipboardCloseGuard {
    fn drop(&mut self) {
        // SAFETY: the guard is only created after `OpenClipboard` succeeded on this thread.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

impl WindowsEmulation {
    async fn spawn_repeat_task(&mut self, key: u32) {
        // there can only be one repeating key and it's
        // always the last to be pressed
        self.kill_repeat_task();
        let repeat_task = tokio::task::spawn_local(async move {
            tokio::time::sleep(DEFAULT_REPEAT_DELAY).await;
            loop {
                key_event(key, 1);
                tokio::time::sleep(DEFAULT_REPEAT_INTERVAL).await;
            }
        });
        self.repeat_task = Some(repeat_task.abort_handle());
    }
    fn kill_repeat_task(&mut self) {
        if let Some(task) = self.repeat_task.take() {
            task.abort();
        }
    }
}

fn send_input_safe(input: INPUT) {
    // SAFETY: the slice holds one fully initialised `INPUT` and the size argument is
    // `size_of::<INPUT>()`.
    unsafe {
        // Never spin here: SendInput can legitimately be rejected by UIPI
        // (for example when the target window has a higher integrity level).
        // The old busy loop froze the service on the first pointer event.
        if SendInput(&[input], std::mem::size_of::<INPUT>() as i32) == 0 {
            log::error!("SendInput failed: {}", windows::core::Error::from_win32());
        }
    }
}

fn send_mouse_input(mi: MOUSEINPUT) {
    send_input_safe(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi },
    });
}

fn send_keyboard_input(ki: KEYBDINPUT) {
    send_input_safe(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    });
}
fn rel_mouse(dx: i32, dy: i32) {
    let mi = MOUSEINPUT {
        dx,
        dy,
        mouseData: 0,
        dwFlags: MOUSEEVENTF_MOVE,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi);
}

fn mouse_button(button: u32, state: u32) {
    let dw_flags = match state {
        0 => match button {
            BTN_LEFT => MOUSEEVENTF_LEFTUP,
            BTN_RIGHT => MOUSEEVENTF_RIGHTUP,
            BTN_MIDDLE => MOUSEEVENTF_MIDDLEUP,
            BTN_BACK => MOUSEEVENTF_XUP,
            BTN_FORWARD => MOUSEEVENTF_XUP,
            _ => return,
        },
        1 => match button {
            BTN_LEFT => MOUSEEVENTF_LEFTDOWN,
            BTN_RIGHT => MOUSEEVENTF_RIGHTDOWN,
            BTN_MIDDLE => MOUSEEVENTF_MIDDLEDOWN,
            BTN_BACK => MOUSEEVENTF_XDOWN,
            BTN_FORWARD => MOUSEEVENTF_XDOWN,
            _ => return,
        },
        _ => return,
    };
    let mouse_data = match button {
        BTN_BACK => XBUTTON1 as u32,
        BTN_FORWARD => XBUTTON2 as u32,
        _ => 0,
    };
    let mi = MOUSEINPUT {
        dx: 0,
        dy: 0, // no movement
        mouseData: mouse_data,
        dwFlags: dw_flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi);
}

fn scroll(axis: u8, value: i32) {
    let event_type = match axis {
        0 => MOUSEEVENTF_WHEEL,
        1 => MOUSEEVENTF_HWHEEL,
        _ => return,
    };
    let mi = MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: -value as u32,
        dwFlags: event_type,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi);
}

fn key_event(key: u32, state: u8) {
    let scancode = match linux_keycode_to_windows_scancode(key) {
        Some(code) => code,
        None => return,
    };
    let extended = scancode > 0xff;
    let scancode = scancode & 0xff;
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags.bitor_assign(KEYEVENTF_EXTENDEDKEY);
    }
    if state == 0 {
        flags.bitor_assign(KEYEVENTF_KEYUP);
    }
    let ki = KEYBDINPUT {
        wVk: Default::default(),
        wScan: scancode,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send_keyboard_input(ki);
}

fn linux_keycode_to_windows_scancode(linux_keycode: u32) -> Option<u16> {
    let linux_scancode = match scancode::Linux::try_from(linux_keycode) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("unknown keycode: {linux_keycode}");
            return None;
        }
    };
    log::trace!("linux code: {linux_scancode:?}");
    let windows_scancode = match scancode::Windows::try_from(linux_scancode) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("failed to translate linux code into windows scancode: {linux_scancode:?}");
            return None;
        }
    };
    log::trace!("windows code: {windows_scancode:?}");
    Some(windows_scancode as u16)
}
