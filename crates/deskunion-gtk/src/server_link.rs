/// A dial to the server failed and the service scheduled another attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerRetry {
    pub in_secs: u32,
    pub awaiting_authorization: bool,
}

impl ServerRetry {
    /// Delays of a second or less are an ongoing reconnect, not a wait worth
    /// announcing.
    pub fn short_text(&self) -> String {
        if self.awaiting_authorization {
            "waiting for authorization".to_string()
        } else if self.in_secs <= 1 {
            "reconnecting…".to_string()
        } else {
            format!("retrying in {}", delay_text(self.in_secs))
        }
    }
}

fn delay_text(secs: u32) -> String {
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min", secs.div_ceil(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retry(in_secs: u32) -> ServerRetry {
        ServerRetry {
            in_secs,
            awaiting_authorization: false,
        }
    }

    #[test]
    fn a_one_second_delay_reads_as_reconnecting() {
        assert_eq!(retry(1).short_text(), "reconnecting…");
        assert_eq!(retry(0).short_text(), "reconnecting…");
    }

    #[test]
    fn longer_delays_name_the_wait() {
        assert_eq!(retry(5).short_text(), "retrying in 5 s");
        assert_eq!(retry(30).short_text(), "retrying in 30 s");
        assert_eq!(retry(60).short_text(), "retrying in 1 min");
        assert_eq!(retry(120).short_text(), "retrying in 2 min");
        assert_eq!(retry(300).short_text(), "retrying in 5 min");
    }

    #[test]
    fn waiting_for_authorization_is_not_a_failure_to_connect() {
        let waiting = ServerRetry {
            in_secs: 15,
            awaiting_authorization: true,
        };
        assert_eq!(waiting.short_text(), "waiting for authorization");
    }
}
