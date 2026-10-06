use super::parse_listening_port;
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
