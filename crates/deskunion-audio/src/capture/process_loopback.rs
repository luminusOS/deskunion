#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn supports_process_loopback(major_version: u32, build_number: u32) -> bool {
    major_version > 10 || (major_version == 10 && build_number >= 20_348)
}

#[cfg(test)]
mod tests {
    use super::supports_process_loopback;

    #[test]
    fn process_loopback_requires_windows_10_build_20348() {
        assert!(!supports_process_loopback(10, 20_000));
        assert!(!supports_process_loopback(10, 20_347));
        assert!(supports_process_loopback(10, 20_348));
        assert!(supports_process_loopback(10, 22_000));
        assert!(supports_process_loopback(11, 22_000));
    }
}
