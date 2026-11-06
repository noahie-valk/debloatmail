//! Priority scheduler, job model, scope coalescing, interval clamping,
//! window-boundary preemption, and 4 Hz progress throttling (`SYNC.md` §3, §4, §12, §13).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use debloatmail_core::{AccountId, FeedId, FolderId, OpId, PublicId};
use debloatmail_engine::deterministic_account_jitter;

/// Default worker pool concurrency limit (`SYNC.md` §3.2).
pub const DEFAULT_WORKER_POOL_SIZE: usize = 4;

/// Maximum concurrent IMAP connections per active account (`SYNC.md` §3.2, §5.7: 1 IDLE + 1 work).
pub const MAX_IMAP_CONNECTIONS_PER_ACCOUNT: usize = 2;

/// Maximum concurrent HTTP feed fetches (`SYNC.md` §3.2).
pub const MAX_CONCURRENT_HTTP_FETCHES: usize = 4;

/// Maximum concurrent HTTP fetches per target host (`SYNC.md` §3.2, §12).
pub const MAX_CONCURRENT_FETCHES_PER_HOST: usize = 1;

/// Maximum total open sockets across the daemon (`SYNC.md` §3.2).
pub const MAX_TOTAL_OPEN_SOCKETS: usize = 16;

/// Coalescing window (in milliseconds) after a job finishes `Done` during which an identical
/// scope request returns the finished `job_id` instead of starting a new run (`SYNC.md` §4.3).
pub const COALESCE_RECENT_DONE_MS: u64 = 1_000;

/// Default feed poll interval in seconds (`SYNC.md` §3.4).
pub const DEFAULT_FEED_INTERVAL_SECS: u64 = 3_600;

/// Minimum allowed feed poll interval in seconds (`SYNC.md` §3.4).
pub const MIN_FEED_INTERVAL_SECS: u64 = 300;

/// Default mail poll interval in seconds (`SYNC.md` §3.4).
pub const DEFAULT_MAIL_INTERVAL_SECS: u64 = 900;

/// Minimum allowed mail poll interval in seconds (`SYNC.md` §3.4).
pub const MIN_MAIL_INTERVAL_SECS: u64 = 60;

/// Sentinel value for indeterminate progress percentage (`0xFFFFFFFF`, `SYNC.md` §13.1).
pub const INDETERMINATE_PERCENT: u32 = 0xFFFF_FFFF;

/// Minimum interval (in milliseconds) between intra-phase `Sync.Progress` signals (4 Hz, `SYNC.md` §13.3).
pub const PROGRESS_THROTTLE_MS: u64 = 250;

/// Coalescing window (in milliseconds) for `Items.Changed` signals (`SYNC.md` §13.2).
pub const ITEMS_CHANGED_COALESCE_MS: u64 = 100;

/// Priority classes for scheduler jobs (`SYNC.md` §3.1).
///
/// Ordered from highest priority (`Interactive`) to lowest (`Housekeeping`).
/// Note: Derive `Ord` orders variants top-to-bottom, so we implement comparison explicitly
/// such that `Interactive > Maintenance > Housekeeping`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PriorityClass {
    Interactive,
    Maintenance,
    Housekeeping,
}

impl PriorityClass {
    fn rank(self) -> u8 {
        match self {
            Self::Interactive => 3,
            Self::Maintenance => 2,
            Self::Housekeeping => 1,
        }
    }
}

impl PartialOrd for PriorityClass {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PriorityClass {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

/// Per-run identifier for an in-memory scheduler job (`SYNC.md` §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub u32);

/// Runtime state of an in-memory scheduler job (`SYNC.md` §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Cooperative cancellation token checked between protocol commands, never mid-command (`SYNC.md` §4.4).
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Scope of a scheduler job, used both for dispatch and for §4.3 idempotency coalescing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JobScope {
    AccountSync {
        account_id: Option<AccountId>,
        folder_id: Option<FolderId>,
        full: bool,
    },
    FeedRefresh {
        feed_id: Option<FeedId>,
    },
    BodyFetch {
        account_id: AccountId,
        item_id: PublicId,
    },
    AttachmentFetch {
        account_id: AccountId,
        item_id: PublicId,
        section_id: String,
    },
    QueueDrain {
        account_id: AccountId,
    },
    HousekeepingGc,
    ThreadRebuild {
        account_id: AccountId,
    },
    Reextract,
}

impl JobScope {
    /// Returns the owning [`AccountId`] if this job operates on a specific account.
    pub fn account_id(&self) -> Option<&AccountId> {
        match self {
            Self::AccountSync { account_id, .. } => account_id.as_ref(),
            Self::BodyFetch { account_id, .. }
            | Self::AttachmentFetch { account_id, .. }
            | Self::QueueDrain { account_id }
            | Self::ThreadRebuild { account_id } => Some(account_id),
            Self::FeedRefresh { .. } | Self::HousekeepingGc | Self::Reextract => None,
        }
    }

