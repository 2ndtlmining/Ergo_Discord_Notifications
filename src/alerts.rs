//! When to alert: debounced state transitions plus reminders (#5).
//!
//! Each monitored subject (a node, the explorers) has a `Tracker`. It is fed
//! the observed condition every poll and returns an event only when a new
//! condition has been seen `confirm_after` times in a row, or when a problem
//! has lasted another `reminder_every`.

use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Changed {
        from: String,
        to: String,
        /// How long the previous condition lasted.
        lasted: Duration,
    },
    Reminder {
        condition: String,
        since: DateTime<Utc>,
    },
}

#[derive(Debug, Default)]
pub struct Tracker {
    confirmed: Option<Confirmed>,
    pending: Option<(String, u32)>,
}

#[derive(Debug)]
struct Confirmed {
    condition: String,
    since: DateTime<Utc>,
    last_alert: DateTime<Utc>,
}

impl Tracker {
    #[cfg(test)]
    /// The condition currently confirmed, if any.
    pub fn condition(&self) -> Option<&str> {
        self.confirmed.as_ref().map(|c| c.condition.as_str())
    }

    pub fn since(&self) -> Option<DateTime<Utc>> {
        self.confirmed.as_ref().map(|c| c.since)
    }

    pub fn observe(
        &mut self,
        condition: &str,
        now: DateTime<Utc>,
        confirm_after: u32,
        reminder_every: Duration,
    ) -> Option<Event> {
        // First observation sets the baseline silently; the startup summary covers it.
        let Some(current) = self.confirmed.as_mut() else {
            self.confirmed = Some(Confirmed {
                condition: condition.to_string(),
                since: now,
                last_alert: now,
            });
            return None;
        };

        if current.condition == condition {
            self.pending = None;
            if condition != "ok" && now - current.last_alert >= reminder_every {
                current.last_alert = now;
                return Some(Event::Reminder {
                    condition: condition.to_string(),
                    since: current.since,
                });
            }
            return None;
        }

        let count = match &self.pending {
            Some((c, n)) if c == condition => n + 1,
            _ => 1,
        };
        if count < confirm_after {
            self.pending = Some((condition.to_string(), count));
            return None;
        }

        self.pending = None;
        let event = Event::Changed {
            from: current.condition.clone(),
            to: condition.to_string(),
            lasted: now - current.since,
        };
        *current = Confirmed {
            condition: condition.to_string(),
            since: now,
            last_alert: now,
        };
        Some(event)
    }
}

/// "1d 3h", "2h 5m", "4m", "30s".
pub fn human_duration(d: Duration) -> String {
    let s = d.num_seconds().max(0);
    let (days, hours, mins) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60);
    match (days, hours, mins) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(mins: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap() + Duration::minutes(mins)
    }

    const REMIND: Duration = Duration::minutes(30);

    #[test]
    fn baseline_is_silent_and_blips_are_debounced() {
        let mut tr = Tracker::default();
        assert_eq!(tr.observe("ok", t(0), 2, REMIND), None);
        // One bad poll followed by recovery: no alert.
        assert_eq!(tr.observe("down", t(1), 2, REMIND), None);
        assert_eq!(tr.observe("ok", t(2), 2, REMIND), None);
        assert_eq!(tr.condition(), Some("ok"));
    }

    #[test]
    fn confirmed_change_then_reminders_then_recovery() {
        let mut tr = Tracker::default();
        tr.observe("ok", t(0), 2, REMIND);
        assert_eq!(tr.observe("down", t(10), 2, REMIND), None);
        assert_eq!(
            tr.observe("down", t(11), 2, REMIND),
            Some(Event::Changed {
                from: "ok".into(),
                to: "down".into(),
                lasted: Duration::minutes(11)
            })
        );
        // Still down: nothing until the cooldown passes.
        assert_eq!(tr.observe("down", t(20), 2, REMIND), None);
        assert_eq!(
            tr.observe("down", t(41), 2, REMIND),
            Some(Event::Reminder {
                condition: "down".into(),
                since: t(11)
            })
        );
        tr.observe("ok", t(50), 2, REMIND);
        assert!(matches!(
            tr.observe("ok", t(51), 2, REMIND),
            Some(Event::Changed { ref to, lasted, .. }) if to == "ok" && lasted == Duration::minutes(40)
        ));
        // Healthy nodes never get reminders.
        assert_eq!(tr.observe("ok", t(500), 2, REMIND), None);
    }

    #[test]
    fn switching_between_problems_alerts() {
        let mut tr = Tracker::default();
        tr.observe("behind", t(0), 2, REMIND);
        tr.observe("down", t(1), 2, REMIND);
        assert!(matches!(
            tr.observe("down", t(2), 2, REMIND),
            Some(Event::Changed { ref from, .. }) if from == "behind"
        ));
    }

    #[test]
    fn formats_durations() {
        assert_eq!(human_duration(Duration::seconds(30)), "30s");
        assert_eq!(human_duration(Duration::minutes(4)), "4m");
        assert_eq!(human_duration(Duration::minutes(125)), "2h 5m");
        assert_eq!(human_duration(Duration::hours(27)), "1d 3h");
    }
}
