//! Finds the render outputs a call application is playing on.
//!
//! Windows reports changes through notifications: a session was created on an
//! output, a session started or stopped playing, an output appeared or went
//! away. None of them carries state. Each only wakes the watcher, which then
//! recomputes the answer from the system, so a notification that is lost or
//! arrives out of order cannot leave the result wrong.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use wasapi::{
    Device, DeviceEnumerator, DeviceEventCallbacks, DeviceEventRegistration, Direction,
    EventCallbacks, EventRegistration, SessionState, WasapiError,
};
use windows::Win32::Media::Audio::{
    IAudioSessionControl, IAudioSessionManager2, IAudioSessionNotification,
    IAudioSessionNotification_Impl,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::core::{Ref, implement};

use super::calls::is_teams_executable;

/// What wakes the watcher.
pub(super) enum Signal {
    /// something about the audio sessions or outputs changed
    Changed,
    Stop,
}

fn notify(signals: &mpsc::Sender<Signal>) {
    let _ = signals.send(Signal::Changed);
}

/// Image path of a process, when Windows lets us ask for it.
fn process_image_path(process_id: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    // SAFETY: `OpenProcess` takes plain values and returns an owned handle,
    // which is closed below on every path that obtained it. The buffer
    // outlives the query and `length` starts as its capacity in characters.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?;
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let queried = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        queried.ok()?;
        Some(String::from_utf16_lossy(&buffer[..length as usize]))
    }
}

/// Receives the notification that a session was created on an output. It
/// implements an interface the application must provide itself; the session
/// it is handed is not used, since the answer is recomputed after the wake-up.
#[implement(IAudioSessionNotification)]
struct SessionCreated {
    signals: mpsc::Sender<Signal>,
}

impl IAudioSessionNotification_Impl for SessionCreated_Impl {
    fn OnSessionCreated(
        &self,
        _new_session: Ref<IAudioSessionControl>,
    ) -> windows::core::Result<()> {
        notify(&self.signals);
        Ok(())
    }
}

/// The new-session registration on one output.
struct OutputWatch {
    manager: IAudioSessionManager2,
    notification: IAudioSessionNotification,
}

impl OutputWatch {
    fn register(device: &Device, signals: &mpsc::Sender<Signal>) -> windows::core::Result<Self> {
        // SAFETY: `Activate` on a valid `IMMDevice` returns an owned interface.
        let manager: IAudioSessionManager2 =
            unsafe { device.as_immdevice().Activate(CLSCTX_ALL, None)? };
        let notification: IAudioSessionNotification = SessionCreated {
            signals: signals.clone(),
        }
        .into();
        // SAFETY: both interfaces are valid. The registration is removed in
        // `Drop` before they are released, and never from inside a callback.
        unsafe { manager.RegisterSessionNotification(&notification)? };
        let watch = Self {
            manager,
            notification,
        };
        // Windows discards new-session notifications until the existing
        // sessions were requested once.
        // SAFETY: plain calls on the valid manager owned by `watch`.
        unsafe { watch.manager.GetSessionEnumerator()?.GetCount()? };
        Ok(watch)
    }
}

impl Drop for OutputWatch {
    fn drop(&mut self) {
        // SAFETY: the interface that was registered, on the manager it was
        // registered with.
        let _ = unsafe {
            self.manager
                .UnregisterSessionNotification(&self.notification)
        };
    }
}

pub(super) struct SessionMonitor {
    enumerator: DeviceEnumerator,
    signals: mpsc::Sender<Signal>,
    /// `None` for an output whose sessions could not be watched, so it is not
    /// retried until the output comes back
    outputs: HashMap<String, Option<OutputWatch>>,
    /// state notifications of the sessions of call applications
    sessions: HashMap<String, EventRegistration>,
    /// image path of each process with a session, and whether it is a call
    /// application; empty when Windows would not tell
    processes: HashMap<u32, (String, bool)>,
    reported_apps: HashSet<String>,
    _output_events: DeviceEventRegistration,
}