    /// Returns `true` if this is an interactive fetch (`BodyFetch` or `AttachmentFetch`)
    /// that preempts `AccountSync` at window boundaries (`SYNC.md` §3.1).
    pub fn is_interactive_fetch(&self) -> bool {
        matches!(self, Self::BodyFetch { .. } | Self::AttachmentFetch { .. })
    }
}

/// A tracked job in the [`Scheduler`].
#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: JobId,
    pub scope: JobScope,
    pub priority: PriorityClass,
    pub state: JobState,
    pub enqueued_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub cancel_token: CancelToken,
}

/// Priority job queue implementing §3.1 priority ordering, §3.1/§3.2 window-boundary preemption,
/// §4.3 scope coalescing (including the 1-second post-completion window), and §4.4 cancellation.
#[derive(Debug, Default)]
pub struct Scheduler {
    next_id: u32,
    jobs: HashMap<JobId, JobRecord>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            jobs: HashMap::new(),
        }
    }

    /// Enqueues a job for `scope` at `priority` or coalesces with an existing job (`SYNC.md` §4.3).
    ///
    /// Returns `(job_id, coalesced)`:
    /// - If an identical `scope` is currently `Queued` or `Running`, returns its `JobId` with `coalesced = true`
    ///   (and upgrades its priority if `priority` is higher).
    /// - If an identical `scope` finished `Done` within [`COALESCE_RECENT_DONE_MS`] (1,000 ms),
    ///   returns that finished job's `JobId` with `coalesced = true`.
    /// - Otherwise mints a new `JobId` in `JobState::Queued` and returns `(job_id, false)`.
    pub fn enqueue(
        &mut self,
        scope: JobScope,
        priority: PriorityClass,
        now_ms: u64,
    ) -> (JobId, bool) {
        // 1. Check active (`Queued` or `Running`) jobs with identical scope
        let active_match = self
            .jobs
            .values()
            .find(|j| j.scope == scope && matches!(j.state, JobState::Queued | JobState::Running))
            .map(|j| j.id);

        if let Some(existing_id) = active_match {
            if let Some(rec) = self.jobs.get_mut(&existing_id)
                && priority > rec.priority
            {
                rec.priority = priority;
            }
            return (existing_id, true);
        }

        // 2. Check recently completed (`Done` within 1,000 ms) jobs with identical scope
        let recent_done_match = self
            .jobs
            .values()
            .filter(|j| j.scope == scope && j.state == JobState::Done)
            .filter_map(|j| {
                let fin = j.finished_at_ms?;
                if now_ms.saturating_sub(fin) <= COALESCE_RECENT_DONE_MS {
                    Some((fin, j.id))
                } else {
                    None
                }
            })
            .max_by_key(|(fin, _)| *fin)
            .map(|(_, id)| id);

        if let Some(done_id) = recent_done_match {
            return (done_id, true);
        }

        let id = JobId(if self.next_id == 0 { 1 } else { self.next_id });
        self.next_id = id.0.wrapping_add(1).max(1);

        let record = JobRecord {
            id,
            scope,
            priority,
            state: JobState::Queued,
            enqueued_at_ms: now_ms,
            finished_at_ms: None,
            cancel_token: CancelToken::new(),
        };
        self.jobs.insert(id, record);
        (id, false)
    }

    /// Selects and transitions the highest-priority `Queued` job to `Running` (`SYNC.md` §3.1, §3.2).
    ///
    /// Enforces:
    /// - Priority order: `Interactive` > `Maintenance` > `Housekeeping` (FIFO tie-breaker by `enqueued_at_ms`, then `id`).
    /// - A `Housekeeping` job is never started while any network (`Interactive` or `Maintenance`) job is `Running` or `Queued` (`SYNC.md` §3.1).
    /// - Per-account serialization: at most one running job per `AccountId`.
    pub fn pop_next_runnable(&mut self) -> Option<JobRecord> {
        let has_active_network_job = self.jobs.values().any(|j| {
            matches!(j.state, JobState::Running | JobState::Queued)
                && j.priority != PriorityClass::Housekeeping
        });

        let running_accounts: Vec<AccountId> = self
            .jobs
            .values()
            .filter(|j| j.state == JobState::Running)
            .filter_map(|j| j.scope.account_id().cloned())
            .collect();

        let next_id = self
            .jobs
            .values()
            .filter(|j| j.state == JobState::Queued)
            .filter(|j| {
                if j.priority == PriorityClass::Housekeeping && has_active_network_job {
                    return false;
                }
                if let Some(acct) = j.scope.account_id()
                    && running_accounts.contains(acct)
                {
                    return false;
                }
                true
            })
            .max_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| b.enqueued_at_ms.cmp(&a.enqueued_at_ms))
                    .then_with(|| b.id.cmp(&a.id))
            })
            .map(|j| j.id)?;

        let rec = self.jobs.get_mut(&next_id)?;
        rec.state = JobState::Running;
        Some(rec.clone())
    }

    /// Returns `true` if an in-progress `AccountSync` on `account_id` should yield at the current
    /// committed 2,000-UID window boundary so an interactive `BodyFetch` or `AttachmentFetch` can run
    /// (`SYNC.md` §3.1 rules 1 & 2, §3.2).
    pub fn should_preempt_at_window_boundary(
        &self,
        account_id: &AccountId,
        sync_priority: PriorityClass,
    ) -> bool {
        self.jobs.values().any(|j| {
            if j.state != JobState::Queued || j.priority != PriorityClass::Interactive {
                return false;
            }
            if !j.scope.is_interactive_fetch() {
                return false;
            }
            // Rule 1: A scheduled (Maintenance) AccountSync of ANY account is preempted by an interactive fetch.
            if sync_priority == PriorityClass::Maintenance {
                return true;
            }
            // Rule 2: An in-progress AccountSync of the SAME account yields at a window boundary.
            j.scope.account_id() == Some(account_id)
        })
    }

    /// Marks a job `Done` and records `finished_at_ms`.
    pub fn mark_done(&mut self, job_id: JobId, now_ms: u64) -> bool {
        if let Some(rec) = self.jobs.get_mut(&job_id) {
            rec.state = JobState::Done;
            rec.finished_at_ms = Some(now_ms);
            true
        } else {
            false
        }
    }

    /// Marks a job `Failed` and records `finished_at_ms`.
    pub fn mark_failed(&mut self, job_id: JobId, now_ms: u64) -> bool {
        if let Some(rec) = self.jobs.get_mut(&job_id) {
            rec.state = JobState::Failed;
            rec.finished_at_ms = Some(now_ms);
            true
        } else {
            false
        }
    }

    /// Re-queues a running `AccountSync` job that yielded at a 2,000-UID window boundary
    /// so an interactive fetch can execute before `AccountSync` resumes (`SYNC.md` §3.1, §3.2).
    pub fn yield_running_job(&mut self, job_id: JobId) -> bool {
        if let Some(rec) = self.jobs.get_mut(&job_id)
            && rec.state == JobState::Running
        {
            rec.state = JobState::Queued;
            true
        } else {
            false
        }
    }

    /// Requests cooperative cancellation for `job_id` (`SYNC.md` §4.4).
    ///
    /// If the job is still `Queued`, transitions it immediately to `Cancelled`.
    /// If the job is `Running`, sets its `CancelToken` so it stops at the next command boundary.
    pub fn cancel(&mut self, job_id: JobId, now_ms: u64) -> bool {
        if let Some(rec) = self.jobs.get_mut(&job_id) {
            rec.cancel_token.cancel();
            if rec.state == JobState::Queued || rec.state == JobState::Running {
                rec.state = JobState::Cancelled;
                rec.finished_at_ms = Some(now_ms);
                return true;
            }
        }
        false
    }

    pub fn get(&self, job_id: JobId) -> Option<&JobRecord> {
        self.jobs.get(&job_id)
    }
}

