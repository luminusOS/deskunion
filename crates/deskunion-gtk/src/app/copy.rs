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

pub(super) const SERVER_IDENTITY_CHANGED: &str = "Server certificate changed";

pub(super) const SERVER_IDENTITY_BLOCKED: &str =
    "DeskUnion does not connect until you trust the new certificate";

/// Pango markup: a wrapped fingerprint must not gain hyphens, or it no
/// longer matches what the server shows.
pub(super) fn server_identity_changed(expected: &str, got: &str) -> String {
    let fingerprint = |text: &str| {
        format!(
            "<span font_family=\"monospace\" insert_hyphens=\"false\">{}</span>",
            gtk::glib::markup_escape_text(text)
        )
    };
    let (expected, got) = (fingerprint(expected), fingerprint(got));
    format!(
        "The server shows a different certificate than the one it showed before. \
         Trust it only if you reinstalled DeskUnion on the server or replaced its \
         certificate; otherwise another computer may be answering in its place.\n\n\
         Previous SHA-256 fingerprint:\n{expected}\n\nNew SHA-256 fingerprint:\n{got}"
    )
}

pub(super) fn server_certificate(pinned: Option<&str>) -> String {
    match pinned {
        Some(fingerprint) => fingerprint.to_owned(),
        None => "Trusted on the first connection".to_owned(),
    }
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
        assert_sentence_case(SERVER_IDENTITY_CHANGED);
        assert_sentence_case(SERVER_IDENTITY_BLOCKED);
        assert_sentence_case(&server_certificate(None));
    }

    /// the user can only judge the change by comparing both fingerprints,
    /// e.g. with the one the server shows on its own screen
    #[test]
    fn the_identity_warning_shows_both_fingerprints() {
        let text = server_identity_changed("aa:bb", "cc:dd");
        assert!(text.contains("aa:bb") && text.contains("cc:dd"));
        assert!(text.find("aa:bb") < text.find("cc:dd"));
        assert_eq!(server_certificate(Some("aa:bb")), "aa:bb");
    }
}
