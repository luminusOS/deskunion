//! User-facing text of the main window: toasts and list placeholders.
//! GNOME writes these in sentence case, and toasts stay short.

use std::fmt::Display;

pub(super) const NO_AUTHORIZED_DEVICES: (&str, &str) = (
    "No authorized devices",
    "Authorize a device using its certificate fingerprint",
);

pub(super) const NO_INCOMING_DEVICES: (&str, &str) = (
    "No devices controlling this computer",
    "Paired devices appear here when they send input",
);

pub(super) const NO_PARKED_DEVICES: (&str, &str) = (
    "No devices waiting",
    "Devices are paired on the first free screen edge and only wait here when all four are taken",
);

pub(super) const NO_AUDIO_STREAMS: (&str, &str) = (
    "No active audio streams",
    "Enable audio sharing on the sending and receiving computers",
);

pub(super) fn device_paired(pos: impl Display) -> String {
    format!("Device paired on the {pos}")
}

pub(super) fn device_waiting(addr: impl Display) -> String {
    format!("{addr} is waiting: every screen edge is taken")
}

pub(super) fn connected_to_server(addr: impl Display) -> String {
    format!("Connected to server {addr}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words that keep their capital in the middle of a sentence.
    const PROPER: [&str; 2] = ["SHA-256", "DeskUnion"];

    fn assert_sentence_case(text: &str) {
        let mut words = text.split_whitespace();
        let first = words.next().expect("empty text");
        let starts_upper = first.chars().next().is_some_and(|c| !c.is_lowercase());
        assert!(starts_upper, "{text:?} must start with a capital");
        for word in words {
            let capital = word.chars().next().is_some_and(char::is_uppercase);
            assert!(
                !capital || PROPER.contains(&word),
                "{text:?} is not in sentence case: {word:?}"
            );
        }
    }

    #[test]
    fn placeholders_and_toasts_use_sentence_case() {
        for (title, subtitle) in [
            NO_AUTHORIZED_DEVICES,
            NO_INCOMING_DEVICES,
            NO_PARKED_DEVICES,
            NO_AUDIO_STREAMS,
        ] {
            assert_sentence_case(title);
            assert_sentence_case(subtitle);
        }
        assert_sentence_case(&device_paired("right"));
        assert_sentence_case(&connected_to_server("192.0.2.1:4242"));
        assert_sentence_case(&device_waiting("192.0.2.1:4242"));
    }
}