/// Computes the effective feed poll interval (`SYNC.md` §3.4, `DATA-MODEL.md` §11.3):
/// `COALESCE(feeds.interval_secs, accounts.poll_interval_secs, config.default_feed_interval_secs)`.
///
/// - Treats `Some(v)` where `v <= 0` as `None`.
/// - Defaults to `3600` seconds.
/// - Clamps values `< 300` seconds to `300` seconds and returns `(interval_secs, was_clamped)`.
pub fn effective_feed_interval_secs(
    feed_interval: Option<i64>,
    account_interval: Option<i64>,
    config_default: Option<u64>,
) -> (u64, bool) {
    let raw = feed_interval
        .filter(|&v| v > 0)
        .map(|v| v as u64)
        .or_else(|| account_interval.filter(|&v| v > 0).map(|v| v as u64))
        .or_else(|| config_default.filter(|&v| v > 0))
        .unwrap_or(DEFAULT_FEED_INTERVAL_SECS);

    if raw < MIN_FEED_INTERVAL_SECS {
        (MIN_FEED_INTERVAL_SECS, true)
    } else {
        (raw, false)
    }
}

/// Computes the effective mail poll interval (`SYNC.md` §3.4):
/// `COALESCE(accounts.poll_interval_secs, config.default_mail_interval_secs)`.
///
/// - Treats `Some(v)` where `v <= 0` as `None`.
/// - Defaults to `900` seconds.
/// - Clamps values `< 60` seconds to `60` seconds and returns `(interval_secs, was_clamped)`.
pub fn effective_mail_interval_secs(
    account_interval: Option<i64>,
    config_default: Option<u64>,
) -> (u64, bool) {
    let raw = account_interval
        .filter(|&v| v > 0)
        .map(|v| v as u64)
        .or_else(|| config_default.filter(|&v| v > 0))
        .unwrap_or(DEFAULT_MAIL_INTERVAL_SECS);

    if raw < MIN_MAIL_INTERVAL_SECS {
        (MIN_MAIL_INTERVAL_SECS, true)
    } else {
        (raw, false)
    }
}

