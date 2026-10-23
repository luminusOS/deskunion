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

    pub fn status_text(&self, hostname: &str, port: u16) -> String {
        if self.awaiting_authorization {
            return format!(
                "Waiting for this computer to be authorized on {hostname}. Trying again in {}",
                delay_text(self.in_secs)
            );
        }
        let wait = if self.in_secs <= 1 {
            "Reconnecting…".to_string()
        } else {
            format!("Retrying in {}", delay_text(self.in_secs))
        };
        format!(
            "Can't reach {hostname}:{port}. {wait}. Check the address and that UDP port {port} is allowed in the server's firewall"
        )
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
        let status = waiting.status_text("desk", 4242);
        assert!(status.contains("authorized on desk"), "{status}");
        assert!(status.contains("15 s"), "{status}");
        assert!(!status.contains("firewall"), "{status}");
    }

    #[test]
    fn the_status_text_names_the_endpoint_and_the_wait() {
        let status = retry(10).status_text("desk", 4242);
        assert!(status.contains("desk:4242"), "{status}");
        assert!(status.contains("Retrying in 10 s"), "{status}");
        assert!(status.contains("UDP port 4242"), "{status}");
    }
}
