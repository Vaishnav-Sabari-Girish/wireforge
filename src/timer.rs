//! TimerScheduler: the ordered set of deadlines the main loop owns.

use std::time::Instant;

/// The timed behaviours the loop owns, driven by ONE scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerId {
    /// Clear the transient hot-reload status line after STATUS_TIMEOUT.
    StatusExpiry,
    /// mtime-poll fallback (non-Linux / inotify unavailable).
    ReloadPoll,
    /// Parse a reloaded file after the write-settle delay.
    ReloadParse,
    /// Fallback terminal-size check (see RESIZE_CHECK_INTERVAL).
    ResizeCheck,
}

/// One timer scheduler owned by the loop (a small ordered set of deadlines).
pub struct TimerScheduler {
    entries: Vec<(Instant, TimerId)>,
}

impl TimerScheduler {
    pub fn new() -> Self {
        TimerScheduler {
            entries: Vec::new(),
        }
    }

    /// Schedule `id` at `at`, replacing any existing entry for the same id.
    pub fn schedule(&mut self, id: TimerId, at: Instant) {
        self.entries.retain(|(_, tid)| *tid != id);
        self.entries.push((at, id));
        self.entries.sort_by_key(|(at, _)| *at);
    }

    pub fn earliest(&self) -> Option<Instant> {
        self.entries.first().map(|(at, _)| *at)
    }

    /// Pop and return every timer whose deadline has passed.
    pub fn fire_due(&mut self, now: Instant) -> Vec<TimerId> {
        let mut due = Vec::new();
        self.entries.retain(|(at, id)| {
            if *at <= now {
                due.push(*id);
                false
            } else {
                true
            }
        });
        due
    }

    /// Remove a pending timer entirely (test-only helper).
    #[cfg(test)]
    pub fn cancel(&mut self, id: TimerId) {
        self.entries.retain(|(_, tid)| *tid != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timer_scheduler_fires_in_order() {
        let mut t = TimerScheduler::new();
        let now = Instant::now();
        assert_eq!(t.earliest(), None, "empty scheduler has no deadline");
        t.schedule(TimerId::StatusExpiry, now + Duration::from_millis(4000));
        t.schedule(TimerId::ReloadParse, now + Duration::from_millis(100));
        assert_eq!(t.earliest(), Some(now + Duration::from_millis(100)));
        // Nothing due yet.
        assert!(t.fire_due(now + Duration::from_millis(50)).is_empty());
        // The reload-parse timer fires first.
        let due = t.fire_due(now + Duration::from_millis(100));
        assert_eq!(due, vec![TimerId::ReloadParse]);
        // The status timer remains.
        assert_eq!(t.earliest(), Some(now + Duration::from_millis(4000)));
        let due2 = t.fire_due(now + Duration::from_secs(10));
        assert_eq!(due2, vec![TimerId::StatusExpiry]);
        assert_eq!(t.earliest(), None);
    }

    #[test]
    fn timer_scheduler_same_id_replaces() {
        let mut t = TimerScheduler::new();
        let now = Instant::now();
        t.schedule(TimerId::StatusExpiry, now + Duration::from_millis(4000));
        // Re-showing a status line replaces the deadline.
        t.schedule(TimerId::StatusExpiry, now + Duration::from_millis(8000));
        let due = t.fire_due(now + Duration::from_secs(5));
        assert!(due.is_empty(), "old deadline must be replaced");
        let due = t.fire_due(now + Duration::from_secs(9));
        assert_eq!(due, vec![TimerId::StatusExpiry]);
        // Cancel removes the entry entirely.
        t.schedule(TimerId::ReloadParse, now + Duration::from_millis(1));
        t.cancel(TimerId::ReloadParse);
        assert_eq!(t.earliest(), None);
    }
}
