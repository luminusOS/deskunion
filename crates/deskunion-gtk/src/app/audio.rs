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

fn audio_device_labels(devices: &[AudioDeviceInfo], configured: Option<&str>) -> Vec<String> {
    let mut labels = vec!["System default".to_string()];
    labels.extend(devices.iter().map(audio_device_label));
    if let Some(id) = configured
        && !devices.iter().any(|device| device.id == id)
    {
        labels.push(format!("Unavailable device ({id})"));
    }
    labels
}

pub fn audio_device_model(
    devices: &[AudioDeviceInfo],
    configured: Option<&str>,
) -> gtk::StringList {
    let labels = audio_device_labels(devices, configured);
    gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>())
}

pub fn audio_device_index(devices: &[AudioDeviceInfo], id: Option<&str>) -> u32 {
    match id {
        None => 0,
        Some(id) => devices
            .iter()
            .position(|d| d.id == id)
            .map_or(devices.len() as u32 + 1, |i| i as u32 + 1),
    }
}

/// Index zero selects the default; an unavailable configured device is kept
/// after the enumerated devices so it cannot masquerade as the default.
pub fn selected_audio_device(
    devices: &[AudioDeviceInfo],
    selected: u32,
    configured: Option<&str>,
) -> Option<String> {
    if selected == 0 {
        None
    } else if let Some(device) = devices.get(selected as usize - 1) {
        Some(device.id.clone())
    } else {
        configured.map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_device_stays_distinct_from_default() {
        let devices = Vec::new();
        assert_eq!(audio_device_index(&devices, None), 0);
        assert_eq!(audio_device_index(&devices, Some("missing")), 1);
        assert_eq!(
            audio_device_labels(&devices, Some("missing")),
            ["System default", "Unavailable device (missing)"]
        );
        assert_eq!(
            selected_audio_device(&devices, 1, Some("missing")).as_deref(),
            Some("missing")
        );
        assert_eq!(selected_audio_device(&devices, 0, Some("missing")), None);
    }

    #[test]
    fn returning_device_keeps_its_selection_and_removes_unavailable_entry() {
        let devices = vec![AudioDeviceInfo {
            id: "headset".to_owned(),
            name: "Headset".to_owned(),
            is_monitor: true,
            is_default: false,
        }];
        assert_eq!(audio_device_index(&devices, Some("headset")), 1);
        assert_eq!(
            audio_device_labels(&devices, Some("headset")),
            ["System default", "Headset (system audio)"]
        );
        assert_eq!(
            selected_audio_device(&devices, 1, None).as_deref(),
            Some("headset")
        );
        assert_eq!(audio_device_index(&devices, Some("other")), 2);
    }
}
