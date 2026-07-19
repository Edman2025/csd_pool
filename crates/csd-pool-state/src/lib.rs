use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DIFFICULTY_ONE_HASHES: f64 = 4_294_967_296.0;

#[derive(Clone, Debug, Default)]
pub struct SharedPoolState {
    inner: Arc<RwLock<PoolState>>,
}

impl SharedPoolState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn connection_guard(&self) -> ConnectionGuard {
        {
            let mut state = self.inner.write().expect("pool state lock");
            state.stratum_connections += 1;
            state.updated_ts = now_ts();
        }
        ConnectionGuard {
            pool_state: self.clone(),
        }
    }

    pub fn record_authorized_worker(&self, address: &str) {
        let mut state = self.inner.write().expect("pool state lock");
        let worker = state.workers.entry(address.to_owned()).or_default();
        worker.last_seen_ts = now_ts();
        state.updated_ts = now_ts();
    }

    pub fn record_share_accepted(&self, address: &str, difficulty: f64, is_block_candidate: bool) {
        let mut state = self.inner.write().expect("pool state lock");
        let now = now_ts();
        let worker = state.workers.entry(address.to_owned()).or_default();
        worker.shares_accepted += 1;
        worker.last_difficulty = difficulty;
        worker.last_seen_ts = now;
        worker.first_share_ts.get_or_insert(now);
        worker.last_share_ts = Some(now);
        worker.share_difficulty_sum += difficulty.max(0.0);
        if is_block_candidate {
            worker.blocks_found += 1;
            state.blocks_found += 1;
        }
        state.shares_accepted += 1;
        state.first_share_ts.get_or_insert(now);
        state.last_share_ts = Some(now);
        state.share_difficulty_sum += difficulty.max(0.0);
        state.round_share_difficulty_sum += difficulty.max(0.0);
        if is_block_candidate {
            state.round_share_difficulty_sum = 0.0;
        }
        state.updated_ts = now;
    }

    pub fn record_share_rejected(&self, address: &str) {
        let mut state = self.inner.write().expect("pool state lock");
        let worker = state.workers.entry(address.to_owned()).or_default();
        worker.shares_rejected += 1;
        worker.last_seen_ts = now_ts();
        state.shares_rejected += 1;
        state.updated_ts = now_ts();
    }

    pub fn record_share_stale(&self, address: &str) {
        let mut state = self.inner.write().expect("pool state lock");
        let worker = state.workers.entry(address.to_owned()).or_default();
        worker.shares_stale += 1;
        worker.last_seen_ts = now_ts();
        state.shares_stale += 1;
        state.updated_ts = now_ts();
    }

    pub fn record_share_validation(&self, elapsed: Duration) {
        let mut state = self.inner.write().expect("pool state lock");
        state.share_validation_count += 1;
        state.share_validation_seconds_sum += elapsed.as_secs_f64();
        state.updated_ts = now_ts();
    }

    pub fn snapshot(&self) -> PoolSnapshot {
        self.inner.read().expect("pool state lock").snapshot()
    }

    fn record_connection_closed(&self) {
        let mut state = self.inner.write().expect("pool state lock");
        state.stratum_connections = state.stratum_connections.saturating_sub(1);
        state.updated_ts = now_ts();
    }
}

#[derive(Debug)]
pub struct ConnectionGuard {
    pool_state: SharedPoolState,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.pool_state.record_connection_closed();
    }
}

#[derive(Clone, Debug, Default)]
struct PoolState {
    workers: BTreeMap<String, WorkerSnapshot>,
    shares_accepted: u64,
    shares_rejected: u64,
    shares_stale: u64,
    blocks_found: u64,
    stratum_connections: u64,
    share_validation_count: u64,
    share_validation_seconds_sum: f64,
    share_difficulty_sum: f64,
    round_share_difficulty_sum: f64,
    first_share_ts: Option<u64>,
    last_share_ts: Option<u64>,
    updated_ts: u64,
}

