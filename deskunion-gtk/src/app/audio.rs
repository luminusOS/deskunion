use deskunion_ipc::AudioDeviceInfo;

/// kbps choices backing `audio_bitrate_combo`, in encode order — see
/// `deskunion/DESKUNION_AUDIO_PLAN.md` §7.4. Values are bits/sec, matching
/// `FrontendRequest::UpdateAudioSettings`'s `bitrate` field.
pub const AUDIO_BITRATES: [u32; 5] = [64_000, 96_000, 128_000, 192_000, 256_000];

pub fn audio_bitrate_index(bitrate: u32) -> u32 {
    AUDIO_BITRATES
        .iter()
        .position(|&b| b == bitrate)
        .unwrap_or(1) as u32
}

/// device picker label, `is_monitor` sources (system-output loopback)
/// get a suffix so they read differently from real input devices.
fn audio_device_label(device: &AudioDeviceInfo) -> String {
    if device.is_monitor {
        format!("{} (system audio)", device.name)
    } else {
        device.name.clone()
    }
}

pub fn audio_device_model(devices: &[AudioDeviceInfo]) -> gtk::StringList {
    let mut labels = vec!["System Default".to_string()];
    labels.extend(devices.iter().map(audio_device_label));
    gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>())
}

/// combo index 0 is the synthetic "System Default" entry (`None`);
/// index `i` for `i >= 1` maps to `devices[i - 1]`.
pub fn selected_audio_device(devices: &[AudioDeviceInfo], selected: u32) -> Option<String> {
    (selected != 0)
        .then(|| devices.get(selected as usize - 1))
        .flatten()
        .map(|d| d.id.clone())
}
