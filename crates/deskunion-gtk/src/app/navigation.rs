#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Connections,
    Audio,
    Logs,
    Settings,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Connections, Page::Audio, Page::Logs, Page::Settings];

    pub fn title(self) -> &'static str {
        match self {
            Page::Connections => "Connections",
            Page::Audio => "Audio",
            Page::Logs => "Logs",
            Page::Settings => "Settings",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Page::Connections => "video-joined-displays-symbolic",
            Page::Audio => "audio-speakers-symbolic",
            Page::Logs => "view-list-symbolic",
            Page::Settings => "emblem-system-symbolic",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Page::Connections => "connections",
            Page::Audio => "audio",
            Page::Logs => "logs",
            Page::Settings => "settings",
        }
    }

    pub fn index(self) -> i32 {
        Page::ALL.iter().position(|page| *page == self).unwrap_or(0) as i32
    }

    pub fn from_index(index: i32) -> Self {
        usize::try_from(index)
            .ok()
            .and_then(|index| Page::ALL.get(index).copied())
            .unwrap_or(Page::Connections)
    }
}

#[cfg(test)]
mod tests {
    use super::Page;

    #[test]
    fn pages_round_trip_through_their_sidebar_index() {
        for page in Page::ALL {
            assert_eq!(Page::from_index(page.index()), page);
        }
        assert_eq!(Page::from_index(-1), Page::Connections);
        assert_eq!(Page::from_index(9), Page::Connections);
    }
}
