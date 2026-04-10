//! Cron-based scheduling for agentd profiles.
//!
//! The scheduler owns timing policy only: it evaluates cron expressions in
//! daemon-local time and dispatches run requests through an abstract
//! [`Dispatcher`]. It does not call `agentd-runner` directly.

use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Local};
use croner::Cron;
use croner::errors::CronError;
use croner::parser::{CronParser, Seconds, Year};

const MAX_IDLE_SLEEP: Duration = Duration::from_secs(1);

/// One scheduled autonomous run source: profile identity, default repo, and a
/// parsed cron expression.
#[derive(Debug, Clone)]
pub struct ScheduledProfile {
    request: ScheduledRunRequest,
    cron: Cron,
}

impl ScheduledProfile {
    /// Parses a five-field cron expression for a scheduled profile.
    pub fn new(profile: String, repo_url: String, schedule: &str) -> Result<Self, CronError> {
        Ok(Self {
            request: ScheduledRunRequest { profile, repo_url },
            cron: parse_schedule(schedule)?,
        })
    }
}

/// A concrete run request emitted by the scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledRunRequest {
    pub profile: String,
    pub repo_url: String,
}

/// Errors returned by a dispatcher implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchError {
    message: String,
}

impl DispatchError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for DispatchError {}

/// Dispatch boundary used by the daemon adapter.
pub trait Dispatcher {
    fn dispatch(&self, request: ScheduledRunRequest) -> Result<(), DispatchError>;
}

/// Time source and sleep boundary for the scheduler loop.
pub trait Clock {
    fn now(&self) -> DateTime<Local>;
    fn sleep(&self, duration: Duration);
}

/// Production clock backed by `chrono::Local` and `std::thread::sleep`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Local> {
        Local::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// In-memory scheduler state that tracks the next fire time per profile.
#[derive(Debug, Clone)]
pub struct Scheduler {
    entries: Vec<ScheduledEntry>,
}

impl Scheduler {
    /// Builds scheduler state from the configured scheduled profiles.
    pub fn new(profiles: Vec<ScheduledProfile>, now: DateTime<Local>) -> Result<Self, CronError> {
        let mut entries = Vec::with_capacity(profiles.len());
        for profile in profiles {
            entries.push(ScheduledEntry::new(profile, now)?);
        }

        Ok(Self { entries })
    }

    /// Returns whether the scheduler has any active profiles.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the next scheduled wake time across all entries.
    pub fn next_wake_at(&self) -> Option<DateTime<Local>> {
        self.entries.iter().map(|entry| entry.next_fire).min()
    }

    /// Dispatches every profile whose next fire time is due at `now`.
    ///
    /// Each due profile dispatches at most once per tick. After dispatch, the
    /// next fire time advances to the first occurrence strictly after `now`,
    /// which intentionally skips missed occurrences instead of backfilling.
    /// Startup seeding is inclusive, but post-dispatch advancement is
    /// intentionally exclusive so one instant cannot dispatch twice.
    pub fn dispatch_due<D: Dispatcher>(
        &mut self,
        now: DateTime<Local>,
        dispatcher: &D,
    ) -> Vec<Result<ScheduledRunRequest, DispatchError>> {
        let mut results = Vec::new();

        for entry in &mut self.entries {
            if entry.next_fire > now {
                continue;
            }

            let request = entry.profile.request.clone();
            let dispatch_result = dispatcher.dispatch(request.clone()).map(|_| request);
            entry.next_fire = entry.profile.cron.find_next_occurrence(&now, false).expect(
                "validated schedules should always have a next occurrence after the current time",
            );
            results.push(dispatch_result);
        }

        results
    }

    fn sleep_duration(&self, now: DateTime<Local>) -> Duration {
        match self.next_wake_at() {
            Some(next_wake_at) => next_wake_at
                .signed_duration_since(now)
                .to_std()
                .unwrap_or(Duration::ZERO)
                .min(MAX_IDLE_SLEEP),
            None => MAX_IDLE_SLEEP,
        }
    }
}

/// Runs the scheduler loop until `shutdown` becomes true.
pub fn run_until_shutdown<D: Dispatcher, C: Clock>(
    scheduler: &mut Scheduler,
    dispatcher: &D,
    clock: &C,
    shutdown: &AtomicBool,
) {
    while !shutdown.load(Ordering::Acquire) {
        let now = clock.now();
        scheduler.dispatch_due(now, dispatcher);

        if shutdown.load(Ordering::Acquire) {
            break;
        }

        clock.sleep(scheduler.sleep_duration(now));
    }
}

#[derive(Debug, Clone)]
struct ScheduledEntry {
    profile: ScheduledProfile,
    next_fire: DateTime<Local>,
}

impl ScheduledEntry {
    fn new(profile: ScheduledProfile, now: DateTime<Local>) -> Result<Self, CronError> {
        let next_fire = profile.cron.find_next_occurrence(&now, true)?;
        Ok(Self { profile, next_fire })
    }
}

