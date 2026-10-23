use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long an output stays captured after the last Teams session on it went
/// quiet, so a pause between two sounds does not hand the stream back and forth.
pub(super) const RELEASE_AFTER: Duration = Duration::from_secs(3);
/// Upper bound between two looks at the audio sessions. Notifications make
/// this rarely matter; it only covers one that Windows never delivered.
pub(super) const RESYNC: Duration = Duration::from_secs(5);
/// After a notification the sessions are looked at quickly for this long:
/// the session list of Windows may not show a new session at once.
const SETTLE: Duration = Duration::from_secs(2);
const SETTLE_STEP: Duration = Duration::from_millis(250);
const MIN_WAIT: Duration = Duration::from_millis(10);

/// Executables of the Teams desktop client. Process loopback returns only
/// silence for them (microsoft/Windows-classic-samples#414), so a call has to
/// be captured from the output it plays on.
const TEAMS_EXECUTABLES: [&str; 2] = ["ms-teams.exe", "teams.exe"];

pub(super) fn is_teams_executable(image_path: &str) -> bool {
    let name = image_path.rsplit(['\\', '/']).next().unwrap_or(image_path);
    TEAMS_EXECUTABLES
        .iter()
        .any(|teams| name.eq_ignore_ascii_case(teams))
}

/// Remembers which outputs carried a Teams session recently.
#[derive(Default)]
pub(super) struct CallTracker {
    last_seen: HashMap<String, Instant>,
}

impl CallTracker {
    /// `playing` are the outputs with an active Teams session right now.
    /// Returns every output that should be captured, in a stable order.
    pub(super) fn update(&mut self, now: Instant, playing: &[String]) -> Vec<String> {
        for device in playing {
            self.last_seen.insert(device.clone(), now);
        }
        self.last_seen
            .retain(|_, seen| now.duration_since(*seen) < RELEASE_AFTER);
        let mut held: Vec<String> = self.last_seen.keys().cloned().collect();
        held.sort();
        held
    }

    /// Time until the next held output is due for release, if it is not
    /// seen playing again.
    pub(super) fn next_release_in(&self, now: Instant) -> Option<Duration> {
        self.last_seen
            .values()
            .map(|seen| (*seen + RELEASE_AFTER).saturating_duration_since(now))
            .min()
    }
}

/// Decides how long to wait before looking at the audio sessions again.
#[derive(Default)]
pub(super) struct ScanPacer {
    settle_until: Option<Instant>,
}

impl ScanPacer {
    /// a notification arrived
    pub(super) fn changed(&mut self, now: Instant) {
        self.settle_until = Some(now + SETTLE);
    }