impl SessionMonitor {
    pub(super) fn new(signals: mpsc::Sender<Signal>) -> Result<Self, WasapiError> {
        let enumerator = DeviceEnumerator::new()?;
        let mut callbacks = DeviceEventCallbacks::new();
        let sender = signals.clone();
        callbacks.set_device_added_callback(move |_| notify(&sender));
        let sender = signals.clone();
        callbacks.set_device_removed_callback(move |_| notify(&sender));
        let sender = signals.clone();
        callbacks.set_device_state_callback(move |_, _| notify(&sender));
        let sender = signals.clone();
        callbacks.set_default_device_callback(move |_, _, _| notify(&sender));
        let output_events = enumerator.register_notification_callback(callbacks)?;
        Ok(Self {
            enumerator,
            signals,
            outputs: HashMap::new(),
            sessions: HashMap::new(),
            processes: HashMap::new(),
            reported_apps: HashSet::new(),
            _output_events: output_events,
        })
    }

    /// Render outputs on which a call application is playing right now.
    pub(super) fn active_call_outputs(&mut self) -> Vec<String> {
        let Ok(collection) = self.enumerator.get_device_collection(&Direction::Render) else {
            return Vec::new();
        };
        let Ok(count) = collection.get_nbr_devices() else {
            return Vec::new();
        };
        let mut present = HashSet::new();
        let mut live_sessions = HashSet::new();
        let mut live_processes = HashSet::new();
        let mut playing = Vec::new();
        for index in 0..count {
            let Ok(device) = collection.get_device_at_index(index) else {
                continue;
            };
            let Ok(id) = device.get_id() else {
                continue;
            };
            self.watch_output(&id, &device);
            if self.scan_output(&device, &mut live_sessions, &mut live_processes) {
                playing.push(id.clone());
            }
            present.insert(id);
        }
        self.outputs.retain(|id, _| present.contains(id));
        self.sessions.retain(|key, _| live_sessions.contains(key));
        self.processes
            .retain(|process_id, _| live_processes.contains(process_id));
        playing
    }

    fn watch_output(&mut self, id: &str, device: &Device) {
        if self.outputs.contains_key(id) {
            return;
        }
        let watch = match OutputWatch::register(device, &self.signals) {
            Ok(watch) => Some(watch),
            Err(error) => {
                log::debug!("not watching new sessions on output {id}: {error}");
                None
            }
        };
        self.outputs.insert(id.to_owned(), watch);
    }

    /// Whether a call application plays on `device`. Also keeps a state
    /// notification registered for each of its sessions.
    fn scan_output(
        &mut self,
        device: &Device,
        live_sessions: &mut HashSet<String>,
        live_processes: &mut HashSet<u32>,
    ) -> bool {
        let Ok(manager) = device.get_iaudiosessionmanager() else {
            return false;
        };
        let Ok(sessions) = manager.get_audiosessionenumerator() else {
            return false;
        };
        let Ok(count) = sessions.get_count() else {
            return false;
        };
        let mut playing = false;
        for index in 0..count {
            let Ok(session) = sessions.get_session(index) else {
                continue;
            };
            let Some(process_id) = session.get_process_id().ok().filter(|id| *id != 0) else {
                continue;
            };
            live_processes.insert(process_id);
            let active = matches!(session.get_state(), Ok(SessionState::Active));
            let call_application = self.is_call_application(process_id, active, device);
            if !call_application {
                continue;
            }
            playing |= active;
            let key = session
                .get_session_instance_identifier()
                .unwrap_or_else(|_| format!("{process_id}"));
            if !self.sessions.contains_key(&key) {
                let mut callbacks = EventCallbacks::new();
                let signals = self.signals.clone();
                callbacks.set_state_callback(move |_| notify(&signals));
                match session.register_session_notification(callbacks) {
                    Ok(registration) => {
                        self.sessions.insert(key.clone(), registration);
                    }
                    Err(error) => log::debug!("not watching session {key}: {error}"),
                }
            }
            live_sessions.insert(key);
        }
        playing
    }

    fn is_call_application(&mut self, process_id: u32, active: bool, device: &Device) -> bool {
        let (path, call_application) = self
            .processes
            .entry(process_id)
            .or_insert_with(|| {
                let path = process_image_path(process_id).unwrap_or_default();
                let call_application = is_teams_executable(&path);
                (path, call_application)
            })
            .clone();
        if active && !path.is_empty() && self.reported_apps.insert(path.clone()) {
            log::debug!(
                "audio session of {path} on {}{}",
                device.get_friendlyname().unwrap_or_default(),
                if call_application {
                    " (call application)"
                } else {
                    ""
                }
            );
        }
        call_application
    }
}
