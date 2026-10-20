#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn supports_process_loopback(major_version: u32, build_number: u32) -> bool {
    major_version > 10 || (major_version == 10 && build_number >= 20_348)
}

#[cfg(any(windows, test))]
pub(super) fn use_process_loopback(
    major_version: u32,
    build_number: u32,
    device: Option<&str>,
) -> bool {
    supports_process_loopback(major_version, build_number)
        && device.is_none_or(|id| id == "process-loopback")
}

#[cfg(test)]
mod tests {
    use super::{supports_process_loopback, use_process_loopback};

    #[test]
    fn process_loopback_requires_windows_10_build_20348() {
        assert!(!supports_process_loopback(10, 20_000));
        assert!(!supports_process_loopback(10, 20_347));
        assert!(supports_process_loopback(10, 20_348));
        assert!(supports_process_loopback(10, 22_000));
        assert!(supports_process_loopback(11, 22_000));
    }

    #[test]
    fn explicit_endpoint_is_not_ignored_on_modern_windows() {
        assert!(use_process_loopback(10, 22000, None));
        assert!(use_process_loopback(10, 22000, Some("process-loopback")));
        assert!(!use_process_loopback(10, 22000, Some("headset-endpoint")));
        assert!(!use_process_loopback(10, 19045, None));
        assert!(!use_process_loopback(10, 19045, Some("process-loopback")));
    }
}