    pub(super) fn next_wait(&mut self, now: Instant, release_in: Option<Duration>) -> Duration {
        let mut wait = RESYNC;
        if self.settle_until.is_some_and(|until| now < until) {
            wait = wait.min(SETTLE_STEP);
        } else {
            self.settle_until = None;
        }
        if let Some(release_in) = release_in {
            wait = wait.min(release_in.max(MIN_WAIT));
        }
        wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn both_teams_clients_are_recognised_whatever_the_path_or_case() {
        assert!(is_teams_executable("ms-teams.exe"));
        assert!(is_teams_executable("Teams.exe"));
        assert!(is_teams_executable(
            r"C:\Program Files\WindowsApps\MSTeams_1.0.0.0_x64__8wekyb3d8bbwe\ms-teams.exe"
        ));
        assert!(is_teams_executable(
            r"C:\Users\me\AppData\Local\Microsoft\Teams\current\TEAMS.EXE"
        ));
        assert!(is_teams_executable("/mnt/c/apps/ms-teams.exe"));
    }

    #[test]
    fn other_programs_are_not_mistaken_for_teams() {
        assert!(!is_teams_executable(r"C:\Windows\System32\svchost.exe"));
        assert!(!is_teams_executable(r"C:\apps\steamteams.exe"));
        assert!(!is_teams_executable(r"C:\apps\ms-teams.exe.bak"));
        assert!(!is_teams_executable(r"C:\teams.exe\other.exe"));
        assert!(!is_teams_executable(""));
    }

    #[test]
    fn an_output_is_captured_as_soon_as_teams_plays_on_it() {
        let mut tracker = CallTracker::default();
        let now = Instant::now();
        assert!(tracker.update(now, &[]).is_empty());
        assert_eq!(tracker.update(now, &devices(&["headset"])), ["headset"]);
    }

    #[test]
    fn a_short_pause_does_not_release_the_output() {
        let mut tracker = CallTracker::default();
        let start = Instant::now();
        tracker.update(start, &devices(&["headset"]));
        let during_pause = start + RELEASE_AFTER - Duration::from_millis(1);
        assert_eq!(tracker.update(during_pause, &[]), ["headset"]);
    }

    #[test]
    fn the_output_is_released_once_teams_stayed_quiet_long_enough() {
        let mut tracker = CallTracker::default();
        let start = Instant::now();
        tracker.update(start, &devices(&["headset"]));
        assert!(tracker.update(start + RELEASE_AFTER, &[]).is_empty());
    }

    #[test]
    fn playing_again_restarts_the_wait() {
        let mut tracker = CallTracker::default();
        let start = Instant::now();
        tracker.update(start, &devices(&["headset"]));
        let later = start + RELEASE_AFTER - Duration::from_millis(1);
        tracker.update(later, &devices(&["headset"]));
        let almost = later + RELEASE_AFTER - Duration::from_millis(1);
        assert_eq!(tracker.update(almost, &[]), ["headset"]);
        assert!(tracker.update(later + RELEASE_AFTER, &[]).is_empty());
    }

    #[test]
    fn outputs_are_tracked_independently_and_listed_in_order() {
        let mut tracker = CallTracker::default();
        let start = Instant::now();
        tracker.update(start, &devices(&["speakers"]));
        let held = tracker.update(start + Duration::from_secs(2), &devices(&["headset"]));
        assert_eq!(held, ["headset", "speakers"]);
        let held = tracker.update(start + RELEASE_AFTER, &devices(&["headset"]));
        assert_eq!(held, ["headset"]);
    }

    #[test]
    fn the_release_deadline_is_the_nearest_one() {
        let mut tracker = CallTracker::default();
        let start = Instant::now();
        assert_eq!(tracker.next_release_in(start), None);
        tracker.update(start, &devices(&["speakers"]));
        tracker.update(start + Duration::from_secs(1), &devices(&["headset"]));
        let now = start + Duration::from_secs(2);
        assert_eq!(tracker.next_release_in(now), Some(Duration::from_secs(1)));
    }

    #[test]
    fn a_due_release_does_not_underflow() {
        let mut tracker = CallTracker::default();
        let start = Instant::now();
        tracker.update(start, &devices(&["headset"]));
        let late = start + RELEASE_AFTER + Duration::from_secs(1);
        assert_eq!(tracker.next_release_in(late), Some(Duration::ZERO));
    }

    #[test]
    fn an_idle_watcher_only_resyncs_now_and_then() {
        let mut pacer = ScanPacer::default();
        assert_eq!(pacer.next_wait(Instant::now(), None), RESYNC);
    }

    #[test]
    fn a_notification_makes_the_next_looks_quick_for_a_while() {
        let mut pacer = ScanPacer::default();
        let start = Instant::now();
        pacer.changed(start);
        assert_eq!(pacer.next_wait(start, None), SETTLE_STEP);
        let almost = start + SETTLE - Duration::from_millis(1);
        assert_eq!(pacer.next_wait(almost, None), SETTLE_STEP);
        assert_eq!(pacer.next_wait(start + SETTLE, None), RESYNC);
    }

    #[test]
    fn a_pending_release_shortens_the_wait() {
        let mut pacer = ScanPacer::default();
        let now = Instant::now();
        let wait = pacer.next_wait(now, Some(Duration::from_millis(700)));
        assert_eq!(wait, Duration::from_millis(700));
        assert_eq!(pacer.next_wait(now, Some(Duration::ZERO)), MIN_WAIT);
    }

    #[test]
    fn a_new_notification_extends_the_quick_period() {
        let mut pacer = ScanPacer::default();
        let start = Instant::now();
        pacer.changed(start);
        let later = start + SETTLE - Duration::from_millis(1);
        pacer.changed(later);
        assert_eq!(pacer.next_wait(start + SETTLE, None), SETTLE_STEP);
    }
}