impl PoolState {
    fn snapshot(&self) -> PoolSnapshot {
        PoolSnapshot {
            workers: self.workers.clone(),
            totals: TotalsSnapshot {
                workers_online: self.workers.len() as u64,
                shares_accepted: self.shares_accepted,
                shares_rejected: self.shares_rejected,
                shares_stale: self.shares_stale,
                blocks_found: self.blocks_found,
                stratum_connections: self.stratum_connections,
                share_validation_count: self.share_validation_count,
                share_validation_seconds_sum: self.share_validation_seconds_sum,
                share_difficulty_sum: self.share_difficulty_sum,
                round_share_difficulty_sum: self.round_share_difficulty_sum,
                pool_hashrate_hs: estimated_hashrate(
                    self.share_difficulty_sum,
                    self.first_share_ts,
                    self.last_share_ts,
                ),
            },
            updated_ts: self.updated_ts,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PoolSnapshot {
    pub workers: BTreeMap<String, WorkerSnapshot>,
    pub totals: TotalsSnapshot,
    pub updated_ts: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct WorkerSnapshot {
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_stale: u64,
    pub blocks_found: u64,
    pub last_difficulty: f64,
    pub last_seen_ts: u64,
    pub share_difficulty_sum: f64,
    pub first_share_ts: Option<u64>,
    pub last_share_ts: Option<u64>,
}

impl WorkerSnapshot {
    pub fn hashrate_hs(&self) -> f64 {
        estimated_hashrate(
            self.share_difficulty_sum,
            self.first_share_ts,
            self.last_share_ts,
        )
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct TotalsSnapshot {
    pub workers_online: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_stale: u64,
    pub blocks_found: u64,
    pub stratum_connections: u64,
    pub share_validation_count: u64,
    pub share_validation_seconds_sum: f64,
    pub share_difficulty_sum: f64,
    pub round_share_difficulty_sum: f64,
    pub pool_hashrate_hs: f64,
}

fn estimated_hashrate(difficulty_sum: f64, first_ts: Option<u64>, last_ts: Option<u64>) -> f64 {
    let (Some(first_ts), Some(last_ts)) = (first_ts, last_ts) else {
        return 0.0;
    };
    let elapsed = last_ts.saturating_sub(first_ts);
    if elapsed == 0 || difficulty_sum <= 0.0 {
        return 0.0;
    }
    (difficulty_sum * DIFFICULTY_ONE_HASHES) / elapsed as f64
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_worker_and_shares() {
        let state = SharedPoolState::new();
        state.record_authorized_worker("abc");
        state.record_share_accepted("abc", 8.0, false);
        state.record_share_rejected("abc");
        state.record_share_stale("abc");

        let snapshot = state.snapshot();
        assert_eq!(snapshot.totals.workers_online, 1);
        assert_eq!(snapshot.totals.shares_accepted, 1);
        assert_eq!(snapshot.totals.shares_rejected, 1);
        assert_eq!(snapshot.totals.shares_stale, 1);
        assert_eq!(snapshot.workers["abc"].last_difficulty, 8.0);
        assert_eq!(snapshot.workers["abc"].share_difficulty_sum, 8.0);
        assert_eq!(snapshot.totals.share_difficulty_sum, 8.0);
        assert_eq!(snapshot.totals.round_share_difficulty_sum, 8.0);
    }

    #[test]
    fn block_candidate_increments_found_count() {
        let state = SharedPoolState::new();
        state.record_share_accepted("abc", 8.0, true);
        let snapshot = state.snapshot();
        assert_eq!(snapshot.totals.blocks_found, 1);
        assert_eq!(snapshot.workers["abc"].blocks_found, 1);
        assert_eq!(snapshot.totals.share_difficulty_sum, 8.0);
        assert_eq!(snapshot.totals.round_share_difficulty_sum, 0.0);
    }

    #[test]
    fn connection_guard_tracks_active_stratum_connections() {
        let state = SharedPoolState::new();
        {
            let _first = state.connection_guard();
            assert_eq!(state.snapshot().totals.stratum_connections, 1);
            {
                let _second = state.connection_guard();
                assert_eq!(state.snapshot().totals.stratum_connections, 2);
            }
            assert_eq!(state.snapshot().totals.stratum_connections, 1);
        }
        assert_eq!(state.snapshot().totals.stratum_connections, 0);
    }

    #[test]
    fn records_share_validation_timing() {
        let state = SharedPoolState::new();
        state.record_share_validation(Duration::from_millis(25));
        state.record_share_validation(Duration::from_millis(75));

        let totals = state.snapshot().totals;
        assert_eq!(totals.share_validation_count, 2);
        assert!((totals.share_validation_seconds_sum - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn estimates_hashrate_from_share_work_window() {
        assert_eq!(estimated_hashrate(16.0, Some(100), Some(100)), 0.0);
        assert_eq!(estimated_hashrate(0.0, Some(100), Some(120)), 0.0);
        assert_eq!(
            estimated_hashrate(16.0, Some(100), Some(120)),
            (16.0 * DIFFICULTY_ONE_HASHES) / 20.0
        );
    }
}
