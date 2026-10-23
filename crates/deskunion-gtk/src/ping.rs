/// How good the link to a client looks from its round trip time. Input should
/// arrive within a frame or two on a local network, so the steps are tight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PingLevel {
    Excellent,
    Good,
    Fair,
    Weak,
    /// connected, but no probe has been answered yet
    Unknown,
}

impl PingLevel {
    pub fn from_ms(ping_ms: Option<u32>) -> Self {
        match ping_ms {
            None => Self::Unknown,
            Some(ms) if ms <= 20 => Self::Excellent,
            Some(ms) if ms <= 50 => Self::Good,
            Some(ms) if ms <= 120 => Self::Fair,
            Some(_) => Self::Weak,
        }
    }

    /// arcs that fill up with the quality of the link
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Excellent => "network-wireless-signal-excellent-symbolic",
            Self::Good => "network-wireless-signal-good-symbolic",
            Self::Fair => "network-wireless-signal-ok-symbolic",
            Self::Weak => "network-wireless-signal-weak-symbolic",
            Self::Unknown => "network-wireless-signal-none-symbolic",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Self::Excellent | Self::Good => "success",
            Self::Fair => "warning",
            Self::Weak => "error",
            Self::Unknown => "dim-label",
        }
    }
}

pub fn ping_label(ping_ms: Option<u32>) -> String {
    match ping_ms {
        Some(ms) => format!("{ms} ms"),
        None => "…".to_string(),
    }
}

pub fn ping_description(ping_ms: Option<u32>) -> String {
    match ping_ms {
        Some(ms) => format!("Round trip to this computer: {ms} ms"),
        None => "Measuring the round trip to this computer".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_gets_worse_as_the_ping_grows() {
        let levels =
            [0, 20, 21, 50, 51, 120, 121, 5_000, u32::MAX].map(|ms| PingLevel::from_ms(Some(ms)));
        assert_eq!(
            levels,
            [
                PingLevel::Excellent,
                PingLevel::Excellent,
                PingLevel::Good,
                PingLevel::Good,
                PingLevel::Fair,
                PingLevel::Fair,
                PingLevel::Weak,
                PingLevel::Weak,
                PingLevel::Weak,
            ]
        );
    }

    #[test]
    fn no_answer_yet_is_not_shown_as_a_good_or_bad_link() {
        assert_eq!(PingLevel::from_ms(None), PingLevel::Unknown);
        assert_eq!(PingLevel::Unknown.css_class(), "dim-label");
        assert_eq!(ping_label(None), "…");
    }

    #[test]
    fn every_level_has_its_own_icon() {
        let levels = [
            PingLevel::Excellent,
            PingLevel::Good,
            PingLevel::Fair,
            PingLevel::Weak,
            PingLevel::Unknown,
        ];
        let mut icons: Vec<_> = levels.iter().map(|level| level.icon_name()).collect();
        icons.sort_unstable();
        icons.dedup();
        assert_eq!(icons.len(), levels.len());
    }

    #[test]
    fn the_text_names_the_value() {
        assert_eq!(ping_label(Some(12)), "12 ms");
        assert!(ping_description(Some(12)).contains("12 ms"));
    }
}