fn parse_schedule(schedule: &str) -> Result<Cron, CronError> {
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
        .parse(schedule)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use chrono::{LocalResult, TimeZone};

    #[derive(Debug, Default)]
    struct RecordingDispatcher {
        requests: Arc<Mutex<Vec<ScheduledRunRequest>>>,
    }

    impl RecordingDispatcher {
        fn new() -> Self {
            Self::default()
        }

        fn requests(&self) -> Vec<ScheduledRunRequest> {
            self.requests.lock().expect("requests should lock").clone()
        }
    }

    impl Dispatcher for RecordingDispatcher {
        fn dispatch(&self, request: ScheduledRunRequest) -> Result<(), DispatchError> {
            self.requests
                .lock()
                .expect("requests should lock")
                .push(request);
            Ok(())
        }
    }

    fn local_datetime(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> DateTime<Local> {
        match Local.with_ymd_and_hms(year, month, day, hour, minute, second) {
            LocalResult::Single(datetime) => datetime,
            other => panic!("expected a single local datetime, got {other:?}"),
        }
    }

    #[test]
    fn dispatches_due_profiles_once_each_when_schedules_overlap() {
        let start = local_datetime(2026, 4, 10, 10, 0, 30);
        let mut scheduler = Scheduler::new(
            vec![
                ScheduledProfile::new(
                    "site-builder".to_string(),
                    "https://example.com/site.git".to_string(),
                    "*/15 * * * *",
                )
                .expect("schedule should parse"),
                ScheduledProfile::new(
                    "code-reviewer".to_string(),
                    "https://example.com/review.git".to_string(),
                    "*/15 * * * *",
                )
                .expect("schedule should parse"),
            ],
            start,
        )
        .expect("scheduler should build");
        let dispatcher = RecordingDispatcher::new();

        let outcomes = scheduler.dispatch_due(local_datetime(2026, 4, 10, 10, 15, 0), &dispatcher);

        assert_eq!(outcomes.len(), 2, "both overlapping schedules should fire");
        assert_eq!(
            dispatcher.requests(),
            vec![
                ScheduledRunRequest {
                    profile: "site-builder".to_string(),
                    repo_url: "https://example.com/site.git".to_string(),
                },
                ScheduledRunRequest {
                    profile: "code-reviewer".to_string(),
                    repo_url: "https://example.com/review.git".to_string(),
                },
            ]
        );
    }

    #[test]
    fn skips_missed_occurrences_instead_of_backfilling() {
        let start = local_datetime(2026, 4, 10, 10, 0, 30);
        let mut scheduler = Scheduler::new(
            vec![
                ScheduledProfile::new(
                    "site-builder".to_string(),
                    "https://example.com/site.git".to_string(),
                    "* * * * *",
                )
                .expect("schedule should parse"),
            ],
            start,
        )
        .expect("scheduler should build");
        let dispatcher = RecordingDispatcher::new();

        let outcomes = scheduler.dispatch_due(local_datetime(2026, 4, 10, 10, 3, 0), &dispatcher);

        assert_eq!(outcomes.len(), 1, "one late tick should dispatch once");
        assert_eq!(
            dispatcher.requests().len(),
            1,
            "missed runs should not backfill"
        );

        let second_tick =
            scheduler.dispatch_due(local_datetime(2026, 4, 10, 10, 3, 30), &dispatcher);
        assert!(
            second_tick.is_empty(),
            "the scheduler should advance straight to the next occurrence after now"
        );

        let third_tick = scheduler.dispatch_due(local_datetime(2026, 4, 10, 10, 4, 0), &dispatcher);
        assert_eq!(
            third_tick.len(),
            1,
            "the next scheduled minute should still fire"
        );
    }

    #[test]
    fn startup_on_a_schedule_boundary_dispatches_immediately_once() {
        let start = local_datetime(2026, 4, 10, 10, 15, 0);
        let mut scheduler = Scheduler::new(
            vec![
                ScheduledProfile::new(
                    "site-builder".to_string(),
                    "https://example.com/site.git".to_string(),
                    "*/15 * * * *",
                )
                .expect("schedule should parse"),
            ],
            start,
        )
        .expect("scheduler should build");
        let dispatcher = RecordingDispatcher::new();

        let first_tick = scheduler.dispatch_due(start, &dispatcher);
        assert_eq!(
            first_tick.len(),
            1,
            "startup on the exact boundary should fire immediately"
        );
        assert_eq!(
            dispatcher.requests(),
            vec![ScheduledRunRequest {
                profile: "site-builder".to_string(),
                repo_url: "https://example.com/site.git".to_string(),
            }]
        );
        assert_eq!(
            scheduler.next_wake_at(),
            Some(local_datetime(2026, 4, 10, 10, 30, 0)),
            "after firing at startup the next wake should advance past the current instant"
        );

        let second_tick = scheduler.dispatch_due(start, &dispatcher);
        assert!(
            second_tick.is_empty(),
            "the same boundary instant should not dispatch twice"
        );
    }

    #[test]
    fn next_wake_at_tracks_the_earliest_scheduled_profile() {
        let start = local_datetime(2026, 4, 10, 10, 0, 30);
        let scheduler = Scheduler::new(
            vec![
                ScheduledProfile::new(
                    "site-builder".to_string(),
                    "https://example.com/site.git".to_string(),
                    "*/20 * * * *",
                )
                .expect("schedule should parse"),
                ScheduledProfile::new(
                    "code-reviewer".to_string(),
                    "https://example.com/review.git".to_string(),
                    "*/5 * * * *",
                )
                .expect("schedule should parse"),
            ],
            start,
        )
        .expect("scheduler should build");

        assert_eq!(
            scheduler.next_wake_at(),
            Some(local_datetime(2026, 4, 10, 10, 5, 0))
        );
    }
}
