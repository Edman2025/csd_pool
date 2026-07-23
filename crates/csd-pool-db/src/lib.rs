use async_trait::async_trait;
use csd_pool_accounting::{
    LedgerEntry, LedgerKind, MinerBalance, PayoutBatchDraft, PayoutRecipient, ShareWeight,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::num::{ParseIntError, TryFromIntError};
use std::sync::{Arc, RwLock};
use thiserror::Error;

const DIFFICULTY_ONE_HASHES: f64 = 4_294_967_296.0;

pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../../../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "control_settings",
        sql: include_str!("../../../migrations/0002_control_settings.sql"),
    },
    Migration {
        version: 3,
        name: "alert_events",
        sql: include_str!("../../../migrations/0003_alert_events.sql"),
    },
    Migration {
        version: 4,
        name: "share_events",
        sql: include_str!("../../../migrations/0004_share_events.sql"),
    },
    Migration {
        version: 5,
        name: "payout_approval",
        sql: include_str!("../../../migrations/0005_payout_approval.sql"),
    },
    Migration {
        version: 6,
        name: "payout_audit_events",
        sql: include_str!("../../../migrations/0006_payout_audit_events.sql"),
    },
    Migration {
        version: 7,
        name: "payouts_default_paused",
        sql: include_str!("../../../migrations/0007_payouts_default_paused.sql"),
    },
    Migration {
        version: 8,
        name: "classify_rejected_block_candidates",
        sql: include_str!("../../../migrations/0008_classify_rejected_block_candidates.sql"),
    },
    Migration {
        version: 9,
        name: "stratum_session_observability",
        sql: include_str!("../../../migrations/0009_stratum_session_observability.sql"),
    },
    Migration {
        version: 10,
        name: "job_heartbeat_observability",
        sql: include_str!("../../../migrations/0010_job_heartbeat_observability.sql"),
    },
    Migration {
        version: 11,
        name: "block_candidate_relay_failure",
        sql: include_str!("../../../migrations/0011_block_candidate_relay_failure.sql"),
    },
];

pub fn all_migrations() -> &'static [Migration] {
    MIGRATIONS
}

pub fn migration_by_version(version: i64) -> Option<&'static Migration> {
    MIGRATIONS
        .iter()
        .find(|migration| migration.version == version)
}

pub async fn run_migrations(pool: &sqlx::PgPool) -> Result<Vec<i64>> {
    use sqlx::Executor;

    // Worker timers can start together during deployment. Serialize the
    // check/apply sequence so migrations cannot race on DDL statements.
    let mut migration_lock = pool.begin().await?;
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(0x4353_4450_4d49_4752_i64)
        .execute(&mut *migration_lock)
        .await?;
    migration_lock
        .execute(
            "create table if not exists schema_migrations (
          version bigint primary key,
          name text not null,
          applied_at timestamptz not null default now()
        )",
        )
        .await?;

    let mut applied = Vec::new();
    for migration in MIGRATIONS {
        let exists: Option<i64> =
            sqlx::query_scalar("select version from schema_migrations where version = $1")
                .bind(migration.version)
                .fetch_optional(&mut *migration_lock)
                .await?;
        if exists.is_some() {
            continue;
        }

        migration_lock.execute(migration.sql).await?;
        sqlx::query(
            "insert into schema_migrations(version, name)
             values ($1, $2)
             on conflict(version) do nothing",
        )
        .bind(migration.version)
        .bind(migration.name)
        .execute(&mut *migration_lock)
        .await?;
        applied.push(migration.version);
    }
    migration_lock.commit().await?;
    Ok(applied)
}

