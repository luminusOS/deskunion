use super::{operation_mode_index, parse_listening_port};
use deskunion_ipc::DEFAULT_PORT;

#[test]
fn blank_port_uses_the_default() {
    assert_eq!(parse_listening_port(""), Some(DEFAULT_PORT));
    assert_eq!(parse_listening_port("  "), Some(DEFAULT_PORT));
}

#[test]
fn listening_port_accepts_the_valid_range() {
    assert_eq!(parse_listening_port("1"), Some(1));
    assert_eq!(parse_listening_port("65535"), Some(u16::MAX));
    assert_eq!(parse_listening_port(" 4243 "), Some(4243));
}

#[test]
fn invalid_port_does_not_silently_change_to_the_default() {
    for value in ["0", "65536", "-1", "abc", "42.42"] {
        assert_eq!(parse_listening_port(value), None, "{value}");
    }
}

#[test]
fn unconfigured_mode_does_not_claim_server_is_selected() {
    use deskunion_ipc::OperationMode;
    assert_eq!(operation_mode_index(OperationMode::Unconfigured), 0);
    assert_eq!(operation_mode_index(OperationMode::Server), 1);
    assert_eq!(operation_mode_index(OperationMode::Client), 2);
}

/// While input emulation restarts on its own (screen lock, a reset EIS
/// socket) the client must say so, not claim emulation is disabled and ask
/// the user to re-enable it.
#[test]
fn a_restarting_emulation_is_not_reported_as_disabled() {
    use super::{ClientEmulation, client_emulation};
    assert_eq!(
        client_emulation(true, false, true),
        ClientEmulation::Running
    );
    assert_eq!(
        client_emulation(false, true, true),
        ClientEmulation::Restarting
    );
    assert_eq!(
        client_emulation(false, false, true),
        ClientEmulation::Disabled
    );
    assert_eq!(
        client_emulation(false, false, false),
        ClientEmulation::Stopped
    );
    assert_eq!(
        client_emulation(false, true, false),
        ClientEmulation::Stopped
    );
}
