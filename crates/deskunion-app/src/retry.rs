use std::time::Duration;

/// How long to wait before the next dial after `failures` failed attempts in a
/// row: the schedule of Deskflow's `ClientApp::retryTime`. It stays at one
/// second for the first 300 failures and then backs off to five minutes.
pub(crate) fn retry_delay(failures: u32) -> Duration {
    const STEPS: [(u32, u64); 6] = [
        (300, 1),
        (360, 5),
        (390, 10),
        (410, 30),
        (420, 60),
        (430, 120),
    ];
    let seconds = STEPS
        .iter()
        .find(|(below, _)| failures < *below)
        .map_or(300, |(_, seconds)| *seconds);
    Duration::from_secs(seconds)
}

/// Failures since the last session. Replacing it with `Backoff::default()`
/// starts over from the short delay, as after a session that dropped.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Backoff {
    failures: u32,
}

impl Backoff {
    #[cfg(test)]
    pub(crate) fn after(failures: u32) -> Self {
        Self { failures }
    }

    /// Delay to wait now; the failure it follows is counted.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let delay = retry_delay(self.failures);
        self.failures = self.failures.saturating_add(1);
        delay
    }
}

/// What the user is told while a dial is being retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetryNotice {
    pub(crate) in_secs: u32,
    pub(crate) awaiting_authorization: bool,
}

impl RetryNotice {
    pub(crate) fn new(delay: Duration, awaiting_authorization: bool) -> Self {
        Self {
            in_secs: u32::try_from(delay.as_secs()).unwrap_or(u32::MAX),
            awaiting_authorization,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(failures: u32) -> u64 {
        retry_delay(failures).as_secs()
    }

    #[test]
    fn stays_at_one_second_for_the_first_300_failures() {
        assert_eq!(secs(0), 1);
        assert_eq!(secs(1), 1);
        assert_eq!(secs(299), 1);
    }

    #[test]
    fn backs_off_at_deskflows_thresholds() {
        assert_eq!(secs(300), 5);
        assert_eq!(secs(359), 5);
        assert_eq!(secs(360), 10);
        assert_eq!(secs(389), 10);
        assert_eq!(secs(390), 30);
        assert_eq!(secs(409), 30);
        assert_eq!(secs(410), 60);
        assert_eq!(secs(419), 60);
        assert_eq!(secs(420), 120);
        assert_eq!(secs(429), 120);
        assert_eq!(secs(430), 300);
        assert_eq!(secs(u32::MAX), 300);
    }

    #[test]
    fn the_delay_never_shrinks_as_failures_pile_up() {
        let mut previous = Duration::ZERO;
        for failures in 0..600 {
            let delay = retry_delay(failures);
            assert!(
                delay >= previous,
                "{failures} failures: {delay:?} < {previous:?}"
            );
            previous = delay;
        }
    }

    #[test]
    fn backoff_counts_each_delay_it_hands_out() {
        let mut backoff = Backoff::default();
        for _ in 0..300 {
            assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        }
        assert_eq!(backoff.next_delay(), Duration::from_secs(5));
    }

    #[test]
    fn a_fresh_backoff_starts_at_the_short_delay() {
        let mut backoff = Backoff::default();
        for _ in 0..350 {
            backoff.next_delay();
        }
        backoff = Backoff::default();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn the_counter_saturates() {
        let mut backoff = Backoff { failures: u32::MAX };
        assert_eq!(backoff.next_delay(), Duration::from_secs(300));
        assert_eq!(backoff.next_delay(), Duration::from_secs(300));
    }

    #[test]
    fn notice_reports_whole_seconds() {
        assert_eq!(
            RetryNotice::new(Duration::from_secs(15), true),
            RetryNotice {
                in_secs: 15,
                awaiting_authorization: true
            }
        );
    }
}
