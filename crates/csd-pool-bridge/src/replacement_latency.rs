use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::mpsc;
use tracing::info;
use uuid::Uuid;

pub(super) const MAX_IN_FLIGHT_EVENTS: usize = 16;
pub(super) const MAX_TRACKED_SESSIONS: usize = 1_024;
pub(super) const MAX_COMPLETED_SUMMARIES: usize = 16;
pub(super) const MAX_LOGS_PER_MINUTE: u32 = 120;
pub(super) const MAX_LOG_QUEUE: usize = 16;
pub(super) const MAX_CORRELATION_KEY_BYTES: usize = 128;
pub(super) const EVENT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UpstreamClass {
    PrimaryAlreadyReady,
    IndependentSentinelAhead,
}

impl UpstreamClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryAlreadyReady => "primary_already_ready",
            Self::IndependentSentinelAhead => "independent_sentinel_ahead",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FanoutOutcome {
    Success,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LatencySummary {
    pub correlation_id: String,
    pub source: &'static str,
    pub status: &'static str,
    pub reason: &'static str,
    pub observed_to_a_ready_ms: Option<u64>,
    pub a_ready_to_published_ms: Option<u64>,
    pub published_to_fanout_ms: Option<u64>,
    pub total_ms: Option<u64>,
    pub target_sessions: usize,
    pub notify_success: usize,
    pub notify_failed: usize,
    pub notify_skipped: usize,
    pub tracking_gap_sessions: usize,
    pub stale_roster_sessions: usize,
    pub dropped_events_total: u64,
    pub dropped_sessions_total: u64,
    pub untracked_sessions_total: u64,
    pub untracked_active_sessions: u64,
    pub roster_cleanup_failures_total: u64,
    pub stale_tracked_sessions: u64,
    pub dropped_logs_total: u64,
    pub lock_failures_total: u64,
    pub lock_contention_total: u64,
    pub lock_poisoned_total: u64,
    pub out_of_order_total: u64,
}

impl LatencySummary {
    fn safe_json(&self) -> serde_json::Value {
        json!({
            "schema": "fast_tip_replacement_latency/v1u2",
            "correlation_id": self.correlation_id,
            "source": self.source,
            "status": self.status,
            "reason": self.reason,
            "observed_to_a_ready_ms": self.observed_to_a_ready_ms,
            "a_ready_to_published_ms": self.a_ready_to_published_ms,
            "published_to_fanout_ms": self.published_to_fanout_ms,
            "total_ms": self.total_ms,
            "target_sessions": self.target_sessions,
            "notify_success": self.notify_success,
            "notify_failed": self.notify_failed,
            "notify_skipped": self.notify_skipped,
            "tracking_gap_sessions": self.tracking_gap_sessions,
            "stale_roster_sessions": self.stale_roster_sessions,
            "dropped_events_total": self.dropped_events_total,
            "dropped_sessions_total": self.dropped_sessions_total,
            "untracked_sessions_total": self.untracked_sessions_total,
            "untracked_active_sessions": self.untracked_active_sessions,
            "roster_cleanup_failures_total": self.roster_cleanup_failures_total,
            "stale_tracked_sessions": self.stale_tracked_sessions,
            "dropped_logs_total": self.dropped_logs_total,
            "lock_failures_total": self.lock_failures_total,
            "lock_contention_total": self.lock_contention_total,
            "lock_poisoned_total": self.lock_poisoned_total,
            "out_of_order_total": self.out_of_order_total,
            "chain_values_recorded": false,
            "session_identities_recorded": false,
            "addresses_recorded": false,
            "credentials_recorded": false
        })
    }
}

#[derive(Debug)]
struct LatencyEvent {
    id: u64,
    source: UpstreamClass,
    dedupe_key: String,
    replacement_key: Option<String>,
    observed_at: Instant,
    a_ready_at: Option<Instant>,
    published_at: Option<Instant>,
    target_sessions: usize,
    pending_sessions: HashSet<u64>,
    notify_success: usize,
    notify_failed: usize,
    notify_skipped: usize,
    tracking_gap_sessions: usize,
    stale_roster_sessions: usize,
}

#[derive(Debug)]
struct LatencyState {
    active_sessions: HashSet<u64>,
    events: HashMap<u64, LatencyEvent>,
    dedupe: HashMap<String, u64>,
    published_jobs: HashMap<String, u64>,
    completed: VecDeque<LatencySummary>,
    log_window_started: Instant,
    logs_in_window: u32,
}

pub(super) struct ReplacementLatencyTelemetry {
    enabled: bool,
    run_id: u32,
    next_id: AtomicU64,
    dropped_events: AtomicU64,
    dropped_sessions: AtomicU64,
    untracked_sessions: AtomicU64,
    untracked_active_sessions: AtomicU64,
    roster_cleanup_failures: AtomicU64,
    stale_tracked_sessions: AtomicU64,
    dropped_logs: AtomicU64,
    active_session_total: AtomicU64,
    lock_contention: AtomicU64,
    lock_poisoned: AtomicU64,
    out_of_order: AtomicU64,
    log_sender: Option<mpsc::Sender<LatencySummary>>,
    state: Mutex<LatencyState>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct LatencyStats {
    pub in_flight_events: usize,
    pub active_session_total: usize,
    pub tracked_sessions: usize,
    pub dropped_events_total: u64,
    pub dropped_sessions_total: u64,
    pub untracked_sessions_total: u64,
    pub untracked_active_sessions: u64,
    pub roster_cleanup_failures_total: u64,
    pub stale_tracked_sessions: u64,
    pub dropped_logs_total: u64,
    pub lock_failures_total: u64,
    pub lock_contention_total: u64,
    pub lock_poisoned_total: u64,
    pub out_of_order_total: u64,
}

impl ReplacementLatencyTelemetry {
    pub(super) fn from_env() -> Arc<Self> {
        let enabled = csd_pool_config::env_flag_enabled(
            std::env::var("CSD_POOL_FAST_TIP_REPLACEMENT_LATENCY_TELEMETRY")
                .ok()
                .as_deref(),
        );
        let mut telemetry = Self::new(enabled);
        if enabled {
            let (sender, mut receiver) = mpsc::channel::<LatencySummary>(MAX_LOG_QUEUE);
            telemetry.log_sender = Some(sender);
            tokio::spawn(async move {
                while let Some(summary) = receiver.recv().await {
                    info!(
                        target: "csd_pool::fast_tip_replacement_latency",
                        telemetry = %summary.safe_json(),
                        "fast tip replacement latency"
                    );
                }
            });
        }
        Arc::new(telemetry)
    }

    fn new(enabled: bool) -> Self {
        let bytes = *Uuid::new_v4().as_bytes();
        Self::with_run_id(
            enabled,
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        )
    }

    fn with_run_id(enabled: bool, run_id: u32) -> Self {
        Self {
            enabled,
            run_id,
            next_id: AtomicU64::new(1),
            dropped_events: AtomicU64::new(0),
            dropped_sessions: AtomicU64::new(0),
            untracked_sessions: AtomicU64::new(0),
            untracked_active_sessions: AtomicU64::new(0),
            roster_cleanup_failures: AtomicU64::new(0),
            stale_tracked_sessions: AtomicU64::new(0),
            dropped_logs: AtomicU64::new(0),
            active_session_total: AtomicU64::new(0),
            lock_contention: AtomicU64::new(0),
            lock_poisoned: AtomicU64::new(0),
            out_of_order: AtomicU64::new(0),
            log_sender: None,
            state: Mutex::new(LatencyState {
                active_sessions: HashSet::new(),
                events: HashMap::new(),
                dedupe: HashMap::new(),
                published_jobs: HashMap::new(),
                completed: VecDeque::new(),
                log_window_started: Instant::now(),
                logs_in_window: 0,
            }),
        }
    }

    pub(super) fn disabled() -> Arc<Self> {
        Arc::new(Self::new(false))
    }

    #[cfg(test)]
    pub(super) fn enabled_for_test(run_id: u32) -> Arc<Self> {
        Arc::new(Self::with_run_id(true, run_id))
    }

    pub(super) fn session(self: &Arc<Self>, session_id: u64) -> TelemetrySession {
        TelemetrySession {
            telemetry: self.clone(),
            session_id,
            authorized: false,
            counted: false,
            tracked: false,
        }
    }

    pub(super) fn observe_upstream(&self, dedupe_key: &str, source: UpstreamClass) -> Option<u64> {
        self.observe_upstream_at(dedupe_key, source, Instant::now())
    }

    fn observe_upstream_at(
        &self,
        dedupe_key: &str,
        source: UpstreamClass,
        now: Instant,
    ) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        if dedupe_key.len() > MAX_CORRELATION_KEY_BYTES {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let mut state = self.try_state()?;
        self.expire_locked(&mut state, now);
        if let Some(id) = state.dedupe.get(dedupe_key).copied() {
            if state.events.contains_key(&id) {
                return Some(id);
            }
            state.dedupe.remove(dedupe_key);
        }
        if state.events.len() >= MAX_IN_FLIGHT_EVENTS {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        state.dedupe.insert(dedupe_key.to_owned(), id);
        state.events.insert(
            id,
            LatencyEvent {
                id,
                source,
                dedupe_key: dedupe_key.to_owned(),
                replacement_key: None,
                observed_at: now,
                a_ready_at: None,
                published_at: None,
                target_sessions: 0,
                pending_sessions: HashSet::new(),
                notify_success: 0,
                notify_failed: 0,
                notify_skipped: 0,
                tracking_gap_sessions: 0,
                stale_roster_sessions: 0,
            },
        );
        Some(id)
    }

    pub(super) fn mark_a_ready(&self, event_id: u64) {
        self.mark_a_ready_at(event_id, Instant::now());
    }

    fn mark_a_ready_at(&self, event_id: u64, now: Instant) {
        if !self.enabled {
            return;
        }
        let Some(mut state) = self.try_state() else {
            return;
        };
        self.expire_locked(&mut state, now);
        let invalid_order = state.events.get(&event_id).is_some_and(|event| {
            event.published_at.is_some() || now.checked_duration_since(event.observed_at).is_none()
        });
        if invalid_order {
            self.out_of_order.fetch_add(1, Ordering::Relaxed);
            self.finish_locked(&mut state, event_id, "failed", "out_of_order", now);
            return;
        }
        if let Some(event) = state.events.get_mut(&event_id) {
            if event.a_ready_at.is_none() {
                event.a_ready_at = Some(now);
            }
        }
    }

    pub(super) fn mark_published(&self, event_id: u64, replacement_key: &str) {
        self.mark_published_at(event_id, replacement_key, Instant::now());
    }

    fn mark_published_at(&self, event_id: u64, replacement_key: &str, now: Instant) {
        if !self.enabled {
            return;
        }
        let Some(mut state) = self.try_state() else {
            return;
        };
        self.expire_locked(&mut state, now);
        if replacement_key.len() > MAX_CORRELATION_KEY_BYTES {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            self.finish_locked(
                &mut state,
                event_id,
                "failed",
                "replacement_key_oversize",
                now,
            );
            return;
        }
        let invalid_order = state.events.get(&event_id).is_some_and(|event| {
            event.a_ready_at.is_none()
                || event.published_at.is_some()
                || event
                    .a_ready_at
                    .and_then(|ready| now.checked_duration_since(ready))
                    .is_none()
        });
        if invalid_order {
            self.out_of_order.fetch_add(1, Ordering::Relaxed);
            self.finish_locked(&mut state, event_id, "failed", "out_of_order", now);
            return;
        }
        let active_total = self.active_session_total.load(Ordering::Relaxed) as usize;
        let tracked_total = state.active_sessions.len();
        let untracked_active = self.untracked_active_sessions.load(Ordering::Relaxed) as usize;
        let stale_tracked = self.stale_tracked_sessions.load(Ordering::Relaxed) as usize;
        let expected_tracked = active_total.saturating_sub(untracked_active);
        if stale_tracked > 0 || tracked_total != expected_tracked {
            if let Some(event) = state.events.get_mut(&event_id) {
                event.published_at = Some(now);
                event.replacement_key = Some(replacement_key.to_owned());
                event.target_sessions = active_total;
                event.notify_skipped = active_total;
                event.tracking_gap_sessions = active_total;
                event.stale_roster_sessions =
                    stale_tracked.max(tracked_total.saturating_sub(active_total));
            } else {
                return;
            }
            let reason = if stale_tracked > 0 || tracked_total > active_total {
                "stale_session_roster"
            } else {
                "session_roster_inconsistent"
            };
            self.finish_locked(&mut state, event_id, "tracking_gap", reason, now);
            return;
        }
        let active_sessions = state.active_sessions.iter().copied().collect();
        let tracking_gap_sessions = untracked_active;
        if let Some(event) = state.events.get_mut(&event_id) {
            event.published_at = Some(now);
            event.replacement_key = Some(replacement_key.to_owned());
            event.target_sessions = active_total;
            event.pending_sessions = active_sessions;
            event.notify_skipped = tracking_gap_sessions;
            event.tracking_gap_sessions = tracking_gap_sessions;
        } else {
            return;
        }
        state
            .published_jobs
            .insert(replacement_key.to_owned(), event_id);
        let complete = state
            .events
            .get(&event_id)
            .is_some_and(|event| event.pending_sessions.is_empty());
        if complete {
            let (status, reason) = if active_total == 0 {
                ("complete", "zero_sessions")
            } else {
                ("tracking_gap", "untracked_sessions")
            };
            self.finish_locked(&mut state, event_id, status, reason, now);
        }
    }

    pub(super) fn fail_event(&self, event_id: u64, reason: &'static str) {
        if !self.enabled {
            return;
        }
        let Some(mut state) = self.try_state() else {
            return;
        };
        self.finish_locked(&mut state, event_id, "failed", reason, Instant::now());
    }

    pub(super) fn expire(&self) {
        if !self.enabled {
            return;
        }
        let Some(mut state) = self.try_state() else {
            return;
        };
        self.expire_locked(&mut state, Instant::now());
    }

    fn activate_session(&self, session_id: u64) -> (bool, bool) {
        if !self.enabled {
            return (false, false);
        }
        self.active_session_total.fetch_add(1, Ordering::Relaxed);
        let Some(mut state) = self.try_state() else {
            self.count_untracked_session();
            return (true, false);
        };
        if state.active_sessions.len() >= MAX_TRACKED_SESSIONS {
            self.count_untracked_session();
            (true, false)
        } else {
            let tracked = state.active_sessions.insert(session_id);
            if !tracked {
                self.count_untracked_session();
            }
            (true, tracked)
        }
    }

    fn count_untracked_session(&self) {
        self.dropped_sessions.fetch_add(1, Ordering::Relaxed);
        self.untracked_sessions.fetch_add(1, Ordering::Relaxed);
        self.untracked_active_sessions
            .fetch_add(1, Ordering::Relaxed);
    }

    fn deactivate_session(&self, session_id: u64, counted: bool, tracked: bool) {
        if !self.enabled || !counted {
            return;
        }
        let _ =
            self.active_session_total
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    Some(value.saturating_sub(1))
                });
        if !tracked {
            let _ = self.untracked_active_sessions.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |value| Some(value.saturating_sub(1)),
            );
            return;
        }
        let Some(mut state) = self.try_state() else {
            self.roster_cleanup_failures.fetch_add(1, Ordering::Relaxed);
            self.stale_tracked_sessions.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if !state.active_sessions.remove(&session_id) {
            self.roster_cleanup_failures.fetch_add(1, Ordering::Relaxed);
        }
        let now = Instant::now();
        let ids: Vec<u64> = state
            .events
            .iter_mut()
            .filter_map(|(id, event)| {
                let removed = event.pending_sessions.remove(&session_id);
                if removed {
                    event.notify_skipped = event.notify_skipped.saturating_add(1);
                }
                (removed && event.published_at.is_some() && event.pending_sessions.is_empty())
                    .then_some(*id)
            })
            .collect();
        for id in ids {
            self.finish_fanout_locked(&mut state, id, "session_disconnected", now);
        }
    }

    fn record_fanout(&self, session_id: u64, replacement_key: &str, outcome: FanoutOutcome) {
        self.record_fanout_at(session_id, replacement_key, outcome, Instant::now());
    }

    fn record_fanout_at(
        &self,
        session_id: u64,
        replacement_key: &str,
        outcome: FanoutOutcome,
        now: Instant,
    ) {
        if !self.enabled {
            return;
        }
        let Some(mut state) = self.try_state() else {
            return;
        };
        self.expire_locked(&mut state, now);
        let Some(event_id) = state.published_jobs.get(replacement_key).copied() else {
            return;
        };
        let superseded: Vec<u64> = state
            .events
            .iter_mut()
            .filter_map(|(id, event)| {
                let removed = *id != event_id && event.pending_sessions.remove(&session_id);
                if removed {
                    event.notify_skipped = event.notify_skipped.saturating_add(1);
                }
                (removed && event.published_at.is_some() && event.pending_sessions.is_empty())
                    .then_some(*id)
            })
            .collect();
        for id in superseded {
            self.finish_fanout_locked(&mut state, id, "superseded", now);
        }
        let Some(event) = state.events.get_mut(&event_id) else {
            return;
        };
        if !event.pending_sessions.remove(&session_id) {
            return;
        }
        match outcome {
            FanoutOutcome::Success => event.notify_success = event.notify_success.saturating_add(1),
            FanoutOutcome::Failed => event.notify_failed = event.notify_failed.saturating_add(1),
            FanoutOutcome::Skipped => event.notify_skipped = event.notify_skipped.saturating_add(1),
        }
        if event.pending_sessions.is_empty() {
            let reason = if event.notify_failed > 0 {
                "partial_notify_failure"
            } else if event.notify_skipped > 0 {
                "partial_notify_skip"
            } else {
                "all_notified"
            };
            self.finish_fanout_locked(&mut state, event_id, reason, now);
        }
    }

    fn expire_locked(&self, state: &mut LatencyState, now: Instant) {
        let expired: Vec<u64> = state
            .events
            .iter()
            .filter_map(|(id, event)| {
                now.checked_duration_since(event.observed_at)
                    .is_some_and(|elapsed| elapsed >= EVENT_TIMEOUT)
                    .then_some(*id)
            })
            .collect();
        for id in expired {
            self.finish_locked(state, id, "timeout", "stage_timeout", now);
        }
    }

    fn finish_locked(
        &self,
        state: &mut LatencyState,
        event_id: u64,
        status: &'static str,
        reason: &'static str,
        finished_at: Instant,
    ) {
        let Some(event) = state.events.remove(&event_id) else {
            return;
        };
        state.dedupe.remove(&event.dedupe_key);
        if let Some(replacement_key) = event.replacement_key.as_ref() {
            state.published_jobs.remove(replacement_key);
        }
        let fanout_unobservable = status == "tracking_gap";
        let summary = LatencySummary {
            correlation_id: self.correlation_id(event.id),
            source: event.source.as_str(),
            status,
            reason,
            observed_to_a_ready_ms: elapsed_ms(event.observed_at, event.a_ready_at),
            a_ready_to_published_ms: elapsed_ms_pair(event.a_ready_at, event.published_at),
            published_to_fanout_ms: (!fanout_unobservable)
                .then(|| elapsed_ms_pair(event.published_at, Some(finished_at)))
                .flatten(),
            total_ms: (!fanout_unobservable)
                .then(|| elapsed_ms(event.observed_at, Some(finished_at)))
                .flatten(),
            target_sessions: event.target_sessions,
            notify_success: event.notify_success,
            notify_failed: event.notify_failed,
            notify_skipped: event
                .notify_skipped
                .saturating_add(event.pending_sessions.len()),
            tracking_gap_sessions: event.tracking_gap_sessions,
            stale_roster_sessions: event.stale_roster_sessions,
            dropped_events_total: self.dropped_events.load(Ordering::Relaxed),
            dropped_sessions_total: self.dropped_sessions.load(Ordering::Relaxed),
            untracked_sessions_total: self.untracked_sessions.load(Ordering::Relaxed),
            untracked_active_sessions: self.untracked_active_sessions.load(Ordering::Relaxed),
            roster_cleanup_failures_total: self.roster_cleanup_failures.load(Ordering::Relaxed),
            stale_tracked_sessions: self.stale_tracked_sessions.load(Ordering::Relaxed),
            dropped_logs_total: self.dropped_logs.load(Ordering::Relaxed),
            lock_failures_total: self.lock_failures_total(),
            lock_contention_total: self.lock_contention.load(Ordering::Relaxed),
            lock_poisoned_total: self.lock_poisoned.load(Ordering::Relaxed),
            out_of_order_total: self.out_of_order.load(Ordering::Relaxed),
        };
        if state.completed.len() >= MAX_COMPLETED_SUMMARIES {
            state.completed.pop_front();
        }
        state.completed.push_back(summary.clone());
        if log_allowed(state, finished_at) {
            if self
                .log_sender
                .as_ref()
                .is_some_and(|sender| sender.try_send(summary).is_err())
            {
                self.dropped_logs.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            self.dropped_logs.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn finish_fanout_locked(
        &self,
        state: &mut LatencyState,
        event_id: u64,
        complete_reason: &'static str,
        finished_at: Instant,
    ) {
        let tracking_gap = state
            .events
            .get(&event_id)
            .is_some_and(|event| event.tracking_gap_sessions > 0);
        if tracking_gap {
            self.finish_locked(
                state,
                event_id,
                "tracking_gap",
                "untracked_sessions",
                finished_at,
            );
        } else {
            self.finish_locked(state, event_id, "complete", complete_reason, finished_at);
        }
    }

    fn correlation_id(&self, event_id: u64) -> String {
        format!("{:08x}-{:08x}", self.run_id, event_id)
    }

    fn try_state(&self) -> Option<std::sync::MutexGuard<'_, LatencyState>> {
        match self.state.try_lock() {
            Ok(state) => Some(state),
            Err(TryLockError::WouldBlock) => {
                self.lock_contention.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(TryLockError::Poisoned(_)) => {
                self.lock_poisoned.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    fn lock_failures_total(&self) -> u64 {
        self.lock_contention
            .load(Ordering::Relaxed)
            .saturating_add(self.lock_poisoned.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
impl ReplacementLatencyTelemetry {
    pub(super) fn stats(&self) -> LatencyStats {
        let mut stats = LatencyStats {
            dropped_events_total: self.dropped_events.load(Ordering::Relaxed),
            dropped_sessions_total: self.dropped_sessions.load(Ordering::Relaxed),
            untracked_sessions_total: self.untracked_sessions.load(Ordering::Relaxed),
            untracked_active_sessions: self.untracked_active_sessions.load(Ordering::Relaxed),
            roster_cleanup_failures_total: self.roster_cleanup_failures.load(Ordering::Relaxed),
            stale_tracked_sessions: self.stale_tracked_sessions.load(Ordering::Relaxed),
            dropped_logs_total: self.dropped_logs.load(Ordering::Relaxed),
            lock_failures_total: self.lock_failures_total(),
            lock_contention_total: self.lock_contention.load(Ordering::Relaxed),
            lock_poisoned_total: self.lock_poisoned.load(Ordering::Relaxed),
            out_of_order_total: self.out_of_order.load(Ordering::Relaxed),
            ..LatencyStats::default()
        };
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        stats.in_flight_events = state.events.len();
        stats.active_session_total = self.active_session_total.load(Ordering::Relaxed) as usize;
        stats.tracked_sessions = state.active_sessions.len();
        stats
    }

    pub(super) fn take_completed(&self) -> Vec<LatencySummary> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.completed.drain(..).collect()
    }

    pub(super) fn hold_state_until_released_for_test(
        &self,
        ready: std::sync::mpsc::SyncSender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        let _state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ready.send(()).unwrap();
        release.recv().unwrap();
    }
}

pub(super) struct TelemetrySession {
    telemetry: Arc<ReplacementLatencyTelemetry>,
    session_id: u64,
    authorized: bool,
    counted: bool,
    tracked: bool,
}

impl TelemetrySession {
    pub(super) fn authorize(&mut self) {
        if self.authorized {
            return;
        }
        self.authorized = true;
        (self.counted, self.tracked) = self.telemetry.activate_session(self.session_id);
    }

    pub(super) fn record_notify(&self, replacement_key: &str, outcome: FanoutOutcome) {
        if self.authorized {
            self.telemetry
                .record_fanout(self.session_id, replacement_key, outcome);
        }
    }
}

impl Drop for TelemetrySession {
    fn drop(&mut self) {
        if self.authorized {
            self.telemetry
                .deactivate_session(self.session_id, self.counted, self.tracked);
        }
    }
}

fn elapsed_ms(start: Instant, end: Option<Instant>) -> Option<u64> {
    elapsed_ms_pair(Some(start), end)
}

fn elapsed_ms_pair(start: Option<Instant>, end: Option<Instant>) -> Option<u64> {
    let elapsed = end?.checked_duration_since(start?)?;
    Some(elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
}

fn log_allowed(state: &mut LatencyState, now: Instant) -> bool {
    if now
        .checked_duration_since(state.log_window_started)
        .is_some_and(|elapsed| elapsed >= Duration::from_secs(60))
    {
        state.log_window_started = now;
        state.logs_in_window = 0;
    }
    if state.logs_in_window >= MAX_LOGS_PER_MINUTE {
        false
    } else {
        state.logs_in_window += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_event(
        telemetry: &Arc<ReplacementLatencyTelemetry>,
        source: UpstreamClass,
        targets: usize,
    ) -> LatencySummary {
        let mut sessions = Vec::new();
        for id in 1..=targets as u64 {
            let mut session = telemetry.session(id);
            session.authorize();
            sessions.push(session);
        }
        let start = Instant::now();
        let event_id = telemetry
            .observe_upstream_at("old-job", source, start)
            .expect("event");
        telemetry.mark_a_ready_at(event_id, start + Duration::from_millis(4));
        telemetry.mark_published_at(
            event_id,
            "replacement-job",
            start + Duration::from_millis(7),
        );
        for session in &sessions {
            telemetry.record_fanout_at(
                session.session_id,
                "replacement-job",
                FanoutOutcome::Success,
                start + Duration::from_millis(9),
            );
        }
        telemetry.take_completed().pop().expect("summary")
    }

    #[test]
    fn normal_four_stage_event_records_all_segments() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(0x0102_0304);
        let summary = complete_event(&telemetry, UpstreamClass::IndependentSentinelAhead, 2);
        assert_eq!(summary.source, "independent_sentinel_ahead");
        assert_eq!(summary.status, "complete");
        assert_eq!(summary.reason, "all_notified");
        assert_eq!(summary.observed_to_a_ready_ms, Some(4));
        assert_eq!(summary.a_ready_to_published_ms, Some(3));
        assert!(summary.published_to_fanout_ms.is_some());
        assert_eq!(summary.target_sessions, 2);
        assert_eq!(summary.notify_success, 2);
        assert_eq!(summary.notify_failed, 0);
        assert_eq!(summary.notify_skipped, 0);
    }

    #[test]
    fn primary_already_ready_is_a_distinct_safe_source() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(7);
        let summary = complete_event(&telemetry, UpstreamClass::PrimaryAlreadyReady, 0);
        assert_eq!(summary.source, "primary_already_ready");
        assert_eq!(summary.reason, "zero_sessions");
        assert_eq!(summary.target_sessions, 0);
    }

    #[test]
    fn duplicate_upstream_tip_reuses_one_inflight_event() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(8);
        let first = telemetry
            .observe_upstream("same-old-job", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        let second = telemetry
            .observe_upstream("same-old-job", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(telemetry.stats().in_flight_events, 1);
    }

    #[test]
    fn out_of_order_publish_is_explicit_and_has_missing_segments() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(9);
        let event = telemetry
            .observe_upstream("old", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_published(event, "new");
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.status, "failed");
        assert_eq!(summary.reason, "out_of_order");
        assert_eq!(summary.observed_to_a_ready_ms, None);
        assert_eq!(summary.a_ready_to_published_ms, None);
        assert_eq!(telemetry.stats().out_of_order_total, 1);
    }

    #[test]
    fn replacement_failure_does_not_leave_an_inflight_event() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(10);
        let event = telemetry
            .observe_upstream("old", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.fail_event(event, "replacement_persist_failed");
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.status, "failed");
        assert_eq!(summary.reason, "replacement_persist_failed");
        assert_eq!(telemetry.stats().in_flight_events, 0);
    }

    #[test]
    fn partial_session_failure_and_disconnect_are_counted() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(11);
        let mut first = telemetry.session(1);
        let mut second = telemetry.session(2);
        let mut third = telemetry.session(3);
        first.authorize();
        second.authorize();
        third.authorize();
        let event = telemetry
            .observe_upstream("old", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_a_ready(event);
        telemetry.mark_published(event, "new");
        first.record_notify("new", FanoutOutcome::Success);
        second.record_notify("new", FanoutOutcome::Failed);
        drop(third);
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.target_sessions, 3);
        assert_eq!(summary.notify_success, 1);
        assert_eq!(summary.notify_failed, 1);
        assert_eq!(summary.notify_skipped, 1);
    }

    #[test]
    fn skipped_fanout_and_zero_session_are_explicit() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(12);
        let mut session = telemetry.session(1);
        session.authorize();
        let event = telemetry
            .observe_upstream("old", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_a_ready(event);
        telemetry.mark_published(event, "new");
        session.record_notify("new", FanoutOutcome::Skipped);
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.reason, "partial_notify_skip");
        assert_eq!(summary.notify_skipped, 1);
    }

    #[test]
    fn event_queue_and_session_tracking_are_bounded() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(13);
        for id in 0..MAX_IN_FLIGHT_EVENTS {
            assert!(
                telemetry
                    .observe_upstream(
                        &format!("old-{id}"),
                        UpstreamClass::IndependentSentinelAhead,
                    )
                    .is_some()
            );
        }
        assert!(
            telemetry
                .observe_upstream("overflow", UpstreamClass::IndependentSentinelAhead)
                .is_none()
        );
        assert_eq!(telemetry.stats().dropped_events_total, 1);

        let sessions: Vec<_> = (0..=MAX_TRACKED_SESSIONS)
            .map(|id| {
                let mut session = telemetry.session(id as u64 + 1);
                session.authorize();
                session
            })
            .collect();
        assert_eq!(telemetry.stats().tracked_sessions, MAX_TRACKED_SESSIONS);
        assert_eq!(telemetry.stats().dropped_sessions_total, 1);
        assert_eq!(telemetry.stats().untracked_sessions_total, 1);
        drop(sessions);
    }

    #[test]
    fn oversized_keys_are_dropped_without_allocation() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(20);
        let oversized = "x".repeat(MAX_CORRELATION_KEY_BYTES + 1);
        assert!(
            telemetry
                .observe_upstream(&oversized, UpstreamClass::IndependentSentinelAhead)
                .is_none()
        );
        assert_eq!(telemetry.stats().dropped_events_total, 1);

        let event = telemetry
            .observe_upstream("old", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_a_ready(event);
        telemetry.mark_published(event, &oversized);
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.reason, "replacement_key_oversize");
        assert_eq!(telemetry.stats().dropped_events_total, 2);
    }

    #[test]
    fn disconnect_before_publish_does_not_complete_unpublished_event() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(21);
        let mut session = telemetry.session(1);
        session.authorize();
        let event = telemetry
            .observe_upstream("old", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        drop(session);
        assert_eq!(telemetry.stats().in_flight_events, 1);
        assert!(telemetry.take_completed().is_empty());
        telemetry.fail_event(event, "replacement_unavailable");
        assert_eq!(
            telemetry.take_completed().pop().unwrap().reason,
            "replacement_unavailable"
        );
    }

    #[test]
    fn timeout_and_monotonic_order_never_report_missing_as_zero() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(14);
        let start = Instant::now();
        let _event = telemetry
            .observe_upstream_at("timeout", UpstreamClass::IndependentSentinelAhead, start)
            .unwrap();
        {
            let mut state = telemetry.state.lock().unwrap();
            telemetry.expire_locked(&mut state, start + EVENT_TIMEOUT);
        }
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.status, "timeout");
        assert_eq!(summary.observed_to_a_ready_ms, None);
        assert_eq!(summary.a_ready_to_published_ms, None);
        assert_eq!(summary.total_ms, Some(EVENT_TIMEOUT.as_millis() as u64));

        let event = telemetry
            .observe_upstream_at("rollback", UpstreamClass::IndependentSentinelAhead, start)
            .unwrap();
        telemetry.mark_a_ready_at(event, start - Duration::from_millis(1));
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.reason, "out_of_order");
        assert_eq!(summary.observed_to_a_ready_ms, None);
    }

    #[test]
    fn process_restart_gets_a_new_non_chain_correlation_namespace() {
        let first = ReplacementLatencyTelemetry::enabled_for_test(0xaaaa_0001);
        let second = ReplacementLatencyTelemetry::enabled_for_test(0xbbbb_0002);
        let first_id = first
            .observe_upstream("same", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        let second_id = second
            .observe_upstream("same", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        assert_eq!(first_id, second_id);
        assert_ne!(
            first.correlation_id(first_id),
            second.correlation_id(second_id)
        );
    }

    #[test]
    fn safe_summary_contains_no_chain_or_session_material() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(15);
        let summary = complete_event(&telemetry, UpstreamClass::IndependentSentinelAhead, 1);
        let encoded = summary.safe_json().to_string();
        for forbidden in [
            "tip_hash",
            "parent_hash",
            "peer_id",
            "miner_address",
            "worker_name",
            "/ip4/",
            "bearer ",
            "\"job_id\":",
            "\"session_id\":",
        ] {
            assert!(!encoded.contains(forbidden), "{forbidden}");
        }
        assert!(encoded.contains("\"schema\":\"fast_tip_replacement_latency/v1u2\""));
        assert!(encoded.contains("\"chain_values_recorded\":false"));
    }

    #[test]
    fn disabled_telemetry_is_a_noop() {
        let telemetry = ReplacementLatencyTelemetry::disabled();
        assert!(
            telemetry
                .observe_upstream("old", UpstreamClass::IndependentSentinelAhead)
                .is_none()
        );
        let mut session = telemetry.session(1);
        session.authorize();
        session.record_notify("new", FanoutOutcome::Failed);
        assert_eq!(telemetry.stats(), LatencyStats::default());
    }

    #[test]
    fn duplicate_hot_path_has_a_fixed_upper_bound() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(16);
        let started = Instant::now();
        for _ in 0..100_000 {
            assert!(
                telemetry
                    .observe_upstream("same", UpstreamClass::IndependentSentinelAhead)
                    .is_some()
            );
        }
        let elapsed = started.elapsed();
        eprintln!(
            "FAST_TIP_TELEMETRY_BENCH duplicate_ops=100000 elapsed_us={}",
            elapsed.as_micros()
        );
        assert!(elapsed < Duration::from_secs(2));
        assert_eq!(telemetry.stats().in_flight_events, 1);
    }

    #[test]
    fn concurrent_fanout_completion_is_counted_once() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(17);
        let mut sessions = (1..=64)
            .map(|id| {
                let mut session = telemetry.session(id);
                session.authorize();
                session
            })
            .collect::<Vec<_>>();
        let event = telemetry
            .observe_upstream("old", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_a_ready(event);
        telemetry.mark_published(event, "new");
        std::thread::scope(|scope| {
            for session in sessions.drain(..) {
                scope.spawn(move || session.record_notify("new", FanoutOutcome::Success));
            }
        });
        {
            let mut state = telemetry
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            telemetry.expire_locked(&mut state, Instant::now() + EVENT_TIMEOUT);
        }
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.target_sessions, 64);
        assert!(summary.notify_success <= 64);
        assert_eq!(summary.notify_failed, 0);
        assert_eq!(
            summary.notify_success + summary.notify_skipped,
            summary.target_sessions
        );
        assert!(matches!(summary.status, "complete" | "timeout"));
    }

    #[test]
    fn poisoned_tracker_is_fail_open() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(18);
        let clone = telemetry.clone();
        let _ = std::thread::spawn(move || {
            let _guard = clone.state.lock().unwrap();
            panic!("poison telemetry lock");
        })
        .join();
        assert!(
            telemetry
                .observe_upstream("old", UpstreamClass::IndependentSentinelAhead)
                .is_none()
        );
        assert_eq!(telemetry.stats().lock_failures_total, 1);
        assert_eq!(telemetry.stats().lock_contention_total, 0);
        assert_eq!(telemetry.stats().lock_poisoned_total, 1);
    }

    #[test]
    fn safe_log_rate_is_bounded_and_counts_drops() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(19);
        for id in 0..=MAX_LOGS_PER_MINUTE {
            let event = telemetry
                .observe_upstream(
                    &format!("old-{id}"),
                    UpstreamClass::IndependentSentinelAhead,
                )
                .unwrap();
            telemetry.mark_a_ready(event);
            telemetry.mark_published(event, &format!("new-{id}"));
        }
        assert_eq!(telemetry.stats().dropped_logs_total, 1);
        assert_eq!(telemetry.stats().in_flight_events, 0);
    }

    #[test]
    fn full_log_queue_drops_without_blocking_completion() {
        let (sender, _receiver) = mpsc::channel(1);
        let mut configured = ReplacementLatencyTelemetry::with_run_id(true, 22);
        configured.log_sender = Some(sender);
        let telemetry = Arc::new(configured);
        for id in 0..2 {
            let event = telemetry
                .observe_upstream(
                    &format!("old-{id}"),
                    UpstreamClass::IndependentSentinelAhead,
                )
                .unwrap();
            telemetry.mark_a_ready(event);
            telemetry.mark_published(event, &format!("new-{id}"));
        }
        assert_eq!(telemetry.take_completed().len(), 2);
        assert_eq!(telemetry.stats().dropped_logs_total, 1);
        assert_eq!(telemetry.stats().in_flight_events, 0);
    }

    #[test]
    fn every_production_state_call_returns_immediately_under_contention() {
        fn assert_nonblocking(call: impl FnOnce()) {
            let started = Instant::now();
            call();
            assert!(
                started.elapsed() < Duration::from_millis(20),
                "telemetry state call blocked for {:?}",
                started.elapsed()
            );
        }

        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(23);
        {
            let _state = telemetry.state.lock().unwrap();
            assert_nonblocking(|| {
                assert!(
                    telemetry
                        .observe_upstream("contended", UpstreamClass::IndependentSentinelAhead)
                        .is_none()
                );
            });
        }

        let ready_event = telemetry
            .observe_upstream("ready", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        {
            let _state = telemetry.state.lock().unwrap();
            assert_nonblocking(|| telemetry.mark_a_ready(ready_event));
        }
        assert!(
            telemetry
                .state
                .lock()
                .unwrap()
                .events
                .get(&ready_event)
                .unwrap()
                .a_ready_at
                .is_none()
        );
        telemetry.fail_event(ready_event, "contention_fixture_cleanup");

        let publish_event = telemetry
            .observe_upstream("publish", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_a_ready(publish_event);
        {
            let _state = telemetry.state.lock().unwrap();
            assert_nonblocking(|| telemetry.mark_published(publish_event, "replacement"));
        }
        {
            let state = telemetry.state.lock().unwrap();
            assert!(
                state
                    .events
                    .get(&publish_event)
                    .unwrap()
                    .published_at
                    .is_none()
            );
            assert!(!state.published_jobs.contains_key("replacement"));
        }
        telemetry.fail_event(publish_event, "contention_fixture_cleanup");

        let mut fanout_session = telemetry.session(1);
        fanout_session.authorize();
        let fanout_event = telemetry
            .observe_upstream("fanout", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_a_ready(fanout_event);
        telemetry.mark_published(fanout_event, "fanout-replacement");
        {
            let _state = telemetry.state.lock().unwrap();
            assert_nonblocking(|| {
                fanout_session.record_notify("fanout-replacement", FanoutOutcome::Success);
            });
        }
        {
            let state = telemetry.state.lock().unwrap();
            let event = state.events.get(&fanout_event).unwrap();
            assert_eq!(event.notify_success, 0);
            assert_eq!(event.pending_sessions.len(), 1);
        }
        telemetry.fail_event(fanout_event, "contention_fixture_cleanup");
        drop(fanout_session);

        let mut dropped_session = telemetry.session(2);
        dropped_session.authorize();
        {
            let _state = telemetry.state.lock().unwrap();
            assert_nonblocking(|| drop(dropped_session));
        }

        let stats = telemetry.stats();
        assert_eq!(stats.lock_contention_total, 5);
        assert_eq!(stats.lock_poisoned_total, 0);
        assert_eq!(stats.lock_failures_total, 5);
        assert_eq!(stats.roster_cleanup_failures_total, 1);
    }

    #[test]
    fn full_log_queue_and_state_contention_remain_fail_open() {
        let (sender, _receiver) = mpsc::channel(1);
        let mut configured = ReplacementLatencyTelemetry::with_run_id(true, 24);
        configured.log_sender = Some(sender);
        let telemetry = Arc::new(configured);
        for id in 0..2 {
            let event = telemetry
                .observe_upstream(
                    &format!("log-{id}"),
                    UpstreamClass::IndependentSentinelAhead,
                )
                .unwrap();
            telemetry.mark_a_ready(event);
            telemetry.mark_published(event, &format!("logged-{id}"));
        }
        let started = Instant::now();
        {
            let _state = telemetry.state.lock().unwrap();
            assert!(
                telemetry
                    .observe_upstream("contended", UpstreamClass::IndependentSentinelAhead)
                    .is_none()
            );
        }
        assert!(started.elapsed() < Duration::from_millis(20));
        assert_eq!(telemetry.stats().dropped_logs_total, 1);
        assert_eq!(telemetry.stats().lock_contention_total, 1);
    }

    #[test]
    fn authorization_contention_preserves_roster_truth_without_false_completion() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(27);
        let mut established = telemetry.session(1);
        established.authorize();
        let mut contended = telemetry.session(2);
        {
            let _state = telemetry.state.lock().unwrap();
            let started = Instant::now();
            contended.authorize();
            assert!(started.elapsed() < Duration::from_millis(20));
            assert!(contended.counted);
            assert!(!contended.tracked);
            assert_eq!(telemetry.active_session_total.load(Ordering::Relaxed), 2);
        }
        let stats = telemetry.stats();
        assert_eq!(stats.active_session_total, 2);
        assert_eq!(stats.tracked_sessions, 1);
        assert_eq!(stats.dropped_sessions_total, 1);
        assert_eq!(stats.untracked_sessions_total, 1);
        assert_eq!(stats.untracked_active_sessions, 1);
        assert_eq!(stats.stale_tracked_sessions, 0);
        assert_eq!(telemetry.stats().lock_contention_total, 1);

        let event = telemetry
            .observe_upstream("roster-gap", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_a_ready(event);
        telemetry.mark_published(event, "roster-gap-replacement");
        established.record_notify("roster-gap-replacement", FanoutOutcome::Success);
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.status, "tracking_gap");
        assert_eq!(summary.reason, "untracked_sessions");
        assert_eq!(summary.target_sessions, 2);
        assert_eq!(summary.notify_success, 1);
        assert_eq!(summary.notify_failed, 0);
        assert_eq!(summary.notify_skipped, 1);
        assert_eq!(summary.tracking_gap_sessions, 1);
        assert_eq!(summary.stale_roster_sessions, 0);
        assert_eq!(summary.published_to_fanout_ms, None);
        assert_eq!(summary.total_ms, None);

        drop(contended);
        assert_eq!(telemetry.stats().active_session_total, 1);
        assert_eq!(telemetry.stats().untracked_active_sessions, 0);
        drop(established);
        assert_eq!(telemetry.stats().active_session_total, 0);
    }

    #[test]
    fn contended_drop_degrades_stale_roster_instead_of_sampling_it() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(28);
        let mut established = telemetry.session(1);
        let mut dropping = telemetry.session(2);
        established.authorize();
        dropping.authorize();
        {
            let _state = telemetry.state.lock().unwrap();
            let started = Instant::now();
            drop(dropping);
            assert!(started.elapsed() < Duration::from_millis(20));
            assert_eq!(telemetry.active_session_total.load(Ordering::Relaxed), 1);
        }
        let stats = telemetry.stats();
        assert_eq!(stats.active_session_total, 1);
        assert_eq!(stats.tracked_sessions, 2);
        assert_eq!(stats.roster_cleanup_failures_total, 1);
        assert_eq!(stats.untracked_active_sessions, 0);
        assert_eq!(stats.stale_tracked_sessions, 1);

        let event = telemetry
            .observe_upstream("stale-roster", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_a_ready(event);
        telemetry.mark_published(event, "stale-roster-replacement");
        established.record_notify("stale-roster-replacement", FanoutOutcome::Success);
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.status, "tracking_gap");
        assert_eq!(summary.reason, "stale_session_roster");
        assert_eq!(summary.target_sessions, 1);
        assert_eq!(summary.notify_success, 0);
        assert_eq!(summary.notify_failed, 0);
        assert_eq!(summary.notify_skipped, 1);
        assert_eq!(summary.tracking_gap_sessions, 1);
        assert_eq!(summary.stale_roster_sessions, 1);
        assert_eq!(summary.published_to_fanout_ms, None);
        assert_eq!(summary.total_ms, None);
        assert!(telemetry.take_completed().is_empty());

        drop(established);
        assert_eq!(telemetry.stats().active_session_total, 0);
    }

    #[test]
    fn offsetting_untracked_and_stale_counts_still_degrade_the_roster() {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(29);
        let mut established = telemetry.session(1);
        let mut dropping = telemetry.session(2);
        let mut untracked = telemetry.session(3);
        established.authorize();
        dropping.authorize();
        {
            let _state = telemetry.state.lock().unwrap();
            drop(dropping);
            untracked.authorize();
        }
        let stats = telemetry.stats();
        assert_eq!(stats.active_session_total, 2);
        assert_eq!(stats.tracked_sessions, 2);
        assert_eq!(stats.untracked_active_sessions, 1);
        assert_eq!(stats.stale_tracked_sessions, 1);

        let event = telemetry
            .observe_upstream("offset-roster", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_a_ready(event);
        telemetry.mark_published(event, "offset-roster-replacement");
        established.record_notify("offset-roster-replacement", FanoutOutcome::Success);
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.status, "tracking_gap");
        assert_eq!(summary.reason, "stale_session_roster");
        assert_eq!(summary.target_sessions, 2);
        assert_eq!(summary.notify_success, 0);
        assert_eq!(summary.notify_skipped, 2);
        assert_eq!(summary.tracking_gap_sessions, 2);
        assert_eq!(summary.stale_roster_sessions, 1);
        assert_eq!(summary.total_ms, None);

        drop(untracked);
        drop(established);
        assert_eq!(telemetry.stats().active_session_total, 0);
    }

    fn capacity_benchmark(targets: usize, run_id: u32) -> (u128, u128) {
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(run_id);
        let sessions = (1..=targets as u64)
            .map(|session_id| {
                let mut session = telemetry.session(session_id);
                session.authorize();
                session
            })
            .collect::<Vec<_>>();
        let event = telemetry
            .observe_upstream("capacity", UpstreamClass::IndependentSentinelAhead)
            .unwrap();
        telemetry.mark_a_ready(event);
        let snapshot_started = Instant::now();
        telemetry.mark_published(event, "capacity-replacement");
        let snapshot_us = snapshot_started.elapsed().as_micros();
        let fanout_started = Instant::now();
        for session in &sessions {
            session.record_notify("capacity-replacement", FanoutOutcome::Success);
        }
        let fanout_us = fanout_started.elapsed().as_micros();
        let summary = telemetry.take_completed().pop().unwrap();
        assert_eq!(summary.target_sessions, targets);
        assert_eq!(summary.notify_success, targets);
        (snapshot_us, fanout_us)
    }

    #[test]
    fn session_capacity_snapshots_and_fanout_have_fixed_upper_bounds() {
        for (targets, run_id) in [(407, 25), (MAX_TRACKED_SESSIONS, 26)] {
            let (snapshot_us, fanout_us) = capacity_benchmark(targets, run_id);
            eprintln!(
                "FAST_TIP_TELEMETRY_CAPACITY sessions={targets} snapshot_us={snapshot_us} fanout_us={fanout_us}"
            );
            assert!(snapshot_us < 100_000);
            assert!(fanout_us < 500_000);
        }
    }

    #[test]
    fn disabled_session_lifecycle_never_touches_state_or_logger() {
        let telemetry = ReplacementLatencyTelemetry::disabled();
        let started = Instant::now();
        for session_id in 1..=100_000 {
            let mut session = telemetry.session(session_id);
            session.authorize();
            drop(session);
        }
        let elapsed = started.elapsed();
        eprintln!(
            "FAST_TIP_TELEMETRY_DISABLED sessions=100000 elapsed_us={}",
            elapsed.as_micros()
        );
        assert!(elapsed < Duration::from_secs(2));
        assert!(telemetry.log_sender.is_none());
        assert_eq!(telemetry.stats(), LatencyStats::default());
    }
}