/// Computes the next scheduled sync timestamp for `account_id` with deterministic `±10%`
/// per-account jitter (`SYNC.md` §3.4).
pub fn schedule_next_account_sync(
    now_secs: i64,
    account_id: &AccountId,
    effective_interval_secs: u64,
) -> i64 {
    let capped = effective_interval_secs.min(u32::MAX as u64) as u32;
    let jitter = deterministic_account_jitter(account_id, capped);
    let delta = (effective_interval_secs as i64 + jitter).max(1);
    now_secs.saturating_add(delta)
}

/// Phases reported in `Sync.Progress` signals (`SYNC.md` §13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    Connecting,
    Listing,
    Fetching,
    Parsing,
    Storing,
    Done,
}

impl SyncPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Listing => "listing",
            Self::Fetching => "fetching",
            Self::Parsing => "parsing",
            Self::Storing => "storing",
            Self::Done => "done",
        }
    }
}

/// Canonical signal events emitted by the sync engine (`SYNC.md` §13.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEvent {
    ItemsChanged {
        revision: u64,
    },
    SyncProgress {
        job_id: JobId,
        phase: SyncPhase,
        percent: u32,
    },
    SyncFinished {
        job_id: JobId,
        success: bool,
        detail: Option<String>,
    },
    FeedsUpdated {
        feed_id: FeedId,
        inserted_count: usize,
    },
    AccountStateChanged {
        account_id: AccountId,
        state: String,
        detail: Option<String>,
    },
    QueueCompleted {
        op_id: OpId,
        success: bool,
        detail: Option<String>,
    },
}

/// Tracks and throttles `Sync.Progress` signals for a single [`JobId`] (`SYNC.md` §13.1–13.3):
/// - Emits immediately on phase transitions.
/// - Emits at most once every 250 ms (4 Hz) within the same phase.
/// - Always emits `percent == 100` so the final value in a phase is never lost.
#[derive(Debug, Clone)]
pub struct ProgressTracker {
    job_id: JobId,
    current_phase: Option<SyncPhase>,
    last_emit_ms: Option<u64>,
    last_percent: Option<u32>,
}

impl ProgressTracker {
    pub fn new(job_id: JobId) -> Self {
        Self {
            job_id,
            current_phase: None,
            last_emit_ms: None,
            last_percent: None,
        }
    }

    pub fn update(&mut self, phase: SyncPhase, percent: u32, now_ms: u64) -> Option<SyncEvent> {
        let phase_changed = self.current_phase != Some(phase);
        let is_completion = percent == 100 && self.last_percent != Some(100);
        let throttle_elapsed = self
            .last_emit_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= PROGRESS_THROTTLE_MS);