pub async fn applied_migration_versions(pool: &sqlx::PgPool) -> Result<Vec<i64>> {
    let versions = sqlx::query_scalar("select version from schema_migrations order by version")
        .fetch_all(pool)
        .await?;
    Ok(versions)
}

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("repository lock poisoned")]
    LockPoisoned,
    #[error("payout batch already exists: {0}")]
    DuplicatePayoutBatch(String),
    #[error("insufficient confirmed balance for payout recipient: {0}")]
    InsufficientPayoutBalance(String),
    #[error("sql error: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("integer parse error: {0}")]
    ParseInt(#[from] ParseIntError),
    #[error("integer conversion error: {0}")]
    IntConversion(#[from] TryFromIntError),
}

pub type Result<T> = std::result::Result<T, RepositoryError>;

pub trait PoolRepository {
    fn append_ledger_entries(&self, entries: &[LedgerEntry]) -> Result<()>;
    fn list_ledger_entries(&self) -> Result<Vec<LedgerEntry>>;
    fn set_balance(&self, balance: MinerBalance) -> Result<()>;
    fn list_balances(&self) -> Result<Vec<MinerBalance>>;
    fn create_payout_batch(&self, draft: PayoutBatchDraft) -> Result<()>;
    fn list_payout_batches(&self) -> Result<Vec<PayoutBatchDraft>>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRecord {
    pub job_id: String,
    pub prev_hash_be_hex: String,
    pub version_hex: String,
    pub nbits_hex: String,
    pub ntime_hex: String,
    pub network_target: [u8; 32],
    pub share_target: [u8; 32],
    pub coinb1_hex: String,
    pub coinb2_hex: String,
    pub merkle_branches_hex: Vec<String>,
    pub clean_jobs: bool,
    pub job_reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShareRecord {
    pub session_id: Option<String>,
    pub miner: String,
    pub worker_name: String,
    pub job_id: String,
    pub difficulty: f64,
    pub hash: [u8; 32],
    pub extranonce2_hex: String,
    pub ntime_hex: String,
    pub nonce_hex: String,
    pub is_block_candidate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareEventRecord {
    pub session_id: Option<String>,
    pub miner: String,
    pub worker_name: String,
    pub job_id: Option<String>,
    pub kind: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRecord {
    pub id: String,
    pub miner: String,
    pub worker_name: String,
    pub remote_addr: String,
    pub remote_port: u16,
    pub user_agent: Option<String>,
    pub extranonce1: String,
    pub server_session_id: u64,
    pub server_release: String,
    pub server_instance: String,
    pub assigned_difficulty: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SessionVersionSummary {
    pub user_agent: String,
    pub server_release: String,
    pub server_instance: String,
    pub active_sessions: u64,
    pub sessions_1h: u64,
    pub accepted_shares_1h: u64,
    pub rejected_shares_1h: u64,
    pub stale_shares_1h: u64,
    pub latest_share_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RecentSession {
    pub id: String,
    pub worker: String,
    pub remote_addr: Option<String>,
    pub remote_port: Option<u16>,
    pub user_agent: Option<String>,
    pub server_session_id: Option<u64>,
    pub server_release: String,
    pub server_instance: String,
    pub assigned_difficulty: f64,
    pub difficulty_updated_at: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub accepted_shares: u64,
    pub rejected_shares: u64,
    pub stale_shares: u64,
    pub latest_share_at: Option<String>,
}

#[derive(sqlx::FromRow)]
struct SessionVersionSummaryRow {
    user_agent: String,
    server_release: String,
    server_instance: String,
    active_sessions: i64,
    sessions_1h: i64,
    accepted_shares_1h: i64,
    rejected_shares_1h: i64,
    stale_shares_1h: i64,
    latest_share_at: Option<String>,
}

impl TryFrom<SessionVersionSummaryRow> for SessionVersionSummary {
    type Error = RepositoryError;

    fn try_from(row: SessionVersionSummaryRow) -> Result<Self> {
        Ok(Self {
            user_agent: row.user_agent,
            server_release: row.server_release,
            server_instance: row.server_instance,
            active_sessions: u64::try_from(row.active_sessions)?,
            sessions_1h: u64::try_from(row.sessions_1h)?,
            accepted_shares_1h: u64::try_from(row.accepted_shares_1h)?,
            rejected_shares_1h: u64::try_from(row.rejected_shares_1h)?,
            stale_shares_1h: u64::try_from(row.stale_shares_1h)?,
            latest_share_at: row.latest_share_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct RecentSessionRow {
    id: String,
    worker: String,
    remote_addr: Option<String>,
    remote_port: Option<i32>,
    user_agent: Option<String>,
    server_session_id: Option<i64>,
    server_release: String,
    server_instance: String,
    assigned_difficulty: String,
    difficulty_updated_at: String,
    started_at: String,
    ended_at: Option<String>,
    accepted_shares: i64,
    rejected_shares: i64,
    stale_shares: i64,
    latest_share_at: Option<String>,
}

impl TryFrom<RecentSessionRow> for RecentSession {
    type Error = RepositoryError;

    fn try_from(row: RecentSessionRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            worker: row.worker,
            remote_addr: row.remote_addr,
            remote_port: row.remote_port.map(u16::try_from).transpose()?,
            user_agent: row.user_agent,
            server_session_id: row.server_session_id.map(u64::try_from).transpose()?,
            server_release: row.server_release,
            server_instance: row.server_instance,
            assigned_difficulty: row.assigned_difficulty.parse().unwrap_or_default(),
            difficulty_updated_at: row.difficulty_updated_at,
            started_at: row.started_at,
            ended_at: row.ended_at,
            accepted_shares: u64::try_from(row.accepted_shares)?,
            rejected_shares: u64::try_from(row.rejected_shares)?,
            stale_shares: u64::try_from(row.stale_shares)?,
            latest_share_at: row.latest_share_at,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockCandidateRecord {
    pub hash_hex: String,
    pub job_id: String,
    pub miner: String,
    pub worker_name: String,
    pub reward_base_units: u128,
    pub effort_pct: f64,
    pub candidate_payload_json: serde_json::Value,
    pub submit_response_json: serde_json::Value,
}

fn node_response_has_local_canonical_relay_failure(response: &serde_json::Value) -> bool {
    response.get("status").and_then(serde_json::Value::as_str)
        == Some("accepted_local_relay_failed")
        || (response
            .pointer("/node_observability/local_canonical")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && response
                .pointer("/node_observability/relay_ack/ok")
                .and_then(serde_json::Value::as_bool)
                == Some(false))
}

fn block_candidate_has_local_canonical_relay_failure(block: &BlockCandidateRecord) -> bool {
    let response = &block.submit_response_json;
    node_response_has_local_canonical_relay_failure(response)
        || response
            .pointer("/parallel_submit/primary_submit/response")
            .is_some_and(node_response_has_local_canonical_relay_failure)
        || response
            .pointer("/parallel_submit/secondary_submit/response")
            .is_some_and(node_response_has_local_canonical_relay_failure)
}

fn block_candidate_has_ambiguous_submit_outcome(block: &BlockCandidateRecord) -> bool {
    let response = &block.submit_response_json;
    response.get("transport_error").is_some()
        || [
            "/parallel_submit/primary_submit/outcome",
            "/parallel_submit/secondary_submit/outcome",
        ]
        .iter()
        .filter_map(|pointer| {
            response
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
        })
        .any(|outcome| matches!(outcome, "transport_error" | "timeout"))
}

fn block_candidate_initial_status(block: &BlockCandidateRecord) -> &'static str {
    let submit_ok = block
        .submit_response_json
        .get("ok")
        .and_then(serde_json::Value::as_bool);
    let accepted_by = block
        .submit_response_json
        .pointer("/parallel_submit/accepted_by")
        .and_then(serde_json::Value::as_str);
    if submit_ok == Some(true) && accepted_by == Some("secondary") {
        return "submitted_secondary";
    }
    if submit_ok == Some(true) && block_candidate_has_local_canonical_relay_failure(block) {
        return "submitted_degraded";
    }
    if block_candidate_has_local_canonical_relay_failure(block) {
        return "relay_failed";
    }
    if submit_ok == Some(false) && !block_candidate_has_ambiguous_submit_outcome(block) {
        "orphaned"
    } else {
        "submitted"
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockRecord {
    pub hash_hex: String,
    pub job_id: String,
    pub status: String,
    pub height: Option<u64>,
    pub confirmations: u64,
    pub reward_base_units: u128,
    pub effort_pct: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockStatusUpdate {
    pub hash_hex: String,
    pub status: String,
    pub height: Option<u64>,
    pub confirmations: u64,
    pub reward_base_units: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewardBlock {
    pub hash_hex: String,
    pub job_id: String,
    pub reward_base_units: u128,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DashboardPoolStats {
    pub workers_online: u64,
    pub miners_online: u64,
    pub total_blocks: u64,
    pub canonical_blocks: u64,
    pub immature_blocks: u64,
    pub orphaned_blocks: u64,
    pub fee_revenue_base_units: u128,
    pub latest_payout_created_ts: u64,
    pub jobs_created: u64,
    pub latest_job_created_ts: u64,
    pub payout_batches_needs_approval: u64,
    pub payout_batches_created: u64,
    pub payout_batches_signed: u64,
    pub payout_batches_submitted: u64,
    pub payout_batches_confirmed: u64,
    pub payout_batches_failed: u64,
    pub payout_batches_cancelled: u64,
    pub payout_amount_base_units_total: u128,
    pub avg_block_effort_pct_24h: f64,
    pub avg_block_effort_pct_7d: f64,
    pub avg_block_effort_pct_lifetime: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DashboardMinerStats {
    pub miner: String,
    pub immature_base_units: u128,
    pub confirmed_base_units: u128,
    pub locked_base_units: u128,
    pub paid_base_units: u128,
    pub workers_total: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_stale: u64,
    pub blocks_found: u64,
    pub last_difficulty: f64,
    pub last_seen_ts: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DashboardWorkerStats {
    pub name: String,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_stale: u64,
    pub blocks_found: u64,
    pub last_difficulty: f64,
    pub last_seen_ts: u64,
    pub connected_at: Option<String>,
    pub last_seen_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DashboardBlock {
    pub height: u64,
    pub hash: String,
    pub finder: String,
    pub worker: String,
    pub reward_base_units: u128,
    pub status: String,
    pub confirmations: u64,
    pub effort_pct: f64,
    pub found_at: String,
    pub confirmed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DashboardPayment {
    pub batch_id: String,
    pub address: String,
    pub amount_base_units: u128,
    pub txid: String,
    pub status: String,
    pub created_at: String,
    pub confirmed_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DashboardHistorySample {
    pub ts: u64,
    pub pool_hs: f64,
    pub net_hs: f64,
    pub workers: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_stale: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PayoutBatchRecord {
    pub batch_id: String,
    pub status: String,
    pub total_base_units: u128,
    pub txid: Option<String>,
    pub raw_tx_hash: Option<String>,
    pub recipients: Vec<PayoutRecipient>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PayoutAuditEvent {
    pub batch_id: String,
    pub actor: String,
    pub action: String,
    pub details: serde_json::Value,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct NodeSampleRecord {
    pub node_name: String,
    pub height: Option<u64>,
    pub chainwork: Option<String>,
    pub peers: Option<u64>,
    pub mempool_size: Option<u64>,
    pub rpc_ms: Option<f64>,
    pub ok: bool,
    pub sampled_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct OfflineWorkerRecord {
    pub miner: String,
    pub worker_name: String,
    pub last_seen_ts: u64,
    pub last_seen_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AcceptedShareGapRecord {
    pub latest_share_ts: u64,
    pub latest_share_at: Option<String>,
    pub quiet_minutes: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LatestJobRecord {
    pub job_id: String,
    pub prev_hash: String,
    pub created_ts: u64,
    pub created_at: Option<String>,
    pub age_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct BlockSubmissionAlertRecord {
    pub hash_hex: String,
    pub job_id: String,
    pub status: String,
    pub submitted_ts: u64,
    pub submitted_at: Option<String>,
    pub age_seconds: u64,
    pub submit_ok: Option<bool>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ShareQualityAlertRecord {
    pub miner: String,
    pub worker_name: String,
    pub accepted_count: u64,
    pub rejected_count: u64,
    pub stale_count: u64,
    pub reject_rate: f64,
    pub stale_rate: f64,
    pub window_minutes: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AlertEvent {
    pub fingerprint: String,
    pub severity: String,
    pub status: String,
    pub kind: String,
    pub subject: String,
    pub message: String,
    pub first_seen_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub resolved_at: Option<String>,
    pub details: serde_json::Value,
}

#[async_trait]
pub trait AsyncPoolRepository {
    async fn append_ledger_entries(&self, entries: &[LedgerEntry]) -> Result<()>;
    async fn list_ledger_entries(&self) -> Result<Vec<LedgerEntry>>;
    async fn set_balance(&self, balance: MinerBalance) -> Result<()>;
    async fn list_balances(&self) -> Result<Vec<MinerBalance>>;
    async fn create_payout_batch(&self, draft: PayoutBatchDraft) -> Result<()>;
    async fn list_payout_batches(&self) -> Result<Vec<PayoutBatchDraft>>;
}

#[async_trait]
pub trait MiningRepository: Send + Sync {
    async fn open_session(&self, session: &SessionRecord) -> Result<()>;
    async fn close_session(&self, session_id: &str) -> Result<()>;
    async fn close_stale_sessions(&self, server_instance: &str) -> Result<u64>;
    async fn update_session_difficulty(&self, session_id: &str, difficulty: f64) -> Result<()>;
    async fn upsert_job(&self, job: &JobRecord) -> Result<()>;
    async fn insert_share(&self, share: &ShareRecord) -> Result<bool>;
    async fn insert_share_event(&self, event: &ShareEventRecord) -> Result<()>;
    async fn record_block_candidate(&self, block: &BlockCandidateRecord) -> Result<bool>;
}

#[async_trait]
pub trait BlockRepository: Send + Sync {
    async fn list_blocks_to_reconcile(&self, limit: i64) -> Result<Vec<BlockRecord>>;
    async fn update_block_status(&self, update: &BlockStatusUpdate) -> Result<bool>;
}

#[async_trait]
pub trait RewardRepository: Send + Sync {
    async fn list_confirmed_unsettled_blocks(&self, limit: i64) -> Result<Vec<RewardBlock>>;
    async fn share_weights_for_job(&self, job_id: &str) -> Result<Vec<ShareWeight>>;
    async fn list_mature_reward_entries(
        &self,
        confirm_depth: u64,
        limit: i64,
    ) -> Result<Vec<LedgerEntry>>;
    async fn list_orphan_reversal_entries(&self, limit: i64) -> Result<Vec<LedgerEntry>>;
}

#[async_trait]
pub trait DashboardRepository: Send + Sync {
    async fn dashboard_pool_stats(&self) -> Result<DashboardPoolStats>;
    async fn dashboard_miner_stats(&self, address: &str) -> Result<Option<DashboardMinerStats>>;
    async fn dashboard_workers_for_miner(&self, address: &str)
    -> Result<Vec<DashboardWorkerStats>>;
    async fn dashboard_recent_blocks(&self, limit: i64) -> Result<Vec<DashboardBlock>>;
    async fn dashboard_recent_payments(&self, limit: i64) -> Result<Vec<DashboardPayment>>;
    async fn dashboard_recent_payments_for_miner(
        &self,
        address: &str,
        limit: i64,
    ) -> Result<Vec<DashboardPayment>>;
    async fn dashboard_history(
        &self,
        range_secs: u64,
        bucket_secs: u64,
    ) -> Result<Vec<DashboardHistorySample>>;
}

#[async_trait]
pub trait ControlRepository: Send + Sync {
    async fn payouts_enabled(&self) -> Result<bool>;
    async fn set_payouts_enabled(&self, enabled: bool) -> Result<()>;
}

#[async_trait]
pub trait PayoutRepository: Send + Sync {
    async fn list_payable_balances(
        &self,
        minimum_payout_base_units: u128,
        limit: i64,
    ) -> Result<Vec<MinerBalance>>;
    async fn create_locked_payout_batch(&self, draft: PayoutBatchDraft) -> Result<bool>;
    async fn create_locked_payout_batch_with_status(
        &self,
        draft: PayoutBatchDraft,
        status: &str,
    ) -> Result<bool>;
    async fn list_payout_batches_by_status(
        &self,
        statuses: &[&str],
        limit: i64,
    ) -> Result<Vec<PayoutBatchRecord>>;
    async fn active_payout_total_today(&self) -> Result<u128>;
    async fn mark_payout_signed(
        &self,
        batch_id: &str,
        txid: &str,
        raw_tx_hash: &str,
    ) -> Result<bool>;
    async fn mark_payout_submitted(&self, batch_id: &str, txid: &str) -> Result<bool>;
    async fn mark_payout_confirmed(&self, batch_id: &str) -> Result<bool>;
    async fn mark_payout_failed(&self, batch_id: &str, reason: &str) -> Result<bool>;
    async fn mark_payout_approved(&self, batch_id: &str) -> Result<bool>;
    async fn append_payout_audit_event(&self, event: &PayoutAuditEvent) -> Result<()>;
    async fn list_payout_audit_events(
        &self,
        batch_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<PayoutAuditEvent>>;
    async fn retry_failed_payout(
        &self,
        batch_id: &str,
        new_batch_id: &str,
    ) -> Result<Option<PayoutBatchRecord>>;
    async fn cancel_payout(
        &self,
        batch_id: &str,
        reason: &str,
    ) -> Result<Option<PayoutBatchRecord>>;
    async fn unlock_failed_payout(&self, batch: &PayoutBatchRecord) -> Result<usize>;
    async fn mark_payout_paid(&self, batch: &PayoutBatchRecord) -> Result<usize>;
}

#[async_trait]
pub trait MonitoringRepository: Send + Sync {
    async fn insert_node_sample(&self, sample: &NodeSampleRecord) -> Result<()>;
    async fn latest_node_samples(&self, limit: i64) -> Result<Vec<NodeSampleRecord>>;
    async fn list_stuck_payout_batches(
        &self,
        older_than_minutes: i64,
        limit: i64,
    ) -> Result<Vec<PayoutBatchRecord>>;
    async fn list_offline_workers(
        &self,
        offline_minutes: i64,
        limit: i64,
    ) -> Result<Vec<OfflineWorkerRecord>>;
    async fn accepted_share_gap(
        &self,
        quiet_minutes: i64,
    ) -> Result<Option<AcceptedShareGapRecord>>;
    async fn latest_job(&self) -> Result<Option<LatestJobRecord>>;
    async fn list_block_submission_alerts(
        &self,
        stuck_minutes: i64,
        limit: i64,
    ) -> Result<Vec<BlockSubmissionAlertRecord>>;
    async fn list_share_quality_alerts(
        &self,
        window_minutes: i64,
        min_total: u64,
        max_reject_rate: f64,
        max_stale_rate: f64,
        limit: i64,
    ) -> Result<Vec<ShareQualityAlertRecord>>;
    async fn upsert_alert(&self, alert: &AlertEvent) -> Result<()>;
    async fn resolve_alert(&self, fingerprint: &str) -> Result<bool>;
    async fn list_alerts(&self, status: Option<&str>, limit: i64) -> Result<Vec<AlertEvent>>;
}

#[derive(Clone)]
pub struct PgRepository {
    pool: sqlx::PgPool,
}

impl PgRepository {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> Result<Self> {
        Ok(Self::new(sqlx::PgPool::connect(database_url).await?))
    }

    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }

    pub async fn session_version_summaries(&self) -> Result<Vec<SessionVersionSummary>> {
        let rows = sqlx::query_as::<_, SessionVersionSummaryRow>(
            "with share_1h as (
               select session_id, count(*)::bigint accepted, max(created_at)::text latest_share_at
               from shares
               where session_id is not null and created_at >= now() - interval '1 hour'
               group by session_id
             ),
             event_1h as (
               select session_id,
                 count(*) filter(where kind = 'rejected')::bigint rejected,
                 count(*) filter(where kind = 'stale')::bigint stale
               from share_events
               where session_id is not null and created_at >= now() - interval '1 hour'
               group by session_id
             )
             select
               coalesce(s.user_agent, 'unknown') user_agent,
               s.server_release,
               s.server_instance,
               count(*) filter(where s.ended_at is null)::bigint active_sessions,
               count(*) filter(where s.started_at >= now() - interval '1 hour')::bigint sessions_1h,
               coalesce(sum(sh.accepted), 0)::bigint accepted_shares_1h,
               coalesce(sum(ev.rejected), 0)::bigint rejected_shares_1h,
               coalesce(sum(ev.stale), 0)::bigint stale_shares_1h,
               max(sh.latest_share_at) latest_share_at
             from sessions s
             left join share_1h sh on sh.session_id = s.id
             left join event_1h ev on ev.session_id = s.id
             where s.ended_at is null or s.started_at >= now() - interval '1 hour'
             group by coalesce(s.user_agent, 'unknown'), s.server_release, s.server_instance
             order by active_sessions desc, user_agent",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn recent_stratum_sessions(&self, limit: i64) -> Result<Vec<RecentSession>> {
        let rows = sqlx::query_as::<_, RecentSessionRow>(
            "select
               s.id::text id,
               w.name worker,
               host(s.remote_addr) remote_addr,
               s.remote_port,
               s.user_agent,
               s.server_session_id,
               s.server_release,
               s.server_instance,
               s.assigned_difficulty::text assigned_difficulty,
               s.difficulty_updated_at::text difficulty_updated_at,
               s.started_at::text started_at,
               s.ended_at::text ended_at,
               coalesce(sh.accepted, 0)::bigint accepted_shares,
               coalesce(ev.rejected, 0)::bigint rejected_shares,
               coalesce(ev.stale, 0)::bigint stale_shares,
               sh.latest_share_at
             from sessions s
             join workers w on w.id = s.worker_id
             left join lateral (
               select count(*)::bigint accepted, max(created_at)::text latest_share_at
               from shares
               where session_id = s.id
             ) sh on true
             left join lateral (
               select
                 count(*) filter(where kind = 'rejected')::bigint rejected,
                 count(*) filter(where kind = 'stale')::bigint stale
               from share_events
               where session_id = s.id
             ) ev on true
             order by s.started_at desc
             limit $1",
        )
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn miner_id_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        address: &str,
    ) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "insert into miners(address, last_seen_at)
             values ($1, now())
             on conflict(address) do update set last_seen_at = excluded.last_seen_at
             returning id",
        )
        .bind(address)
        .fetch_one(&mut **tx)
        .await?)
    }

    async fn miner_id(&self, address: &str) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "insert into miners(address, last_seen_at)
             values ($1, now())
             on conflict(address) do update set last_seen_at = excluded.last_seen_at
             returning id",
        )
        .bind(address)
        .fetch_one(&self.pool)
        .await?)
    }

    async fn worker_id_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        miner_id: i64,
        worker_name: &str,
    ) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "insert into workers(miner_id, name, last_seen_at)
             values ($1, $2, now())
             on conflict(miner_id, name) do update set last_seen_at = excluded.last_seen_at
             returning id",
        )
        .bind(miner_id)
        .bind(worker_name)
        .fetch_one(&mut **tx)
        .await?)
    }
}

#[async_trait]
impl AsyncPoolRepository for PgRepository {
    async fn append_ledger_entries(&self, entries: &[LedgerEntry]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for entry in entries {
            let miner_id = match entry.miner.as_deref() {
                Some(address) => Some(Self::miner_id_in_tx(&mut tx, address).await?),
                None => None,
            };
            let inserted = sqlx::query(
                "insert into ledger_entries(miner_id, amount_base_units, kind, ref_type, ref_id)
                 values ($1, $2::numeric, $3, $4, $5)
                 on conflict do nothing",
            )
            .bind(miner_id)
            .bind(entry.amount_base_units.to_string())
            .bind(entry.kind.as_str())
            .bind(&entry.ref_type)
            .bind(&entry.ref_id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
                == 1;

            if inserted {
                match (&entry.kind, miner_id) {
                    (LedgerKind::RewardImmature, Some(miner_id)) if entry.amount_base_units > 0 => {
                        sqlx::query(
                            "insert into balance_cache(miner_id, immature_base_units, updated_at)
                             values ($1, $2::numeric, now())
                             on conflict(miner_id) do update set
                               immature_base_units = balance_cache.immature_base_units + excluded.immature_base_units,
                               updated_at = excluded.updated_at",
                        )
                        .bind(miner_id)
                        .bind(entry.amount_base_units.to_string())
                        .execute(&mut *tx)
                        .await?;
                    }
                    (LedgerKind::RewardMature, Some(miner_id)) if entry.amount_base_units > 0 => {
                        sqlx::query(
                            "insert into balance_cache(miner_id, immature_base_units, confirmed_base_units, updated_at)
                             values ($1, 0, $2::numeric, now())
                             on conflict(miner_id) do update set
                               immature_base_units = greatest(balance_cache.immature_base_units - excluded.confirmed_base_units, 0),
                               confirmed_base_units = balance_cache.confirmed_base_units + excluded.confirmed_base_units,
                               updated_at = excluded.updated_at",
                        )
                        .bind(miner_id)
                        .bind(entry.amount_base_units.to_string())
                        .execute(&mut *tx)
                        .await?;
                    }
                    (LedgerKind::RewardOrphanReversal, Some(miner_id))
                        if entry.amount_base_units < 0 =>
                    {
                        let amount_abs = (-entry.amount_base_units).to_string();
                        let was_matured: bool = sqlx::query_scalar(
                            "select exists (
                               select 1 from ledger_entries
                               where miner_id = $1
                                 and kind = 'reward_mature'
                                 and ref_type = $2
                                 and ref_id = $3
                             )",
                        )
                        .bind(miner_id)
                        .bind(&entry.ref_type)
                        .bind(&entry.ref_id)
                        .fetch_one(&mut *tx)
                        .await?;

                        sqlx::query(
                            "insert into balance_cache(miner_id, updated_at)
                             values ($1, now())
                             on conflict(miner_id) do nothing",
                        )
                        .bind(miner_id)
                        .execute(&mut *tx)
                        .await?;

                        if was_matured {
                            sqlx::query(
                                "update balance_cache set
                                   confirmed_base_units = greatest(confirmed_base_units - $2::numeric, 0),
                                   updated_at = now()
                                 where miner_id = $1",
                            )
                            .bind(miner_id)
                            .bind(amount_abs)
                            .execute(&mut *tx)
                            .await?;
                        } else {
                            sqlx::query(
                                "update balance_cache set
                                   immature_base_units = greatest(immature_base_units - $2::numeric, 0),
                                   updated_at = now()
                                 where miner_id = $1",
                            )
                            .bind(miner_id)
                            .bind(amount_abs)
                            .execute(&mut *tx)
                            .await?;
                        }
                    }
                    (LedgerKind::PayoutLock, Some(miner_id)) if entry.amount_base_units < 0 => {
                        let amount_abs = (-entry.amount_base_units).to_string();
                        sqlx::query(
                            "insert into balance_cache(miner_id, updated_at)
                             values ($1, now())
                             on conflict(miner_id) do nothing",
                        )
                        .bind(miner_id)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query(
                            "update balance_cache set
                               confirmed_base_units = greatest(confirmed_base_units - $2::numeric, 0),
                               locked_base_units = locked_base_units + $2::numeric,
                               updated_at = now()
                             where miner_id = $1",
                        )
                        .bind(miner_id)
                        .bind(amount_abs)
                        .execute(&mut *tx)
                        .await?;
                    }
                    (LedgerKind::PayoutSent, Some(miner_id)) if entry.amount_base_units < 0 => {
                        let amount_abs = (-entry.amount_base_units).to_string();
                        sqlx::query(
                            "update balance_cache set
                               locked_base_units = greatest(locked_base_units - $2::numeric, 0),
                               paid_base_units = paid_base_units + $2::numeric,
                               updated_at = now()
                             where miner_id = $1",
                        )
                        .bind(miner_id)
                        .bind(amount_abs)
                        .execute(&mut *tx)
                        .await?;
                    }
                    (LedgerKind::PayoutFailedUnlock, Some(miner_id))
                        if entry.amount_base_units > 0 =>
                    {
                        sqlx::query(
                            "update balance_cache set
                               locked_base_units = greatest(locked_base_units - $2::numeric, 0),
                               confirmed_base_units = confirmed_base_units + $2::numeric,
                               updated_at = now()
                             where miner_id = $1",
                        )
                        .bind(miner_id)
                        .bind(entry.amount_base_units.to_string())
                        .execute(&mut *tx)
                        .await?;
                    }
                    _ => {}
                }
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn list_ledger_entries(&self) -> Result<Vec<LedgerEntry>> {
        let rows = sqlx::query_as::<_, LedgerEntryRow>(
            "select miners.address as miner,
                    ledger_entries.amount_base_units::text as amount_base_units,
                    ledger_entries.kind,
                    ledger_entries.ref_type,
                    ledger_entries.ref_id
             from ledger_entries
             left join miners on miners.id = ledger_entries.miner_id
             order by ledger_entries.id asc",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn set_balance(&self, balance: MinerBalance) -> Result<()> {
        let miner_id = self.miner_id(&balance.miner).await?;
        sqlx::query(
            "insert into balance_cache(miner_id, confirmed_base_units, updated_at)
             values ($1, $2::numeric, now())
             on conflict(miner_id) do update set
               confirmed_base_units = excluded.confirmed_base_units,
               updated_at = excluded.updated_at",
        )
        .bind(miner_id)
        .bind(balance.confirmed_base_units.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_balances(&self) -> Result<Vec<MinerBalance>> {
        let rows = sqlx::query_as::<_, BalanceRow>(
            "select miners.address as miner,
                    miners.address as address,
                    balance_cache.confirmed_base_units::text as confirmed_base_units
             from balance_cache
             join miners on miners.id = balance_cache.miner_id
             order by miners.address asc",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn create_payout_batch(&self, draft: PayoutBatchDraft) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let insert = sqlx::query(
            "insert into payout_batches(id, status, total_base_units, recipient_count)
             values ($1, 'dry_run_ok', $2::numeric, $3)
             on conflict(id) do nothing",
        )
        .bind(&draft.batch_id)
        .bind(draft.total_base_units.to_string())
        .bind(i32::try_from(draft.recipients.len())?)
        .execute(&mut *tx)
        .await?;
        if insert.rows_affected() == 0 {
            return Err(RepositoryError::DuplicatePayoutBatch(draft.batch_id));
        }

        for recipient in &draft.recipients {
            let miner_id = Self::miner_id_in_tx(&mut tx, &recipient.miner).await?;
            sqlx::query(
                "insert into payout_recipients(batch_id, miner_id, address, amount_base_units)
                 values ($1, $2, $3, $4::numeric)",
            )
            .bind(&draft.batch_id)
            .bind(miner_id)
            .bind(&recipient.address)
            .bind(recipient.amount_base_units.to_string())
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    async fn list_payout_batches(&self) -> Result<Vec<PayoutBatchDraft>> {
        let batch_rows = sqlx::query_as::<_, PayoutBatchRow>(
            "select id as batch_id, total_base_units::text as total_base_units
             from payout_batches
             order by created_at asc, id asc",
        )
        .fetch_all(&self.pool)
        .await?;
        let recipient_rows = sqlx::query_as::<_, PayoutRecipientRow>(
            "select payout_recipients.batch_id,
                    miners.address as miner,
                    payout_recipients.address,
                    payout_recipients.amount_base_units::text as amount_base_units
             from payout_recipients
             join miners on miners.id = payout_recipients.miner_id
             order by payout_recipients.batch_id asc, miners.address asc",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut recipients_by_batch = BTreeMap::<String, Vec<PayoutRecipient>>::new();
        for row in recipient_rows {
            recipients_by_batch
                .entry(row.batch_id.clone())
                .or_default()
                .push(row.try_into()?);
        }

        batch_rows
            .into_iter()
            .map(|row| {
                let recipients = recipients_by_batch
                    .remove(&row.batch_id)
                    .unwrap_or_default();
                Ok(PayoutBatchDraft {
                    batch_id: row.batch_id,
                    total_base_units: row.total_base_units.parse()?,
                    recipients,
                    lock_entries: vec![],
                })
            })
            .collect()
    }
}

#[async_trait]
impl MiningRepository for PgRepository {
    async fn open_session(&self, session: &SessionRecord) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let miner_id = Self::miner_id_in_tx(&mut tx, &session.miner).await?;
        let worker_id = Self::worker_id_in_tx(&mut tx, miner_id, &session.worker_name).await?;
        sqlx::query(
            "insert into sessions(
               id, worker_id, remote_addr, remote_port, user_agent, extranonce1,
               server_session_id, server_release, server_instance, assigned_difficulty
             )
             values ($1::uuid, $2, $3::inet, $4, $5, $6, $7, $8, $9, $10::numeric)
             on conflict(id) do nothing",
        )
        .bind(&session.id)
        .bind(worker_id)
        .bind(&session.remote_addr)
        .bind(i32::from(session.remote_port))
        .bind(&session.user_agent)
        .bind(&session.extranonce1)
        .bind(i64::try_from(session.server_session_id)?)
        .bind(&session.server_release)
        .bind(&session.server_instance)
        .bind(session.assigned_difficulty.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn close_session(&self, session_id: &str) -> Result<()> {
        sqlx::query(
            "update sessions
             set ended_at = coalesce(ended_at, now())
             where id = $1::uuid",
        )
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn close_stale_sessions(&self, server_instance: &str) -> Result<u64> {
        Ok(sqlx::query(
            "update sessions
             set ended_at = now()
             where server_instance = $1 and ended_at is null",
        )
        .bind(server_instance)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    async fn update_session_difficulty(&self, session_id: &str, difficulty: f64) -> Result<()> {
        sqlx::query(
            "update sessions
             set assigned_difficulty = $2::numeric, difficulty_updated_at = now()
             where id = $1::uuid",
        )
        .bind(session_id)
        .bind(difficulty.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn upsert_job(&self, job: &JobRecord) -> Result<()> {
        sqlx::query(
            "insert into jobs(
               id, prev_hash, version_hex, nbits_hex, ntime_hex,
               network_target, share_target, coinb1_hex, coinb2_hex,
               merkle_branches_json, clean_jobs, job_reason
             )
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::jsonb, $11, $12)
             on conflict(id) do update set
               prev_hash = excluded.prev_hash,
               version_hex = excluded.version_hex,
               nbits_hex = excluded.nbits_hex,
               ntime_hex = excluded.ntime_hex,
               network_target = excluded.network_target,
               share_target = excluded.share_target,
               coinb1_hex = excluded.coinb1_hex,
               coinb2_hex = excluded.coinb2_hex,
               merkle_branches_json = excluded.merkle_branches_json,
               clean_jobs = excluded.clean_jobs,
               job_reason = excluded.job_reason",
        )
        .bind(&job.job_id)
        .bind(&job.prev_hash_be_hex)
        .bind(&job.version_hex)
        .bind(&job.nbits_hex)
        .bind(&job.ntime_hex)
        .bind(job.network_target.to_vec())
        .bind(job.share_target.to_vec())
        .bind(&job.coinb1_hex)
        .bind(&job.coinb2_hex)
        .bind(serde_json::to_string(&job.merkle_branches_hex)?)
        .bind(job.clean_jobs)
        .bind(&job.job_reason)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn insert_share(&self, share: &ShareRecord) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let miner_id = Self::miner_id_in_tx(&mut tx, &share.miner).await?;
        let worker_id = Self::worker_id_in_tx(&mut tx, miner_id, &share.worker_name).await?;
        let inserted = sqlx::query(
            "insert into shares(
               worker_id, session_id, job_id, difficulty, hash,
               extranonce2, ntime, nonce, is_block_candidate
             )
             values ($1, $2::uuid, $3, $4::numeric, $5, $6, $7, $8, $9)
             on conflict(job_id, worker_id, extranonce2, ntime, nonce) do nothing",
        )
        .bind(worker_id)
        .bind(&share.session_id)
        .bind(&share.job_id)
        .bind(share.difficulty.to_string())
        .bind(share.hash.to_vec())
        .bind(&share.extranonce2_hex)
        .bind(&share.ntime_hex)
        .bind(&share.nonce_hex)
        .bind(share.is_block_candidate)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        tx.commit().await?;
        Ok(inserted)
    }

    async fn insert_share_event(&self, event: &ShareEventRecord) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let miner_id = Self::miner_id_in_tx(&mut tx, &event.miner).await?;
        let worker_id = Self::worker_id_in_tx(&mut tx, miner_id, &event.worker_name).await?;
        sqlx::query(
            "insert into share_events(
               miner_id, worker_id, session_id, job_id, kind, reason
             )
             values ($1, $2, $3::uuid, $4, $5, $6)",
        )
        .bind(miner_id)
        .bind(worker_id)
        .bind(&event.session_id)
        .bind(&event.job_id)
        .bind(&event.kind)
        .bind(&event.reason)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn record_block_candidate(&self, block: &BlockCandidateRecord) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let miner_id = Self::miner_id_in_tx(&mut tx, &block.miner).await?;
        let worker_id = Self::worker_id_in_tx(&mut tx, miner_id, &block.worker_name).await?;
        let initial_status = block_candidate_initial_status(block);
        let inserted = sqlx::query(
            "insert into blocks(
               hash, job_id, finder_worker_id, reward_base_units, status,
               effort_pct, candidate_payload_json, submit_response_json, submitted_at, orphaned_at
             )
             values (
               $1, $2, $3, $4::numeric, $5, $6::numeric, $7::jsonb, $8::jsonb, now(),
               case when $5 = 'orphaned' then now() else null end
             )
             on conflict(hash) do nothing",
        )
        .bind(&block.hash_hex)
        .bind(&block.job_id)
        .bind(worker_id)
        .bind(block.reward_base_units.to_string())
        .bind(initial_status)
        .bind(block.effort_pct.to_string())
        .bind(block.candidate_payload_json.to_string())
        .bind(block.submit_response_json.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        tx.commit().await?;
        Ok(inserted)
    }
}

#[async_trait]
impl BlockRepository for PgRepository {
    async fn list_blocks_to_reconcile(&self, limit: i64) -> Result<Vec<BlockRecord>> {
        let rows = sqlx::query_as::<_, BlockRow>(
            "select hash as hash_hex,
                    coalesce(job_id, '') as job_id,
                    status,
                    height,
                    confirmations,
                    reward_base_units::text as reward_base_units,
                    coalesce(effort_pct, 0)::text as effort_pct
             from blocks
             where status in ('submitted', 'submitted_secondary', 'submitted_degraded', 'relay_failed', 'seen_on_chain', 'immature')
             order by submitted_at asc nulls last, id asc
             limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn update_block_status(&self, update: &BlockStatusUpdate) -> Result<bool> {
        let result = sqlx::query(
            "update blocks set
               status = $2,
               height = $3,
               confirmations = $4,
               reward_base_units = $5::numeric,
               seen_at = case
                 when $2 in ('seen_on_chain', 'immature', 'confirmed') and seen_at is null
                 then now() else seen_at end,
               confirmed_at = case when $2 = 'confirmed' then coalesce(confirmed_at, now()) else confirmed_at end,
               orphaned_at = case when $2 = 'orphaned' then coalesce(orphaned_at, now()) else orphaned_at end
             where hash = $1",
        )
        .bind(&update.hash_hex)
        .bind(&update.status)
        .bind(update.height.map(|height| height as i64))
        .bind(i32::try_from(update.confirmations)?)
        .bind(update.reward_base_units.to_string())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}

#[async_trait]
impl RewardRepository for PgRepository {
    async fn list_confirmed_unsettled_blocks(&self, limit: i64) -> Result<Vec<RewardBlock>> {
        let rows = sqlx::query_as::<_, RewardBlockRow>(
            "select hash as hash_hex,
                    job_id,
                    reward_base_units::text as reward_base_units
             from blocks
             where status = 'confirmed'
               and reward_base_units > 0
               and job_id is not null
               and not exists (
                 select 1 from ledger_entries
                 where ledger_entries.ref_type = 'block'
                   and ledger_entries.ref_id = blocks.hash
                   and ledger_entries.kind in ('reward_immature', 'pool_fee')
               )
             order by confirmed_at asc nulls last, id asc
             limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn share_weights_for_job(&self, job_id: &str) -> Result<Vec<ShareWeight>> {
        let rows = sqlx::query_as::<_, ShareWeightRow>(
            "select miners.address as miner,
                    sum(shares.difficulty)::text as difficulty
             from shares
             join workers on workers.id = shares.worker_id
             join miners on miners.id = workers.miner_id
             where shares.job_id = $1
             group by miners.address
             order by miners.address asc",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn list_mature_reward_entries(
        &self,
        confirm_depth: u64,
        limit: i64,
    ) -> Result<Vec<LedgerEntry>> {
        let rows = sqlx::query_as::<_, MatureRewardRow>(
            "select miners.address as miner,
                    immature.amount_base_units::text as amount_base_units,
                    immature.ref_id
             from ledger_entries immature
             join miners on miners.id = immature.miner_id
             join blocks on blocks.hash = immature.ref_id
             where immature.kind = 'reward_immature'
               and immature.ref_type = 'block'
               and blocks.status = 'confirmed'
               and blocks.confirmations >= $1
               and not exists (
                 select 1 from ledger_entries mature
                 where mature.kind = 'reward_mature'
                   and mature.ref_type = immature.ref_type
                   and mature.ref_id = immature.ref_id
                   and mature.miner_id = immature.miner_id
               )
             order by blocks.confirmed_at asc nulls last, immature.id asc
             limit $2",
        )
        .bind(i32::try_from(confirm_depth)?)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn list_orphan_reversal_entries(&self, limit: i64) -> Result<Vec<LedgerEntry>> {
        let rows = sqlx::query_as::<_, OrphanRewardRow>(
            "select miners.address as miner,
                    ('-' || immature.amount_base_units::text) as amount_base_units,
                    immature.ref_id
             from ledger_entries immature
             join miners on miners.id = immature.miner_id
             join blocks on blocks.hash = immature.ref_id
             where immature.kind = 'reward_immature'
               and immature.ref_type = 'block'
               and blocks.status = 'orphaned'
               and not exists (
                 select 1 from ledger_entries reversal
                 where reversal.kind = 'reward_orphan_reversal'
                   and reversal.ref_type = immature.ref_type
                   and reversal.ref_id = immature.ref_id
                   and reversal.miner_id = immature.miner_id
               )
             order by blocks.orphaned_at asc nulls last, immature.id asc
             limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

#[async_trait]
impl DashboardRepository for PgRepository {
    async fn dashboard_pool_stats(&self) -> Result<DashboardPoolStats> {
        let row = sqlx::query_as::<_, DashboardPoolStatsRow>(
            "select
               coalesce((
                 select count(*)
                 from workers
                 where last_seen_at >= now() - interval '5 minutes'
               ), 0)::text as workers_online,
               coalesce((
                 select count(distinct miner_id)
                 from workers
                 where last_seen_at >= now() - interval '5 minutes'
               ), 0)::text as miners_online,
               count(*)::text as total_blocks,
               count(*) filter (where status = 'confirmed')::text as canonical_blocks,
               count(*) filter (where status in ('seen_on_chain', 'immature'))::text as immature_blocks,
               count(*) filter (where status = 'orphaned')::text as orphaned_blocks,
               coalesce((
                 select sum(amount_base_units)
                 from ledger_entries
                 where kind = 'pool_fee'
               ), 0)::text as fee_revenue_base_units,
               coalesce((
                 select extract(epoch from max(created_at))::bigint
                 from payout_batches
               ), 0)::text as latest_payout_created_ts,
               coalesce((
                 select count(*)
                 from jobs
               ), 0)::text as jobs_created,
               coalesce((
                 select extract(epoch from max(created_at))::bigint
                 from jobs
               ), 0)::text as latest_job_created_ts,
               coalesce((
                 select count(*) filter (where status = 'needs_approval')
                 from payout_batches
               ), 0)::text as payout_batches_needs_approval,
               coalesce((
                 select count(*) filter (where status = 'created')
                 from payout_batches
               ), 0)::text as payout_batches_created,
               coalesce((
                 select count(*) filter (where status = 'signed')
                 from payout_batches
               ), 0)::text as payout_batches_signed,
               coalesce((
                 select count(*) filter (where status = 'submitted')
                 from payout_batches
               ), 0)::text as payout_batches_submitted,
               coalesce((
                 select count(*) filter (where status = 'confirmed')
                 from payout_batches
               ), 0)::text as payout_batches_confirmed,
               coalesce((
                 select count(*) filter (where status = 'failed')
                 from payout_batches
               ), 0)::text as payout_batches_failed,
               coalesce((
                 select count(*) filter (where status = 'cancelled')
                 from payout_batches
               ), 0)::text as payout_batches_cancelled,
               coalesce((
                 select sum(total_base_units)
                 from payout_batches
                 where status in ('submitted', 'confirmed')
               ), 0)::text as payout_amount_base_units_total,
               coalesce(
                 avg(effort_pct) filter (
                   where effort_pct > 0
                     and coalesce(submitted_at, seen_at, confirmed_at, orphaned_at)
                       >= now() - interval '24 hours'
                 ),
                 0
               )::text as avg_block_effort_pct_24h,
               coalesce(
                 avg(effort_pct) filter (
                   where effort_pct > 0
                     and coalesce(submitted_at, seen_at, confirmed_at, orphaned_at)
                       >= now() - interval '7 days'
                 ),
                 0
               )::text as avg_block_effort_pct_7d,
               coalesce(avg(effort_pct) filter (where effort_pct > 0), 0)::text
                 as avg_block_effort_pct_lifetime
             from blocks",
        )
        .fetch_one(&self.pool)
        .await?;
        row.try_into()
    }

    async fn dashboard_miner_stats(&self, address: &str) -> Result<Option<DashboardMinerStats>> {
        let row = sqlx::query_as::<_, DashboardMinerStatsRow>(
            "select
               miners.address as miner,
               coalesce(balance_cache.immature_base_units, 0)::text as immature_base_units,
               coalesce(balance_cache.confirmed_base_units, 0)::text as confirmed_base_units,
               coalesce(balance_cache.locked_base_units, 0)::text as locked_base_units,
               coalesce(balance_cache.paid_base_units, 0)::text as paid_base_units,
               coalesce(worker_stats.workers_total, 0)::text as workers_total,
               coalesce(share_stats.shares_accepted, 0)::text as shares_accepted,
               coalesce(event_stats.shares_rejected, 0)::text as shares_rejected,
               coalesce(event_stats.shares_stale, 0)::text as shares_stale,
               coalesce(block_stats.blocks_found, 0)::text as blocks_found,
               coalesce(share_stats.last_difficulty, 0)::text as last_difficulty,
               coalesce(worker_stats.last_seen_ts, 0)::text as last_seen_ts
             from miners
             left join balance_cache on balance_cache.miner_id = miners.id
             left join (
               select
                 miner_id,
                 count(*) as workers_total,
                 extract(epoch from max(last_seen_at))::bigint as last_seen_ts
               from workers
               group by miner_id
             ) worker_stats on worker_stats.miner_id = miners.id
             left join (
               select
                 workers.miner_id,
                 count(shares.id) as shares_accepted,
                 max(shares.difficulty) as last_difficulty
               from shares
               join workers on workers.id = shares.worker_id
               group by workers.miner_id
             ) share_stats on share_stats.miner_id = miners.id
             left join (
               select
                 workers.miner_id,
                 count(*) filter (where share_events.kind = 'rejected') as shares_rejected,
                 count(*) filter (where share_events.kind = 'stale') as shares_stale
               from share_events
               join workers on workers.id = share_events.worker_id
               group by workers.miner_id
             ) event_stats on event_stats.miner_id = miners.id
             left join (
               select workers.miner_id, count(distinct blocks.id) as blocks_found
               from blocks
               join workers on workers.id = blocks.finder_worker_id
               group by workers.miner_id
             ) block_stats on block_stats.miner_id = miners.id
             where miners.address = $1
             limit 1",
        )
        .bind(address)
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn dashboard_workers_for_miner(
        &self,
        address: &str,
    ) -> Result<Vec<DashboardWorkerStats>> {
        let rows = sqlx::query_as::<_, DashboardWorkerStatsRow>(
            "select
               workers.name,
               coalesce(share_stats.shares_accepted, 0)::text as shares_accepted,
               coalesce(event_stats.shares_rejected, 0)::text as shares_rejected,
               coalesce(event_stats.shares_stale, 0)::text as shares_stale,
               coalesce(block_stats.blocks_found, 0)::text as blocks_found,
               coalesce(share_stats.last_difficulty, 0)::text as last_difficulty,
               coalesce(extract(epoch from workers.last_seen_at)::bigint, 0)::text as last_seen_ts,
               workers.created_at::text as connected_at,
               workers.last_seen_at::text as last_seen_at
             from workers
             join miners on miners.id = workers.miner_id
             left join (
               select
                 worker_id,
                 count(*) as shares_accepted,
                 max(difficulty) as last_difficulty
               from shares
               group by worker_id
             ) share_stats on share_stats.worker_id = workers.id
             left join (
               select
                 worker_id,
                 count(*) filter (where kind = 'rejected') as shares_rejected,
                 count(*) filter (where kind = 'stale') as shares_stale
               from share_events
               group by worker_id
             ) event_stats on event_stats.worker_id = workers.id
             left join (
               select finder_worker_id as worker_id, count(distinct id) as blocks_found
               from blocks
               group by finder_worker_id
             ) block_stats on block_stats.worker_id = workers.id
             where miners.address = $1
             order by workers.last_seen_at desc nulls last, workers.name asc",
        )
        .bind(address)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn dashboard_recent_blocks(&self, limit: i64) -> Result<Vec<DashboardBlock>> {
        let rows = sqlx::query_as::<_, DashboardBlockRow>(
            "select
               coalesce(blocks.height, 0)::text as height,
               blocks.hash,
               coalesce(miners.address, '') as finder,
               coalesce(workers.name, '') as worker,
               blocks.reward_base_units::text as reward_base_units,
               blocks.status,
               blocks.confirmations::text as confirmations,
               coalesce(blocks.effort_pct, 0)::text as effort_pct,
               coalesce(blocks.submitted_at, blocks.seen_at, blocks.confirmed_at, blocks.orphaned_at)::text as found_at,
               blocks.confirmed_at::text as confirmed_at
             from blocks
             left join workers on workers.id = blocks.finder_worker_id
             left join miners on miners.id = workers.miner_id
             order by coalesce(blocks.height, 0) desc, blocks.id desc
             limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn dashboard_recent_payments(&self, limit: i64) -> Result<Vec<DashboardPayment>> {
        self.dashboard_payments(None, limit).await
    }

    async fn dashboard_recent_payments_for_miner(
        &self,
        address: &str,
        limit: i64,
    ) -> Result<Vec<DashboardPayment>> {
        self.dashboard_payments(Some(address), limit).await
    }

    async fn dashboard_history(
        &self,
        range_secs: u64,
        bucket_secs: u64,
    ) -> Result<Vec<DashboardHistorySample>> {
        let range_secs = i64::try_from(range_secs.max(1))?;
        let bucket_secs = i64::try_from(bucket_secs.max(1))?;
        let rows = sqlx::query_as::<_, DashboardHistorySampleRow>(
            "with accepted as (
               select
                 ((extract(epoch from shares.created_at)::bigint / $2) * $2)::text as ts,
                 count(*)::text as shares_accepted,
                 coalesce(sum(shares.difficulty), 0)::text as difficulty_sum,
                 count(distinct shares.worker_id)::text as workers
               from shares
               where shares.created_at >= now() - ($1::text || ' seconds')::interval
               group by 1
             ),
             events as (
               select
                 ((extract(epoch from share_events.created_at)::bigint / $2) * $2)::text as ts,
                 count(*) filter (where share_events.kind = 'rejected')::text as shares_rejected,
                 count(*) filter (where share_events.kind = 'stale')::text as shares_stale
               from share_events
               where share_events.created_at >= now() - ($1::text || ' seconds')::interval
               group by 1
             )
             select
               coalesce(accepted.ts, events.ts) as ts,
               (coalesce(accepted.difficulty_sum, '0')::numeric * $3::numeric / $2::numeric)::text
                 as pool_hs,
               0::text as net_hs,
               coalesce(accepted.workers, '0') as workers,
               coalesce(accepted.shares_accepted, '0') as shares_accepted,
               coalesce(events.shares_rejected, '0') as shares_rejected,
               coalesce(events.shares_stale, '0') as shares_stale
             from accepted
             full join events using (ts)
             order by coalesce(accepted.ts, events.ts) asc",
        )
        .bind(range_secs)
        .bind(bucket_secs)
        .bind(DIFFICULTY_ONE_HASHES.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

impl PgRepository {
    async fn dashboard_payments(
        &self,
        address: Option<&str>,
        limit: i64,
    ) -> Result<Vec<DashboardPayment>> {
        let rows = sqlx::query_as::<_, DashboardPaymentRow>(
            "select
               payout_batches.id as batch_id,
               payout_recipients.address,
               payout_recipients.amount_base_units::text as amount_base_units,
               coalesce(payout_batches.txid, '') as txid,
               payout_batches.status,
               payout_batches.created_at::text as created_at,
               payout_batches.confirmed_at::text as confirmed_at
             from payout_recipients
             join payout_batches on payout_batches.id = payout_recipients.batch_id
             join miners on miners.id = payout_recipients.miner_id
             where ($1::text is null or miners.address = $1)
             order by payout_batches.created_at desc, payout_batches.id desc
             limit $2",
        )
        .bind(address)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

#[async_trait]
impl MonitoringRepository for PgRepository {
    async fn insert_node_sample(&self, sample: &NodeSampleRecord) -> Result<()> {
        sqlx::query(
            "insert into node_samples(
               node_name, height, chainwork, peers, mempool_size, rpc_ms, ok
             )
             values ($1, $2, $3, $4, $5, $6::numeric, $7)",
        )
        .bind(&sample.node_name)
        .bind(sample.height.map(|height| height as i64))
        .bind(&sample.chainwork)
        .bind(sample.peers.map(|peers| peers as i32))
        .bind(sample.mempool_size.map(|size| size as i32))
        .bind(sample.rpc_ms.map(|ms| ms.to_string()))
        .bind(sample.ok)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn latest_node_samples(&self, limit: i64) -> Result<Vec<NodeSampleRecord>> {
        let rows = sqlx::query_as::<_, NodeSampleRow>(
            "select distinct on (node_name)
                    node_name,
                    height,
                    chainwork,
                    peers,
                    mempool_size,
                    rpc_ms::text as rpc_ms,
                    ok,
                    sampled_at::text as sampled_at
             from node_samples
             order by node_name asc, sampled_at desc
             limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn list_stuck_payout_batches(
        &self,
        older_than_minutes: i64,
        limit: i64,
    ) -> Result<Vec<PayoutBatchRecord>> {
        let rows = sqlx::query_as::<_, PayoutBatchRecordRow>(
            "select id as batch_id,
                    status,
                    total_base_units::text as total_base_units,
                    txid,
                    raw_tx_hash
             from payout_batches
             where status in ('created', 'signed', 'submitted')
               and coalesce(submitted_at, signed_at, created_at)
                 < now() - ($1::text || ' minutes')::interval
             order by coalesce(submitted_at, signed_at, created_at) asc, id asc
             limit $2",
        )
        .bind(older_than_minutes.max(1).to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        self.hydrate_payout_batch_records(rows).await
    }

    async fn list_offline_workers(
        &self,
        offline_minutes: i64,
        limit: i64,
    ) -> Result<Vec<OfflineWorkerRecord>> {
        let rows = sqlx::query_as::<_, OfflineWorkerRow>(
            "select
               miners.address as miner,
               workers.name as worker_name,
               coalesce(extract(epoch from workers.last_seen_at)::bigint, 0)::text as last_seen_ts,
               workers.last_seen_at::text as last_seen_at
             from workers
             join miners on miners.id = workers.miner_id
             where workers.last_seen_at is not null
               and workers.last_seen_at < now() - ($1::text || ' minutes')::interval
             order by workers.last_seen_at asc, workers.id asc
             limit $2",
        )
        .bind(offline_minutes.max(1).to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn accepted_share_gap(
        &self,
        quiet_minutes: i64,
    ) -> Result<Option<AcceptedShareGapRecord>> {
        let row = sqlx::query_as::<_, AcceptedShareGapRow>(
            "select
               coalesce(extract(epoch from max(created_at))::bigint, 0)::text as latest_share_ts,
               max(created_at)::text as latest_share_at
             from shares
             having max(created_at) is not null
                and max(created_at) < now() - ($1::text || ' minutes')::interval",
        )
        .bind(quiet_minutes.max(1).to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            AcceptedShareGapRecord::try_from(row).map(|mut gap| {
                gap.quiet_minutes = quiet_minutes.max(1);
                gap
            })
        })
        .transpose()
    }

    async fn latest_job(&self) -> Result<Option<LatestJobRecord>> {
        let row = sqlx::query_as::<_, LatestJobRow>(
            "select
               id as job_id,
               prev_hash,
               extract(epoch from created_at)::bigint::text as created_ts,
               created_at::text as created_at,
               greatest(0, extract(epoch from now() - created_at)::bigint)::text as age_seconds
             from jobs
             order by created_at desc, id desc
             limit 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn list_block_submission_alerts(
        &self,
        stuck_minutes: i64,
        limit: i64,
    ) -> Result<Vec<BlockSubmissionAlertRecord>> {
        let rows = sqlx::query_as::<_, BlockSubmissionAlertRow>(
            "select
               hash as hash_hex,
               coalesce(job_id, '') as job_id,
               status,
               coalesce(extract(epoch from submitted_at)::bigint, 0)::text as submitted_ts,
               submitted_at::text as submitted_at,
               greatest(0, extract(epoch from now() - submitted_at)::bigint)::text as age_seconds,
               case
                 when submit_response_json ? 'ok' then (submit_response_json->>'ok')::boolean
                 else null
               end as submit_ok,
               case
                 when status = 'relay_failed' then 'local_canonical_relay_failed'
                 when status = 'submitted_secondary' then 'authority_failed_secondary_accepted'
                 when status = 'submitted_degraded' then 'redundant_relay_failed'
                 when submit_response_json ? 'ok' and (submit_response_json->>'ok')::boolean = false
                 then 'submit_response_not_ok'
                 else 'submitted_too_old'
               end as reason
             from blocks
             where status in ('submitted', 'submitted_secondary', 'submitted_degraded', 'relay_failed')
               and (
                 status in ('submitted_secondary', 'submitted_degraded', 'relay_failed')
                 or
                 (submit_response_json ? 'ok' and (submit_response_json->>'ok')::boolean = false)
                 or submitted_at < now() - ($1::text || ' minutes')::interval
               )
             order by submitted_at asc nulls last, id asc
             limit $2",
        )
        .bind(stuck_minutes.max(1).to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn list_share_quality_alerts(
        &self,
        window_minutes: i64,
        min_total: u64,
        max_reject_rate: f64,
        max_stale_rate: f64,
        limit: i64,
    ) -> Result<Vec<ShareQualityAlertRecord>> {
        let rows = sqlx::query_as::<_, ShareQualityRow>(
            "with accepted as (
               select worker_id, count(*)::text as accepted_count
               from shares
               where created_at >= now() - ($1::text || ' minutes')::interval
               group by worker_id
             ),
             events as (
               select worker_id,
                      (count(*) filter (where kind = 'rejected'))::text as rejected_count,
                      (count(*) filter (where kind = 'stale'))::text as stale_count
               from share_events
               where created_at >= now() - ($1::text || ' minutes')::interval
               group by worker_id
             )
             select miners.address as miner,
                    workers.name as worker_name,
                    coalesce(accepted.accepted_count, '0') as accepted_count,
                    coalesce(events.rejected_count, '0') as rejected_count,
                    coalesce(events.stale_count, '0') as stale_count
             from workers
             join miners on miners.id = workers.miner_id
             left join accepted on accepted.worker_id = workers.id
             left join events on events.worker_id = workers.id
             where coalesce(accepted.accepted_count, '0')::bigint
                 + coalesce(events.rejected_count, '0')::bigint
                 + coalesce(events.stale_count, '0')::bigint >= $2
             order by (
                 coalesce(events.rejected_count, '0')::numeric
                 + coalesce(events.stale_count, '0')::numeric
               ) / greatest(
                 coalesce(accepted.accepted_count, '0')::numeric
                 + coalesce(events.rejected_count, '0')::numeric
                 + coalesce(events.stale_count, '0')::numeric,
                 1
               ) desc,
               workers.last_seen_at desc nulls last
             limit $3",
        )
        .bind(window_minutes.max(1).to_string())
        .bind(i64::try_from(min_total)?)
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await?;
        let mut alerts = Vec::new();
        for row in rows {
            if let Some(alert) = share_quality_alert_from_counts(
                row,
                window_minutes.max(1),
                max_reject_rate,
                max_stale_rate,
            )? {
                alerts.push(alert);
            }
        }
        Ok(alerts)
    }

    async fn upsert_alert(&self, alert: &AlertEvent) -> Result<()> {
        sqlx::query(
            "insert into alert_events(
               fingerprint, severity, status, kind, subject, message, details_json
             )
             values ($1, $2, 'active', $3, $4, $5, $6::jsonb)
             on conflict(fingerprint) do update set
               severity = excluded.severity,
               status = 'active',
               kind = excluded.kind,
               subject = excluded.subject,
               message = excluded.message,
               last_seen_at = now(),
               resolved_at = null,
               details_json = excluded.details_json",
        )
        .bind(&alert.fingerprint)
        .bind(&alert.severity)
        .bind(&alert.kind)
        .bind(&alert.subject)
        .bind(&alert.message)
        .bind(serde_json::to_string(&alert.details)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn resolve_alert(&self, fingerprint: &str) -> Result<bool> {
        let result = sqlx::query(
            "update alert_events set
               status = 'resolved',
               resolved_at = coalesce(resolved_at, now()),
               last_seen_at = now()
             where fingerprint = $1 and status = 'active'",
        )
        .bind(fingerprint)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn list_alerts(&self, status: Option<&str>, limit: i64) -> Result<Vec<AlertEvent>> {
        let rows = sqlx::query_as::<_, AlertEventRow>(
            "select fingerprint,
                    severity,
                    status,
                    kind,
                    subject,
                    message,
                    first_seen_at::text as first_seen_at,
                    last_seen_at::text as last_seen_at,
                    resolved_at::text as resolved_at,
                    details_json
             from alert_events
             where ($1::text is null or status = $1)
             order by last_seen_at desc, id desc
             limit $2",
        )
        .bind(status)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

#[async_trait]
impl ControlRepository for PgRepository {
    async fn payouts_enabled(&self) -> Result<bool> {
        let value: Option<String> =
            sqlx::query_scalar("select value from pool_settings where key = 'payouts_enabled'")
                .fetch_optional(&self.pool)
                .await?;
        Ok(value
            .as_deref()
            .map(|value| matches!(value, "true" | "1" | "yes" | "on"))
            .unwrap_or(false))
    }

    async fn set_payouts_enabled(&self, enabled: bool) -> Result<()> {
        sqlx::query(
            "insert into pool_settings(key, value, updated_at)
             values ('payouts_enabled', $1, now())
             on conflict(key) do update set
               value = excluded.value,
               updated_at = excluded.updated_at",
        )
        .bind(if enabled { "true" } else { "false" })
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl PayoutRepository for PgRepository {
    async fn list_payable_balances(
        &self,
        minimum_payout_base_units: u128,
        limit: i64,
    ) -> Result<Vec<MinerBalance>> {
        let rows = sqlx::query_as::<_, BalanceRow>(
            "select miners.address as miner,
                    miners.address as address,
                    balance_cache.confirmed_base_units::text as confirmed_base_units
             from balance_cache
             join miners on miners.id = balance_cache.miner_id
             where balance_cache.confirmed_base_units >= $1::numeric
             order by balance_cache.confirmed_base_units desc, miners.address asc
             limit $2",
        )
        .bind(minimum_payout_base_units.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn create_locked_payout_batch(&self, draft: PayoutBatchDraft) -> Result<bool> {
        self.create_locked_payout_batch_with_status(draft, "created")
            .await
    }

    async fn create_locked_payout_batch_with_status(
        &self,
        draft: PayoutBatchDraft,
        status: &str,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let insert = sqlx::query(
            "insert into payout_batches(id, status, total_base_units, recipient_count)
             values ($1, $4, $2::numeric, $3)
             on conflict(id) do nothing",
        )
        .bind(&draft.batch_id)
        .bind(draft.total_base_units.to_string())
        .bind(i32::try_from(draft.recipients.len())?)
        .bind(status)
        .execute(&mut *tx)
        .await?;
        if insert.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }

        for recipient in &draft.recipients {
            let miner_id = Self::miner_id_in_tx(&mut tx, &recipient.miner).await?;
            sqlx::query(
                "insert into payout_recipients(batch_id, miner_id, address, amount_base_units)
                 values ($1, $2, $3, $4::numeric)",
            )
            .bind(&draft.batch_id)
            .bind(miner_id)
            .bind(&recipient.address)
            .bind(recipient.amount_base_units.to_string())
            .execute(&mut *tx)
            .await?;

            let amount = recipient.amount_base_units.to_string();
            sqlx::query(
                "insert into ledger_entries(miner_id, amount_base_units, kind, ref_type, ref_id)
                 values ($1, $2::numeric * -1, 'payout_lock', 'payout_batch', $3)
                 on conflict do nothing",
            )
            .bind(miner_id)
            .bind(&amount)
            .bind(&draft.batch_id)
            .execute(&mut *tx)
            .await?;
            let locked = sqlx::query(
                "update balance_cache set
                   confirmed_base_units = greatest(confirmed_base_units - $2::numeric, 0),
                   locked_base_units = locked_base_units + $2::numeric,
                   updated_at = now()
                 where miner_id = $1
                   and confirmed_base_units >= $2::numeric",
            )
            .bind(miner_id)
            .bind(&amount)
            .execute(&mut *tx)
            .await?
            .rows_affected()
                == 1;
            if !locked {
                return Err(RepositoryError::InsufficientPayoutBalance(
                    recipient.miner.clone(),
                ));
            }
        }

        tx.commit().await?;
        Ok(true)
    }

    async fn list_payout_batches_by_status(
        &self,
        statuses: &[&str],
        limit: i64,
    ) -> Result<Vec<PayoutBatchRecord>> {
        let batch_rows = sqlx::query_as::<_, PayoutBatchRecordRow>(
            "select id as batch_id,
                    status,
                    total_base_units::text as total_base_units,
                    txid,
                    raw_tx_hash
             from payout_batches
             where status = any($1)
             order by created_at asc, id asc
             limit $2",
        )
        .bind(statuses)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        self.hydrate_payout_batch_records(batch_rows).await
    }

    async fn active_payout_total_today(&self) -> Result<u128> {
        let total: Option<String> = sqlx::query_scalar(
            "select coalesce(sum(total_base_units), 0)::text
             from payout_batches
             where status in ('needs_approval', 'created', 'signed', 'submitted', 'confirmed')
               and created_at >= current_date",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(total.unwrap_or_else(|| "0".to_owned()).parse()?)
    }

    async fn mark_payout_signed(
        &self,
        batch_id: &str,
        txid: &str,
        raw_tx_hash: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "update payout_batches set
               status = 'signed',
               txid = $2,
               raw_tx_hash = $3,
               signed_at = coalesce(signed_at, now())
             where id = $1 and status = 'created'",
        )
        .bind(batch_id)
        .bind(txid)
        .bind(raw_tx_hash)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn mark_payout_submitted(&self, batch_id: &str, txid: &str) -> Result<bool> {
        let result = sqlx::query(
            "update payout_batches set
               status = 'submitted',
               txid = coalesce(txid, $2),
               submitted_at = coalesce(submitted_at, now())
             where id = $1 and status in ('signed', 'submitted')",
        )
        .bind(batch_id)
        .bind(txid)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn mark_payout_confirmed(&self, batch_id: &str) -> Result<bool> {
        let result = sqlx::query(
            "update payout_batches set
               status = 'confirmed',
               confirmed_at = coalesce(confirmed_at, now())
             where id = $1 and status in ('submitted', 'signed')",
        )
        .bind(batch_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn mark_payout_failed(&self, batch_id: &str, reason: &str) -> Result<bool> {
        let result = sqlx::query(
            "update payout_batches set
               status = 'failed',
               failed_at = coalesce(failed_at, now()),
               failure_reason = $2
             where id = $1 and status in ('needs_approval', 'created', 'signed', 'submitted')",
        )
        .bind(batch_id)
        .bind(reason)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn mark_payout_approved(&self, batch_id: &str) -> Result<bool> {
        let result = sqlx::query(
            "update payout_batches set
               status = 'created'
             where id = $1 and status = 'needs_approval'",
        )
        .bind(batch_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn append_payout_audit_event(&self, event: &PayoutAuditEvent) -> Result<()> {
        sqlx::query(
            "insert into payout_audit_events(batch_id, actor, action, details_json)
             values ($1, $2, $3, $4)",
        )
        .bind(&event.batch_id)
        .bind(&event.actor)
        .bind(&event.action)
        .bind(&event.details)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_payout_audit_events(
        &self,
        batch_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<PayoutAuditEvent>> {
        let rows = sqlx::query_as::<_, PayoutAuditEventRow>(
            "select batch_id,
                    actor,
                    action,
                    details_json as details,
                    created_at::text as created_at
             from payout_audit_events
             where ($1::text is null or batch_id = $1)
             order by created_at desc, id desc
             limit $2",
        )
        .bind(batch_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn retry_failed_payout(
        &self,
        batch_id: &str,
        new_batch_id: &str,
    ) -> Result<Option<PayoutBatchRecord>> {
        let Some(original) = self
            .payout_batch_by_id_and_statuses(batch_id, &["failed", "cancelled"])
            .await?
        else {
            return Ok(None);
        };
        let draft = PayoutBatchDraft {
            batch_id: new_batch_id.to_owned(),
            recipients: original.recipients.clone(),
            total_base_units: original.total_base_units,
            lock_entries: vec![],
        };
        if !self.create_locked_payout_batch(draft).await? {
            return Ok(None);
        }
        self.payout_batch_by_id_and_statuses(new_batch_id, &["created"])
            .await
    }

    async fn cancel_payout(
        &self,
        batch_id: &str,
        reason: &str,
    ) -> Result<Option<PayoutBatchRecord>> {
        let Some(batch) = self
            .payout_batch_by_id_and_statuses(batch_id, &["needs_approval", "created", "signed"])
            .await?
        else {
            return Ok(None);
        };
        let updated = sqlx::query(
            "update payout_batches set
               status = 'cancelled',
               failed_at = coalesce(failed_at, now()),
               failure_reason = $2
             where id = $1 and status in ('needs_approval', 'created', 'signed')",
        )
        .bind(batch_id)
        .bind(reason)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1;
        if updated {
            self.unlock_failed_payout(&batch).await?;
            let mut cancelled = batch;
            cancelled.status = "cancelled".to_owned();
            Ok(Some(cancelled))
        } else {
            Ok(None)
        }
    }

    async fn unlock_failed_payout(&self, batch: &PayoutBatchRecord) -> Result<usize> {
        let entries = batch
            .recipients
            .iter()
            .map(|recipient| LedgerEntry {
                miner: Some(recipient.miner.clone()),
                amount_base_units: recipient.amount_base_units as i128,
                kind: LedgerKind::PayoutFailedUnlock,
                ref_type: "payout_batch".to_owned(),
                ref_id: batch.batch_id.clone(),
            })
            .collect::<Vec<_>>();
        self.append_ledger_entries(&entries).await?;
        Ok(entries.len())
    }

    async fn mark_payout_paid(&self, batch: &PayoutBatchRecord) -> Result<usize> {
        let entries = batch
            .recipients
            .iter()
            .map(|recipient| LedgerEntry {
                miner: Some(recipient.miner.clone()),
                amount_base_units: -(recipient.amount_base_units as i128),
                kind: LedgerKind::PayoutSent,
                ref_type: "payout_batch".to_owned(),
                ref_id: batch.batch_id.clone(),
            })
            .collect::<Vec<_>>();
        self.append_ledger_entries(&entries).await?;
        Ok(entries.len())
    }
}

impl PgRepository {
    async fn payout_batch_by_id_and_statuses(
        &self,
        batch_id: &str,
        statuses: &[&str],
    ) -> Result<Option<PayoutBatchRecord>> {
        let rows = sqlx::query_as::<_, PayoutBatchRecordRow>(
            "select id as batch_id,
                    status,
                    total_base_units::text as total_base_units,
                    txid,
                    raw_tx_hash
             from payout_batches
             where id = $1 and status = any($2)",
        )
        .bind(batch_id)
        .bind(statuses)
        .fetch_all(&self.pool)
        .await?;
        Ok(self.hydrate_payout_batch_records(rows).await?.pop())
    }

    async fn hydrate_payout_batch_records(
        &self,
        batch_rows: Vec<PayoutBatchRecordRow>,
    ) -> Result<Vec<PayoutBatchRecord>> {
        if batch_rows.is_empty() {
            return Ok(vec![]);
        }
        let batch_ids = batch_rows
            .iter()
            .map(|row| row.batch_id.clone())
            .collect::<Vec<_>>();
        let recipient_rows = sqlx::query_as::<_, PayoutRecipientRow>(
            "select payout_recipients.batch_id,
                    miners.address as miner,
                    payout_recipients.address,
                    payout_recipients.amount_base_units::text as amount_base_units
             from payout_recipients
             join miners on miners.id = payout_recipients.miner_id
             where payout_recipients.batch_id = any($1)
             order by payout_recipients.batch_id asc, miners.address asc",
        )
        .bind(&batch_ids)
        .fetch_all(&self.pool)
        .await?;

        let mut recipients_by_batch = BTreeMap::<String, Vec<PayoutRecipient>>::new();
        for row in recipient_rows {
            recipients_by_batch
                .entry(row.batch_id.clone())
                .or_default()
                .push(row.try_into()?);
        }

        batch_rows
            .into_iter()
            .map(|row| {
                Ok(PayoutBatchRecord {
                    batch_id: row.batch_id.clone(),
                    status: row.status,
                    total_base_units: row.total_base_units.parse()?,
                    txid: row.txid,
                    raw_tx_hash: row.raw_tx_hash,
                    recipients: recipients_by_batch
                        .remove(&row.batch_id)
                        .unwrap_or_default(),
                })
            })
            .collect()
    }
}

#[derive(sqlx::FromRow)]
struct RewardBlockRow {
    hash_hex: String,
    job_id: Option<String>,
    reward_base_units: String,
}

impl TryFrom<RewardBlockRow> for RewardBlock {
    type Error = RepositoryError;

    fn try_from(row: RewardBlockRow) -> Result<Self> {
        Ok(Self {
            hash_hex: row.hash_hex,
            job_id: row.job_id.unwrap_or_default(),
            reward_base_units: row.reward_base_units.parse()?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ShareWeightRow {
    miner: String,
    difficulty: String,
}

#[derive(sqlx::FromRow)]
struct MatureRewardRow {
    miner: String,
    amount_base_units: String,
    ref_id: String,
}

impl TryFrom<MatureRewardRow> for LedgerEntry {
    type Error = RepositoryError;

    fn try_from(row: MatureRewardRow) -> Result<Self> {
        Ok(Self {
            miner: Some(row.miner),
            amount_base_units: row.amount_base_units.parse()?,
            kind: LedgerKind::RewardMature,
            ref_type: "block".to_owned(),
            ref_id: row.ref_id,
        })
    }
}

#[derive(sqlx::FromRow)]
struct OrphanRewardRow {
    miner: String,
    amount_base_units: String,
    ref_id: String,
}

impl TryFrom<OrphanRewardRow> for LedgerEntry {
    type Error = RepositoryError;

    fn try_from(row: OrphanRewardRow) -> Result<Self> {
        Ok(Self {
            miner: Some(row.miner),
            amount_base_units: row.amount_base_units.parse()?,
            kind: LedgerKind::RewardOrphanReversal,
            ref_type: "block".to_owned(),
            ref_id: row.ref_id,
        })
    }
}

impl TryFrom<ShareWeightRow> for ShareWeight {
    type Error = RepositoryError;

    fn try_from(row: ShareWeightRow) -> Result<Self> {
        let integer_part = row
            .difficulty
            .split('.')
            .next()
            .unwrap_or(&row.difficulty)
            .to_owned();
        Ok(Self {
            miner: row.miner,
            difficulty: integer_part.parse()?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct DashboardPoolStatsRow {
    workers_online: String,
    miners_online: String,
    total_blocks: String,
    canonical_blocks: String,
    immature_blocks: String,
    orphaned_blocks: String,
    fee_revenue_base_units: String,
    latest_payout_created_ts: String,
    jobs_created: String,
    latest_job_created_ts: String,
    payout_batches_needs_approval: String,
    payout_batches_created: String,
    payout_batches_signed: String,
    payout_batches_submitted: String,
    payout_batches_confirmed: String,
    payout_batches_failed: String,
    payout_batches_cancelled: String,
    payout_amount_base_units_total: String,
    avg_block_effort_pct_24h: String,
    avg_block_effort_pct_7d: String,
    avg_block_effort_pct_lifetime: String,
}

impl TryFrom<DashboardPoolStatsRow> for DashboardPoolStats {
    type Error = RepositoryError;

    fn try_from(row: DashboardPoolStatsRow) -> Result<Self> {
        Ok(Self {
            workers_online: row.workers_online.parse()?,
            miners_online: row.miners_online.parse()?,
            total_blocks: row.total_blocks.parse()?,
            canonical_blocks: row.canonical_blocks.parse()?,
            immature_blocks: row.immature_blocks.parse()?,
            orphaned_blocks: row.orphaned_blocks.parse()?,
            fee_revenue_base_units: row.fee_revenue_base_units.parse()?,
            latest_payout_created_ts: row.latest_payout_created_ts.parse()?,
            jobs_created: row.jobs_created.parse()?,
            latest_job_created_ts: row.latest_job_created_ts.parse()?,
            payout_batches_needs_approval: row.payout_batches_needs_approval.parse()?,
            payout_batches_created: row.payout_batches_created.parse()?,
            payout_batches_signed: row.payout_batches_signed.parse()?,
            payout_batches_submitted: row.payout_batches_submitted.parse()?,
            payout_batches_confirmed: row.payout_batches_confirmed.parse()?,
            payout_batches_failed: row.payout_batches_failed.parse()?,
            payout_batches_cancelled: row.payout_batches_cancelled.parse()?,
            payout_amount_base_units_total: row.payout_amount_base_units_total.parse()?,
            avg_block_effort_pct_24h: row.avg_block_effort_pct_24h.parse().unwrap_or_default(),
            avg_block_effort_pct_7d: row.avg_block_effort_pct_7d.parse().unwrap_or_default(),
            avg_block_effort_pct_lifetime: row
                .avg_block_effort_pct_lifetime
                .parse()
                .unwrap_or_default(),
        })
    }
}

#[derive(sqlx::FromRow)]
struct DashboardMinerStatsRow {
    miner: String,
    immature_base_units: String,
    confirmed_base_units: String,
    locked_base_units: String,
    paid_base_units: String,
    workers_total: String,
    shares_accepted: String,
    shares_rejected: String,
    shares_stale: String,
    blocks_found: String,
    last_difficulty: String,
    last_seen_ts: String,
}

impl TryFrom<DashboardMinerStatsRow> for DashboardMinerStats {
    type Error = RepositoryError;

    fn try_from(row: DashboardMinerStatsRow) -> Result<Self> {
        Ok(Self {
            miner: row.miner,
            immature_base_units: row.immature_base_units.parse()?,
            confirmed_base_units: row.confirmed_base_units.parse()?,
            locked_base_units: row.locked_base_units.parse()?,
            paid_base_units: row.paid_base_units.parse()?,
            workers_total: row.workers_total.parse()?,
            shares_accepted: row.shares_accepted.parse()?,
            shares_rejected: row.shares_rejected.parse()?,
            shares_stale: row.shares_stale.parse()?,
            blocks_found: row.blocks_found.parse()?,
            last_difficulty: row.last_difficulty.parse().unwrap_or_default(),
            last_seen_ts: row.last_seen_ts.parse()?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct DashboardWorkerStatsRow {
    name: String,
    shares_accepted: String,
    shares_rejected: String,
    shares_stale: String,
    blocks_found: String,
    last_difficulty: String,
    last_seen_ts: String,
    connected_at: Option<String>,
    last_seen_at: Option<String>,
}

impl TryFrom<DashboardWorkerStatsRow> for DashboardWorkerStats {
    type Error = RepositoryError;

    fn try_from(row: DashboardWorkerStatsRow) -> Result<Self> {
        Ok(Self {
            name: row.name,
            shares_accepted: row.shares_accepted.parse()?,
            shares_rejected: row.shares_rejected.parse()?,
            shares_stale: row.shares_stale.parse()?,
            blocks_found: row.blocks_found.parse()?,
            last_difficulty: row.last_difficulty.parse().unwrap_or_default(),
            last_seen_ts: row.last_seen_ts.parse()?,
            connected_at: row.connected_at,
            last_seen_at: row.last_seen_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct DashboardHistorySampleRow {
    ts: String,
    pool_hs: String,
    net_hs: String,
    workers: String,
    shares_accepted: String,
    shares_rejected: String,
    shares_stale: String,
}

impl TryFrom<DashboardHistorySampleRow> for DashboardHistorySample {
    type Error = RepositoryError;

    fn try_from(row: DashboardHistorySampleRow) -> Result<Self> {
        Ok(Self {
            ts: row.ts.parse()?,
            pool_hs: row.pool_hs.parse().unwrap_or_default(),
            net_hs: row.net_hs.parse().unwrap_or_default(),
            workers: row.workers.parse()?,
            shares_accepted: row.shares_accepted.parse()?,
            shares_rejected: row.shares_rejected.parse()?,
            shares_stale: row.shares_stale.parse()?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct OfflineWorkerRow {
    miner: String,
    worker_name: String,
    last_seen_ts: String,
    last_seen_at: Option<String>,
}

impl TryFrom<OfflineWorkerRow> for OfflineWorkerRecord {
    type Error = RepositoryError;

    fn try_from(row: OfflineWorkerRow) -> Result<Self> {
        Ok(Self {
            miner: row.miner,
            worker_name: row.worker_name,
            last_seen_ts: row.last_seen_ts.parse()?,
            last_seen_at: row.last_seen_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct AcceptedShareGapRow {
    latest_share_ts: String,
    latest_share_at: Option<String>,
}

impl TryFrom<AcceptedShareGapRow> for AcceptedShareGapRecord {
    type Error = RepositoryError;

    fn try_from(row: AcceptedShareGapRow) -> Result<Self> {
        Ok(Self {
            latest_share_ts: row.latest_share_ts.parse()?,
            latest_share_at: row.latest_share_at,
            quiet_minutes: 0,
        })
    }
}

#[derive(sqlx::FromRow)]
struct LatestJobRow {
    job_id: String,
    prev_hash: String,
    created_ts: String,
    created_at: Option<String>,
    age_seconds: String,
}

impl TryFrom<LatestJobRow> for LatestJobRecord {
    type Error = RepositoryError;

    fn try_from(row: LatestJobRow) -> Result<Self> {
        Ok(Self {
            job_id: row.job_id,
            prev_hash: row.prev_hash,
            created_ts: row.created_ts.parse()?,
            created_at: row.created_at,
            age_seconds: row.age_seconds.parse()?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct BlockSubmissionAlertRow {
    hash_hex: Option<String>,
    job_id: String,
    status: String,
    submitted_ts: String,
    submitted_at: Option<String>,
    age_seconds: String,
    submit_ok: Option<bool>,
    reason: String,
}

impl TryFrom<BlockSubmissionAlertRow> for BlockSubmissionAlertRecord {
    type Error = RepositoryError;

    fn try_from(row: BlockSubmissionAlertRow) -> Result<Self> {
        Ok(Self {
            hash_hex: row.hash_hex.unwrap_or_default(),
            job_id: row.job_id,
            status: row.status,
            submitted_ts: row.submitted_ts.parse()?,
            submitted_at: row.submitted_at,
            age_seconds: row.age_seconds.parse()?,
            submit_ok: row.submit_ok,
            reason: row.reason,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ShareQualityRow {
    miner: String,
    worker_name: String,
    accepted_count: String,
    rejected_count: String,
    stale_count: String,
}

fn share_quality_alert_from_counts(
    row: ShareQualityRow,
    window_minutes: i64,
    max_reject_rate: f64,
    max_stale_rate: f64,
) -> Result<Option<ShareQualityAlertRecord>> {
    let accepted_count = row.accepted_count.parse::<u64>()?;
    let rejected_count = row.rejected_count.parse::<u64>()?;
    let stale_count = row.stale_count.parse::<u64>()?;
    let total = accepted_count + rejected_count + stale_count;
    if total == 0 {
        return Ok(None);
    }
    let reject_rate = rejected_count as f64 / total as f64;
    let stale_rate = stale_count as f64 / total as f64;
    if reject_rate <= max_reject_rate && stale_rate <= max_stale_rate {
        return Ok(None);
    }
    Ok(Some(ShareQualityAlertRecord {
        miner: row.miner,
        worker_name: row.worker_name,
        accepted_count,
        rejected_count,
        stale_count,
        reject_rate,
        stale_rate,
        window_minutes,
    }))
}

#[derive(sqlx::FromRow)]
struct DashboardBlockRow {
    height: String,
    hash: Option<String>,
    finder: String,
    worker: String,
    reward_base_units: String,
    status: String,
    confirmations: String,
    effort_pct: String,
    found_at: Option<String>,
    confirmed_at: Option<String>,
}

impl TryFrom<DashboardBlockRow> for DashboardBlock {
    type Error = RepositoryError;

    fn try_from(row: DashboardBlockRow) -> Result<Self> {
        Ok(Self {
            height: row.height.parse()?,
            hash: row.hash.unwrap_or_default(),
            finder: row.finder,
            worker: row.worker,
            reward_base_units: row.reward_base_units.parse()?,
            status: row.status,
            confirmations: row.confirmations.parse()?,
            effort_pct: row.effort_pct.parse().unwrap_or_default(),
            found_at: row.found_at.unwrap_or_default(),
            confirmed_at: row.confirmed_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct DashboardPaymentRow {
    batch_id: String,
    address: String,
    amount_base_units: String,
    txid: String,
    status: String,
    created_at: String,
    confirmed_at: Option<String>,
}

impl TryFrom<DashboardPaymentRow> for DashboardPayment {
    type Error = RepositoryError;

    fn try_from(row: DashboardPaymentRow) -> Result<Self> {
        Ok(Self {
            batch_id: row.batch_id,
            address: row.address,
            amount_base_units: row.amount_base_units.parse()?,
            txid: row.txid,
            status: row.status,
            created_at: row.created_at,
            confirmed_at: row.confirmed_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct BlockRow {
    hash_hex: String,
    job_id: String,
    status: String,
    height: Option<i64>,
    confirmations: i32,
    reward_base_units: String,
    effort_pct: String,
}

impl TryFrom<BlockRow> for BlockRecord {
    type Error = RepositoryError;

    fn try_from(row: BlockRow) -> Result<Self> {
        Ok(Self {
            hash_hex: row.hash_hex,
            job_id: row.job_id,
            status: row.status,
            height: row.height.map(u64::try_from).transpose()?,
            confirmations: u64::try_from(row.confirmations)?,
            reward_base_units: row.reward_base_units.parse()?,
            effort_pct: row.effort_pct.parse().unwrap_or_default(),
        })
    }
}

#[derive(sqlx::FromRow)]
struct LedgerEntryRow {
    miner: Option<String>,
    amount_base_units: String,
    kind: String,
    ref_type: String,
    ref_id: String,
}

impl TryFrom<LedgerEntryRow> for LedgerEntry {
    type Error = RepositoryError;

    fn try_from(row: LedgerEntryRow) -> Result<Self> {
        Ok(Self {
            miner: row.miner,
            amount_base_units: row.amount_base_units.parse()?,
            kind: serde_json::from_value(serde_json::Value::String(row.kind))?,
            ref_type: row.ref_type,
            ref_id: row.ref_id,
        })
    }
}

#[derive(sqlx::FromRow)]
struct BalanceRow {
    miner: String,
    address: String,
    confirmed_base_units: String,
}

impl TryFrom<BalanceRow> for MinerBalance {
    type Error = RepositoryError;

    fn try_from(row: BalanceRow) -> Result<Self> {
        Ok(Self {
            miner: row.miner,
            address: row.address,
            confirmed_base_units: row.confirmed_base_units.parse()?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct PayoutBatchRow {
    batch_id: String,
    total_base_units: String,
}

#[derive(sqlx::FromRow)]
struct PayoutBatchRecordRow {
    batch_id: String,
    status: String,
    total_base_units: String,
    txid: Option<String>,
    raw_tx_hash: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PayoutAuditEventRow {
    batch_id: String,
    actor: String,
    action: String,
    details: serde_json::Value,
    created_at: Option<String>,
}

impl From<PayoutAuditEventRow> for PayoutAuditEvent {
    fn from(row: PayoutAuditEventRow) -> Self {
        Self {
            batch_id: row.batch_id,
            actor: row.actor,
            action: row.action,
            details: row.details,
            created_at: row.created_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct NodeSampleRow {
    node_name: String,
    height: Option<i64>,
    chainwork: Option<String>,
    peers: Option<i32>,
    mempool_size: Option<i32>,
    rpc_ms: Option<String>,
    ok: bool,
    sampled_at: Option<String>,
}

impl TryFrom<NodeSampleRow> for NodeSampleRecord {
    type Error = RepositoryError;

    fn try_from(row: NodeSampleRow) -> Result<Self> {
        Ok(Self {
            node_name: row.node_name,
            height: row.height.map(u64::try_from).transpose()?,
            chainwork: row.chainwork,
            peers: row.peers.map(u64::try_from).transpose()?,
            mempool_size: row.mempool_size.map(u64::try_from).transpose()?,
            rpc_ms: row.rpc_ms.map(|value| value.parse().unwrap_or_default()),
            ok: row.ok,
            sampled_at: row.sampled_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct AlertEventRow {
    fingerprint: String,
    severity: String,
    status: String,
    kind: String,
    subject: String,
    message: String,
    first_seen_at: Option<String>,
    last_seen_at: Option<String>,
    resolved_at: Option<String>,
    details_json: serde_json::Value,
}

impl TryFrom<AlertEventRow> for AlertEvent {
    type Error = RepositoryError;

    fn try_from(row: AlertEventRow) -> Result<Self> {
        Ok(Self {
            fingerprint: row.fingerprint,
            severity: row.severity,
            status: row.status,
            kind: row.kind,
            subject: row.subject,
            message: row.message,
            first_seen_at: row.first_seen_at,
            last_seen_at: row.last_seen_at,
            resolved_at: row.resolved_at,
            details: row.details_json,
        })
    }
}

#[derive(sqlx::FromRow)]
struct PayoutRecipientRow {
    batch_id: String,
    miner: String,
    address: String,
    amount_base_units: String,
}

impl TryFrom<PayoutRecipientRow> for PayoutRecipient {
    type Error = RepositoryError;

    fn try_from(row: PayoutRecipientRow) -> Result<Self> {
        Ok(Self {
            miner: row.miner,
            address: row.address,
            amount_base_units: row.amount_base_units.parse()?,
        })
    }
}

#[derive(Clone, Default)]
pub struct InMemoryRepository {
    inner: Arc<RwLock<InMemoryData>>,
}

#[derive(Default)]
struct InMemoryData {
    ledger_entries: Vec<LedgerEntry>,
    balances: BTreeMap<String, MinerBalance>,
    payout_batches: BTreeMap<String, PayoutBatchDraft>,
    payout_statuses: BTreeMap<String, String>,
    payout_txids: BTreeMap<String, String>,
    payout_raw_txs: BTreeMap<String, String>,
    payout_audit_events: Vec<PayoutAuditEvent>,
    node_samples: Vec<NodeSampleRecord>,
    alerts: BTreeMap<String, AlertEvent>,
    settings: BTreeMap<String, String>,
    jobs: BTreeMap<String, JobRecord>,
    sessions: BTreeMap<String, (SessionRecord, bool)>,
    shares: Vec<ShareRecord>,
    share_events: Vec<ShareEventRecord>,
    block_candidates: BTreeMap<String, BlockCandidateRecord>,
    block_statuses: BTreeMap<String, BlockRecord>,
    reward_blocks: BTreeMap<String, RewardBlock>,
}

impl InMemoryRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MonitoringRepository for InMemoryRepository {
    async fn insert_node_sample(&self, sample: &NodeSampleRecord) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        inner.node_samples.push(sample.clone());
        Ok(())
    }

    async fn latest_node_samples(&self, limit: i64) -> Result<Vec<NodeSampleRecord>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mut samples_by_node = BTreeMap::<String, NodeSampleRecord>::new();
        for sample in &inner.node_samples {
            samples_by_node.insert(sample.node_name.clone(), sample.clone());
        }
        let mut samples = samples_by_node.into_values().collect::<Vec<_>>();
        samples.truncate(usize::try_from(limit.max(0))?);
        Ok(samples)
    }

    async fn list_stuck_payout_batches(
        &self,
        _older_than_minutes: i64,
        limit: i64,
    ) -> Result<Vec<PayoutBatchRecord>> {
        self.list_payout_batches_by_status(&["created", "signed", "submitted"], limit)
            .await
    }

    async fn list_offline_workers(
        &self,
        _offline_minutes: i64,
        _limit: i64,
    ) -> Result<Vec<OfflineWorkerRecord>> {
        Ok(vec![])
    }

    async fn accepted_share_gap(
        &self,
        _quiet_minutes: i64,
    ) -> Result<Option<AcceptedShareGapRecord>> {
        Ok(None)
    }

    async fn latest_job(&self) -> Result<Option<LatestJobRecord>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let Some(job) = inner.jobs.values().max_by_key(|job| job.job_id.as_str()) else {
            return Ok(None);
        };
        Ok(Some(LatestJobRecord {
            job_id: job.job_id.clone(),
            prev_hash: job.prev_hash_be_hex.clone(),
            created_ts: 0,
            created_at: None,
            age_seconds: 0,
        }))
    }

    async fn list_block_submission_alerts(
        &self,
        _stuck_minutes: i64,
        limit: i64,
    ) -> Result<Vec<BlockSubmissionAlertRecord>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mut alerts = Vec::new();
        for (hash_hex, candidate) in &inner.block_candidates {
            let submit_ok = candidate
                .submit_response_json
                .get("ok")
                .and_then(|value| value.as_bool());
            let status = inner
                .block_statuses
                .get(hash_hex)
                .map(|block| block.status.clone())
                .unwrap_or_else(|| "submitted".to_owned());
            if submit_ok == Some(false)
                || matches!(
                    status.as_str(),
                    "submitted_secondary" | "submitted_degraded" | "relay_failed"
                )
            {
                let reason = match status.as_str() {
                    "submitted_secondary" => "authority_failed_secondary_accepted".to_owned(),
                    "submitted_degraded" => "redundant_relay_failed".to_owned(),
                    _ if block_candidate_has_local_canonical_relay_failure(candidate) => {
                        "local_canonical_relay_failed".to_owned()
                    }
                    _ => "submit_response_not_ok".to_owned(),
                };
                alerts.push(BlockSubmissionAlertRecord {
                    hash_hex: hash_hex.clone(),
                    job_id: candidate.job_id.clone(),
                    status,
                    submitted_ts: 0,
                    submitted_at: None,
                    age_seconds: 0,
                    submit_ok,
                    reason,
                });
            }
        }
        alerts.truncate(usize::try_from(limit.max(0))?);
        Ok(alerts)
    }

    async fn list_share_quality_alerts(
        &self,
        window_minutes: i64,
        min_total: u64,
        max_reject_rate: f64,
        max_stale_rate: f64,
        limit: i64,
    ) -> Result<Vec<ShareQualityAlertRecord>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mut counts = BTreeMap::<(String, String), (u64, u64, u64)>::new();
        for share in &inner.shares {
            counts
                .entry((share.miner.clone(), share.worker_name.clone()))
                .or_default()
                .0 += 1;
        }
        for event in &inner.share_events {
            let entry = counts
                .entry((event.miner.clone(), event.worker_name.clone()))
                .or_default();
            match event.kind.as_str() {
                "rejected" => entry.1 += 1,
                "stale" => entry.2 += 1,
                _ => {}
            }
        }
        let mut alerts = Vec::new();
        for ((miner, worker_name), (accepted_count, rejected_count, stale_count)) in counts {
            let total = accepted_count + rejected_count + stale_count;
            if total < min_total || total == 0 {
                continue;
            }
            let reject_rate = rejected_count as f64 / total as f64;
            let stale_rate = stale_count as f64 / total as f64;
            if reject_rate <= max_reject_rate && stale_rate <= max_stale_rate {
                continue;
            }
            alerts.push(ShareQualityAlertRecord {
                miner,
                worker_name,
                accepted_count,
                rejected_count,
                stale_count,
                reject_rate,
                stale_rate,
                window_minutes,
            });
        }
        alerts.truncate(usize::try_from(limit.max(0))?);
        Ok(alerts)
    }

    async fn upsert_alert(&self, alert: &AlertEvent) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mut alert = alert.clone();
        alert.status = "active".to_owned();
        inner.alerts.insert(alert.fingerprint.clone(), alert);
        Ok(())
    }

    async fn resolve_alert(&self, fingerprint: &str) -> Result<bool> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let Some(alert) = inner.alerts.get_mut(fingerprint) else {
            return Ok(false);
        };
        if alert.status == "active" {
            alert.status = "resolved".to_owned();
            return Ok(true);
        }
        Ok(false)
    }

    async fn list_alerts(&self, status: Option<&str>, limit: i64) -> Result<Vec<AlertEvent>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mut alerts = inner
            .alerts
            .values()
            .filter(|alert| status.map(|status| alert.status == status).unwrap_or(true))
            .cloned()
            .collect::<Vec<_>>();
        alerts.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
        alerts.truncate(usize::try_from(limit.max(0))?);
        Ok(alerts)
    }
}

impl PoolRepository for InMemoryRepository {
    fn append_ledger_entries(&self, entries: &[LedgerEntry]) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        for entry in entries {
            match (&entry.kind, entry.miner.as_ref()) {
                (LedgerKind::RewardMature, Some(miner)) if entry.amount_base_units > 0 => {
                    let amount = u128::try_from(entry.amount_base_units)?;
                    inner
                        .balances
                        .entry(miner.clone())
                        .and_modify(|balance| {
                            balance.confirmed_base_units += amount;
                        })
                        .or_insert_with(|| MinerBalance {
                            miner: miner.clone(),
                            address: miner.clone(),
                            confirmed_base_units: amount,
                        });
                }
                (LedgerKind::RewardOrphanReversal, Some(miner)) if entry.amount_base_units < 0 => {
                    let amount = u128::try_from(-entry.amount_base_units)?;
                    if let Some(balance) = inner.balances.get_mut(miner) {
                        balance.confirmed_base_units =
                            balance.confirmed_base_units.saturating_sub(amount);
                    }
                }
                (LedgerKind::PayoutLock, Some(miner)) if entry.amount_base_units < 0 => {
                    let amount = u128::try_from(-entry.amount_base_units)?;
                    if let Some(balance) = inner.balances.get_mut(miner) {
                        balance.confirmed_base_units =
                            balance.confirmed_base_units.saturating_sub(amount);
                    }
                }
                (LedgerKind::PayoutFailedUnlock, Some(miner)) if entry.amount_base_units > 0 => {
                    let amount = u128::try_from(entry.amount_base_units)?;
                    inner
                        .balances
                        .entry(miner.clone())
                        .and_modify(|balance| {
                            balance.confirmed_base_units += amount;
                        })
                        .or_insert_with(|| MinerBalance {
                            miner: miner.clone(),
                            address: miner.clone(),
                            confirmed_base_units: amount,
                        });
                }
                _ => {}
            }
        }
        inner.ledger_entries.extend_from_slice(entries);
        Ok(())
    }

    fn list_ledger_entries(&self) -> Result<Vec<LedgerEntry>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        Ok(inner.ledger_entries.clone())
    }

    fn set_balance(&self, balance: MinerBalance) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        inner.balances.insert(balance.miner.clone(), balance);
        Ok(())
    }

    fn list_balances(&self) -> Result<Vec<MinerBalance>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        Ok(inner.balances.values().cloned().collect())
    }

    fn create_payout_batch(&self, draft: PayoutBatchDraft) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        if inner.payout_batches.contains_key(&draft.batch_id) {
            return Err(RepositoryError::DuplicatePayoutBatch(draft.batch_id));
        }
        inner
            .payout_statuses
            .insert(draft.batch_id.clone(), "dry_run_ok".to_owned());
        inner.payout_batches.insert(draft.batch_id.clone(), draft);
        Ok(())
    }

    fn list_payout_batches(&self) -> Result<Vec<PayoutBatchDraft>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        Ok(inner.payout_batches.values().cloned().collect())
    }
}

#[async_trait]
impl ControlRepository for InMemoryRepository {
    async fn payouts_enabled(&self) -> Result<bool> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        Ok(inner
            .settings
            .get("payouts_enabled")
            .map(|value| matches!(value.as_str(), "true" | "1" | "yes" | "on"))
            .unwrap_or(false))
    }

    async fn set_payouts_enabled(&self, enabled: bool) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        inner.settings.insert(
            "payouts_enabled".to_owned(),
            if enabled { "true" } else { "false" }.to_owned(),
        );
        Ok(())
    }
}

#[async_trait]
impl PayoutRepository for InMemoryRepository {
    async fn list_payable_balances(
        &self,
        minimum_payout_base_units: u128,
        limit: i64,
    ) -> Result<Vec<MinerBalance>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mut balances = inner
            .balances
            .values()
            .filter(|balance| balance.confirmed_base_units >= minimum_payout_base_units)
            .cloned()
            .collect::<Vec<_>>();
        balances.sort_by(|a, b| {
            b.confirmed_base_units
                .cmp(&a.confirmed_base_units)
                .then_with(|| a.miner.cmp(&b.miner))
        });
        balances.truncate(usize::try_from(limit.max(0))?);
        Ok(balances)
    }

    async fn create_locked_payout_batch(&self, draft: PayoutBatchDraft) -> Result<bool> {
        self.create_locked_payout_batch_with_status(draft, "created")
            .await
    }

    async fn create_locked_payout_batch_with_status(
        &self,
        draft: PayoutBatchDraft,
        status: &str,
    ) -> Result<bool> {
        let lock_entries = if draft.lock_entries.is_empty() {
            draft
                .recipients
                .iter()
                .map(|recipient| LedgerEntry {
                    miner: Some(recipient.miner.clone()),
                    amount_base_units: -(recipient.amount_base_units as i128),
                    kind: LedgerKind::PayoutLock,
                    ref_type: "payout_batch".to_owned(),
                    ref_id: draft.batch_id.clone(),
                })
                .collect::<Vec<_>>()
        } else {
            draft.lock_entries.clone()
        };
        {
            let mut inner = self
                .inner
                .write()
                .map_err(|_| RepositoryError::LockPoisoned)?;
            if inner.payout_batches.contains_key(&draft.batch_id) {
                return Ok(false);
            }
            inner
                .payout_statuses
                .insert(draft.batch_id.clone(), status.to_owned());
            inner
                .payout_batches
                .insert(draft.batch_id.clone(), draft.clone());
        }
        self.append_ledger_entries(&lock_entries)?;
        Ok(true)
    }

    async fn list_payout_batches_by_status(
        &self,
        statuses: &[&str],
        limit: i64,
    ) -> Result<Vec<PayoutBatchRecord>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mut batches = inner
            .payout_batches
            .iter()
            .filter(|(batch_id, _)| {
                inner
                    .payout_statuses
                    .get(*batch_id)
                    .map(|status| statuses.contains(&status.as_str()))
                    .unwrap_or(false)
            })
            .map(|draft| PayoutBatchRecord {
                batch_id: draft.1.batch_id.clone(),
                status: inner
                    .payout_statuses
                    .get(draft.0)
                    .cloned()
                    .unwrap_or_else(|| "created".to_owned()),
                total_base_units: draft.1.total_base_units,
                txid: inner.payout_txids.get(draft.0).cloned(),
                raw_tx_hash: inner.payout_raw_txs.get(draft.0).cloned(),
                recipients: draft.1.recipients.clone(),
            })
            .collect::<Vec<_>>();
        batches.truncate(usize::try_from(limit.max(0))?);
        Ok(batches)
    }

    async fn active_payout_total_today(&self) -> Result<u128> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        Ok(inner
            .payout_batches
            .iter()
            .filter(|(batch_id, _)| {
                inner
                    .payout_statuses
                    .get(*batch_id)
                    .map(|status| {
                        matches!(
                            status.as_str(),
                            "needs_approval" | "created" | "signed" | "submitted" | "confirmed"
                        )
                    })
                    .unwrap_or(false)
            })
            .map(|(_, draft)| draft.total_base_units)
            .sum())
    }

    async fn mark_payout_signed(
        &self,
        batch_id: &str,
        txid: &str,
        raw_tx_hash: &str,
    ) -> Result<bool> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        if inner.payout_statuses.get(batch_id).map(String::as_str) != Some("created") {
            return Ok(false);
        }
        inner
            .payout_statuses
            .insert(batch_id.to_owned(), "signed".to_owned());
        inner
            .payout_txids
            .insert(batch_id.to_owned(), txid.to_owned());
        inner
            .payout_raw_txs
            .insert(batch_id.to_owned(), raw_tx_hash.to_owned());
        Ok(true)
    }

    async fn mark_payout_submitted(&self, batch_id: &str, txid: &str) -> Result<bool> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        if !matches!(
            inner.payout_statuses.get(batch_id).map(String::as_str),
            Some("signed" | "submitted")
        ) {
            return Ok(false);
        }
        inner
            .payout_statuses
            .insert(batch_id.to_owned(), "submitted".to_owned());
        inner
            .payout_txids
            .insert(batch_id.to_owned(), txid.to_owned());
        Ok(true)
    }

    async fn mark_payout_confirmed(&self, batch_id: &str) -> Result<bool> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        if !matches!(
            inner.payout_statuses.get(batch_id).map(String::as_str),
            Some("submitted" | "signed")
        ) {
            return Ok(false);
        }
        inner
            .payout_statuses
            .insert(batch_id.to_owned(), "confirmed".to_owned());
        Ok(true)
    }

    async fn mark_payout_failed(&self, batch_id: &str, _reason: &str) -> Result<bool> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        if !matches!(
            inner.payout_statuses.get(batch_id).map(String::as_str),
            Some("needs_approval" | "created" | "signed" | "submitted")
        ) {
            return Ok(false);
        }
        inner
            .payout_statuses
            .insert(batch_id.to_owned(), "failed".to_owned());
        Ok(true)
    }

    async fn mark_payout_approved(&self, batch_id: &str) -> Result<bool> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        if inner.payout_statuses.get(batch_id).map(String::as_str) != Some("needs_approval") {
            return Ok(false);
        }
        inner
            .payout_statuses
            .insert(batch_id.to_owned(), "created".to_owned());
        Ok(true)
    }

    async fn append_payout_audit_event(&self, event: &PayoutAuditEvent) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        inner.payout_audit_events.push(event.clone());
        Ok(())
    }

    async fn list_payout_audit_events(
        &self,
        batch_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<PayoutAuditEvent>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mut events = inner
            .payout_audit_events
            .iter()
            .filter(|event| {
                batch_id
                    .map(|batch_id| event.batch_id == batch_id)
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();
        events.reverse();
        events.truncate(usize::try_from(limit.max(0))?);
        Ok(events)
    }

    async fn retry_failed_payout(
        &self,
        batch_id: &str,
        new_batch_id: &str,
    ) -> Result<Option<PayoutBatchRecord>> {
        let original = {
            let inner = self
                .inner
                .read()
                .map_err(|_| RepositoryError::LockPoisoned)?;
            if !matches!(
                inner.payout_statuses.get(batch_id).map(String::as_str),
                Some("failed" | "cancelled")
            ) {
                return Ok(None);
            }
            inner.payout_batches.get(batch_id).cloned()
        };
        let Some(original) = original else {
            return Ok(None);
        };
        let draft = PayoutBatchDraft {
            batch_id: new_batch_id.to_owned(),
            recipients: original.recipients,
            total_base_units: original.total_base_units,
            lock_entries: vec![],
        };
        if !self.create_locked_payout_batch(draft).await? {
            return Ok(None);
        }
        Ok(self
            .list_payout_batches_by_status(&["created"], i64::MAX)
            .await?
            .into_iter()
            .find(|batch| batch.batch_id == new_batch_id))
    }

    async fn cancel_payout(
        &self,
        batch_id: &str,
        _reason: &str,
    ) -> Result<Option<PayoutBatchRecord>> {
        let batch = {
            let mut inner = self
                .inner
                .write()
                .map_err(|_| RepositoryError::LockPoisoned)?;
            if !matches!(
                inner.payout_statuses.get(batch_id).map(String::as_str),
                Some("needs_approval" | "created" | "signed")
            ) {
                return Ok(None);
            }
            inner
                .payout_statuses
                .insert(batch_id.to_owned(), "cancelled".to_owned());
            inner
                .payout_batches
                .get(batch_id)
                .map(|draft| PayoutBatchRecord {
                    batch_id: draft.batch_id.clone(),
                    status: "cancelled".to_owned(),
                    total_base_units: draft.total_base_units,
                    txid: inner.payout_txids.get(batch_id).cloned(),
                    raw_tx_hash: inner.payout_raw_txs.get(batch_id).cloned(),
                    recipients: draft.recipients.clone(),
                })
        };
        if let Some(batch) = batch.as_ref() {
            self.unlock_failed_payout(batch).await?;
        }
        Ok(batch)
    }

    async fn unlock_failed_payout(&self, batch: &PayoutBatchRecord) -> Result<usize> {
        let entries = batch
            .recipients
            .iter()
            .map(|recipient| LedgerEntry {
                miner: Some(recipient.miner.clone()),
                amount_base_units: recipient.amount_base_units as i128,
                kind: LedgerKind::PayoutFailedUnlock,
                ref_type: "payout_batch".to_owned(),
                ref_id: batch.batch_id.clone(),
            })
            .collect::<Vec<_>>();
        self.append_ledger_entries(&entries)?;
        Ok(entries.len())
    }

    async fn mark_payout_paid(&self, batch: &PayoutBatchRecord) -> Result<usize> {
        let entries = batch
            .recipients
            .iter()
            .map(|recipient| LedgerEntry {
                miner: Some(recipient.miner.clone()),
                amount_base_units: -(recipient.amount_base_units as i128),
                kind: LedgerKind::PayoutSent,
                ref_type: "payout_batch".to_owned(),
                ref_id: batch.batch_id.clone(),
            })
            .collect::<Vec<_>>();
        self.append_ledger_entries(&entries)?;
        Ok(entries.len())
    }
}

#[async_trait]
impl MiningRepository for InMemoryRepository {
    async fn open_session(&self, session: &SessionRecord) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        inner
            .sessions
            .entry(session.id.clone())
            .or_insert_with(|| (session.clone(), true));
        Ok(())
    }

    async fn close_session(&self, session_id: &str) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        if let Some((_session, active)) = inner.sessions.get_mut(session_id) {
            *active = false;
        }
        Ok(())
    }

    async fn close_stale_sessions(&self, server_instance: &str) -> Result<u64> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mut closed = 0_u64;
        for (session, active) in inner.sessions.values_mut() {
            if *active && session.server_instance == server_instance {
                *active = false;
                closed += 1;
            }
        }
        Ok(closed)
    }

    async fn update_session_difficulty(&self, session_id: &str, difficulty: f64) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        if let Some((session, _active)) = inner.sessions.get_mut(session_id) {
            session.assigned_difficulty = difficulty;
        }
        Ok(())
    }

    async fn upsert_job(&self, job: &JobRecord) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        inner.jobs.insert(job.job_id.clone(), job.clone());
        Ok(())
    }

    async fn insert_share(&self, share: &ShareRecord) -> Result<bool> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        if inner.shares.iter().any(|existing| {
            existing.job_id == share.job_id
                && existing.miner == share.miner
                && existing.worker_name == share.worker_name
                && existing.extranonce2_hex == share.extranonce2_hex
                && existing.ntime_hex == share.ntime_hex
                && existing.nonce_hex == share.nonce_hex
        }) {
            return Ok(false);
        }
        inner.shares.push(share.clone());
        Ok(true)
    }

    async fn insert_share_event(&self, event: &ShareEventRecord) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        inner.share_events.push(event.clone());
        Ok(())
    }

    async fn record_block_candidate(&self, block: &BlockCandidateRecord) -> Result<bool> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        if inner.block_candidates.contains_key(&block.hash_hex) {
            return Ok(false);
        }
        inner
            .block_candidates
            .insert(block.hash_hex.clone(), block.clone());
        inner.block_statuses.insert(
            block.hash_hex.clone(),
            BlockRecord {
                hash_hex: block.hash_hex.clone(),
                job_id: block.job_id.clone(),
                status: block_candidate_initial_status(block).to_owned(),
                height: None,
                confirmations: 0,
                reward_base_units: block.reward_base_units,
                effort_pct: block.effort_pct,
            },
        );
        if block.reward_base_units > 0 {
            inner.reward_blocks.insert(
                block.hash_hex.clone(),
                RewardBlock {
                    hash_hex: block.hash_hex.clone(),
                    job_id: block.job_id.clone(),
                    reward_base_units: block.reward_base_units,
                },
            );
        }
        Ok(true)
    }
}

#[async_trait]
impl BlockRepository for InMemoryRepository {
    async fn list_blocks_to_reconcile(&self, limit: i64) -> Result<Vec<BlockRecord>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        Ok(inner
            .block_statuses
            .values()
            .filter(|block| {
                matches!(
                    block.status.as_str(),
                    "submitted"
                        | "submitted_secondary"
                        | "submitted_degraded"
                        | "relay_failed"
                        | "seen_on_chain"
                        | "immature"
                )
            })
            .take(usize::try_from(limit.max(0))?)
            .cloned()
            .collect())
    }

    async fn update_block_status(&self, update: &BlockStatusUpdate) -> Result<bool> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let Some(block) = inner.block_statuses.get_mut(&update.hash_hex) else {
            return Ok(false);
        };
        let job_id = block.job_id.clone();
        block.status = update.status.clone();
        block.height = update.height;
        block.confirmations = update.confirmations;
        block.reward_base_units = update.reward_base_units;
        if update.status == "confirmed" && update.reward_base_units > 0 {
            inner.reward_blocks.insert(
                update.hash_hex.clone(),
                RewardBlock {
                    hash_hex: update.hash_hex.clone(),
                    job_id,
                    reward_base_units: update.reward_base_units,
                },
            );
        }
        Ok(true)
    }
}

#[async_trait]
impl RewardRepository for InMemoryRepository {
    async fn list_confirmed_unsettled_blocks(&self, limit: i64) -> Result<Vec<RewardBlock>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let settled_refs = inner
            .ledger_entries
            .iter()
            .filter(|entry| entry.ref_type == "block")
            .map(|entry| entry.ref_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        Ok(inner
            .reward_blocks
            .values()
            .filter(|block| !settled_refs.contains(&block.hash_hex))
            .take(usize::try_from(limit.max(0))?)
            .cloned()
            .collect())
    }

    async fn share_weights_for_job(&self, job_id: &str) -> Result<Vec<ShareWeight>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mut weights = BTreeMap::<String, u64>::new();
        for share in inner.shares.iter().filter(|share| share.job_id == job_id) {
            *weights.entry(share.miner.clone()).or_default() += share.difficulty as u64;
        }
        Ok(weights
            .into_iter()
            .map(|(miner, difficulty)| ShareWeight { miner, difficulty })
            .collect())
    }

    async fn list_mature_reward_entries(
        &self,
        confirm_depth: u64,
        limit: i64,
    ) -> Result<Vec<LedgerEntry>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let mature_refs = inner
            .ledger_entries
            .iter()
            .filter(|entry| entry.kind == LedgerKind::RewardMature)
            .map(|entry| (entry.miner.clone(), entry.ref_id.clone()))
            .collect::<std::collections::BTreeSet<_>>();
        Ok(inner
            .ledger_entries
            .iter()
            .filter(|entry| entry.kind == LedgerKind::RewardImmature && entry.ref_type == "block")
            .filter(|entry| {
                inner
                    .block_statuses
                    .get(&entry.ref_id)
                    .map(|block| {
                        block.status == "confirmed" && block.confirmations >= confirm_depth
                    })
                    .unwrap_or(false)
            })
            .filter(|entry| !mature_refs.contains(&(entry.miner.clone(), entry.ref_id.clone())))
            .take(usize::try_from(limit.max(0))?)
            .map(|entry| LedgerEntry {
                miner: entry.miner.clone(),
                amount_base_units: entry.amount_base_units,
                kind: LedgerKind::RewardMature,
                ref_type: entry.ref_type.clone(),
                ref_id: entry.ref_id.clone(),
            })
            .collect())
    }

    async fn list_orphan_reversal_entries(&self, limit: i64) -> Result<Vec<LedgerEntry>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        let reversed_refs = inner
            .ledger_entries
            .iter()
            .filter(|entry| entry.kind == LedgerKind::RewardOrphanReversal)
            .map(|entry| (entry.miner.clone(), entry.ref_id.clone()))
            .collect::<std::collections::BTreeSet<_>>();
        Ok(inner
            .ledger_entries
            .iter()
            .filter(|entry| entry.kind == LedgerKind::RewardImmature && entry.ref_type == "block")
            .filter(|entry| {
                inner
                    .block_statuses
                    .get(&entry.ref_id)
                    .map(|block| block.status == "orphaned")
                    .unwrap_or(false)
            })
            .filter(|entry| !reversed_refs.contains(&(entry.miner.clone(), entry.ref_id.clone())))
            .take(usize::try_from(limit.max(0))?)
            .map(|entry| LedgerEntry {
                miner: entry.miner.clone(),
                amount_base_units: -entry.amount_base_units,
                kind: LedgerKind::RewardOrphanReversal,
                ref_type: entry.ref_type.clone(),
                ref_id: entry.ref_id.clone(),
            })
            .collect())
    }
}

impl InMemoryRepository {
    pub fn list_sessions(&self) -> Result<Vec<(SessionRecord, bool)>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        Ok(inner.sessions.values().cloned().collect())
    }

    pub fn list_jobs(&self) -> Result<Vec<JobRecord>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        Ok(inner.jobs.values().cloned().collect())
    }

    pub fn list_shares(&self) -> Result<Vec<ShareRecord>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        Ok(inner.shares.clone())
    }

    pub fn list_block_candidates(&self) -> Result<Vec<BlockCandidateRecord>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RepositoryError::LockPoisoned)?;
        Ok(inner.block_candidates.values().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_ordered_and_non_empty() {
        assert!(!MIGRATIONS.is_empty());
        let mut last = 0;
        for migration in MIGRATIONS {
            assert!(migration.version > last);
            assert!(!migration.name.is_empty());
            assert!(
                migration.sql.contains("create table")
                    || migration.sql.contains("alter table")
                    || migration.sql.contains("create index")
                    || migration.sql.contains("update ")
            );
            last = migration.version;
        }
    }

    #[test]
    fn dashboard_history_sample_row_parses_counts_and_hashrate() {
        let sample = DashboardHistorySample::try_from(DashboardHistorySampleRow {
            ts: "1781542920".to_owned(),
            pool_hs: "57266230613.333333".to_owned(),
            net_hs: "0".to_owned(),
            workers: "3".to_owned(),
            shares_accepted: "120".to_owned(),
            shares_rejected: "4".to_owned(),
            shares_stale: "1".to_owned(),
        })
        .unwrap();

        assert_eq!(sample.ts, 1_781_542_920);
        assert_eq!(sample.workers, 3);
        assert_eq!(sample.shares_accepted, 120);
        assert_eq!(sample.shares_rejected, 4);
        assert_eq!(sample.shares_stale, 1);
        assert!(sample.pool_hs > 57_000_000_000.0);
    }

    #[test]
    fn finds_migration_by_version() {
        let migration = migration_by_version(1).unwrap();
        assert_eq!(migration.name, "init");
        let migration = migration_by_version(2).unwrap();
        assert_eq!(migration.name, "control_settings");
        assert!(migration.sql.contains("pool_settings"));
        let migration = migration_by_version(3).unwrap();
        assert_eq!(migration.name, "alert_events");
        assert!(migration.sql.contains("alert_events"));
        let migration = migration_by_version(4).unwrap();
        assert_eq!(migration.name, "share_events");
        assert!(migration.sql.contains("share_events"));
        let migration = migration_by_version(5).unwrap();
        assert_eq!(migration.name, "payout_approval");
        assert!(migration.sql.contains("needs_approval"));
        let migration = migration_by_version(6).unwrap();
        assert_eq!(migration.name, "payout_audit_events");
        assert!(migration.sql.contains("payout_audit_events"));
        let migration = migration_by_version(7).unwrap();
        assert_eq!(migration.name, "payouts_default_paused");
        assert!(migration.sql.contains("payouts_enabled"));
        let migration = migration_by_version(11).unwrap();
        assert_eq!(migration.name, "block_candidate_relay_failure");
        assert!(migration.sql.contains("'relay_failed'"));
    }

    #[test]
    fn init_migration_keeps_share_duplicate_guard_without_partition_conflict() {
        let sql = migration_by_version(1).unwrap().sql.to_lowercase();
        assert!(sql.contains("create table if not exists shares"));
        assert!(sql.contains("unique(job_id, worker_id, extranonce2, ntime, nonce)"));
        assert!(!sql.contains("partition by range(created_at)"));
    }

    #[test]
    fn init_migration_has_pool_fee_safe_ledger_idempotency_index() {
        let sql = migration_by_version(1).unwrap().sql.to_lowercase();
        assert!(sql.contains("create unique index if not exists ledger_entries_idempotency_idx"));
        assert!(sql.contains("coalesce(miner_id::text, 'pool')"));
        assert!(!sql.contains("unique(miner_id, kind, ref_type, ref_id)"));
    }

    #[test]
    fn init_migration_uses_text_payout_batch_ids() {
        let sql = migration_by_version(1).unwrap().sql.to_lowercase();
        assert!(sql.contains("create table if not exists payout_batches"));
        assert!(sql.contains("id text primary key"));
        assert!(sql.contains("batch_id text not null references payout_batches(id)"));
        assert!(!sql.contains("batch_id uuid"));
    }

    #[test]
    fn init_migration_can_store_candidate_submit_payloads() {
        let sql = migration_by_version(1).unwrap().sql.to_lowercase();
        assert!(sql.contains("candidate_payload_json jsonb"));
        assert!(sql.contains("submit_response_json jsonb"));
        assert!(sql.contains("reward_base_units numeric not null default 0"));
        assert!(sql.contains("blocks_submitted_idx"));
    }

    #[test]
    fn rejected_candidate_migration_preserves_ambiguous_transport_failures() {
        let sql = migration_by_version(8).unwrap().sql.to_lowercase();
        assert!(sql.contains("set status = 'orphaned'"));
        assert!(sql.contains("http_status"));
        assert!(sql.contains("4[0-9]{2}"));
        assert!(!sql.contains("5[0-9]{2}"));
    }

    #[test]
    fn session_observability_migration_links_share_events_and_versions() {
        let sql = migration_by_version(9).unwrap().sql.to_lowercase();
        assert!(sql.contains("server_session_id"));
        assert!(sql.contains("server_release"));
        assert!(sql.contains("server_instance"));
        assert!(sql.contains("remote_port"));
        assert!(sql.contains("assigned_difficulty"));
        assert!(sql.contains("difficulty_updated_at"));
        assert!(sql.contains("share_events"));
        assert!(sql.contains("session_id uuid references sessions(id)"));
        assert!(sql.contains("sessions_active_started_idx"));
        assert!(sql.contains("sessions_instance_active_idx"));
        assert!(sql.contains("share_events_session_created_idx"));
        assert!(sql.contains("shares_session_created_idx"));
    }

    #[test]
    fn job_heartbeat_migration_records_reason() {
        let sql = migration_by_version(10).unwrap().sql.to_lowercase();
        assert!(sql.contains("job_reason"));
        assert!(sql.contains("'tip_change'"));
        assert!(sql.contains("'heartbeat'"));
        assert!(sql.contains("jobs_reason_created_idx"));
    }

    #[tokio::test]
    async fn in_memory_repository_tracks_session_lifecycle() {
        let repo = InMemoryRepository::new();
        let session = SessionRecord {
            id: "019ebba5-5bf0-72e0-9b3d-bdd6475186cb".to_owned(),
            miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worker_name: "rig-a".to_owned(),
            remote_addr: "203.0.113.10".to_owned(),
            remote_port: 43_210,
            user_agent: Some("csd-pool-miner/v0.2.3".to_owned()),
            extranonce1: "01000000".to_owned(),
            server_session_id: 42,
            server_release: "csd-pool@revision".to_owned(),
            server_instance: "test".to_owned(),
            assigned_difficulty: 8.0,
        };
        repo.open_session(&session).await.unwrap();
        repo.update_session_difficulty(&session.id, 16.0)
            .await
            .unwrap();
        let mut expected = session.clone();
        expected.assigned_difficulty = 16.0;
        assert_eq!(
            repo.list_sessions().unwrap(),
            vec![(expected.clone(), true)]
        );
        repo.close_session(&session.id).await.unwrap();
        assert_eq!(
            repo.list_sessions().unwrap(),
            vec![(expected.clone(), false)]
        );
        let mut stale = expected;
        stale.id = "019ebba5-5bf0-72e0-9b3d-bdd6475186cc".to_owned();
        repo.open_session(&stale).await.unwrap();
        assert_eq!(repo.close_stale_sessions("other").await.unwrap(), 0);
        assert_eq!(repo.close_stale_sessions("test").await.unwrap(), 1);
        assert!(
            repo.list_sessions()
                .unwrap()
                .iter()
                .all(|(_session, active)| !active)
        );
    }

    #[tokio::test]
    async fn postgres_session_observability_round_trip_when_configured() {
        let Ok(database_url) = std::env::var("CSD_POOL_TEST_DATABASE_URL") else {
            return;
        };
        let repo = PgRepository::connect(&database_url).await.unwrap();
        run_migrations(repo.pool()).await.unwrap();
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            & 0xffff_ffff_ffff;
        let session_id = format!("00000000-0000-4000-8000-{suffix:012x}");
        let miner = "0123456789abcdef0123456789abcdef01234567";
        let session = SessionRecord {
            id: session_id.clone(),
            miner: miner.to_owned(),
            worker_name: "integration-rig".to_owned(),
            remote_addr: "203.0.113.10".to_owned(),
            remote_port: 43_210,
            user_agent: Some("csd-pool-miner/integration".to_owned()),
            extranonce1: "01000000".to_owned(),
            server_session_id: 42,
            server_release: "csd-pool@integration".to_owned(),
            server_instance: "integration".to_owned(),
            assigned_difficulty: 8.0,
        };
        repo.open_session(&session).await.unwrap();
        repo.update_session_difficulty(&session_id, 16.0)
            .await
            .unwrap();
        let job = JobRecord {
            job_id: format!("session-integration-{suffix:012x}"),
            prev_hash_be_hex: "00".repeat(32),
            version_hex: "20000000".to_owned(),
            nbits_hex: "1d00ffff".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            network_target: [0xff; 32],
            share_target: [0xff; 32],
            coinb1_hex: "aa".to_owned(),
            coinb2_hex: "bb".to_owned(),
            merkle_branches_hex: vec![],
            clean_jobs: true,
            job_reason: "tip_change".to_owned(),
        };
        repo.upsert_job(&job).await.unwrap();
        assert!(
            repo.insert_share(&ShareRecord {
                session_id: Some(session_id.clone()),
                miner: miner.to_owned(),
                worker_name: session.worker_name.clone(),
                job_id: job.job_id.clone(),
                difficulty: 8.0,
                hash: [1; 32],
                extranonce2_hex: "01020304".to_owned(),
                ntime_hex: "665544cc".to_owned(),
                nonce_hex: "00000001".to_owned(),
                is_block_candidate: false,
            })
            .await
            .unwrap()
        );
        repo.insert_share_event(&ShareEventRecord {
            session_id: Some(session_id.clone()),
            miner: miner.to_owned(),
            worker_name: session.worker_name.clone(),
            job_id: Some(job.job_id),
            kind: "rejected".to_owned(),
            reason: "low_difficulty".to_owned(),
        })
        .await
        .unwrap();

        let versions = repo.session_version_summaries().await.unwrap();
        let version = versions
            .iter()
            .find(|version| version.user_agent == "csd-pool-miner/integration")
            .unwrap();
        assert_eq!(version.active_sessions, 1);
        assert_eq!(version.server_instance, "integration");
        assert_eq!(version.accepted_shares_1h, 1);
        assert_eq!(version.rejected_shares_1h, 1);
        let recent = repo.recent_stratum_sessions(20).await.unwrap();
        let observed = recent
            .iter()
            .find(|observed| observed.id == session_id)
            .unwrap();
        assert_eq!(observed.remote_port, Some(43_210));
        assert_eq!(observed.server_session_id, Some(42));
        assert_eq!(observed.server_instance, "integration");
        assert_eq!(observed.assigned_difficulty, 16.0);
        assert_eq!(observed.accepted_shares, 1);
        assert_eq!(observed.rejected_shares, 1);

        repo.close_session(&session_id).await.unwrap();
        let recent = repo.recent_stratum_sessions(20).await.unwrap();
        assert!(
            recent
                .iter()
                .find(|observed| observed.id == session_id)
                .unwrap()
                .ended_at
                .is_some()
        );
    }

    #[test]
    fn converts_postgres_ledger_rows_to_domain_entries() {
        let entry = LedgerEntry::try_from(LedgerEntryRow {
            miner: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            amount_base_units: "123".to_owned(),
            kind: "reward_immature".to_owned(),
            ref_type: "block".to_owned(),
            ref_id: "block-1".to_owned(),
        })
        .unwrap();

        assert_eq!(entry.amount_base_units, 123);
        assert_eq!(entry.kind, csd_pool_accounting::LedgerKind::RewardImmature);
    }

    #[test]
    fn converts_postgres_balance_and_recipient_rows() {
        let balance = MinerBalance::try_from(BalanceRow {
            miner: "a".to_owned(),
            address: "addr-a".to_owned(),
            confirmed_base_units: "250000000".to_owned(),
        })
        .unwrap();
        assert_eq!(balance.confirmed_base_units, 250_000_000);

        let recipient = PayoutRecipient::try_from(PayoutRecipientRow {
            batch_id: "batch-1".to_owned(),
            miner: "a".to_owned(),
            address: "addr-a".to_owned(),
            amount_base_units: "100".to_owned(),
        })
        .unwrap();
        assert_eq!(recipient.amount_base_units, 100);
    }

    #[test]
    fn share_quality_alert_filters_by_rates() {
        let alert = share_quality_alert_from_counts(
            ShareQualityRow {
                miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                worker_name: "rig-a".to_owned(),
                accepted_count: "80".to_owned(),
                rejected_count: "20".to_owned(),
                stale_count: "0".to_owned(),
            },
            10,
            0.05,
            0.02,
        )
        .unwrap()
        .unwrap();

        assert_eq!(alert.rejected_count, 20);
        assert!((alert.reject_rate - 0.2).abs() < f64::EPSILON);

        let healthy = share_quality_alert_from_counts(
            ShareQualityRow {
                miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                worker_name: "rig-a".to_owned(),
                accepted_count: "99".to_owned(),
                rejected_count: "1".to_owned(),
                stale_count: "0".to_owned(),
            },
            10,
            0.05,
            0.02,
        )
        .unwrap();
        assert!(healthy.is_none());
    }

    #[tokio::test]
    async fn in_memory_mining_repository_stores_jobs_and_deduplicates_shares() {
        let repo = InMemoryRepository::new();
        let job = JobRecord {
            job_id: "job-1".to_owned(),
            prev_hash_be_hex: "00".repeat(32),
            version_hex: "20000000".to_owned(),
            nbits_hex: "207fffff".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            network_target: [0; 32],
            share_target: [0xff; 32],
            coinb1_hex: "aa".to_owned(),
            coinb2_hex: "bb".to_owned(),
            merkle_branches_hex: vec![],
            clean_jobs: true,
            job_reason: "tip_change".to_owned(),
        };
        repo.upsert_job(&job).await.unwrap();
        repo.upsert_job(&job).await.unwrap();
        assert_eq!(repo.list_jobs().unwrap(), vec![job.clone()]);
        let latest_job = repo.latest_job().await.unwrap().unwrap();
        assert_eq!(latest_job.job_id, "job-1");
        assert_eq!(latest_job.age_seconds, 0);

        let share = ShareRecord {
            session_id: None,
            miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worker_name: "rig-a".to_owned(),
            job_id: job.job_id.clone(),
            difficulty: 8.0,
            hash: [1; 32],
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            nonce_hex: "00000001".to_owned(),
            is_block_candidate: false,
        };

        assert!(repo.insert_share(&share).await.unwrap());
        assert!(!repo.insert_share(&share).await.unwrap());
        assert_eq!(repo.list_shares().unwrap(), vec![share.clone()]);

        repo.insert_share_event(&ShareEventRecord {
            session_id: None,
            miner: share.miner.clone(),
            worker_name: share.worker_name.clone(),
            job_id: Some(share.job_id.clone()),
            kind: "rejected".to_owned(),
            reason: "low_difficulty".to_owned(),
        })
        .await
        .unwrap();
        let quality = repo
            .list_share_quality_alerts(10, 1, 0.25, 1.0, 10)
            .await
            .unwrap();
        assert_eq!(quality.len(), 1);
        assert_eq!(quality[0].rejected_count, 1);

        let block = BlockCandidateRecord {
            hash_hex: "11".repeat(32),
            job_id: "job-1".to_owned(),
            miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worker_name: "rig-a".to_owned(),
            reward_base_units: 5_000_000_000,
            effort_pct: 87.5,
            candidate_payload_json: serde_json::json!({"header_hex": "aa"}),
            submit_response_json: serde_json::json!({"ok": true}),
        };
        assert!(repo.record_block_candidate(&block).await.unwrap());
        assert!(!repo.record_block_candidate(&block).await.unwrap());
        assert_eq!(repo.list_block_candidates().unwrap(), vec![block]);
        assert!(
            repo.list_block_submission_alerts(10, 10)
                .await
                .unwrap()
                .is_empty()
        );

        let pending = repo.list_blocks_to_reconcile(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, "submitted");
        assert_eq!(pending[0].effort_pct, 87.5);

        let updated = repo
            .update_block_status(&BlockStatusUpdate {
                hash_hex: "11".repeat(32),
                status: "confirmed".to_owned(),
                height: Some(42),
                confirmations: 10,
                reward_base_units: 5_000_000_000,
            })
            .await
            .unwrap();
        assert!(updated);
        assert!(repo.list_blocks_to_reconcile(10).await.unwrap().is_empty());

        let reward_blocks = repo.list_confirmed_unsettled_blocks(10).await.unwrap();
        assert_eq!(reward_blocks.len(), 1);
        assert_eq!(reward_blocks[0].job_id, job.job_id);
        assert_eq!(reward_blocks[0].reward_base_units, 5_000_000_000);

        let weights = repo.share_weights_for_job(&job.job_id).await.unwrap();
        assert_eq!(
            weights,
            vec![ShareWeight {
                miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                difficulty: 8,
            }]
        );

        let immature_entry = LedgerEntry {
            miner: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            amount_base_units: 4_950_000_000,
            kind: LedgerKind::RewardImmature,
            ref_type: "block".to_owned(),
            ref_id: "11".repeat(32),
        };
        assert!(
            repo.list_mature_reward_entries(10, 10)
                .await
                .unwrap()
                .is_empty()
        );
        repo.append_ledger_entries(&[immature_entry]).unwrap();
        assert!(
            repo.list_confirmed_unsettled_blocks(10)
                .await
                .unwrap()
                .is_empty()
        );

        let mature_entries = repo.list_mature_reward_entries(10, 10).await.unwrap();
        assert_eq!(mature_entries.len(), 1);
        assert_eq!(mature_entries[0].kind, LedgerKind::RewardMature);
        assert_eq!(mature_entries[0].amount_base_units, 4_950_000_000);
        repo.append_ledger_entries(&mature_entries).unwrap();
        assert!(
            repo.list_mature_reward_entries(10, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            repo.list_balances().unwrap()[0].confirmed_base_units,
            4_950_000_000
        );

        repo.update_block_status(&BlockStatusUpdate {
            hash_hex: "11".repeat(32),
            status: "orphaned".to_owned(),
            height: Some(42),
            confirmations: 0,
            reward_base_units: 5_000_000_000,
        })
        .await
        .unwrap();

        let reversal_entries = repo.list_orphan_reversal_entries(10).await.unwrap();
        assert_eq!(reversal_entries.len(), 1);
        assert_eq!(reversal_entries[0].kind, LedgerKind::RewardOrphanReversal);
        assert_eq!(reversal_entries[0].amount_base_units, -4_950_000_000);
        repo.append_ledger_entries(&reversal_entries).unwrap();
        assert!(
            repo.list_orphan_reversal_entries(10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(repo.list_balances().unwrap()[0].confirmed_base_units, 0);
    }

    #[tokio::test]
    async fn in_memory_repository_lists_failed_block_submission_alerts() {
        let repo = InMemoryRepository::new();
        let block = BlockCandidateRecord {
            hash_hex: "33".repeat(32),
            job_id: "job-failed".to_owned(),
            miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worker_name: "rig-a".to_owned(),
            reward_base_units: 0,
            effort_pct: 0.0,
            candidate_payload_json: serde_json::json!({"header_hex": "aa"}),
            submit_response_json: serde_json::json!({"ok": false, "error": "rejected"}),
        };
        assert!(repo.record_block_candidate(&block).await.unwrap());

        let alerts = repo.list_block_submission_alerts(10, 10).await.unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].hash_hex, "33".repeat(32));
        assert_eq!(alerts[0].submit_ok, Some(false));
        assert_eq!(alerts[0].reason, "submit_response_not_ok");
        assert!(repo.list_blocks_to_reconcile(10).await.unwrap().is_empty());
    }

    #[test]
    fn candidate_status_distinguishes_rejection_from_ambiguous_transport_failure() {
        let mut block = BlockCandidateRecord {
            hash_hex: "44".repeat(32),
            job_id: "job-status".to_owned(),
            miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worker_name: "rig-a".to_owned(),
            reward_base_units: 0,
            effort_pct: 1.0,
            candidate_payload_json: serde_json::json!({}),
            submit_response_json: serde_json::json!({
                "ok": false,
                "http_status": 409,
                "error": "stale tip"
            }),
        };
        assert_eq!(block_candidate_initial_status(&block), "orphaned");

        block.submit_response_json = serde_json::json!({
            "ok": false,
            "transport_error": "connection reset",
            "retryable": true
        });
        assert_eq!(block_candidate_initial_status(&block), "submitted");

        block.submit_response_json = serde_json::json!({
            "ok": false,
            "parallel_submit": {
                "status": "both_failed",
                "accepted_by": "none",
                "primary_submit": {"outcome": "timeout", "response": null},
                "secondary_submit": {"outcome": "rejected", "response": {"ok": false}}
            }
        });
        assert_eq!(block_candidate_initial_status(&block), "submitted");

        block.submit_response_json = serde_json::json!({
            "ok": false,
            "parallel_submit": {
                "status": "both_failed",
                "accepted_by": "none",
                "primary_submit": {"outcome": "rejected", "response": {"ok": false}},
                "secondary_submit": {"outcome": "transport_error", "response": null}
            }
        });
        assert_eq!(block_candidate_initial_status(&block), "submitted");

        block.submit_response_json = serde_json::json!({"ok": true});
        assert_eq!(block_candidate_initial_status(&block), "submitted");

        block.submit_response_json = serde_json::json!({
            "ok": true,
            "parallel_submit": {
                "status": "secondary_accepted_authority_failed",
                "accepted_by": "secondary",
                "primary_submit": {"outcome": "rejected"},
                "secondary_submit": {"outcome": "accepted"}
            }
        });
        assert_eq!(
            block_candidate_initial_status(&block),
            "submitted_secondary"
        );

        block.submit_response_json = serde_json::json!({
            "ok": true,
            "parallel_submit": {
                "status": "authority_accepted_secondary_local_canonical_relay_failed",
                "accepted_by": "authority",
                "primary_submit": {"response": {"ok": true}},
                "secondary_submit": {
                    "response": {
                        "ok": false,
                        "status": "accepted_local_relay_failed",
                        "node_observability": {
                            "local_canonical": true,
                            "relay_ack": {"ok": false}
                        }
                    }
                }
            }
        });
        assert_eq!(block_candidate_initial_status(&block), "submitted_degraded");

        block.submit_response_json = serde_json::json!({
            "ok": false,
            "status": "accepted_local_relay_failed",
            "error": "InsufficientPeers",
            "node_observability": {
                "local_canonical": true,
                "relay_ack": {"ok": false, "status": "insufficient_peers"}
            }
        });
        assert_eq!(block_candidate_initial_status(&block), "relay_failed");

        block.submit_response_json = serde_json::json!({
            "ok": false,
            "parallel_submit": {
                "aggregate_status": "both_local_canonical_relay_failed",
                "primary_submit": {
                    "outcome": "rejected",
                    "response": {
                        "ok": false,
                        "status": "accepted_local_relay_failed",
                        "node_observability": {
                            "local_canonical": true,
                            "relay_ack": {"ok": false}
                        }
                    }
                },
                "secondary_submit": {
                    "outcome": "rejected",
                    "response": {
                        "ok": false,
                        "status": "accepted_local_relay_failed",
                        "node_observability": {
                            "local_canonical": true,
                            "relay_ack": {"ok": false}
                        }
                    }
                }
            }
        });
        assert_eq!(block_candidate_initial_status(&block), "relay_failed");
    }

    #[test]
    fn in_memory_repository_stores_ledger_and_balances() {
        let repo = InMemoryRepository::new();
        repo.append_ledger_entries(&[LedgerEntry {
            miner: Some("a".to_owned()),
            amount_base_units: 100,
            kind: csd_pool_accounting::LedgerKind::RewardImmature,
            ref_type: "block".to_owned(),
            ref_id: "block-1".to_owned(),
        }])
        .unwrap();
        repo.set_balance(MinerBalance {
            miner: "a".to_owned(),
            address: "addr-a".to_owned(),
            confirmed_base_units: 100,
        })
        .unwrap();

        assert_eq!(repo.list_ledger_entries().unwrap().len(), 1);
        assert_eq!(repo.list_balances().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn in_memory_payout_repository_locks_and_unlocks_balances() {
        let repo = InMemoryRepository::new();
        repo.set_balance(MinerBalance {
            miner: "a".to_owned(),
            address: "a".to_owned(),
            confirmed_base_units: 250,
        })
        .unwrap();
        let balances = repo.list_payable_balances(100, 10).await.unwrap();
        assert_eq!(balances.len(), 1);

        let draft = csd_pool_accounting::payout_batch_draft(
            "batch-1",
            csd_pool_accounting::PayoutSelection {
                recipients: vec![PayoutRecipient {
                    miner: "a".to_owned(),
                    address: "a".to_owned(),
                    amount_base_units: 250,
                }],
                total_base_units: 250,
            },
        );
        assert!(repo.create_locked_payout_batch(draft).await.unwrap());
        assert_eq!(repo.list_balances().unwrap()[0].confirmed_base_units, 0);
        assert_eq!(repo.active_payout_total_today().await.unwrap(), 250);
        assert_eq!(
            repo.list_ledger_entries().unwrap()[0].kind,
            LedgerKind::PayoutLock
        );

        let batch = repo
            .list_payout_batches_by_status(&["created"], 10)
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(repo.unlock_failed_payout(&batch).await.unwrap(), 1);
        repo.mark_payout_failed(&batch.batch_id, "test")
            .await
            .unwrap();
        assert_eq!(repo.active_payout_total_today().await.unwrap(), 0);
        assert_eq!(repo.list_balances().unwrap()[0].confirmed_base_units, 250);
        assert_eq!(
            repo.list_ledger_entries().unwrap()[1].kind,
            LedgerKind::PayoutFailedUnlock
        );
    }

    #[tokio::test]
    async fn in_memory_payout_repository_requires_approval_before_signing() {
        let repo = InMemoryRepository::new();
        repo.set_balance(MinerBalance {
            miner: "a".to_owned(),
            address: "a".to_owned(),
            confirmed_base_units: 250,
        })
        .unwrap();
        let draft = csd_pool_accounting::payout_batch_draft(
            "batch-approval",
            csd_pool_accounting::PayoutSelection {
                recipients: vec![PayoutRecipient {
                    miner: "a".to_owned(),
                    address: "a".to_owned(),
                    amount_base_units: 250,
                }],
                total_base_units: 250,
            },
        );

        assert!(
            repo.create_locked_payout_batch_with_status(draft, "needs_approval")
                .await
                .unwrap()
        );
        assert!(
            repo.list_payout_batches_by_status(&["created"], 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(repo.active_payout_total_today().await.unwrap(), 250);
        assert!(repo.mark_payout_approved("batch-approval").await.unwrap());

        let batch = repo
            .list_payout_batches_by_status(&["created"], 10)
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(batch.batch_id, "batch-approval");
    }

    #[tokio::test]
    async fn in_memory_payout_repository_records_audit_events() {
        let repo = InMemoryRepository::new();
        repo.append_payout_audit_event(&PayoutAuditEvent {
            batch_id: "batch-a".to_owned(),
            actor: "test".to_owned(),
            action: "approve".to_owned(),
            details: serde_json::json!({ "ok": true }),
            created_at: None,
        })
        .await
        .unwrap();
        repo.append_payout_audit_event(&PayoutAuditEvent {
            batch_id: "batch-b".to_owned(),
            actor: "test".to_owned(),
            action: "cancel".to_owned(),
            details: serde_json::json!({ "reason": "manual" }),
            created_at: None,
        })
        .await
        .unwrap();

        let all = repo.list_payout_audit_events(None, 10).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].batch_id, "batch-b");
        let filtered = repo
            .list_payout_audit_events(Some("batch-a"), 10)
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].action, "approve");
    }

    #[tokio::test]
    async fn in_memory_payout_repository_cancels_and_retries_batches() {
        let repo = InMemoryRepository::new();
        repo.set_balance(MinerBalance {
            miner: "a".to_owned(),
            address: "a".to_owned(),
            confirmed_base_units: 500,
        })
        .unwrap();
        let draft = csd_pool_accounting::payout_batch_draft(
            "batch-1",
            csd_pool_accounting::PayoutSelection {
                recipients: vec![PayoutRecipient {
                    miner: "a".to_owned(),
                    address: "a".to_owned(),
                    amount_base_units: 250,
                }],
                total_base_units: 250,
            },
        );
        assert!(repo.create_locked_payout_batch(draft).await.unwrap());
        assert_eq!(repo.list_balances().unwrap()[0].confirmed_base_units, 250);

        let cancelled = repo
            .cancel_payout("batch-1", "operator test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert_eq!(repo.list_balances().unwrap()[0].confirmed_base_units, 500);

        let retry = repo
            .retry_failed_payout("batch-1", "batch-2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retry.batch_id, "batch-2");
        assert_eq!(retry.status, "created");
        assert_eq!(repo.list_balances().unwrap()[0].confirmed_base_units, 250);
    }

    #[tokio::test]
    async fn in_memory_control_repository_toggles_payouts() {
        let repo = InMemoryRepository::new();
        assert!(!repo.payouts_enabled().await.unwrap());
        repo.set_payouts_enabled(true).await.unwrap();
        assert!(repo.payouts_enabled().await.unwrap());
        repo.set_payouts_enabled(false).await.unwrap();
        assert!(!repo.payouts_enabled().await.unwrap());
        repo.set_payouts_enabled(true).await.unwrap();
        assert!(repo.payouts_enabled().await.unwrap());
    }

    #[tokio::test]
    async fn in_memory_monitoring_repository_records_alerts_and_samples() {
        let repo = InMemoryRepository::new();
        repo.insert_node_sample(&NodeSampleRecord {
            node_name: "node:a".to_owned(),
            height: Some(42),
            chainwork: Some("abc".to_owned()),
            peers: Some(8),
            mempool_size: None,
            rpc_ms: Some(12.5),
            ok: true,
            sampled_at: None,
        })
        .await
        .unwrap();
        assert_eq!(repo.latest_node_samples(10).await.unwrap().len(), 1);
        assert!(repo.list_offline_workers(15, 10).await.unwrap().is_empty());
        assert!(repo.accepted_share_gap(10).await.unwrap().is_none());

        repo.upsert_alert(&AlertEvent {
            fingerprint: "health:node:a".to_owned(),
            severity: "critical".to_owned(),
            status: "active".to_owned(),
            kind: "service_health".to_owned(),
            subject: "node:a".to_owned(),
            message: "node down".to_owned(),
            first_seen_at: None,
            last_seen_at: None,
            resolved_at: None,
            details: serde_json::json!({"ok": false}),
        })
        .await
        .unwrap();
        assert_eq!(repo.list_alerts(Some("active"), 10).await.unwrap().len(), 1);
        assert!(repo.resolve_alert("health:node:a").await.unwrap());
        assert!(
            repo.list_alerts(Some("active"), 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            repo.list_alerts(Some("resolved"), 10).await.unwrap().len(),
            1
        );
    }

    #[test]
    fn in_memory_repository_rejects_duplicate_payout_batch() {
        let repo = InMemoryRepository::new();
        let draft = PayoutBatchDraft {
            batch_id: "batch-1".to_owned(),
            recipients: vec![],
            total_base_units: 0,
            lock_entries: vec![],
        };
        repo.create_payout_batch(draft.clone()).unwrap();
        assert!(matches!(
            repo.create_payout_batch(draft),
            Err(RepositoryError::DuplicatePayoutBatch(_))
        ));
    }
}