        if phase_changed || is_completion || throttle_elapsed {
            self.current_phase = Some(phase);
            self.last_emit_ms = Some(now_ms);
            self.last_percent = Some(percent);
            Some(SyncEvent::SyncProgress {
                job_id: self.job_id,
                phase,
                percent,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_ordering_and_housekeeping_deferral() {
        assert!(PriorityClass::Interactive > PriorityClass::Maintenance);
        assert!(PriorityClass::Maintenance > PriorityClass::Housekeeping);

        let mut sched = Scheduler::new();
        let acct = AccountId::new("018f6e80-7c00-7218-971e-7f62b2512f71").unwrap();
        let item = PublicId::new("018f6e80-7c00-7218-971e-7f62b2512001").unwrap();

        let (gc_id, _) = sched.enqueue(JobScope::HousekeepingGc, PriorityClass::Housekeeping, 100);
        let (sync_id, _) = sched.enqueue(
            JobScope::AccountSync {
                account_id: Some(acct.clone()),
                folder_id: None,
                full: false,
            },
            PriorityClass::Maintenance,
            101,
        );
        let (body_id, _) = sched.enqueue(
            JobScope::BodyFetch {
                account_id: acct.clone(),
                item_id: item,
            },
            PriorityClass::Interactive,
            102,
        );

        // Interactive BodyFetch must pop first!
        let first = sched.pop_next_runnable().unwrap();
        assert_eq!(first.id, body_id);

        // While BodyFetch is running on `acct` and AccountSync is queued, HousekeepingGc cannot run!
        assert!(sched.pop_next_runnable().is_none());

        // Complete BodyFetch -> now AccountSync on `acct` can run
        sched.mark_done(body_id, 200);
        let second = sched.pop_next_runnable().unwrap();
        assert_eq!(second.id, sync_id);

        // Housekeeping still waits while AccountSync is running
        assert!(sched.pop_next_runnable().is_none());

        sched.mark_done(sync_id, 300);
        let third = sched.pop_next_runnable().unwrap();
        assert_eq!(third.id, gc_id);
    }

    #[test]
    fn test_scope_coalescing_and_one_second_post_done_window() {
        let mut sched = Scheduler::new();
        let scope = JobScope::AccountSync {
            account_id: None,
            folder_id: None,
            full: false,
        };

        let (id1, coalesced1) = sched.enqueue(scope.clone(), PriorityClass::Maintenance, 1_000);
        assert!(!coalesced1);

        // Duplicate while Queued -> coalesces to id1 and upgrades priority to Interactive
        let (id2, coalesced2) = sched.enqueue(scope.clone(), PriorityClass::Interactive, 1_100);
        assert!(coalesced2);
        assert_eq!(id1, id2);
        assert_eq!(sched.get(id1).unwrap().priority, PriorityClass::Interactive);

        let running = sched.pop_next_runnable().unwrap();
        assert_eq!(running.id, id1);
        sched.mark_done(id1, 2_000);

        // Request within 1,000 ms (at 2,500 ms) -> coalesces to finished id1
        let (id3, coalesced3) = sched.enqueue(scope.clone(), PriorityClass::Interactive, 2_500);
        assert!(coalesced3);
        assert_eq!(id1, id3);

        // Request after > 1,000 ms (at 3,001 ms) -> starts a new job
        let (id4, coalesced4) = sched.enqueue(scope, PriorityClass::Interactive, 3_001);
        assert!(!coalesced4);
        assert_ne!(id1, id4);
    }

    #[test]
    fn test_interval_clamping_and_zero_as_null() {
        // 0 is treated as NULL -> falls back to default 3600
        assert_eq!(
            effective_feed_interval_secs(Some(0), Some(0), None),
            (3_600, false)
        );
        // 5 seconds is clamped to 300 seconds with warning flag = true
        assert_eq!(
            effective_feed_interval_secs(Some(5), None, None),
            (300, true)
        );
        // Mail interval < 60 clamped to 60
        assert_eq!(effective_mail_interval_secs(Some(15), None), (60, true));
        assert_eq!(effective_mail_interval_secs(Some(0), None), (900, false));
    }

    #[test]
    fn test_progress_throttling_and_guaranteed_100_percent() {
        let mut tracker = ProgressTracker::new(JobId(7));

        // Initial phase transition always emits
        assert!(tracker.update(SyncPhase::Fetching, 10, 1_000).is_some());
        // 50 ms later in same phase (< 250 ms) -> throttled
        assert!(tracker.update(SyncPhase::Fetching, 20, 1_050).is_none());
        // 100% completion within the throttle window is NEVER dropped
        assert!(tracker.update(SyncPhase::Fetching, 100, 1_100).is_some());
        // Phase transition to Storing immediately emits even at 1_110 ms
        assert!(tracker.update(SyncPhase::Storing, 0, 1_110).is_some());
    }
}
