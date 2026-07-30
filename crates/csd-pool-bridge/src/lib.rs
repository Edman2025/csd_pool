#![allow(clippy::collapsible_if)] // Production remains on Rust 1.86, before stable let chains.

mod replacement_latency;

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions, symlink_metadata};
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use csd_pool_consensus::{
    SubmitSolution, VerifiedShare, WorkTemplate, coinbase_bytes, compose_extranonce,
    difficulty_for_target, parse_le_u32_hex_bytes, parse_u32_hex, verify_share_with_difficulty,
};
use csd_pool_db::{
    JobRecord, MiningRepository, PgRepository, SessionRecord, ShareEventRecord, ShareRecord,
};
use csd_pool_node::{
    BlockCandidateSubmitRequest, CandidateTemplateMaterial, CsdNodeClient, LiveTemplateProvider,
    NodeHealth, PoolJob, StatelessBlockCandidateSubmitRequest, StaticTemplateProvider,
    SubmitBlockResponse, TemplateProvider,
};
use csd_pool_protocol::{
    Request, Response, SubmitParams, notify, response_error, response_ok, serialize_line,
    set_difficulty,
};
use csd_pool_state::SharedPoolState;
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio::time::{Instant as TokioInstant, MissedTickBehavior, interval, timeout};
use tracing::{debug, info, warn};
use uuid::Uuid;

use replacement_latency::{FanoutOutcome, ReplacementLatencyTelemetry, UpstreamClass};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
const STALE_JOB_RECONNECT_THRESHOLD: u32 = 3;
const CLIENT_WRITE_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] csd_pool_protocol::ProtocolError),
    #[error("node error: {0}")]
    Node(#[from] csd_pool_node::NodeError),
    #[error("config error: {0}")]
    Config(#[from] csd_pool_config::ConfigError),
    #[error("invalid bridge config: {0}")]
    InvalidConfig(String),
    #[error("repository error: {0}")]
    Repository(#[from] csd_pool_db::RepositoryError),
    #[error("client write exceeded the bounded deadline")]
    ClientWriteTimeout,
}

pub type Result<T> = std::result::Result<T, BridgeError>;

pub async fn run_stratum_server(listen: &str, pool_state: SharedPoolState) -> Result<()> {
    let provider = template_provider_from_env()?;
    let tip_sentinel = tip_sentinel_from_env()?;
    let block_submitter = block_submitter_from_env()?;
    let repository = mining_repository_from_env().await?;
    let abuse = Arc::new(AbuseManager::new(abuse_config_from_env()?));
    let vardiff = vardiff_config_from_env()?;
    run_stratum_server_with_provider_abuse_and_sentinel(
        listen,
        pool_state,
        provider,
        tip_sentinel,
        repository,
        block_submitter,
        abuse,
        vardiff,
    )
    .await
}

pub async fn run_stratum_server_with_provider(
    listen: &str,
    pool_state: SharedPoolState,
    provider: Arc<dyn TemplateProvider>,
    repository: Option<Arc<dyn MiningRepository>>,
    block_submitter: Option<Arc<dyn BlockSubmitter>>,
) -> Result<()> {
    let abuse = Arc::new(AbuseManager::default());
    let vardiff = VardiffConfig::default();
    run_stratum_server_with_provider_and_abuse(
        listen,
        pool_state,
        provider,
        repository,
        block_submitter,
        abuse,
        vardiff,
    )
    .await
}

pub async fn run_stratum_server_with_provider_and_abuse(
    listen: &str,
    pool_state: SharedPoolState,
    provider: Arc<dyn TemplateProvider>,
    repository: Option<Arc<dyn MiningRepository>>,
    block_submitter: Option<Arc<dyn BlockSubmitter>>,
    abuse: Arc<AbuseManager>,
    vardiff: VardiffConfig,
) -> Result<()> {
    run_stratum_server_with_provider_abuse_and_sentinel(
        listen,
        pool_state,
        provider,
        None,
        repository,
        block_submitter,
        abuse,
        vardiff,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_stratum_server_with_provider_abuse_and_sentinel(
    listen: &str,
    pool_state: SharedPoolState,
    provider: Arc<dyn TemplateProvider>,
    tip_sentinel: Option<Arc<dyn JobTipSentinel>>,
    repository: Option<Arc<dyn MiningRepository>>,
    block_submitter: Option<Arc<dyn BlockSubmitter>>,
    abuse: Arc<AbuseManager>,
    vardiff: VardiffConfig,
) -> Result<()> {
    let jobs = SharedJobWatch::start_with_sentinel(
        provider,
        tip_sentinel,
        repository.clone(),
        pool_state.clone(),
    )
    .await?;
    let listener = TcpListener::bind(listen).await?;
    info!(listen, "csd stratum bridge listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        let permit = match abuse.try_open(peer.ip()) {
            Ok(permit) => permit,
            Err(reject) => {
                warn!(%peer, reason = ?reject, "stratum connection rejected");
                continue;
            }
        };
        let pool_state = pool_state.clone();
        let job_rx = jobs.subscribe();
        let retained_jobs = jobs.retained_jobs();
        let repository = repository.clone();
        let block_submitter = block_submitter.clone();
        let abuse = abuse.clone();
        let vardiff = vardiff.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(
                stream,
                peer,
                pool_state,
                job_rx,
                retained_jobs,
                repository,
                block_submitter,
                abuse,
                vardiff,
                permit,
            )
            .await
            {
                warn!(%peer, %err, "client session ended with error");
            }
        });
    }
}

struct SharedJobWatch {
    sender: watch::Sender<Arc<PoolJob>>,
    retained_jobs: RetainedJobs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobReason {
    TipChange,
    Heartbeat,
}

impl JobReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::TipChange => "tip_change",
            Self::Heartbeat => "heartbeat",
        }
    }

    fn from_job(job: &PoolJob) -> Self {
        if job.notify.clean_jobs {
            Self::TipChange
        } else {
            Self::Heartbeat
        }
    }
}

#[derive(Clone)]
struct RetainedJob {
    job: Arc<PoolJob>,
    published_at: TokioInstant,
    clean_generation: u64,
}

#[derive(Clone)]
struct RetainedJobs {
    inner: Arc<RwLock<HashMap<String, RetainedJob>>>,
    invalidated_parents: Arc<RwLock<HashSet<[u8; 32]>>>,
    current_clean_generation: Arc<AtomicU64>,
    replacement_latency: Arc<ReplacementLatencyTelemetry>,
}

impl Default for RetainedJobs {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            invalidated_parents: Arc::new(RwLock::new(HashSet::new())),
            current_clean_generation: Arc::new(AtomicU64::new(0)),
            replacement_latency: ReplacementLatencyTelemetry::disabled(),
        }
    }
}

impl RetainedJobs {
    #[cfg(test)]
    fn with_initial(job: Arc<PoolJob>) -> Self {
        Self::with_initial_and_telemetry(job, ReplacementLatencyTelemetry::disabled())
    }

    fn with_initial_and_telemetry(
        job: Arc<PoolJob>,
        replacement_latency: Arc<ReplacementLatencyTelemetry>,
    ) -> Self {
        let jobs = Self {
            replacement_latency,
            ..Self::default()
        };
        jobs.current_clean_generation.store(1, Ordering::Release);
        jobs.inner.write().expect("retained jobs lock").insert(
            job.template.job_id.clone(),
            RetainedJob {
                job,
                published_at: TokioInstant::now(),
                clean_generation: 1,
            },
        );
        jobs
    }

    fn replacement_latency(&self) -> Arc<ReplacementLatencyTelemetry> {
        self.replacement_latency.clone()
    }

    fn get(&self, job_id: &str) -> Option<Arc<PoolJob>> {
        self.inner
            .read()
            .expect("retained jobs lock")
            .get(job_id)
            .map(|entry| entry.job.clone())
    }

    fn get_for_submit(&self, job_id: &str) -> Option<Arc<PoolJob>> {
        let job = self.get(job_id)?;
        (!self.parent_is_invalidated(&job.template.prev)).then_some(job)
    }

    fn job_parent_is_invalidated(&self, job_id: &str) -> bool {
        self.get(job_id)
            .is_some_and(|job| self.parent_is_invalidated(&job.template.prev))
    }

    fn clean_generation(&self, job_id: &str) -> Option<u64> {
        self.inner
            .read()
            .expect("retained jobs lock")
            .get(job_id)
            .map(|entry| entry.clean_generation)
    }

    fn current_clean_generation(&self) -> u64 {
        self.current_clean_generation.load(Ordering::Acquire)
    }

    fn stale_generation_class(
        &self,
        job_id: &str,
        last_delivered_generation: u64,
    ) -> StaleGenerationClass {
        let current_generation = self.current_clean_generation();
        if last_delivered_generation < current_generation {
            return StaleGenerationClass::NotifyDeliveryLag;
        }
        if self
            .clean_generation(job_id)
            .is_some_and(|submitted_generation| submitted_generation < current_generation)
        {
            return StaleGenerationClass::MinerSwitchLag;
        }
        StaleGenerationClass::Unclassified
    }

    fn parent_is_invalidated(&self, parent: &[u8; 32]) -> bool {
        self.invalidated_parents
            .read()
            .expect("invalidated parents lock")
            .contains(parent)
    }

    fn publish(&self, job: Arc<PoolJob>, reason: JobReason, retention: Duration) {
        let now = TokioInstant::now();
        let clean_generation = match reason {
            JobReason::TipChange => self
                .current_clean_generation
                .fetch_add(1, Ordering::AcqRel)
                .saturating_add(1),
            JobReason::Heartbeat => self.current_clean_generation(),
        };
        let mut jobs = self.inner.write().expect("retained jobs lock");
        jobs.retain(|_, entry| now.saturating_duration_since(entry.published_at) <= retention);
        let retained_parents = jobs
            .values()
            .map(|entry| entry.job.template.prev)
            .collect::<HashSet<_>>();
        let mut invalidated = self
            .invalidated_parents
            .write()
            .expect("invalidated parents lock");
        if reason == JobReason::TipChange {
            invalidated.extend(
                retained_parents
                    .iter()
                    .copied()
                    .filter(|parent| parent != &job.template.prev),
            );
        }
        invalidated.retain(|parent| retained_parents.contains(parent));
        invalidated.remove(&job.template.prev);
        jobs.insert(
            job.template.job_id.clone(),
            RetainedJob {
                job,
                published_at: now,
                clean_generation,
            },
        );
    }

    fn invalidate_parent(&self, parent: &[u8; 32]) -> usize {
        let invalidated = self
            .inner
            .read()
            .expect("retained jobs lock")
            .values()
            .filter(|entry| &entry.job.template.prev == parent)
            .count();
        self.invalidated_parents
            .write()
            .expect("invalidated parents lock")
            .insert(*parent);
        invalidated
    }

    fn parent_is_retained(&self, parent: &[u8; 32]) -> bool {
        !self.parent_is_invalidated(parent)
            && self
                .inner
                .read()
                .expect("retained jobs lock")
                .values()
                .any(|entry| &entry.job.template.prev == parent)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.read().expect("retained jobs lock").len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StaleGenerationClass {
    NotifyDeliveryLag,
    MinerSwitchLag,
    Unclassified,
}

impl StaleGenerationClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotifyDeliveryLag => "notify_delivery_lag",
            Self::MinerSwitchLag => "miner_switch_lag",
            Self::Unclassified => "unclassified",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedNodeTip {
    height: u64,
    tip_hex: String,
    chainwork_hex: String,
}

impl ObservedNodeTip {
    fn from_health(health: &NodeHealth) -> std::result::Result<Self, String> {
        let height = health
            .height
            .ok_or_else(|| "tip sentinel health is missing height".to_owned())?;
        let tip_hex = canonical_health_hash(
            health
                .tip
                .as_deref()
                .ok_or_else(|| "tip sentinel health is missing tip".to_owned())?,
        )?;
        let chainwork_hex = canonical_chainwork(
            health
                .chainwork
                .as_deref()
                .ok_or_else(|| "tip sentinel health is missing chainwork".to_owned())?,
        )?;
        Ok(Self {
            height,
            tip_hex,
            chainwork_hex,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TipSentinelObservation {
    primary: ObservedNodeTip,
    secondary: ObservedNodeTip,
    observed_unix_ms: u64,
}

#[async_trait::async_trait]
trait JobTipSentinel: Send + Sync {
    async fn observe(&self) -> std::result::Result<TipSentinelObservation, String>;
}

#[derive(Clone)]
struct CsdJobTipSentinel {
    primary: CsdNodeClient,
    secondary: CsdNodeClient,
    timeout: Duration,
}

#[async_trait::async_trait]
impl JobTipSentinel for CsdJobTipSentinel {
    async fn observe(&self) -> std::result::Result<TipSentinelObservation, String> {
        let (primary, secondary) = tokio::join!(
            timeout(self.timeout, self.primary.health()),
            timeout(self.timeout, self.secondary.health())
        );
        let primary = primary
            .map_err(|_| "primary tip sentinel health timed out".to_owned())?
            .map_err(|error| error.to_string())?;
        let secondary = secondary
            .map_err(|_| "secondary tip sentinel health timed out".to_owned())?
            .map_err(|error| error.to_string())?;
        Ok(TipSentinelObservation {
            primary: ObservedNodeTip::from_health(&primary)?,
            secondary: ObservedNodeTip::from_health(&secondary)?,
            observed_unix_ms: unix_ms(),
        })
    }
}

fn canonical_health_hash(value: &str) -> std::result::Result<String, String> {
    let value = value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))
        .unwrap_or_else(|| value.trim());
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("tip sentinel tip is not canonical 32-byte hex".to_owned());
    }
    Ok(value.to_ascii_lowercase())
}

fn canonical_chainwork(value: &str) -> std::result::Result<String, String> {
    let value = value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))
        .unwrap_or_else(|| value.trim());
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("tip sentinel chainwork is not hex".to_owned());
    }
    let normalized = value.trim_start_matches('0');
    Ok(if normalized.is_empty() {
        "0".to_owned()
    } else {
        normalized.to_ascii_lowercase()
    })
}

fn chainwork_at_least(left: &str, right: &str) -> bool {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len() > right.len() || (left.len() == right.len() && left >= right)
}

fn tip_sentinel_invalidates_parent(
    current_parent: &[u8; 32],
    observation: &TipSentinelObservation,
) -> bool {
    let current_parent = hex::encode(current_parent);
    if observation.primary.tip_hex != current_parent {
        return true;
    }
    observation.secondary.height > observation.primary.height
        || (observation.secondary.height == observation.primary.height
            && observation.secondary.tip_hex != observation.primary.tip_hex
            && chainwork_at_least(
                &observation.secondary.chainwork_hex,
                &observation.primary.chainwork_hex,
            ))
}

impl SharedJobWatch {
    async fn start_with_sentinel(
        provider: Arc<dyn TemplateProvider>,
        tip_sentinel: Option<Arc<dyn JobTipSentinel>>,
        repository: Option<Arc<dyn MiningRepository>>,
        pool_state: SharedPoolState,
    ) -> Result<Self> {
        let requested_refresh_secs = env_u64("CSD_POOL_TEMPLATE_REFRESH_SECS", 2);
        let template_mode = std::env::var("CSD_POOL_TEMPLATE_MODE").unwrap_or_default();
        let refresh_secs =
            bounded_template_refresh_secs(requested_refresh_secs, template_mode.as_str());
        let heartbeat_secs = env_u64("CSD_POOL_JOB_HEARTBEAT_SECS", 120);
        let retention_secs = env_u64("CSD_POOL_JOB_RETENTION_SECS", 900)
            .max(heartbeat_secs.saturating_mul(2))
            .max(300);
        if refresh_secs != requested_refresh_secs.max(1) {
            warn!(
                requested_refresh_secs,
                refresh_secs, "limiting live template tip polling interval"
            );
        }
        info!(
            refresh_secs,
            heartbeat_secs,
            retention_secs,
            template_mode = template_mode.as_str(),
            "configured mining template refresh"
        );
        Self::start_with_policy_and_sentinel(
            provider,
            tip_sentinel,
            repository,
            pool_state,
            Duration::from_secs(refresh_secs),
            (heartbeat_secs > 0).then(|| Duration::from_secs(heartbeat_secs)),
            Duration::from_secs(retention_secs),
            Duration::from_millis(
                env_u64("CSD_POOL_SECONDARY_TIP_SENTINEL_POLL_MS", 250).clamp(100, 5_000),
            ),
        )
        .await
    }

    #[cfg(test)]
    async fn start_with_refresh(
        provider: Arc<dyn TemplateProvider>,
        repository: Option<Arc<dyn MiningRepository>>,
        refresh: Duration,
    ) -> Result<Self> {
        Self::start_with_policy(
            provider,
            repository,
            SharedPoolState::new(),
            refresh,
            Some(refresh),
            refresh.saturating_mul(8),
        )
        .await
    }

    #[cfg(test)]
    async fn start_with_policy(
        provider: Arc<dyn TemplateProvider>,
        repository: Option<Arc<dyn MiningRepository>>,
        pool_state: SharedPoolState,
        refresh: Duration,
        heartbeat: Option<Duration>,
        retention: Duration,
    ) -> Result<Self> {
        Self::start_with_policy_and_sentinel(
            provider, None, repository, pool_state, refresh, heartbeat, retention, refresh,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_with_policy_and_sentinel(
        provider: Arc<dyn TemplateProvider>,
        tip_sentinel: Option<Arc<dyn JobTipSentinel>>,
        repository: Option<Arc<dyn MiningRepository>>,
        pool_state: SharedPoolState,
        refresh: Duration,
        heartbeat: Option<Duration>,
        retention: Duration,
        sentinel_poll: Duration,
    ) -> Result<Self> {
        Self::start_with_policy_and_sentinel_and_telemetry(
            provider,
            tip_sentinel,
            repository,
            pool_state,
            refresh,
            heartbeat,
            retention,
            sentinel_poll,
            ReplacementLatencyTelemetry::from_env(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_with_policy_and_sentinel_and_telemetry(
        provider: Arc<dyn TemplateProvider>,
        tip_sentinel: Option<Arc<dyn JobTipSentinel>>,
        repository: Option<Arc<dyn MiningRepository>>,
        pool_state: SharedPoolState,
        refresh: Duration,
        heartbeat: Option<Duration>,
        retention: Duration,
        sentinel_poll: Duration,
        replacement_latency: Arc<ReplacementLatencyTelemetry>,
    ) -> Result<Self> {
        let mut initial_job = provider.current_job().await?;
        initial_job.notify.clean_jobs = true;
        let initial = Arc::new(initial_job);
        if let Some(repository) = repository.as_deref() {
            repository
                .upsert_job(&job_record_from_pool_job(&initial, JobReason::TipChange))
                .await?;
        }
        pool_state.record_job_notify(JobReason::TipChange.as_str());
        let retained_jobs =
            RetainedJobs::with_initial_and_telemetry(initial.clone(), replacement_latency.clone());
        let (sender, _) = watch::channel(initial);
        let refresh_sender = sender.clone();
        let refresh_retained_jobs = retained_jobs.clone();
        tokio::spawn(async move {
            let mut ticker = interval(refresh);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await;
            let mut sentinel_ticker = interval(sentinel_poll);
            sentinel_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            sentinel_ticker.tick().await;
            let mut last_publish_at = TokioInstant::now();
            let mut active_latency_event = None;
            loop {
                replacement_latency.expire();
                let sentinel_observation = if let Some(sentinel) = tip_sentinel.as_ref() {
                    tokio::select! {
                        _ = ticker.tick() => None,
                        _ = sentinel_ticker.tick() => {
                            match sentinel.observe().await {
                                Ok(observation) => Some(observation),
                                Err(error) => {
                                    warn!(%error, "secondary tip sentinel observation failed");
                                    continue;
                                }
                            }
                        }
                    }
                } else {
                    ticker.tick().await;
                    None
                };
                let current = refresh_sender.borrow().clone();
                let sentinel_forced_refresh =
                    sentinel_observation.as_ref().is_some_and(|observation| {
                        tip_sentinel_invalidates_parent(&current.template.prev, observation)
                    });
                if sentinel_forced_refresh {
                    let source = if sentinel_observation.as_ref().is_some_and(|observation| {
                        observation.primary.tip_hex != hex::encode(current.template.prev)
                    }) {
                        UpstreamClass::PrimaryAlreadyReady
                    } else {
                        UpstreamClass::IndependentSentinelAhead
                    };
                    active_latency_event = replacement_latency
                        .observe_upstream(&current.template.job_id, source)
                        .or(active_latency_event);
                    let invalidated =
                        refresh_retained_jobs.invalidate_parent(&current.template.prev);
                    warn!(
                        job_id = current.template.job_id,
                        parent = %hex::encode(current.template.prev),
                        invalidated_jobs = invalidated,
                        primary_height = sentinel_observation.as_ref().map(|value| value.primary.height),
                        secondary_height = sentinel_observation.as_ref().map(|value| value.secondary.height),
                        primary_tip = sentinel_observation.as_ref().map(|value| value.primary.tip_hex.as_str()),
                        secondary_tip = sentinel_observation.as_ref().map(|value| value.secondary.tip_hex.as_str()),
                        sentinel_observed_unix_ms = sentinel_observation.as_ref().map(|value| value.observed_unix_ms),
                        "tip sentinel invalidated stale A-parent jobs; waiting for an A template"
                    );
                }
                let heartbeat_due =
                    heartbeat.is_some_and(|heartbeat| last_publish_at.elapsed() >= heartbeat);
                let observed_tip = match provider.current_tip().await {
                    Ok(Some(tip))
                        if tip == current.template.prev
                            && !heartbeat_due
                            && !sentinel_forced_refresh =>
                    {
                        continue;
                    }
                    Ok(tip) => tip,
                    Err(err) => {
                        if let Some(event_id) = active_latency_event.take() {
                            replacement_latency.fail_event(event_id, "a_tip_read_failed");
                        }
                        warn!(%err, "mining tip refresh failed; retaining current job");
                        continue;
                    }
                };
                if observed_tip.is_some_and(|tip| tip != current.template.prev) {
                    active_latency_event = replacement_latency
                        .observe_upstream(
                            &current.template.job_id,
                            UpstreamClass::PrimaryAlreadyReady,
                        )
                        .or(active_latency_event);
                }
                let refresh_started = Instant::now();
                let mut next = match provider.current_job().await {
                    Ok(job) => job,
                    Err(err) => {
                        if let Some(event_id) = active_latency_event.take() {
                            replacement_latency.fail_event(event_id, "a_template_read_failed");
                        }
                        warn!(%err, "mining template refresh failed; retaining current job");
                        continue;
                    }
                };
                let latest_tip = match provider.current_tip().await {
                    Ok(tip) => tip.or(observed_tip),
                    Err(err) => {
                        if let Some(event_id) = active_latency_event.take() {
                            replacement_latency.fail_event(event_id, "a_tip_revalidation_failed");
                        }
                        warn!(
                            %err,
                            job_id = next.template.job_id,
                            "mining tip revalidation failed; retaining current job"
                        );
                        continue;
                    }
                };
                if let Some(tip) = latest_tip {
                    if next.template.prev != tip {
                        warn!(
                            job_id = next.template.job_id,
                            template_parent = %hex::encode(next.template.prev),
                            live_tip = %hex::encode(tip),
                            "discarding stale mining template"
                        );
                        continue;
                    }
                }
                if !refresh_retained_jobs.parent_is_retained(&next.template.prev)
                    && next.template.prev == current.template.prev
                {
                    warn!(
                        job_id = next.template.job_id,
                        parent = %hex::encode(next.template.prev),
                        "refusing to republish a parent invalidated by the tip sentinel"
                    );
                    continue;
                }
                let reason = if next.template.prev != current.template.prev {
                    JobReason::TipChange
                } else {
                    JobReason::Heartbeat
                };
                next.notify.clean_jobs = reason == JobReason::TipChange;
                if next.template.job_id == current.template.job_id {
                    continue;
                }
                let next = Arc::new(next);
                if reason == JobReason::TipChange {
                    if let Some(event_id) = active_latency_event {
                        replacement_latency.mark_a_ready(event_id);
                    }
                }
                if let Some(repository) = repository.as_deref() {
                    if let Err(err) = repository
                        .upsert_job(&job_record_from_pool_job(&next, reason))
                        .await
                    {
                        if let Some(event_id) = active_latency_event.take() {
                            replacement_latency.fail_event(event_id, "replacement_persist_failed");
                        }
                        warn!(%err, job_id = next.template.job_id, "refusing unpersisted mining job");
                        continue;
                    }
                }
                let job_age_secs = last_publish_at.elapsed().as_secs_f64();
                refresh_retained_jobs.publish(next.clone(), reason, retention);
                pool_state.record_job_notify(reason.as_str());
                info!(
                    job_id = next.template.job_id,
                    job_reason = reason.as_str(),
                    job_age_secs,
                    clean_jobs = next.notify.clean_jobs,
                    refresh_ms = refresh_started.elapsed().as_millis(),
                    "broadcasting refreshed mining job"
                );
                if reason == JobReason::TipChange {
                    if let Some(event_id) = active_latency_event.take() {
                        replacement_latency.mark_published(event_id, &next.template.job_id);
                    }
                }
                refresh_sender.send_replace(next);
                last_publish_at = TokioInstant::now();
            }
        });
        Ok(Self {
            sender,
            retained_jobs,
        })
    }

    fn subscribe(&self) -> watch::Receiver<Arc<PoolJob>> {
        self.sender.subscribe()
    }

    fn retained_jobs(&self) -> RetainedJobs {
        self.retained_jobs.clone()
    }
}

fn bounded_template_refresh_secs(requested: u64, template_mode: &str) -> u64 {
    let requested = requested.max(1);
    if template_mode.trim().eq_ignore_ascii_case("live") {
        requested.min(5)
    } else {
        requested
    }
}

pub fn stratum_listen() -> String {
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        if let Ok(config) = csd_pool_config::PoolConfig::from_file(path) {
            return config.stratum.listen;
        }
    }
    std::env::var("CSD_POOL_STRATUM_LISTEN").unwrap_or_else(|_| "127.0.0.1:3333".to_owned())
}

pub fn template_provider_from_env() -> Result<Arc<dyn TemplateProvider>> {
    let mode = std::env::var("CSD_POOL_TEMPLATE_MODE").unwrap_or_else(|_| "static".to_owned());
    if mode.eq_ignore_ascii_case("static") {
        return Ok(Arc::new(StaticTemplateProvider::easy_job("static-1")));
    }

    if !mode.eq_ignore_ascii_case("live") {
        return Err(BridgeError::InvalidConfig(format!(
            "unknown CSD_POOL_TEMPLATE_MODE={mode}; expected static or live"
        )));
    }

    let (node_url, pool_address) = live_template_config()?;
    let node = CsdNodeClient::from_env(node_url);
    if csd_pool_config::env_flag_enabled(
        std::env::var("CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED")
            .ok()
            .as_deref(),
    ) {
        Ok(Arc::new(LiveTemplateProvider::new_stateless(
            node,
            pool_address,
        )))
    } else {
        Ok(Arc::new(LiveTemplateProvider::new(node, pool_address)))
    }
}

fn tip_sentinel_from_env() -> Result<Option<Arc<dyn JobTipSentinel>>> {
    if !tip_sentinel_enabled(
        std::env::var("CSD_POOL_SECONDARY_TIP_SENTINEL_ENABLED")
            .ok()
            .as_deref(),
    ) {
        return Ok(None);
    }
    let mode = std::env::var("CSD_POOL_TEMPLATE_MODE").unwrap_or_else(|_| "static".to_owned());
    if !mode.eq_ignore_ascii_case("live") {
        return Err(BridgeError::InvalidConfig(
            "secondary tip sentinel requires live template mode".into(),
        ));
    }
    let (primary_url, _) = live_template_config()?;
    let explicit_sentinel_url = std::env::var("CSD_POOL_TIP_SENTINEL_NODE_URL")
        .ok()
        .filter(|value| !value.is_empty());
    let sentinel_url = select_tip_sentinel_node_url(explicit_sentinel_url.as_deref(), || {
        secondary_submit_node_url_from_env(&primary_url)
    })?;
    if normalized_node_url(&primary_url) == normalized_node_url(&sentinel_url) {
        return Err(BridgeError::InvalidConfig(
            "secondary tip sentinel requires distinct primary and sentinel node URLs".into(),
        ));
    }
    let sentinel_token = select_tip_sentinel_node_token(
        explicit_sentinel_url.is_some(),
        std::env::var("CSD_POOL_TIP_SENTINEL_NODE_TOKEN")
            .ok()
            .as_deref(),
        std::env::var("CSD_POOL_NODE_TOKEN").ok().as_deref(),
    )?;
    let sentinel_ca_pem = load_tip_sentinel_ca_pem(
        explicit_sentinel_url.is_some(),
        &sentinel_url,
        std::env::var("CSD_POOL_TIP_SENTINEL_NODE_CA_CERT")
            .ok()
            .as_deref(),
    )?;
    let mut secondary = CsdNodeClient::new(sentinel_url);
    if let Some(pem) = sentinel_ca_pem.as_deref() {
        secondary = secondary.with_root_certificate_pem(pem)?;
    }
    secondary = secondary.with_bearer_token(sentinel_token);
    Ok(Some(Arc::new(CsdJobTipSentinel {
        primary: CsdNodeClient::from_env(primary_url),
        secondary,
        timeout: Duration::from_millis(
            env_u64("CSD_POOL_SECONDARY_TIP_SENTINEL_TIMEOUT_MS", 750).clamp(100, 5_000),
        ),
    })))
}

fn tip_sentinel_enabled(value: Option<&str>) -> bool {
    csd_pool_config::env_flag_enabled(value)
}

fn live_template_config() -> Result<(String, String)> {
    let env_node_url = std::env::var("CSD_POOL_NODE_URL").ok();
    let env_pool_address = std::env::var("CSD_POOL_MINING_ADDRESS").ok();

    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        let config = csd_pool_config::PoolConfig::from_file(path)?;
        let node_url = env_node_url
            .or_else(|| {
                config
                    .csd_nodes
                    .iter()
                    .find(|node| node.role.split(',').any(|role| role.trim() == "template"))
                    .map(|node| node.rpc_url.clone())
            })
            .ok_or_else(|| BridgeError::InvalidConfig("no template CSD node configured".into()))?;
        let pool_address = env_pool_address.unwrap_or(config.pool.mining_address);
        validate_pool_address(&pool_address)?;
        return Ok((node_url, pool_address));
    }

    let node_url = env_node_url.ok_or_else(|| {
        BridgeError::InvalidConfig(
            "CSD_POOL_NODE_URL is required for live template mode without CSD_POOL_CONFIG".into(),
        )
    })?;
    let pool_address = env_pool_address.ok_or_else(|| {
        BridgeError::InvalidConfig(
            "CSD_POOL_MINING_ADDRESS is required for live template mode without CSD_POOL_CONFIG"
                .into(),
        )
    })?;
    validate_pool_address(&pool_address)?;
    Ok((node_url, pool_address))
}

fn validate_pool_address(address: &str) -> Result<()> {
    if valid_addr20(address) {
        Ok(())
    } else {
        Err(BridgeError::InvalidConfig(
            "pool mining address must be 40 lowercase hex chars".into(),
        ))
    }
}

pub fn abuse_config_from_env() -> Result<AbuseConfig> {
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        let config = csd_pool_config::PoolConfig::from_file(path)?;
        return Ok(config.abuse.into());
    }

    Ok(AbuseConfig {
        max_connections_per_ip: env_u32("CSD_POOL_MAX_CONNECTIONS_PER_IP", 32),
        max_sessions_per_address: env_u32("CSD_POOL_MAX_SESSIONS_PER_ADDRESS", 64),
        malformed_frame_limit: env_u32("CSD_POOL_MALFORMED_FRAME_LIMIT", 8),
        auth_failure_limit: env_u32("CSD_POOL_AUTH_FAILURE_LIMIT", 5),
        invalid_share_limit: env_u32("CSD_POOL_INVALID_SHARE_LIMIT", 16),
        ban_secs: env_u64("CSD_POOL_BAN_SECS", 600),
    })
}

pub fn vardiff_config_from_env() -> Result<VardiffConfig> {
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        let config = csd_pool_config::PoolConfig::from_file(path)?;
        return Ok(config.stratum.into());
    }

    Ok(VardiffConfig {
        initial_difficulty: env_f64("CSD_POOL_INITIAL_DIFFICULTY", 8.0),
        min_difficulty: env_f64("CSD_POOL_MIN_DIFFICULTY", 8.0),
        max_difficulty: env_f64("CSD_POOL_MAX_DIFFICULTY", 512.0),
        target_share_secs: env_u64("CSD_POOL_TARGET_SHARE_SECS", 20),
        ewma_alpha: env_f64("CSD_POOL_VARDIFF_EWMA_ALPHA", 0.25),
        raise_ratio: env_f64("CSD_POOL_VARDIFF_RAISE_RATIO", 0.70),
        lower_ratio: env_f64("CSD_POOL_VARDIFF_LOWER_RATIO", 1.40),
        min_adjust_secs: env_u64("CSD_POOL_VARDIFF_MIN_ADJUST_SECS", 120),
        max_adjust_factor: env_f64("CSD_POOL_VARDIFF_MAX_ADJUST_FACTOR", 2.0),
        transition_grace_secs: env_u64("CSD_POOL_VARDIFF_TRANSITION_GRACE_SECS", 120),
    }
    .normalized())
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub async fn mining_repository_from_env() -> Result<Option<Arc<dyn MiningRepository>>> {
    let database_url = require_persistent_database(
        database_url_from_env()?,
        std::env::var("CSD_POOL_TEMPLATE_MODE").ok().as_deref(),
        std::env::var("CSD_POOL_REQUIRE_DATABASE").ok().as_deref(),
    )?;
    let Some(database_url) = database_url else {
        return Ok(None);
    };

    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    let instance = server_instance();
    let stale_sessions = repo.close_stale_sessions(&instance).await?;
    if stale_sessions > 0 {
        info!(
            server_instance = instance,
            stale_sessions, "closed stale stratum sessions from previous process"
        );
    }
    Ok(Some(Arc::new(repo)))
}

pub fn block_submitter_from_env() -> Result<Option<Arc<dyn BlockSubmitter>>> {
    let submit_value = std::env::var("CSD_POOL_SUBMIT_CANDIDATES").ok();
    let mode = std::env::var("CSD_POOL_TEMPLATE_MODE").ok();
    let enabled = require_candidate_submission(
        csd_pool_config::env_flag_enabled(submit_value.as_deref()),
        mode.as_deref(),
    )?;
    if !enabled {
        return Ok(None);
    }

    let node_url = submit_node_url_from_env()?;
    if mode
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("live"))
    {
        let (template_node_url, _) = live_template_config()?;
        require_template_submit_affinity(&template_node_url, &node_url)?;
    }
    if csd_pool_config::env_flag_enabled(
        std::env::var("CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED")
            .ok()
            .as_deref(),
    ) {
        return Err(BridgeError::InvalidConfig(
            "BLOCKED_JOB_CACHE_AFFINITY: the legacy parallel submit path cannot submit a node-a job to an empty node-b cache"
                .into(),
        ));
    }
    if !csd_pool_config::env_flag_enabled(
        std::env::var("CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED")
            .ok()
            .as_deref(),
    ) {
        return Ok(Some(Arc::new(CsdNodeBlockSubmitter::new(
            CsdNodeClient::from_env(node_url),
        ))));
    }

    let secondary_url = secondary_submit_node_url_from_env(&node_url)?;
    if normalized_node_url(&node_url) == normalized_node_url(&secondary_url) {
        return Err(BridgeError::InvalidConfig(
            "parallel candidate submit requires distinct primary and secondary node URLs".into(),
        ));
    }
    let secondary_control = secondary_candidate_control_from_env()?;
    let primary = Arc::new(CsdCandidateNodeEndpoint::new(
        submit_node_name_from_env(),
        CsdNodeClient::from_env(node_url),
    ));
    let secondary = Arc::new(CsdCandidateNodeEndpoint::new(
        secondary_submit_node_name_from_env(),
        CsdNodeClient::from_env(secondary_url),
    ));
    Ok(Some(Arc::new(ParallelNodeBlockSubmitter::new(
        primary,
        secondary,
        ParallelSubmitConfig {
            primary_timeout: Duration::from_millis(env_u64(
                "CSD_POOL_PRIMARY_SUBMIT_TIMEOUT_MS",
                2_000,
            )),
            secondary_timeout: Duration::from_millis(env_u64(
                "CSD_POOL_SECONDARY_SUBMIT_TIMEOUT_MS",
                750,
            )),
            health_refresh: Duration::from_millis(env_u64(
                "CSD_POOL_PARALLEL_NODE_HEALTH_REFRESH_MS",
                1_000,
            )),
            health_max_age: Duration::from_millis(env_u64(
                "CSD_POOL_PARALLEL_NODE_HEALTH_MAX_AGE_MS",
                3_000,
            )),
            secondary_max_in_flight: env_u64("CSD_POOL_STATELESS_SECONDARY_MAX_IN_FLIGHT", 1)
                .clamp(1, 4) as usize,
            circuit_failure_threshold: env_u64("CSD_POOL_STATELESS_SECONDARY_CIRCUIT_FAILURES", 3)
                .clamp(1, 100),
            circuit_cooldown: Duration::from_millis(
                env_u64("CSD_POOL_STATELESS_SECONDARY_CIRCUIT_COOLDOWN_MS", 30_000)
                    .clamp(1_000, 300_000),
            ),
        },
        secondary_control,
    ))))
}

fn secondary_candidate_control_from_env() -> Result<SecondaryCandidateControl> {
    let mode = std::env::var("CSD_POOL_STATELESS_PARALLEL_CANDIDATE_MODE")
        .unwrap_or_else(|_| "one_shot".to_owned());
    match mode.trim().to_ascii_lowercase().as_str() {
        "one_shot" => {
            let budget = parallel_candidate_submit_budget(
                std::env::var("CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_BUDGET")
                    .ok()
                    .as_deref(),
            )?;
            Ok(SecondaryCandidateControl::OneShot {
                budget,
                latch: PersistentCandidateLatch::new(persistent_candidate_latch_path_from_env()?)?,
            })
        }
        "continuous_bounded" => Ok(SecondaryCandidateControl::ContinuousBounded(
            PersistentCandidateJournal::new(persistent_candidate_state_dir_from_env()?)?,
        )),
        _ => Err(BridgeError::InvalidConfig(
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_MODE must be one_shot or continuous_bounded"
                .into(),
        )),
    }
}

fn persistent_candidate_latch_path_from_env() -> Result<PathBuf> {
    let value = std::env::var("CSD_POOL_STATELESS_PARALLEL_CANDIDATE_LATCH_PATH")
        .map_err(|_| {
            BridgeError::InvalidConfig(
                "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_LATCH_PATH is required for the stateless one-shot canary"
                    .into(),
            )
        })?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(BridgeError::InvalidConfig(
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_LATCH_PATH must be absolute".into(),
        ));
    }
    Ok(path)
}

fn persistent_candidate_state_dir_from_env() -> Result<PathBuf> {
    let value = std::env::var("CSD_POOL_STATELESS_PARALLEL_CANDIDATE_STATE_DIR").map_err(|_| {
        BridgeError::InvalidConfig(
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_STATE_DIR is required for continuous_bounded mode"
                .into(),
        )
    })?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(BridgeError::InvalidConfig(
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_STATE_DIR must be absolute".into(),
        ));
    }
    Ok(path)
}

fn parallel_candidate_submit_budget(value: Option<&str>) -> Result<u64> {
    let raw = value.unwrap_or("1").trim();
    let budget = raw.parse::<u64>().map_err(|_| {
        BridgeError::InvalidConfig(
            "CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_BUDGET must be the integer 1".into(),
        )
    })?;
    if budget != 1 {
        return Err(BridgeError::InvalidConfig(
            "CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_BUDGET must be 1 for the production candidate canary"
                .into(),
        ));
    }
    Ok(budget)
}

fn require_template_submit_affinity(template_node_url: &str, submit_node_url: &str) -> Result<()> {
    if template_node_url.trim_end_matches('/') == submit_node_url.trim_end_matches('/') {
        return Ok(());
    }
    Err(BridgeError::InvalidConfig(
        "template and submit CSD node URLs must match because mining jobs are node-local".into(),
    ))
}

fn require_persistent_database(
    database_url: Option<String>,
    template_mode: Option<&str>,
    explicit_requirement: Option<&str>,
) -> Result<Option<String>> {
    if database_url.is_none()
        && csd_pool_config::persistent_database_required(template_mode, explicit_requirement)
    {
        return Err(BridgeError::InvalidConfig(
            "persistent PostgreSQL is required in live mode; set CSD_POOL_DATABASE_URL or the [database].url_env variable".into(),
        ));
    }
    Ok(database_url)
}

fn require_candidate_submission(enabled: bool, template_mode: Option<&str>) -> Result<bool> {
    if !enabled && template_mode.is_some_and(|mode| mode.trim().eq_ignore_ascii_case("live")) {
        return Err(BridgeError::InvalidConfig(
            "candidate block submission is required in live mode; set CSD_POOL_SUBMIT_CANDIDATES=true".into(),
        ));
    }
    Ok(enabled)
}

fn submit_node_url_from_env() -> Result<String> {
    if let Ok(url) = std::env::var("CSD_POOL_SUBMIT_NODE_URL") {
        if !url.is_empty() {
            return Ok(url);
        }
    }
    if let Ok(url) = std::env::var("CSD_POOL_NODE_URL") {
        if !url.is_empty() {
            return Ok(url);
        }
    }
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        let config = csd_pool_config::PoolConfig::from_file(path)?;
        if let Some(node) = config
            .csd_nodes
            .iter()
            .find(|node| node.role.split(',').any(|role| role.trim() == "submit"))
        {
            return Ok(node.rpc_url.clone());
        }
    }
    Err(BridgeError::InvalidConfig(
        "candidate submission enabled but no submit CSD node is configured".into(),
    ))
}

fn secondary_submit_node_url_from_env(primary_url: &str) -> Result<String> {
    if let Ok(url) = std::env::var("CSD_POOL_SECONDARY_SUBMIT_NODE_URL") {
        if !url.is_empty() {
            return Ok(url);
        }
    }
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        let config = csd_pool_config::PoolConfig::from_file(path)?;
        if let Some(node) = config.csd_nodes.iter().find(|node| {
            node.role.split(',').any(|role| role.trim() == "submit")
                && normalized_node_url(&node.rpc_url) != normalized_node_url(primary_url)
        }) {
            return Ok(node.rpc_url.clone());
        }
    }
    Err(BridgeError::InvalidConfig(
        "parallel candidate submit enabled but no distinct secondary submit node is configured"
            .into(),
    ))
}

fn select_tip_sentinel_node_url(
    explicit: Option<&str>,
    fallback: impl FnOnce() -> Result<String>,
) -> Result<String> {
    match explicit.filter(|value| !value.is_empty()) {
        Some(url) => Ok(url.to_owned()),
        None => fallback(),
    }
}

fn select_tip_sentinel_node_token(
    independent_url: bool,
    sentinel_token: Option<&str>,
    node_token: Option<&str>,
) -> Result<Option<String>> {
    let sentinel_token = sentinel_token
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    if independent_url {
        return sentinel_token.map(Some).ok_or_else(|| {
            BridgeError::InvalidConfig(
                "independent tip sentinel URL requires CSD_POOL_TIP_SENTINEL_NODE_TOKEN".into(),
            )
        });
    }
    Ok(sentinel_token.or_else(|| {
        node_token
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
    }))
}

fn load_tip_sentinel_ca_pem(
    independent_url: bool,
    sentinel_url: &str,
    path: Option<&str>,
) -> Result<Option<Vec<u8>>> {
    if !independent_url {
        if path.is_none_or(|value| value.is_empty()) {
            return Ok(None);
        }
        return Err(BridgeError::InvalidConfig(
            "CSD_POOL_TIP_SENTINEL_NODE_CA_CERT is only valid with an independent sentinel URL"
                .into(),
        ));
    }
    if !sentinel_url.to_ascii_lowercase().starts_with("https://") {
        return Err(BridgeError::InvalidConfig(
            "CSD_POOL_TIP_SENTINEL_NODE_CA_CERT requires an HTTPS sentinel URL".into(),
        ));
    }
    let path = path.filter(|value| !value.is_empty()).ok_or_else(|| {
        BridgeError::InvalidConfig(
            "independent HTTPS tip sentinel requires CSD_POOL_TIP_SENTINEL_NODE_CA_CERT".into(),
        )
    })?;
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(BridgeError::InvalidConfig(
            "CSD_POOL_TIP_SENTINEL_NODE_CA_CERT must be an absolute path".into(),
        ));
    }
    let metadata = symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BridgeError::InvalidConfig(
            "CSD_POOL_TIP_SENTINEL_NODE_CA_CERT must be a regular non-symlink file".into(),
        ));
    }
    const MAX_SENTINEL_CA_BYTES: u64 = 64 * 1024;
    if metadata.len() == 0 || metadata.len() > MAX_SENTINEL_CA_BYTES {
        return Err(BridgeError::InvalidConfig(
            "CSD_POOL_TIP_SENTINEL_NODE_CA_CERT has an invalid size".into(),
        ));
    }
    Ok(Some(std::fs::read(path)?))
}

fn submit_node_name_from_env() -> String {
    std::env::var("CSD_POOL_PRIMARY_SUBMIT_NODE_NAME")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "node-a".to_owned())
}

fn secondary_submit_node_name_from_env() -> String {
    std::env::var("CSD_POOL_SECONDARY_SUBMIT_NODE_NAME")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "node-b".to_owned())
}

fn normalized_node_url(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

fn database_url_from_env() -> Result<Option<String>> {
    let env_name = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .map(|config| config.database.url_env)
            .unwrap_or_else(|_| "CSD_POOL_DATABASE_URL".to_owned())
    } else {
        "CSD_POOL_DATABASE_URL".to_owned()
    };

    Ok(std::env::var(env_name)
        .ok()
        .filter(|value| !value.is_empty()))
}

async fn write_client_frame_with_deadline<W>(
    writer: &mut W,
    frame: &str,
    deadline: Duration,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    match timeout(deadline, writer.write_all(frame.as_bytes())).await {
        Ok(result) => result.map_err(BridgeError::from),
        Err(_) => Err(BridgeError::ClientWriteTimeout),
    }
}

async fn write_client_frame<W>(writer: &mut W, frame: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_client_frame_with_deadline(writer, frame, CLIENT_WRITE_DEADLINE).await
}

async fn next_clean_job(receiver: &mut watch::Receiver<Arc<PoolJob>>) -> Option<Arc<PoolJob>> {
    loop {
        receiver.changed().await.ok()?;
        let job = receiver.borrow_and_update().clone();
        if job.notify.clean_jobs {
            return Some(job);
        }
    }
}

enum AuthorizedSessionEvent {
    Read(std::io::Result<usize>),
    Job(Option<Arc<PoolJob>>),
}

#[allow(clippy::too_many_arguments)]
async fn handle_client(
    stream: TcpStream,
    peer: SocketAddr,
    pool_state: SharedPoolState,
    mut job_rx: watch::Receiver<Arc<PoolJob>>,
    retained_jobs: RetainedJobs,
    repository: Option<Arc<dyn MiningRepository>>,
    block_submitter: Option<Arc<dyn BlockSubmitter>>,
    abuse: Arc<AbuseManager>,
    vardiff_config: VardiffConfig,
    _permit: ConnectionPermit,
) -> Result<()> {
    let _connection_guard = pool_state.connection_guard();
    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let mut replacement_latency_session = retained_jobs.replacement_latency().session(session_id);
    let session_uuid = Uuid::new_v4().to_string();
    let extranonce1_le = (session_id as u32).to_le_bytes();
    // Stratum carries extranonce1 as the raw four-byte value encoded as hex.
    // The official CSD v0.2.3 miner decodes those bytes and interprets them as
    // a little-endian u32, so the wire value must be the LE byte encoding (for
    // session 1 this is "01000000", not the integer formatting "00000001").
    let extranonce1 = hex::encode(extranonce1_le);
    let mut authorized_worker: Option<String> = None;
    let mut user_agent: Option<String> = None;
    let mut session_persisted = false;
    let mut _address_permit: Option<AddressSessionPermit> = None;
    let mut seen_shares = HashSet::new();
    let mut vardiff = VardiffState::new(vardiff_config);
    let mut difficulty_suggestion_seen = false;
    let mut accepted_share_seen = false;
    let mut unknown_job_streak = 0_u32;
    let mut last_delivered_clean_generation = 0_u64;
    let mut clean_job_rx = job_rx.clone();

    info!(%peer, session_id, %session_uuid, "client connected");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    let result: Result<()> = async {
        loop {
        line.clear();
        let n = if authorized_worker.is_some() {
            let event = tokio::select! {
                biased;
                job = next_clean_job(&mut clean_job_rx) => AuthorizedSessionEvent::Job(job),
                changed = job_rx.changed() => {
                    let job = changed
                        .ok()
                        .map(|()| job_rx.borrow_and_update().clone());
                    AuthorizedSessionEvent::Job(job)
                }
                result = reader.read_line(&mut line) => AuthorizedSessionEvent::Read(result),
            };
            match event {
                AuthorizedSessionEvent::Read(result) => result?,
                AuthorizedSessionEvent::Job(Some(current_job)) => {
                    job_rx.borrow_and_update();
                    clean_job_rx.borrow_and_update();
                    if retained_jobs
                        .get_for_submit(&current_job.template.job_id)
                        .is_some()
                    {
                        let frame = serialize_line(&notify(&current_job.notify))?;
                        let result = write_client_frame(&mut write_half, &frame).await;
                        replacement_latency_session.record_notify(
                            &current_job.template.job_id,
                            if result.is_ok() {
                                FanoutOutcome::Success
                            } else {
                                FanoutOutcome::Failed
                            },
                        );
                        result?;
                        if let Some(generation) =
                            retained_jobs.clean_generation(&current_job.template.job_id)
                        {
                            last_delivered_clean_generation =
                                last_delivered_clean_generation.max(generation);
                        }
                    } else {
                        replacement_latency_session.record_notify(
                            &current_job.template.job_id,
                            FanoutOutcome::Skipped,
                        );
                    }
                    continue;
                }
                AuthorizedSessionEvent::Job(None) => {
                        return Ok(());
                }
            }
        } else {
            reader.read_line(&mut line).await?
        };
        if n == 0 {
            info!(%peer, session_id, %session_uuid, "client disconnected");
            return Ok(());
        }

        let request: Request = match serde_json::from_str(line.trim()) {
            Ok(req) => req,
            Err(err) => {
                warn!(%peer, session_id, %session_uuid, %err, "malformed json frame");
                if abuse.record_malformed_frame(peer.ip()) {
                    warn!(%peer, session_id, %session_uuid, "closing session after malformed frame ban");
                    return Ok(());
                }
                continue;
            }
        };

        debug!(%peer, session_id, %session_uuid, method = request.method, "request");
        let mut pending_difficulty: Option<f64> = None;
        let mut resync_after_stale_submit = false;
        let response = match request.method.as_str() {
            "mining.subscribe" => {
                user_agent = parse_subscribe_user_agent(&request.params);
                subscribe_response(request.id, &extranonce1)
            }
            "mining.authorize" => {
                let worker = parse_authorize_worker(&request.params);
                match worker {
                    Some(worker) if valid_addr20(&worker) => {
                        if authorized_worker.as_deref() == Some(worker.as_str()) {
                            response_ok(request.id.unwrap_or(0))
                        } else {
                            match abuse.try_open_address_session(&worker) {
                                Ok(permit) => {
                                    pool_state.record_authorized_worker(&worker);
                                    if !session_persisted {
                                        if let Some(repository) = repository.as_deref() {
                                            let record = SessionRecord {
                                                id: session_uuid.clone(),
                                                miner: worker.clone(),
                                                worker_name: parse_authorize_worker_name(
                                                    &request.params,
                                                ),
                                                remote_addr: peer.ip().to_string(),
                                                remote_port: peer.port(),
                                                user_agent: user_agent.clone(),
                                                extranonce1: extranonce1.clone(),
                                                server_session_id: session_id,
                                                server_release: server_release(),
                                                server_instance: server_instance(),
                                                assigned_difficulty: vardiff.current_difficulty(),
                                            };
                                            match repository.open_session(&record).await {
                                                Ok(()) => session_persisted = true,
                                                Err(err) => warn!(
                                                    %peer,
                                                    session_id,
                                                    %session_uuid,
                                                    %err,
                                                    "failed to persist stratum session; mining continues"
                                                ),
                                            }
                                        }
                                    }
                                    info!(
                                        %peer,
                                        session_id,
                                        %session_uuid,
                                        worker,
                                        user_agent = user_agent.as_deref().unwrap_or("unknown"),
                                        release = server_release(),
                                        "client authorized"
                                    );
                                    authorized_worker = Some(worker);
                                    replacement_latency_session.authorize();
                                    _address_permit = Some(permit);
                                    response_ok(request.id.unwrap_or(0))
                                }
                                Err(AbuseReject::TooManyAddressSessions) => response_error(
                                    request.id.unwrap_or(0),
                                    25,
                                    "too many sessions for address",
                                ),
                                Err(reject) => {
                                    warn!(%peer, session_id, reason = ?reject, "address authorization rejected");
                                    response_error(request.id.unwrap_or(0), 20, "invalid address")
                                }
                            }
                        }
                    }
                    _ => {
                        abuse.record_auth_failure(peer.ip());
                        response_error(request.id.unwrap_or(0), 20, "invalid address")
                    }
                }
            }
            "mining.suggest_difficulty" => {
                match parse_suggested_difficulty(&request.params) {
                    Some(suggested) => {
                        if !difficulty_suggestion_seen && !accepted_share_seen {
                            difficulty_suggestion_seen = true;
                            pending_difficulty =
                                vardiff.apply_suggested_difficulty(suggested, Instant::now());
                            debug!(
                                %peer,
                                session_id,
                                %session_uuid,
                                suggested,
                                applied = pending_difficulty.unwrap_or(vardiff.current_difficulty()),
                                "processed initial session difficulty suggestion"
                            );
                        }
                        response_ok(request.id.unwrap_or(0))
                    }
                    None => response_error(
                        request.id.unwrap_or(0),
                        20,
                        "invalid suggested difficulty",
                    ),
                }
            }
            "mining.submit" => {
                if authorized_worker.is_none() {
                    response_error(request.id.unwrap_or(0), 24, "unauthorized")
                } else {
                    let worker_address = authorized_worker.as_deref().unwrap_or_default();
                    match SubmitParams::parse(&request.params) {
                        Ok(submit) => {
                            let submitted_job = retained_jobs.get_for_submit(&submit.job_id);
                            let invalidated_parent =
                                retained_jobs.job_parent_is_invalidated(&submit.job_id);
                            if !authorized_submit_worker(worker_address, &submit.worker_name) {
                                pool_state.record_share_rejected(worker_address);
                                abuse.record_invalid_share(peer.ip());
                                persist_share_event(
                                    repository.as_deref(),
                                    &share_event_from_submit(
                                        session_persisted.then_some(session_uuid.as_str()),
                                        worker_address,
                                        &submit,
                                        "rejected",
                                        "unauthorized_worker",
                                    ),
                                )
                                .await?;
                                response_error(request.id.unwrap_or(0), 20, "invalid worker")
                            } else if invalidated_parent {
                                unknown_job_streak = unknown_job_streak.saturating_add(1);
                                resync_after_stale_submit = true;
                                let generation_class = retained_jobs.stale_generation_class(
                                    &submit.job_id,
                                    last_delivered_clean_generation,
                                );
                                pool_state.record_share_stale(worker_address);
                                pool_state
                                    .record_stale_generation_fence(generation_class.as_str());
                                persist_share_event(
                                    repository.as_deref(),
                                    &share_event_from_submit(
                                        session_persisted.then_some(session_uuid.as_str()),
                                        worker_address,
                                        &submit,
                                        "stale",
                                        "invalidated_parent",
                                    ),
                                )
                                .await?;
                                response_error(request.id.unwrap_or(0), 21, "stale job")
                            } else if submitted_job.is_none() {
                                unknown_job_streak = unknown_job_streak.saturating_add(1);
                                resync_after_stale_submit = true;
                                pool_state.record_share_stale(worker_address);
                                persist_share_event(
                                    repository.as_deref(),
                                    &share_event_from_submit(
                                        session_persisted.then_some(session_uuid.as_str()),
                                        worker_address,
                                        &submit,
                                        "stale",
                                        "unknown_job",
                                    ),
                                )
                                .await?;
                                response_error(request.id.unwrap_or(0), 21, "unknown job")
                            } else if !seen_shares.insert(ShareKey::from_submit(&submit)) {
                                unknown_job_streak = 0;
                                pool_state.record_share_rejected(worker_address);
                                persist_share_event(
                                    repository.as_deref(),
                                    &share_event_from_submit(
                                        session_persisted.then_some(session_uuid.as_str()),
                                        worker_address,
                                        &submit,
                                        "rejected",
                                        "duplicate_share",
                                    ),
                                )
                                .await?;
                                response_error(request.id.unwrap_or(0), 22, "duplicate share")
                            } else {
                                unknown_job_streak = 0;
                                let Some(submitted_job) = submitted_job else {
                                    unreachable!("retained job checked above");
                                };
                                let validation_started = Instant::now();
                                let validation_result = verify_submit_for_vardiff(
                                    &submitted_job.template,
                                    extranonce1_le,
                                    &submit,
                                    &vardiff,
                                    validation_started,
                                );
                                pool_state.record_share_validation(validation_started.elapsed());
                                match validation_result {
                                    Ok((share, accepted_difficulty)) => {
                                        let candidate_observation =
                                            share.is_block_candidate.then(CandidateObservation::now);
                                        let persisted = persist_accepted_share(
                                            repository.as_deref(),
                                            &submitted_job,
                                            worker_address,
                                            &submit,
                                            &share,
                                            accepted_difficulty,
                                            session_persisted.then_some(session_uuid.as_str()),
                                        )
                                        .await?;
                                        if persisted {
                                            accepted_share_seen = true;
                                            if share.is_block_candidate {
                                                submit_block_candidate(
                                                    block_submitter.as_deref(),
                                                    repository.as_deref(),
                                                    &submitted_job,
                                                    &pool_state,
                                                    worker_address,
                                                    &submit,
                                                    &share,
                                                    extranonce1_le,
                                                    accepted_difficulty,
                                                    candidate_observation
                                                        .expect("candidate observation recorded"),
                                                )
                                                .await?;
                                            }
                                            pool_state.record_share_accepted(
                                                worker_address,
                                                accepted_difficulty,
                                                share.is_block_candidate,
                                            );
                                            pending_difficulty =
                                                vardiff.record_accepted_share(Instant::now());
                                            debug!(
                                                %peer,
                                                session_id,
                                                %session_uuid,
                                                worker = submit.worker_name,
                                                job_id = submit.job_id,
                                                hash = hex::encode(share.hash),
                                                difficulty = accepted_difficulty,
                                                candidate = share.is_block_candidate,
                                                "share accepted"
                                            );
                                            response_ok(request.id.unwrap_or(0))
                                        } else {
                                            pool_state.record_share_rejected(worker_address);
                                            persist_share_event(
                                                repository.as_deref(),
                                                &share_event_from_submit(
                                                    session_persisted
                                                        .then_some(session_uuid.as_str()),
                                                    worker_address,
                                                    &submit,
                                                    "rejected",
                                                    "duplicate_share",
                                                ),
                                            )
                                            .await?;
                                            response_error(
                                                request.id.unwrap_or(0),
                                                22,
                                                "duplicate share",
                                            )
                                        }
                                    }
                                    Err(err) => {
                                        pool_state.record_share_rejected(worker_address);
                                        // Vardiff transitions can produce routine low shares; banning
                                        // their shared NAT address would evict otherwise valid miners.
                                        persist_share_event(
                                            repository.as_deref(),
                                            &share_event_from_submit(
                                                session_persisted
                                                    .then_some(session_uuid.as_str()),
                                                worker_address,
                                                &submit,
                                                "rejected",
                                                "low_difficulty",
                                            ),
                                        )
                                        .await?;
                                        debug!(%peer, session_id, %session_uuid, %err, "low difficulty share rejected");
                                        response_error(
                                            request.id.unwrap_or(0),
                                            23,
                                            "low difficulty share",
                                        )
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            pool_state.record_share_rejected(worker_address);
                            abuse.record_invalid_share(peer.ip());
                            persist_share_event(
                                repository.as_deref(),
                                &ShareEventRecord {
                                    session_id: session_persisted
                                        .then_some(session_uuid.clone()),
                                    miner: worker_address.to_owned(),
                                    worker_name: "default".to_owned(),
                                    job_id: None,
                                    kind: "rejected".to_owned(),
                                    reason: "malformed_submit".to_owned(),
                                },
                            )
                            .await?;
                            response_error(request.id.unwrap_or(0), 20, "malformed submit")
                        }
                    }
                }
            }
            _ => response_error(request.id.unwrap_or(0), 20, "unknown method"),
        };

        write_client_frame(&mut write_half, &serialize_line(&response)?).await?;

        if resync_after_stale_submit {
            let mut current_job = (*job_rx.borrow_and_update().clone()).clone();
            if retained_jobs
                .get_for_submit(&current_job.template.job_id)
                .is_some()
            {
                current_job.notify.clean_jobs = true;
                write_client_frame(
                    &mut write_half,
                    &serialize_line(&notify(&current_job.notify))?,
                )
                .await?;
                if let Some(generation) =
                    retained_jobs.clean_generation(&current_job.template.job_id)
                {
                    last_delivered_clean_generation =
                        last_delivered_clean_generation.max(generation);
                }
                info!(
                    %peer,
                    session_id,
                    %session_uuid,
                    unknown_job_streak,
                    job_id = current_job.template.job_id,
                    "resent current clean job after stale submit"
                );
            }
            if unknown_job_streak >= STALE_JOB_RECONNECT_THRESHOLD {
                warn!(
                    %peer,
                    session_id,
                    %session_uuid,
                    unknown_job_streak,
                    "closing session after repeated stale job submissions"
                );
                return Ok(());
            }
        }

        if abuse.is_banned(peer.ip()) {
            warn!(%peer, session_id, %session_uuid, "closing banned stratum session");
            return Ok(());
        }

        if request.method == "mining.authorize" && authorized_worker.is_some() {
            write_client_frame(
                &mut write_half,
                &serialize_line(&set_difficulty(vardiff.current_difficulty()))?,
            )
            .await?;
            let current_job = job_rx.borrow_and_update().clone();
            if retained_jobs
                .get_for_submit(&current_job.template.job_id)
                .is_some()
            {
                write_client_frame(
                    &mut write_half,
                    &serialize_line(&notify(&current_job.notify))?,
                )
                .await?;
                if let Some(generation) =
                    retained_jobs.clean_generation(&current_job.template.job_id)
                {
                    last_delivered_clean_generation =
                        last_delivered_clean_generation.max(generation);
                }
            }
        } else if let Some(difficulty) = pending_difficulty {
            write_client_frame(
                &mut write_half,
                &serialize_line(&set_difficulty(difficulty))?,
            )
            .await?;
            let mut difficulty_job = (*job_rx.borrow().clone()).clone();
            difficulty_job.notify.clean_jobs = false;
            if retained_jobs
                .get_for_submit(&difficulty_job.template.job_id)
                .is_some()
            {
                write_client_frame(
                    &mut write_half,
                    &serialize_line(&notify(&difficulty_job.notify))?,
                )
                .await?;
                if let Some(generation) =
                    retained_jobs.clean_generation(&difficulty_job.template.job_id)
                {
                    last_delivered_clean_generation =
                        last_delivered_clean_generation.max(generation);
                }
            }
            if session_persisted {
                if let Some(repository) = repository.as_deref() {
                    if let Err(err) = repository
                        .update_session_difficulty(&session_uuid, difficulty)
                        .await
                    {
                        warn!(
                            %peer,
                            session_id,
                            %session_uuid,
                            %err,
                            "failed to persist session difficulty; mining continues"
                        );
                    }
                }
            }
            debug!(
                %peer,
                session_id,
                %session_uuid,
                difficulty,
                "session difficulty updated"
            );
        }
        }
    }
    .await;

    if session_persisted {
        if let Some(repository) = repository.as_deref() {
            if let Err(err) = repository.close_session(&session_uuid).await {
                warn!(
                    %peer,
                    session_id,
                    %session_uuid,
                    %err,
                    "failed to close persisted stratum session"
                );
            }
        }
    }
    result
}

#[async_trait::async_trait]
pub trait BlockSubmitter: Send + Sync {
    async fn submit_candidate(
        &self,
        submission: &BlockCandidateSubmission,
    ) -> Result<SubmitBlockResponse>;
}

#[derive(Clone, Debug)]
pub struct BlockCandidateSubmission {
    candidate: BlockCandidateSubmitRequest,
    candidate_material: Option<Arc<CandidateTemplateMaterial>>,
}

impl BlockCandidateSubmission {
    fn stateless_request(&self) -> Option<StatelessBlockCandidateSubmitRequest<'_>> {
        self.candidate_material
            .as_deref()
            .map(|material| StatelessBlockCandidateSubmitRequest {
                candidate: &self.candidate,
                candidate_material: material,
            })
    }
}

#[async_trait::async_trait]
trait CandidateNodeEndpoint: Send + Sync {
    fn name(&self) -> &str;

    async fn health(&self) -> std::result::Result<NodeHealth, String>;

    async fn submit_cached_candidate(
        &self,
        candidate: &BlockCandidateSubmitRequest,
    ) -> std::result::Result<SubmitBlockResponse, String>;

    async fn submit_full_candidate(
        &self,
        candidate: &StatelessBlockCandidateSubmitRequest<'_>,
    ) -> std::result::Result<SubmitBlockResponse, String>;
}

#[derive(Clone)]
pub struct CsdNodeBlockSubmitter {
    node: CsdNodeClient,
}

impl CsdNodeBlockSubmitter {
    pub fn new(node: CsdNodeClient) -> Self {
        Self { node }
    }
}

#[async_trait::async_trait]
impl BlockSubmitter for CsdNodeBlockSubmitter {
    async fn submit_candidate(
        &self,
        submission: &BlockCandidateSubmission,
    ) -> Result<SubmitBlockResponse> {
        Ok(self.node.submit_candidate(&submission.candidate).await?)
    }
}

#[derive(Clone)]
struct CsdCandidateNodeEndpoint {
    name: String,
    node: CsdNodeClient,
}

impl CsdCandidateNodeEndpoint {
    fn new(name: String, node: CsdNodeClient) -> Self {
        Self { name, node }
    }
}

#[async_trait::async_trait]
impl CandidateNodeEndpoint for CsdCandidateNodeEndpoint {
    fn name(&self) -> &str {
        &self.name
    }

    async fn health(&self) -> std::result::Result<NodeHealth, String> {
        self.node.health().await.map_err(|err| err.to_string())
    }

    async fn submit_cached_candidate(
        &self,
        candidate: &BlockCandidateSubmitRequest,
    ) -> std::result::Result<SubmitBlockResponse, String> {
        self.node
            .submit_candidate(candidate)
            .await
            .map_err(|err| err.to_string())
    }

    async fn submit_full_candidate(
        &self,
        candidate: &StatelessBlockCandidateSubmitRequest<'_>,
    ) -> std::result::Result<SubmitBlockResponse, String> {
        self.node
            .submit_full_candidate(candidate)
            .await
            .map_err(|err| err.to_string())
    }
}

#[derive(Clone)]
struct PersistentCandidateLatch {
    path: Arc<PathBuf>,
    #[cfg(test)]
    claim_delay: Duration,
}

impl PersistentCandidateLatch {
    fn new(path: PathBuf) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
            .ok_or_else(|| {
                BridgeError::InvalidConfig("persistent candidate latch has no parent".into())
            })?;
        let parent_metadata = symlink_metadata(parent).map_err(|error| {
            BridgeError::InvalidConfig(format!(
                "persistent candidate latch parent is unavailable: {error}"
            ))
        })?;
        if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(BridgeError::InvalidConfig(
                "persistent candidate latch parent must be a real directory".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = parent_metadata.permissions().mode();
            let writable_by_others = mode & 0o022 != 0;
            let sticky = mode & 0o1000 != 0;
            if writable_by_others && !sticky {
                return Err(BridgeError::InvalidConfig(
                    "persistent candidate latch parent must not be group/world writable without the sticky bit"
                        .into(),
                ));
            }
        }
        match symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;

                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(BridgeError::InvalidConfig(
                            "persistent candidate latch file must have mode 0600 or stricter"
                                .into(),
                        ));
                    }
                }
            }
            Ok(_) => {
                return Err(BridgeError::InvalidConfig(
                    "persistent candidate latch must be absent or a regular file".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(BridgeError::InvalidConfig(format!(
                    "persistent candidate latch cannot be inspected: {error}"
                )));
            }
        }
        Ok(Self {
            path: Arc::new(path),
            #[cfg(test)]
            claim_delay: Duration::ZERO,
        })
    }

    #[cfg(test)]
    fn with_claim_delay(mut self, claim_delay: Duration) -> Self {
        self.claim_delay = claim_delay;
        self
    }

    fn claim(&self, candidate_hash: &str, configured: u64) -> SecondaryCandidateClaim {
        let before = u64::from(!Path::new(self.path.as_ref()).exists());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(self.path.as_ref()) {
            Ok(mut file) => {
                let marker = format!(
                    "version=1\nclaimed_unix_ms={}\ncandidate_hash={}\n",
                    unix_ms(),
                    candidate_hash
                );
                let persistence = file.write_all(marker.as_bytes()).and_then(|()| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                    }
                    #[cfg(test)]
                    if !self.claim_delay.is_zero() {
                        std::thread::sleep(self.claim_delay);
                    }
                    file.sync_all()?;
                    let parent = self.path.parent().ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "persistent candidate latch has no parent",
                        )
                    })?;
                    File::open(parent)?.sync_all()
                });
                match persistence {
                    Ok(()) => SecondaryCandidateClaim {
                        mode: "one_shot",
                        configured: Some(configured),
                        before: Some(before),
                        claimed: true,
                        remaining: Some(0),
                        claim_key: candidate_hash.to_owned(),
                        persistence: "o_excl_file",
                        error: None,
                    },
                    Err(error) => SecondaryCandidateClaim {
                        mode: "one_shot",
                        configured: Some(configured),
                        before: Some(before),
                        claimed: false,
                        remaining: Some(0),
                        claim_key: candidate_hash.to_owned(),
                        persistence: "o_excl_file",
                        error: Some(format!("persistent latch write failed: {error}")),
                    },
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                SecondaryCandidateClaim {
                    mode: "one_shot",
                    configured: Some(configured),
                    before: Some(0),
                    claimed: false,
                    remaining: Some(0),
                    claim_key: candidate_hash.to_owned(),
                    persistence: "o_excl_file",
                    error: None,
                }
            }
            Err(error) => SecondaryCandidateClaim {
                mode: "one_shot",
                configured: Some(configured),
                before: Some(before),
                claimed: false,
                remaining: Some(0),
                claim_key: candidate_hash.to_owned(),
                persistence: "o_excl_file",
                error: Some(format!("persistent latch claim failed: {error}")),
            },
        }
    }
}

#[derive(Clone)]
struct PersistentCandidateJournal {
    directory: Arc<PathBuf>,
    #[cfg(test)]
    claim_delay: Duration,
}

impl PersistentCandidateJournal {
    fn new(directory: PathBuf) -> Result<Self> {
        validate_persistent_candidate_directory(&directory)?;
        Ok(Self {
            directory: Arc::new(directory),
            #[cfg(test)]
            claim_delay: Duration::ZERO,
        })
    }

    #[cfg(test)]
    fn with_claim_delay(mut self, claim_delay: Duration) -> Self {
        self.claim_delay = claim_delay;
        self
    }

    fn claim(&self, candidate_hash: &str) -> SecondaryCandidateClaim {
        if candidate_hash.len() != 64
            || !candidate_hash
                .bytes()
                .all(|value| value.is_ascii_hexdigit())
        {
            return SecondaryCandidateClaim::failed(
                "continuous_bounded",
                candidate_hash,
                "candidate hash is not canonical 32-byte hex".to_owned(),
            );
        }
        let claim_key = candidate_hash.to_ascii_lowercase();
        let path = self.directory.join(format!("{claim_key}.claim"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                let marker = format!(
                    "version=1\nclaimed_unix_ms={}\ncandidate_hash={claim_key}\n",
                    unix_ms()
                );
                let persistence = file.write_all(marker.as_bytes()).and_then(|()| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                    }
                    #[cfg(test)]
                    if !self.claim_delay.is_zero() {
                        std::thread::sleep(self.claim_delay);
                    }
                    file.sync_all()?;
                    File::open(self.directory.as_ref())?.sync_all()
                });
                match persistence {
                    Ok(()) => SecondaryCandidateClaim {
                        mode: "continuous_bounded",
                        configured: None,
                        before: None,
                        claimed: true,
                        remaining: None,
                        claim_key,
                        persistence: "o_excl_per_candidate",
                        error: None,
                    },
                    Err(error) => SecondaryCandidateClaim::failed(
                        "continuous_bounded",
                        &claim_key,
                        format!("persistent candidate journal write failed: {error}"),
                    ),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                SecondaryCandidateClaim {
                    mode: "continuous_bounded",
                    configured: None,
                    before: None,
                    claimed: false,
                    remaining: None,
                    claim_key,
                    persistence: "o_excl_per_candidate",
                    error: None,
                }
            }
            Err(error) => SecondaryCandidateClaim::failed(
                "continuous_bounded",
                &claim_key,
                format!("persistent candidate journal claim failed: {error}"),
            ),
        }
    }
}

fn validate_persistent_candidate_directory(directory: &Path) -> Result<()> {
    let metadata = symlink_metadata(directory).map_err(|error| {
        BridgeError::InvalidConfig(format!(
            "persistent candidate state directory is unavailable: {error}"
        ))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(BridgeError::InvalidConfig(
            "persistent candidate state path must be a real directory".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(BridgeError::InvalidConfig(
                "persistent candidate state directory must have mode 0700 or stricter".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Clone)]
enum SecondaryCandidateControl {
    OneShot {
        budget: u64,
        latch: PersistentCandidateLatch,
    },
    ContinuousBounded(PersistentCandidateJournal),
}

impl SecondaryCandidateControl {
    fn mode(&self) -> &'static str {
        match self {
            Self::OneShot { .. } => "one_shot",
            Self::ContinuousBounded(_) => "continuous_bounded",
        }
    }

    fn not_attempted(&self) -> SecondaryCandidateClaim {
        match self {
            Self::OneShot { budget, .. } => {
                SecondaryCandidateClaim::not_attempted("one_shot", Some(*budget), Some(*budget))
            }
            Self::ContinuousBounded(_) => {
                SecondaryCandidateClaim::not_attempted("continuous_bounded", None, None)
            }
        }
    }

    fn failed(&self, candidate_hash: &str, error: String) -> SecondaryCandidateClaim {
        match self {
            Self::OneShot { budget, .. } => {
                SecondaryCandidateClaim::failed_one_shot(*budget, candidate_hash, error)
            }
            Self::ContinuousBounded(_) => {
                SecondaryCandidateClaim::failed("continuous_bounded", candidate_hash, error)
            }
        }
    }

    fn claim(&self, candidate_hash: &str) -> SecondaryCandidateClaim {
        match self {
            Self::OneShot { budget, latch } => latch.claim(candidate_hash, *budget),
            Self::ContinuousBounded(journal) => journal.claim(candidate_hash),
        }
    }

    fn exhausted_reason(&self) -> &'static str {
        match self {
            Self::OneShot { .. } => "candidate_budget_exhausted",
            Self::ContinuousBounded(_) => "candidate_already_submitted_to_secondary",
        }
    }

    fn persistence_failure_reason(&self) -> &'static str {
        match self {
            Self::OneShot { .. } => "candidate_budget_persistence_failed",
            Self::ContinuousBounded(_) => "candidate_control_persistence_failed",
        }
    }
}

#[derive(Clone)]
struct SecondaryCircuitBreaker {
    failure_threshold: u64,
    cooldown: Duration,
    state: Arc<Mutex<SecondaryCircuitState>>,
}

#[derive(Clone, Debug)]
struct SecondaryCircuitState {
    consecutive_failures: u64,
    open_until: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct SecondaryCircuitSnapshot {
    consecutive_failures: u64,
    open_remaining_ms: u64,
}

impl SecondaryCircuitBreaker {
    fn new(failure_threshold: u64, cooldown: Duration) -> Self {
        Self {
            failure_threshold,
            cooldown,
            state: Arc::new(Mutex::new(SecondaryCircuitState {
                consecutive_failures: 0,
                open_until: None,
            })),
        }
    }

    fn eligibility(&self) -> std::result::Result<(), &'static str> {
        let mut state = self
            .state
            .lock()
            .expect("secondary circuit breaker lock poisoned");
        if state
            .open_until
            .is_some_and(|deadline| deadline > Instant::now())
        {
            return Err("secondary_circuit_open");
        }
        state.open_until = None;
        Ok(())
    }

    fn record(&self, audit: &SubmitAudit) {
        let mut state = self
            .state
            .lock()
            .expect("secondary circuit breaker lock poisoned");
        if audit.outcome == "accepted" {
            state.consecutive_failures = 0;
            state.open_until = None;
            return;
        }
        if audit.outcome == "skipped" {
            return;
        }
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        if state.consecutive_failures >= self.failure_threshold {
            state.open_until = Some(Instant::now() + self.cooldown);
        }
    }

    fn snapshot(&self) -> SecondaryCircuitSnapshot {
        let state = self
            .state
            .lock()
            .expect("secondary circuit breaker lock poisoned");
        SecondaryCircuitSnapshot {
            consecutive_failures: state.consecutive_failures,
            open_remaining_ms: state
                .open_until
                .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
                .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
                .unwrap_or(0),
        }
    }
}

#[derive(Clone)]
struct ParallelNodeBlockSubmitter {
    primary: Arc<dyn CandidateNodeEndpoint>,
    secondary: Arc<dyn CandidateNodeEndpoint>,
    primary_timeout: Duration,
    secondary_timeout: Duration,
    health_refresh: Duration,
    health_max_age: Duration,
    health_snapshot: Arc<RwLock<Option<ParallelHealthSnapshot>>>,
    secondary_control: SecondaryCandidateControl,
    secondary_in_flight: Arc<Semaphore>,
    secondary_circuit: SecondaryCircuitBreaker,
}

#[derive(Clone, Copy)]
struct ParallelSubmitConfig {
    primary_timeout: Duration,
    secondary_timeout: Duration,
    health_refresh: Duration,
    health_max_age: Duration,
    secondary_max_in_flight: usize,
    circuit_failure_threshold: u64,
    circuit_cooldown: Duration,
}

impl ParallelNodeBlockSubmitter {
    fn new(
        primary: Arc<dyn CandidateNodeEndpoint>,
        secondary: Arc<dyn CandidateNodeEndpoint>,
        config: ParallelSubmitConfig,
        secondary_control: SecondaryCandidateControl,
    ) -> Self {
        let submitter = Self {
            primary,
            secondary,
            primary_timeout: bounded_submit_timeout(config.primary_timeout),
            secondary_timeout: bounded_submit_timeout(config.secondary_timeout),
            health_refresh: config
                .health_refresh
                .clamp(Duration::from_millis(100), Duration::from_secs(10)),
            health_max_age: config
                .health_max_age
                .clamp(Duration::from_millis(250), Duration::from_secs(30)),
            health_snapshot: Arc::new(RwLock::new(None)),
            secondary_control,
            secondary_in_flight: Arc::new(Semaphore::new(config.secondary_max_in_flight)),
            secondary_circuit: SecondaryCircuitBreaker::new(
                config.circuit_failure_threshold,
                config.circuit_cooldown,
            ),
        };
        submitter.start_health_watch();
        submitter
    }

    fn start_health_watch(&self) {
        let submitter = self.clone();
        tokio::spawn(async move {
            loop {
                submitter.refresh_health_once().await;
                tokio::time::sleep(submitter.health_refresh).await;
            }
        });
    }

    async fn refresh_health_once(&self) {
        let previous = self.current_health();
        let (primary, secondary) = tokio::join!(
            health_with_timeout(self.primary.as_ref(), self.secondary_timeout),
            health_with_timeout(self.secondary.as_ref(), self.secondary_timeout)
        );
        let observed_unix_ms = unix_ms();
        let snapshot = ParallelHealthSnapshot {
            primary_tip_observed_since_unix_ms: tip_observed_since(
                previous.as_ref().map(|value| &value.primary),
                previous
                    .as_ref()
                    .and_then(|value| value.primary_tip_observed_since_unix_ms),
                &primary,
                observed_unix_ms,
            ),
            secondary_tip_observed_since_unix_ms: tip_observed_since(
                previous.as_ref().map(|value| &value.secondary),
                previous
                    .as_ref()
                    .and_then(|value| value.secondary_tip_observed_since_unix_ms),
                &secondary,
                observed_unix_ms,
            ),
            primary,
            secondary,
            observed_at: Instant::now(),
            observed_unix_ms,
        };
        *self
            .health_snapshot
            .write()
            .expect("parallel health snapshot lock poisoned") = Some(snapshot);
    }

    fn current_health(&self) -> Option<ParallelHealthSnapshot> {
        self.health_snapshot
            .read()
            .expect("parallel health snapshot lock poisoned")
            .clone()
    }

    #[cfg(test)]
    fn one_shot_latch_path(&self) -> &Path {
        match &self.secondary_control {
            SecondaryCandidateControl::OneShot { latch, .. } => latch.path.as_ref(),
            SecondaryCandidateControl::ContinuousBounded(_) => {
                panic!("submitter does not use the one-shot latch")
            }
        }
    }
}

#[async_trait::async_trait]
impl BlockSubmitter for ParallelNodeBlockSubmitter {
    async fn submit_candidate(
        &self,
        submission: &BlockCandidateSubmission,
    ) -> Result<SubmitBlockResponse> {
        let candidate = &submission.candidate;
        let full_candidate = submission.stateless_request();
        let health = self.current_health();
        let primary_submit = submit_cached_to_node(
            self.primary.as_ref(),
            candidate,
            self.primary_timeout,
            "authority",
        );
        let secondary_submit = async {
            let Some(full_candidate) = full_candidate.as_ref() else {
                return (
                    self.secondary_control.not_attempted(),
                    skipped_submit_audit(
                        self.secondary.name(),
                        "best_effort",
                        "stateless_candidate_material_missing",
                    ),
                );
            };
            if let Err(reason) = self.secondary_circuit.eligibility() {
                return (
                    self.secondary_control.not_attempted(),
                    skipped_submit_audit(self.secondary.name(), "best_effort", reason),
                );
            }
            let _in_flight = if self.secondary_control.mode() == "continuous_bounded" {
                let Ok(permit) = self.secondary_in_flight.clone().try_acquire_owned() else {
                    return (
                        self.secondary_control.not_attempted(),
                        skipped_submit_audit(
                            self.secondary.name(),
                            "best_effort",
                            "secondary_concurrency_limit",
                        ),
                    );
                };
                Some(permit)
            } else {
                None
            };
            if let Err(reason) =
                secondary_submit_eligibility(health.as_ref(), self.health_max_age, candidate)
            {
                return (
                    self.secondary_control.not_attempted(),
                    skipped_submit_audit(self.secondary.name(), "best_effort", reason),
                );
            }
            let control = self.secondary_control.clone();
            let candidate_hash = candidate.hash_hex.clone();
            let claim_hash = candidate_hash.clone();
            let claim_task = tokio::task::spawn_blocking(move || control.claim(&claim_hash));
            let secondary_claim =
                match tokio::time::timeout(self.secondary_timeout, claim_task).await {
                    Ok(Ok(claim)) => claim,
                    Ok(Err(error)) => self.secondary_control.failed(
                        &candidate_hash,
                        format!("persistent latch task failed: {error}"),
                    ),
                    Err(_) => self.secondary_control.failed(
                        &candidate_hash,
                        format!(
                            "persistent secondary claim timed out after {} ms",
                            self.secondary_timeout.as_millis()
                        ),
                    ),
                };
            let post_claim_health = self.current_health();
            let eligibility = if secondary_claim.error.is_some() {
                Err(self.secondary_control.persistence_failure_reason())
            } else if !secondary_claim.claimed {
                Err(self.secondary_control.exhausted_reason())
            } else {
                secondary_submit_eligibility(
                    post_claim_health.as_ref(),
                    self.health_max_age,
                    candidate,
                )
            };
            let audit = match eligibility {
                Ok(()) => {
                    submit_full_to_node(
                        self.secondary.as_ref(),
                        full_candidate,
                        self.secondary_timeout,
                        "best_effort",
                    )
                    .await
                }
                Err(reason) => skipped_submit_audit(self.secondary.name(), "best_effort", reason),
            };
            self.secondary_circuit.record(&audit);
            (secondary_claim, audit)
        };
        let (primary_audit, (secondary_claim, secondary_audit)) =
            tokio::join!(primary_submit, secondary_submit);

        let primary_ok = primary_audit
            .response
            .as_ref()
            .is_some_and(|value| value.ok);
        let secondary_ok = secondary_audit
            .response
            .as_ref()
            .is_some_and(|value| value.ok);
        let status =
            parallel_submit_status(primary_ok, secondary_ok, &primary_audit, &secondary_audit);
        let primary_health = health
            .as_ref()
            .map(|value| value.primary.clone())
            .unwrap_or_else(|| unavailable_health_audit(self.primary.name()));
        let secondary_health = health
            .as_ref()
            .map(|value| value.secondary.clone())
            .unwrap_or_else(|| unavailable_health_audit(self.secondary.name()));
        let mut response = if secondary_ok && !primary_ok {
            secondary_audit
                .response
                .clone()
                .expect("secondary_ok requires a response")
        } else {
            primary_audit
                .response
                .clone()
                .unwrap_or_else(|| SubmitBlockResponse {
                    ok: false,
                    hash: Some(candidate.hash_hex.clone()),
                    extra: serde_json::json!({
                        "status": "authority_submit_failed",
                        "retryable": true,
                    }),
                })
        };
        response.ok = primary_ok || secondary_ok;
        let circuit = self.secondary_circuit.snapshot();
        let mut extra = response.extra.as_object().cloned().unwrap_or_default();
        extra.insert(
            "parallel_submit".to_owned(),
            serde_json::json!({
                "enabled": true,
                "authority": self.primary.name(),
                "secondary": self.secondary.name(),
                "status": status,
                "authority_ok": primary_ok,
                "secondary_ok": secondary_ok,
                "accepted_by": match (primary_ok, secondary_ok) {
                    (true, true) => "both",
                    (true, false) => "authority",
                    (false, true) => "secondary",
                    (false, false) => "none",
                },
                "local_canonical_nodes": {
                    "authority": primary_audit.response.as_ref().is_some_and(node_response_has_local_canonical_relay_failure),
                    "secondary": secondary_audit.response.as_ref().is_some_and(node_response_has_local_canonical_relay_failure),
                },
                "secondary_candidate_control": {
                    "mode": secondary_claim.mode,
                    "persistent_global_switch": "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED",
                    "claim_key": secondary_claim.claim_key,
                    "claimed": secondary_claim.claimed,
                    "persistence": secondary_claim.persistence,
                    "error": secondary_claim.error,
                    "circuit": {
                        "failure_threshold": self.secondary_circuit.failure_threshold,
                        "consecutive_failures": circuit.consecutive_failures,
                        "open_remaining_ms": circuit.open_remaining_ms,
                    },
                },
                "secondary_candidate_budget": {
                    "configured": secondary_claim.configured,
                    "before": secondary_claim.before,
                    "claimed": secondary_claim.claimed,
                    "remaining": secondary_claim.remaining,
                    "persistence": secondary_claim.persistence,
                    "error": secondary_claim.error,
                },
                "health_age_ms": health.as_ref().map(ParallelHealthSnapshot::age_ms),
                "candidate_parent_health": health
                    .as_ref()
                    .map(|snapshot| candidate_parent_health_json(snapshot, candidate)),
                "primary_health": primary_health_json(&primary_health),
                "secondary_health": primary_health_json(&secondary_health),
                "primary_submit": submit_audit_json(&primary_audit),
                "secondary_submit": submit_audit_json(&secondary_audit),
            }),
        );
        response.extra = serde_json::Value::Object(extra);
        info!(
            job_id = candidate.job_id,
            hash = candidate.hash_hex,
            authority = self.primary.name(),
            secondary = self.secondary.name(),
            status,
            authority_ok = primary_ok,
            secondary_ok,
            primary_outcome = primary_audit.outcome,
            secondary_outcome = secondary_audit.outcome,
            secondary_skip_reason = secondary_audit.skip_reason.unwrap_or("none"),
            secondary_control_mode = secondary_claim.mode,
            secondary_claimed = secondary_claim.claimed,
            secondary_budget_remaining = ?secondary_claim.remaining,
            secondary_circuit_failures = circuit.consecutive_failures,
            secondary_circuit_open_ms = circuit.open_remaining_ms,
            primary_elapsed_us = primary_audit.elapsed_us,
            secondary_elapsed_us = secondary_audit.elapsed_us,
            "parallel candidate submission completed"
        );
        Ok(response)
    }
}

#[derive(Clone, Debug)]
struct SecondaryCandidateClaim {
    mode: &'static str,
    configured: Option<u64>,
    before: Option<u64>,
    claimed: bool,
    remaining: Option<u64>,
    claim_key: String,
    persistence: &'static str,
    error: Option<String>,
}

impl SecondaryCandidateClaim {
    fn not_attempted(mode: &'static str, configured: Option<u64>, remaining: Option<u64>) -> Self {
        Self {
            mode,
            configured,
            before: configured,
            claimed: false,
            remaining,
            claim_key: String::new(),
            persistence: "not_attempted",
            error: None,
        }
    }

    fn failed(mode: &'static str, candidate_hash: &str, error: String) -> Self {
        Self {
            mode,
            configured: None,
            before: None,
            claimed: false,
            remaining: None,
            claim_key: candidate_hash.to_owned(),
            persistence: "o_excl_per_candidate",
            error: Some(error),
        }
    }

    fn failed_one_shot(configured: u64, candidate_hash: &str, error: String) -> Self {
        Self {
            mode: "one_shot",
            configured: Some(configured),
            before: Some(configured),
            claimed: false,
            remaining: Some(0),
            claim_key: candidate_hash.to_owned(),
            persistence: "o_excl_file",
            error: Some(error),
        }
    }
}

#[derive(Clone, Debug)]
struct HealthAudit {
    node: String,
    started_unix_ms: u64,
    finished_unix_ms: u64,
    elapsed_us: u64,
    outcome: &'static str,
    snapshot: Option<NodeHealth>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct ParallelHealthSnapshot {
    primary: HealthAudit,
    secondary: HealthAudit,
    observed_at: Instant,
    observed_unix_ms: u64,
    primary_tip_observed_since_unix_ms: Option<u64>,
    secondary_tip_observed_since_unix_ms: Option<u64>,
}

impl ParallelHealthSnapshot {
    fn age_ms(&self) -> u64 {
        self.observed_at
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

fn health_tip_identity(audit: &HealthAudit) -> Option<(u64, String, String)> {
    let snapshot = audit.snapshot.as_ref()?;
    Some((
        snapshot.height?,
        canonical_health_hash(snapshot.tip.as_deref()?).ok()?,
        canonical_chainwork(snapshot.chainwork.as_deref()?).ok()?,
    ))
}

fn tip_observed_since(
    previous: Option<&HealthAudit>,
    previous_since: Option<u64>,
    current: &HealthAudit,
    observed_unix_ms: u64,
) -> Option<u64> {
    let current_identity = health_tip_identity(current)?;
    if previous.and_then(health_tip_identity).as_ref() == Some(&current_identity) {
        Some(previous_since.unwrap_or(observed_unix_ms))
    } else {
        Some(observed_unix_ms)
    }
}

fn candidate_parent_health_json(
    snapshot: &ParallelHealthSnapshot,
    candidate: &BlockCandidateSubmitRequest,
) -> serde_json::Value {
    let candidate_parent = CanonicalBlockHash::from_candidate_header(candidate);
    let primary_tip = snapshot
        .primary
        .snapshot
        .as_ref()
        .and_then(|health| health.tip.as_deref())
        .and_then(CanonicalBlockHash::from_display_hex);
    let secondary_tip = snapshot
        .secondary
        .snapshot
        .as_ref()
        .and_then(|health| health.tip.as_deref())
        .and_then(CanonicalBlockHash::from_display_hex);
    serde_json::json!({
        "health_observed_unix_ms": snapshot.observed_unix_ms,
        "primary_tip_observed_since_unix_ms": snapshot.primary_tip_observed_since_unix_ms,
        "primary_tip_age_ms": snapshot.primary_tip_observed_since_unix_ms
            .map(|since| snapshot.observed_unix_ms.saturating_sub(since)),
        "secondary_tip_observed_since_unix_ms": snapshot.secondary_tip_observed_since_unix_ms,
        "secondary_tip_age_ms": snapshot.secondary_tip_observed_since_unix_ms
            .map(|since| snapshot.observed_unix_ms.saturating_sub(since)),
        "candidate_parent_matches_primary": candidate_parent.is_some() && candidate_parent == primary_tip,
        "candidate_parent_matches_secondary": candidate_parent.is_some() && candidate_parent == secondary_tip,
        "nodes_same_tip": primary_tip.is_some() && primary_tip == secondary_tip,
        "scope": "local_node_tip_observation",
        "global_freshness_proven": false,
    })
}

#[derive(Clone, Debug)]
struct SubmitAudit {
    node: String,
    role: &'static str,
    started_unix_ms: u64,
    finished_unix_ms: u64,
    elapsed_us: u64,
    outcome: &'static str,
    response: Option<SubmitBlockResponse>,
    error: Option<String>,
    skip_reason: Option<&'static str>,
}

fn node_response_has_local_canonical_relay_failure(response: &SubmitBlockResponse) -> bool {
    response
        .extra
        .get("status")
        .and_then(serde_json::Value::as_str)
        == Some("accepted_local_relay_failed")
        || (response
            .extra
            .pointer("/node_observability/local_canonical")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && response
                .extra
                .pointer("/node_observability/relay_ack/ok")
                .and_then(serde_json::Value::as_bool)
                == Some(false))
}

fn submit_outcome(response: &SubmitBlockResponse) -> &'static str {
    if response.ok {
        "accepted"
    } else if node_response_has_local_canonical_relay_failure(response) {
        "local_canonical_relay_failed"
    } else {
        "rejected"
    }
}

async fn health_with_timeout(
    endpoint: &dyn CandidateNodeEndpoint,
    timeout_duration: Duration,
) -> HealthAudit {
    let started_unix_ms = unix_ms();
    let started = Instant::now();
    let result = timeout(timeout_duration, endpoint.health()).await;
    let elapsed_us = elapsed_us(started);
    let finished_unix_ms = unix_ms();
    match result {
        Ok(Ok(snapshot)) => HealthAudit {
            node: endpoint.name().to_owned(),
            started_unix_ms,
            finished_unix_ms,
            elapsed_us,
            outcome: "ok",
            snapshot: Some(snapshot),
            error: None,
        },
        Ok(Err(error)) => HealthAudit {
            node: endpoint.name().to_owned(),
            started_unix_ms,
            finished_unix_ms,
            elapsed_us,
            outcome: "error",
            snapshot: None,
            error: Some(error),
        },
        Err(_) => HealthAudit {
            node: endpoint.name().to_owned(),
            started_unix_ms,
            finished_unix_ms,
            elapsed_us,
            outcome: "timeout",
            snapshot: None,
            error: Some(format!(
                "health timeout after {} ms",
                timeout_duration.as_millis()
            )),
        },
    }
}

async fn submit_cached_to_node(
    endpoint: &dyn CandidateNodeEndpoint,
    candidate: &BlockCandidateSubmitRequest,
    timeout_duration: Duration,
    role: &'static str,
) -> SubmitAudit {
    let started_unix_ms = unix_ms();
    let started = Instant::now();
    let result = timeout(
        timeout_duration,
        endpoint.submit_cached_candidate(candidate),
    )
    .await;
    let elapsed_us = elapsed_us(started);
    let finished_unix_ms = unix_ms();
    match result {
        Ok(Ok(response)) => SubmitAudit {
            node: endpoint.name().to_owned(),
            role,
            started_unix_ms,
            finished_unix_ms,
            elapsed_us,
            outcome: submit_outcome(&response),
            response: Some(response),
            error: None,
            skip_reason: None,
        },
        Ok(Err(error)) => SubmitAudit {
            node: endpoint.name().to_owned(),
            role,
            started_unix_ms,
            finished_unix_ms,
            elapsed_us,
            outcome: "transport_error",
            response: None,
            error: Some(error),
            skip_reason: None,
        },
        Err(_) => SubmitAudit {
            node: endpoint.name().to_owned(),
            role,
            started_unix_ms,
            finished_unix_ms,
            elapsed_us,
            outcome: "timeout",
            response: None,
            error: Some(format!(
                "submit timeout after {} ms",
                timeout_duration.as_millis()
            )),
            skip_reason: None,
        },
    }
}

async fn submit_full_to_node(
    endpoint: &dyn CandidateNodeEndpoint,
    candidate: &StatelessBlockCandidateSubmitRequest<'_>,
    timeout_duration: Duration,
    role: &'static str,
) -> SubmitAudit {
    let started_unix_ms = unix_ms();
    let started = Instant::now();
    let result = timeout(timeout_duration, endpoint.submit_full_candidate(candidate)).await;
    let elapsed_us = elapsed_us(started);
    let finished_unix_ms = unix_ms();
    match result {
        Ok(Ok(response)) => SubmitAudit {
            node: endpoint.name().to_owned(),
            role,
            started_unix_ms,
            finished_unix_ms,
            elapsed_us,
            outcome: submit_outcome(&response),
            response: Some(response),
            error: None,
            skip_reason: None,
        },
        Ok(Err(error)) => SubmitAudit {
            node: endpoint.name().to_owned(),
            role,
            started_unix_ms,
            finished_unix_ms,
            elapsed_us,
            outcome: "transport_error",
            response: None,
            error: Some(error),
            skip_reason: None,
        },
        Err(_) => SubmitAudit {
            node: endpoint.name().to_owned(),
            role,
            started_unix_ms,
            finished_unix_ms,
            elapsed_us,
            outcome: "timeout",
            response: None,
            error: Some(format!(
                "submit timeout after {} ms",
                timeout_duration.as_millis()
            )),
            skip_reason: None,
        },
    }
}

fn secondary_submit_eligibility(
    health: Option<&ParallelHealthSnapshot>,
    max_age: Duration,
    candidate: &BlockCandidateSubmitRequest,
) -> std::result::Result<(), &'static str> {
    let Some(health) = health else {
        return Err("health_not_ready");
    };
    if health.observed_at.elapsed() > max_age {
        return Err("health_stale");
    }
    let primary = &health.primary;
    let secondary = &health.secondary;
    let (Some(primary), Some(secondary)) = (primary.snapshot.as_ref(), secondary.snapshot.as_ref())
    else {
        return Err("health_unavailable");
    };
    let primary_complete = primary.height.is_some()
        && primary
            .tip
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        && primary
            .chainwork
            .as_deref()
            .is_some_and(|value| !value.is_empty());
    let secondary_complete = secondary.height.is_some()
        && secondary
            .tip
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        && secondary
            .chainwork
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        && secondary.peers.is_some();
    if !primary_complete || !secondary_complete {
        return Err("health_incomplete");
    }
    if secondary.peers == Some(0) {
        return Err("secondary_no_peers");
    }
    let primary_tip =
        CanonicalBlockHash::from_display_hex(primary.tip.as_deref().ok_or("health_incomplete")?)
            .ok_or("health_incomplete")?;
    let secondary_tip =
        CanonicalBlockHash::from_display_hex(secondary.tip.as_deref().ok_or("health_incomplete")?)
            .ok_or("health_incomplete")?;
    if primary.height != secondary.height
        || primary_tip != secondary_tip
        || primary.chainwork != secondary.chainwork
    {
        return Err("chain_state_mismatch");
    }
    let candidate_parent = CanonicalBlockHash::from_candidate_header(candidate)
        .ok_or("candidate_parent_unavailable")?;
    if primary_tip != candidate_parent {
        return Err("candidate_parent_mismatch");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CanonicalBlockHash([u8; 32]);

impl CanonicalBlockHash {
    fn from_display_hex(value: &str) -> Option<Self> {
        let value = value.trim();
        let value = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .unwrap_or(value);
        let decoded = hex::decode(value).ok()?;
        let bytes: [u8; 32] = decoded.try_into().ok()?;
        Some(Self(bytes))
    }

    fn from_candidate_header(candidate: &BlockCandidateSubmitRequest) -> Option<Self> {
        let header = hex::decode(&candidate.header_hex).ok()?;
        if header.len() != 84 {
            return None;
        }
        let bytes: [u8; 32] = header[4..36].try_into().ok()?;
        Some(Self(bytes))
    }
}

fn unavailable_health_audit(node: &str) -> HealthAudit {
    let now = unix_ms();
    HealthAudit {
        node: node.to_owned(),
        started_unix_ms: now,
        finished_unix_ms: now,
        elapsed_us: 0,
        outcome: "not_observed",
        snapshot: None,
        error: None,
    }
}

fn skipped_submit_audit(node: &str, role: &'static str, reason: &'static str) -> SubmitAudit {
    let now = unix_ms();
    SubmitAudit {
        node: node.to_owned(),
        role,
        started_unix_ms: now,
        finished_unix_ms: now,
        elapsed_us: 0,
        outcome: "skipped",
        response: None,
        error: None,
        skip_reason: Some(reason),
    }
}

fn parallel_submit_status(
    primary_ok: bool,
    secondary_ok: bool,
    primary: &SubmitAudit,
    secondary: &SubmitAudit,
) -> &'static str {
    match (primary_ok, secondary_ok, secondary.outcome) {
        (true, true, _) => "both_accepted",
        (true, false, "skipped") => "authority_accepted_secondary_skipped",
        (true, false, "local_canonical_relay_failed") => {
            "authority_accepted_secondary_local_canonical_relay_failed"
        }
        (true, false, _) => "authority_accepted_secondary_failed",
        (false, true, _) if primary.outcome == "local_canonical_relay_failed" => {
            "secondary_accepted_authority_local_canonical_relay_failed"
        }
        (false, true, _) => "secondary_accepted_authority_failed",
        (false, false, "local_canonical_relay_failed")
            if primary.outcome == "local_canonical_relay_failed" =>
        {
            "both_local_canonical_relay_failed"
        }
        (false, false, _) if primary.outcome == "local_canonical_relay_failed" => {
            "authority_local_canonical_relay_failed"
        }
        (false, false, "local_canonical_relay_failed") => "secondary_local_canonical_relay_failed",
        (false, false, "skipped") => "authority_failed_secondary_skipped",
        (false, false, _) => "both_failed",
    }
}

fn primary_health_json(audit: &HealthAudit) -> serde_json::Value {
    serde_json::json!({
        "node": audit.node,
        "started_unix_ms": audit.started_unix_ms,
        "finished_unix_ms": audit.finished_unix_ms,
        "elapsed_us": audit.elapsed_us,
        "outcome": audit.outcome,
        "height": audit.snapshot.as_ref().and_then(|value| value.height),
        "tip": audit.snapshot.as_ref().and_then(|value| value.tip.as_deref()),
        "chainwork": audit.snapshot.as_ref().and_then(|value| value.chainwork.as_deref()),
        "peers": audit.snapshot.as_ref().and_then(|value| value.peers),
        "error": audit.error,
    })
}

fn submit_audit_json(audit: &SubmitAudit) -> serde_json::Value {
    serde_json::json!({
        "node": audit.node,
        "role": audit.role,
        "started_unix_ms": audit.started_unix_ms,
        "finished_unix_ms": audit.finished_unix_ms,
        "elapsed_us": audit.elapsed_us,
        "outcome": audit.outcome,
        "response": audit.response,
        "error": audit.error,
        "skip_reason": audit.skip_reason,
    })
}

fn bounded_submit_timeout(value: Duration) -> Duration {
    value.clamp(Duration::from_millis(100), Duration::from_secs(10))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Clone, Copy, Debug)]
struct CandidateObservation {
    detected_at: Instant,
    detected_unix_ms: u64,
}

impl CandidateObservation {
    fn now() -> Self {
        Self {
            detected_at: Instant::now(),
            detected_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        }
    }

    fn elapsed_us(self) -> u64 {
        self.detected_at
            .elapsed()
            .as_micros()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

#[allow(clippy::too_many_arguments)]
async fn submit_block_candidate(
    submitter: Option<&dyn BlockSubmitter>,
    repository: Option<&dyn MiningRepository>,
    job: &PoolJob,
    pool_state: &SharedPoolState,
    miner: &str,
    submit: &SubmitParams,
    share: &VerifiedShare,
    extranonce1_le: [u8; 4],
    difficulty: f64,
    observation: CandidateObservation,
) -> Result<()> {
    let Some(submitter) = submitter else {
        warn!(
            job_id = submit.job_id,
            hash = hex::encode(share.hash),
            "block candidate found but candidate submission is disabled"
        );
        return Ok(());
    };

    let effort_pct = block_effort_pct(job, pool_state, difficulty);
    let submission = BlockCandidateSubmission {
        candidate: block_candidate_submit_request(job, miner, submit, share, extranonce1_le),
        candidate_material: job
            .candidate_material
            .read()
            .expect("candidate material slot lock poisoned")
            .clone(),
    };
    let request = &submission.candidate;
    let detected_to_submit_start_us = observation.elapsed_us();
    let node_submit_started = Instant::now();
    let submit_result = submitter.submit_candidate(&submission).await;
    let node_roundtrip_us = elapsed_us(node_submit_started);
    let response = match submit_result {
        Ok(mut response) => {
            attach_candidate_observability(
                &mut response,
                observation,
                detected_to_submit_start_us,
                node_roundtrip_us,
            );
            response
        }
        Err(err) => {
            // Keep the solved header auditable and queryable by the block
            // reconciler even when the node call fails before returning HTTP.
            if let Some(repository) = repository {
                let mut failed_response = SubmitBlockResponse {
                    ok: false,
                    hash: Some(request.hash_hex.clone()),
                    extra: serde_json::json!({
                        "transport_error": err.to_string(),
                        "retryable": true,
                    }),
                };
                attach_candidate_observability(
                    &mut failed_response,
                    observation,
                    detected_to_submit_start_us,
                    node_roundtrip_us,
                );
                let record_started = Instant::now();
                repository
                    .record_block_candidate(&block_candidate_record(
                        miner,
                        &submission,
                        &failed_response,
                        effort_pct,
                    ))
                    .await?;
                let candidate_record_us = elapsed_us(record_started);
                let candidate_total_us = observation.elapsed_us();
                pool_state.record_candidate_propagation(
                    Duration::from_micros(detected_to_submit_start_us),
                    Duration::from_micros(node_roundtrip_us),
                    Duration::from_micros(candidate_record_us),
                    Duration::from_micros(candidate_total_us),
                    None,
                    None,
                );
                warn!(
                    job_id = request.job_id,
                    hash = request.hash_hex,
                    candidate_detected_to_submit_start_us = detected_to_submit_start_us,
                    node_roundtrip_us,
                    candidate_record_us,
                    candidate_total_us,
                    %err,
                    "block candidate submission transport failed; keeping miner session active"
                );
                return Ok(());
            }
            let candidate_total_us = observation.elapsed_us();
            pool_state.record_candidate_propagation(
                Duration::from_micros(detected_to_submit_start_us),
                Duration::from_micros(node_roundtrip_us),
                Duration::ZERO,
                Duration::from_micros(candidate_total_us),
                None,
                None,
            );
            warn!(
                job_id = request.job_id,
                hash = request.hash_hex,
                candidate_detected_to_submit_start_us = detected_to_submit_start_us,
                node_roundtrip_us,
                candidate_record_us = 0,
                candidate_total_us,
                %err,
                "block candidate submission transport failed; keeping miner session active"
            );
            return Ok(());
        }
    };
    let record_started = Instant::now();
    if let Some(repository) = repository {
        repository
            .record_block_candidate(&block_candidate_record(
                miner,
                &submission,
                &response,
                effort_pct,
            ))
            .await?;
    }
    let candidate_record_us = elapsed_us(record_started);
    let candidate_total_us = observation.elapsed_us();
    let node_observability = candidate_node_observability(&response);
    let node_accept = candidate_node_duration(&response, "accept_elapsed_us");
    let relay_enqueue = candidate_node_duration(&response, "relay_enqueue_elapsed_us");
    pool_state.record_candidate_propagation(
        Duration::from_micros(detected_to_submit_start_us),
        Duration::from_micros(node_roundtrip_us),
        Duration::from_micros(candidate_record_us),
        Duration::from_micros(candidate_total_us),
        node_accept,
        relay_enqueue,
    );
    if response.ok {
        info!(
            job_id = request.job_id,
            hash = request.hash_hex,
            effort_pct,
            candidate_detected_to_submit_start_us = detected_to_submit_start_us,
            node_roundtrip_us,
            candidate_record_us,
            candidate_total_us,
            node_observability = %node_observability,
            "block candidate submitted"
        );
    } else {
        warn!(
            job_id = request.job_id,
            hash = request.hash_hex,
            effort_pct,
            candidate_detected_to_submit_start_us = detected_to_submit_start_us,
            node_roundtrip_us,
            candidate_record_us,
            candidate_total_us,
            node_observability = %node_observability,
            response = %serde_json::to_string(&response).unwrap_or_default(),
            "block candidate rejected; keeping miner session active"
        );
    }
    Ok(())
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().try_into().unwrap_or(u64::MAX)
}

fn attach_candidate_observability(
    response: &mut SubmitBlockResponse,
    observation: CandidateObservation,
    detected_to_submit_start_us: u64,
    node_roundtrip_us: u64,
) {
    let mut extra = response.extra.as_object().cloned().unwrap_or_default();
    extra.insert(
        "pool_observability".to_owned(),
        serde_json::json!({
            "candidate_detected_unix_ms": observation.detected_unix_ms,
            "candidate_detected_to_submit_start_us": detected_to_submit_start_us,
            "node_roundtrip_us": node_roundtrip_us,
        }),
    );
    response.extra = serde_json::Value::Object(extra);
}

fn candidate_node_observability(response: &SubmitBlockResponse) -> String {
    serde_json::to_string(
        response
            .extra
            .get("node_observability")
            .unwrap_or(&serde_json::Value::Null),
    )
    .unwrap_or_else(|_| "null".to_owned())
}

fn candidate_node_duration(response: &SubmitBlockResponse, field: &str) -> Option<Duration> {
    response
        .extra
        .get("node_observability")
        .and_then(|value| value.get(field))
        .and_then(serde_json::Value::as_u64)
        .map(Duration::from_micros)
}

fn block_effort_pct(job: &PoolJob, pool_state: &SharedPoolState, current_difficulty: f64) -> f64 {
    let expected_difficulty =
        difficulty_for_target(&job.template.share_target, &job.template.network_target);
    let round_difficulty =
        pool_state.snapshot().totals.round_share_difficulty_sum + current_difficulty.max(0.0);
    if expected_difficulty <= 0.0 || round_difficulty <= 0.0 {
        0.0
    } else {
        (round_difficulty / expected_difficulty) * 100.0
    }
}

fn block_candidate_submit_request(
    job: &PoolJob,
    miner: &str,
    submit: &SubmitParams,
    share: &VerifiedShare,
    extranonce1_le: [u8; 4],
) -> BlockCandidateSubmitRequest {
    let extranonce2_le = parse_le_u32_hex_bytes("extranonce2", &submit.extranonce2_hex)
        .expect("verified submit has four-byte extranonce2");
    let extranonce = compose_extranonce(extranonce1_le, extranonce2_le);
    BlockCandidateSubmitRequest {
        job_id: submit.job_id.clone(),
        worker_name: worker_name_from_submit(miner, &submit.worker_name),
        header_hex: hex::encode(share.header),
        hash_hex: hex::encode(share.hash),
        coinbase_txid_hex: hex::encode(share.coinbase_txid),
        coinbase_hex: hex::encode(coinbase_bytes(
            &job.template.coinbase_prefix,
            extranonce,
            &job.template.coinbase_suffix,
        )),
        merkle_root_hex: hex::encode(share.merkle_root),
        extranonce2_hex: submit.extranonce2_hex.clone(),
        ntime_hex: submit.ntime_hex.clone(),
        nonce_hex: submit.nonce_hex.clone(),
    }
}

fn block_candidate_record(
    miner: &str,
    submission: &BlockCandidateSubmission,
    response: &SubmitBlockResponse,
    effort_pct: f64,
) -> csd_pool_db::BlockCandidateRecord {
    let request = &submission.candidate;
    let material = submission.candidate_material.as_deref();
    csd_pool_db::BlockCandidateRecord {
        hash_hex: response
            .hash
            .clone()
            .unwrap_or_else(|| request.hash_hex.clone()),
        job_id: request.job_id.clone(),
        miner: miner.to_owned(),
        worker_name: request.worker_name.clone(),
        reward_base_units: 0,
        effort_pct,
        candidate_payload_json: serde_json::json!({
            "worker_name": request.worker_name,
            "job_id": request.job_id,
            "extranonce2_hex": request.extranonce2_hex,
            "nonce_hex": request.nonce_hex,
            "hash_hex": request.hash_hex,
            "block_template_sha256_hex": material.map(|value| &value.block_template_sha256_hex),
            "block_template_bytes": material.map(|value| value.block_template_bytes),
        }),
        submit_response_json: serde_json::to_value(response).unwrap_or_else(|_| {
            serde_json::json!({
                "ok": response.ok,
                "hash": response.hash,
            })
        }),
    }
}

async fn persist_accepted_share(
    repository: Option<&dyn MiningRepository>,
    job: &PoolJob,
    miner: &str,
    submit: &SubmitParams,
    share: &VerifiedShare,
    difficulty: f64,
    session_id: Option<&str>,
) -> Result<bool> {
    let Some(repository) = repository else {
        return Ok(true);
    };

    repository
        .upsert_job(&job_record_from_pool_job(job, JobReason::from_job(job)))
        .await?;
    repository
        .insert_share(&share_record_from_submit(
            session_id, miner, submit, share, difficulty,
        ))
        .await
        .map_err(BridgeError::from)
}

async fn persist_share_event(
    repository: Option<&dyn MiningRepository>,
    event: &ShareEventRecord,
) -> Result<()> {
    let Some(repository) = repository else {
        return Ok(());
    };

    repository.insert_share_event(event).await?;
    Ok(())
}

fn share_event_from_submit(
    session_id: Option<&str>,
    miner: &str,
    submit: &SubmitParams,
    kind: &str,
    reason: &str,
) -> ShareEventRecord {
    ShareEventRecord {
        session_id: session_id.map(str::to_owned),
        miner: miner.to_owned(),
        worker_name: worker_name_from_submit(miner, &submit.worker_name),
        job_id: Some(submit.job_id.clone()),
        kind: kind.to_owned(),
        reason: reason.to_owned(),
    }
}

fn job_record_from_pool_job(job: &PoolJob, reason: JobReason) -> JobRecord {
    JobRecord {
        job_id: job.notify.job_id.clone(),
        prev_hash_be_hex: job.notify.prev_hash_be_hex.clone(),
        version_hex: job.notify.version_hex.clone(),
        nbits_hex: job.notify.nbits_hex.clone(),
        ntime_hex: job.notify.ntime_hex.clone(),
        network_target: job.template.network_target,
        share_target: job.template.share_target,
        coinb1_hex: job.notify.coinb1_hex.clone(),
        coinb2_hex: job.notify.coinb2_hex.clone(),
        merkle_branches_hex: job.notify.merkle_branches_hex.clone(),
        clean_jobs: job.notify.clean_jobs,
        job_reason: reason.as_str().to_owned(),
    }
}

fn share_record_from_submit(
    session_id: Option<&str>,
    miner: &str,
    submit: &SubmitParams,
    share: &VerifiedShare,
    difficulty: f64,
) -> ShareRecord {
    ShareRecord {
        session_id: session_id.map(str::to_owned),
        miner: miner.to_owned(),
        worker_name: worker_name_from_submit(miner, &submit.worker_name),
        job_id: submit.job_id.clone(),
        difficulty,
        hash: share.hash,
        extranonce2_hex: submit.extranonce2_hex.clone(),
        ntime_hex: submit.ntime_hex.clone(),
        nonce_hex: submit.nonce_hex.clone(),
        is_block_candidate: share.is_block_candidate,
    }
}

fn worker_name_from_submit(miner: &str, submit_worker_name: &str) -> String {
    submit_worker_name
        .strip_prefix(miner)
        .and_then(|suffix| suffix.strip_prefix('.'))
        .filter(|suffix| !suffix.is_empty())
        .unwrap_or("default")
        .to_owned()
}

fn subscribe_response(id: Option<u64>, extranonce1: &str) -> Response {
    Response {
        id,
        result: serde_json::json!([[], extranonce1, 4]),
        error: None,
    }
}

#[cfg(test)]
fn extranonce1_for_session(session_id: u64) -> String {
    hex::encode((session_id as u32).to_le_bytes())
}

fn parse_authorize_worker(params: &Value) -> Option<String> {
    let username = params
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(Value::as_str)
        .map(str::trim)?;
    let username = username
        .strip_prefix("0x")
        .or_else(|| username.strip_prefix("0X"))
        .unwrap_or(username)
        .to_ascii_lowercase();
    let (address, worker) = username
        .split_once('.')
        .map_or((username.as_str(), None), |(address, worker)| {
            (address, Some(worker))
        });
    if !valid_addr20(address) || worker.is_some_and(|name| !valid_worker_name(name)) {
        return None;
    }
    Some(address.to_owned())
}

fn parse_authorize_worker_name(params: &Value) -> String {
    params
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(Value::as_str)
        .map(str::trim)
        .and_then(|username| username.split_once('.').map(|(_, worker)| worker))
        .filter(|worker| valid_worker_name(worker))
        .unwrap_or("default")
        .to_owned()
}

fn parse_subscribe_user_agent(params: &Value) -> Option<String> {
    params
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(256).collect())
}

fn server_release() -> String {
    let name = std::env::var("CSD_POOL_RELEASE_NAME")
        .unwrap_or_else(|_| env!("CARGO_PKG_NAME").to_owned());
    let revision =
        std::env::var("CSD_POOL_RELEASE_REVISION").unwrap_or_else(|_| "unknown".to_owned());
    format!("{name}@{revision}")
}

fn server_instance() -> String {
    std::env::var("CSD_POOL_INSTANCE_ID").unwrap_or_else(|_| "default".to_owned())
}

fn parse_suggested_difficulty(params: &Value) -> Option<f64> {
    let value = params.as_array()?.first()?;
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn valid_addr20(addr: &str) -> bool {
    addr.len() == 40 && addr.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_worker_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn authorized_submit_worker(miner: &str, submitted: &str) -> bool {
    let normalized = submitted
        .strip_prefix("0x")
        .or_else(|| submitted.strip_prefix("0X"))
        .unwrap_or(submitted)
        .to_ascii_lowercase();
    if normalized == miner {
        return true;
    }
    normalized
        .strip_prefix(&format!("{miner}."))
        .is_some_and(valid_worker_name)
}

fn verify_submit(
    template: &WorkTemplate,
    extranonce1_le: [u8; 4],
    submit: &SubmitParams,
    difficulty: f64,
) -> std::result::Result<csd_pool_consensus::VerifiedShare, csd_pool_consensus::ConsensusError> {
    let solution = SubmitSolution {
        extranonce2_le: parse_le_u32_hex_bytes("extranonce2_hex", &submit.extranonce2_hex)?,
        ntime: parse_u32_hex("ntime_hex", &submit.ntime_hex)?,
        nonce: parse_u32_hex("nonce_hex", &submit.nonce_hex)?,
    };
    verify_share_with_difficulty(template, extranonce1_le, &solution, difficulty)
}

fn verify_submit_for_vardiff(
    template: &WorkTemplate,
    extranonce1_le: [u8; 4],
    submit: &SubmitParams,
    vardiff: &VardiffState,
    now: Instant,
) -> std::result::Result<(VerifiedShare, f64), csd_pool_consensus::ConsensusError> {
    let current_difficulty = vardiff.current_difficulty();
    match verify_submit(template, extranonce1_le, submit, current_difficulty) {
        Ok(share) => Ok((share, current_difficulty)),
        Err(current_error) => {
            let Some(previous_difficulty) = vardiff.previous_difficulty_in_grace(now) else {
                return Err(current_error);
            };
            verify_submit(template, extranonce1_le, submit, previous_difficulty)
                .map(|share| (share, previous_difficulty))
                .map_err(|_| current_error)
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ShareKey {
    job_id: String,
    extranonce2_hex: String,
    ntime_hex: String,
    nonce_hex: String,
}

impl ShareKey {
    fn from_submit(submit: &SubmitParams) -> Self {
        Self {
            job_id: submit.job_id.clone(),
            extranonce2_hex: submit.extranonce2_hex.clone(),
            ntime_hex: submit.ntime_hex.clone(),
            nonce_hex: submit.nonce_hex.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VardiffConfig {
    pub initial_difficulty: f64,
    pub min_difficulty: f64,
    pub max_difficulty: f64,
    pub target_share_secs: u64,
    pub ewma_alpha: f64,
    pub raise_ratio: f64,
    pub lower_ratio: f64,
    pub min_adjust_secs: u64,
    pub max_adjust_factor: f64,
    pub transition_grace_secs: u64,
}

impl Default for VardiffConfig {
    fn default() -> Self {
        csd_pool_config::StratumSection::default().into()
    }
}

impl From<csd_pool_config::StratumSection> for VardiffConfig {
    fn from(value: csd_pool_config::StratumSection) -> Self {
        Self {
            initial_difficulty: value.initial_difficulty,
            min_difficulty: value.min_difficulty,
            max_difficulty: value.max_difficulty,
            target_share_secs: value.target_share_secs,
            ewma_alpha: value.vardiff_ewma_alpha,
            raise_ratio: value.vardiff_raise_ratio,
            lower_ratio: value.vardiff_lower_ratio,
            min_adjust_secs: value.vardiff_min_adjust_secs,
            max_adjust_factor: value.vardiff_max_adjust_factor,
            transition_grace_secs: value.vardiff_transition_grace_secs,
        }
        .normalized()
    }
}

impl VardiffConfig {
    fn normalized(mut self) -> Self {
        if !self.min_difficulty.is_finite() || self.min_difficulty <= 0.0 {
            self.min_difficulty = 1.0;
        }
        if !self.max_difficulty.is_finite() || self.max_difficulty < self.min_difficulty {
            self.max_difficulty = self.min_difficulty;
        }
        self.min_difficulty = self.min_difficulty.ceil();
        self.max_difficulty = self.max_difficulty.floor().max(self.min_difficulty);
        if !self.initial_difficulty.is_finite() || self.initial_difficulty <= 0.0 {
            self.initial_difficulty = self.min_difficulty;
        }
        self.initial_difficulty = self
            .initial_difficulty
            .round()
            .clamp(self.min_difficulty, self.max_difficulty);
        if self.target_share_secs == 0 {
            self.target_share_secs = 20;
        }
        if !self.ewma_alpha.is_finite() || !(0.0..=1.0).contains(&self.ewma_alpha) {
            self.ewma_alpha = 0.25;
        }
        if self.ewma_alpha == 0.0 {
            self.ewma_alpha = 0.25;
        }
        if !self.raise_ratio.is_finite() || !(0.0..1.0).contains(&self.raise_ratio) {
            self.raise_ratio = 0.70;
        }
        if !self.lower_ratio.is_finite() || self.lower_ratio <= 1.0 {
            self.lower_ratio = 1.40;
        }
        if self.min_adjust_secs == 0 {
            self.min_adjust_secs = 120;
        }
        if !self.max_adjust_factor.is_finite() || self.max_adjust_factor < 1.0 {
            self.max_adjust_factor = 2.0;
        }
        if self.transition_grace_secs == 0 {
            self.transition_grace_secs = 120;
        }
        self
    }

    fn quantize_difficulty(&self, difficulty: f64) -> f64 {
        difficulty
            .round()
            .clamp(self.min_difficulty, self.max_difficulty)
    }
}

#[derive(Debug)]
pub struct VardiffState {
    config: VardiffConfig,
    current_difficulty: f64,
    previous_difficulty: Option<f64>,
    transition_until: Option<Instant>,
    last_share_at: Option<Instant>,
    last_adjust_at: Option<Instant>,
    ewma_share_secs: Option<f64>,
}

impl VardiffState {
    pub fn new(config: VardiffConfig) -> Self {
        let config = config.normalized();
        Self {
            current_difficulty: config.initial_difficulty,
            config,
            previous_difficulty: None,
            transition_until: None,
            last_share_at: None,
            last_adjust_at: None,
            ewma_share_secs: None,
        }
    }

    pub fn current_difficulty(&self) -> f64 {
        self.current_difficulty
    }

    pub fn previous_difficulty_in_grace(&self, now: Instant) -> Option<f64> {
        self.transition_until
            .filter(|until| now <= *until)
            .and(self.previous_difficulty)
            .filter(|previous| *previous < self.current_difficulty)
    }

    pub fn record_accepted_share(&mut self, now: Instant) -> Option<f64> {
        let Some(last_share_at) = self.last_share_at.replace(now) else {
            self.last_adjust_at = Some(now);
            return None;
        };
        let elapsed_secs = now
            .checked_duration_since(last_share_at)
            .unwrap_or_default()
            .as_secs_f64()
            .clamp(0.05, self.config.target_share_secs as f64 * 4.0);
        let target = self.config.target_share_secs as f64;
        let ewma = self
            .ewma_share_secs
            .map(|previous| {
                self.config.ewma_alpha * elapsed_secs + (1.0 - self.config.ewma_alpha) * previous
            })
            .unwrap_or(elapsed_secs);
        self.ewma_share_secs = Some(ewma);

        let last_adjust_at = self.last_adjust_at.get_or_insert(now);
        if now
            .checked_duration_since(*last_adjust_at)
            .unwrap_or_default()
            < Duration::from_secs(self.config.min_adjust_secs)
        {
            return None;
        }

        let desired_factor = if ewma < target * self.config.raise_ratio {
            (target / ewma).clamp(1.0, self.config.max_adjust_factor)
        } else if ewma > target * self.config.lower_ratio {
            (target / ewma).clamp(1.0 / self.config.max_adjust_factor, 1.0)
        } else {
            return None;
        };
        let next = (self.current_difficulty * desired_factor)
            .clamp(self.config.min_difficulty, self.config.max_difficulty);
        let next = self.config.quantize_difficulty(next);

        if relative_difference(next, self.current_difficulty) < 0.01 {
            return None;
        }
        self.apply_difficulty(next, now)
    }

    pub fn apply_suggested_difficulty(
        &mut self,
        suggested_difficulty: f64,
        now: Instant,
    ) -> Option<f64> {
        if !suggested_difficulty.is_finite() || suggested_difficulty <= 0.0 {
            return None;
        }
        if self.last_adjust_at.is_some_and(|last_adjust_at| {
            now.checked_duration_since(last_adjust_at)
                .unwrap_or_default()
                < Duration::from_secs(self.config.min_adjust_secs)
        }) {
            return None;
        }
        let single_step_min = self.current_difficulty / self.config.max_adjust_factor;
        let single_step_max = self.current_difficulty * self.config.max_adjust_factor;
        let next = suggested_difficulty
            .clamp(self.config.min_difficulty, self.config.max_difficulty)
            .clamp(single_step_min, single_step_max)
            .clamp(self.config.min_difficulty, self.config.max_difficulty);
        let next = self.config.quantize_difficulty(next);
        if relative_difference(next, self.current_difficulty) < 0.01 {
            return None;
        }
        self.ewma_share_secs = None;
        self.last_adjust_at = Some(now);
        self.apply_difficulty(next, now)
    }

    fn apply_difficulty(&mut self, next: f64, now: Instant) -> Option<f64> {
        let next = self.config.quantize_difficulty(next);
        if relative_difference(next, self.current_difficulty) < 0.01 {
            return None;
        }
        self.previous_difficulty = Some(self.current_difficulty);
        self.current_difficulty = next;
        self.transition_until = Some(now + Duration::from_secs(self.config.transition_grace_secs));
        self.last_adjust_at = Some(now);
        Some(next)
    }
}

fn relative_difference(left: f64, right: f64) -> f64 {
    if right == 0.0 {
        left.abs()
    } else {
        ((left - right) / right).abs()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbuseConfig {
    pub max_connections_per_ip: u32,
    pub max_sessions_per_address: u32,
    pub malformed_frame_limit: u32,
    pub auth_failure_limit: u32,
    pub invalid_share_limit: u32,
    pub ban_secs: u64,
}

impl Default for AbuseConfig {
    fn default() -> Self {
        csd_pool_config::AbuseSection::default().into()
    }
}

impl From<csd_pool_config::AbuseSection> for AbuseConfig {
    fn from(value: csd_pool_config::AbuseSection) -> Self {
        Self {
            max_connections_per_ip: value.max_connections_per_ip,
            max_sessions_per_address: value.max_sessions_per_address,
            malformed_frame_limit: value.malformed_frame_limit,
            auth_failure_limit: value.auth_failure_limit,
            invalid_share_limit: value.invalid_share_limit,
            ban_secs: value.ban_secs,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum AbuseReject {
    Banned,
    TooManyConnections,
    TooManyAddressSessions,
}

#[derive(Debug)]
pub struct AbuseManager {
    config: AbuseConfig,
    state: Mutex<HashMap<IpAddr, IpAbuseState>>,
    address_sessions: Mutex<HashMap<String, u32>>,
}

impl Default for AbuseManager {
    fn default() -> Self {
        Self::new(AbuseConfig::default())
    }
}

impl AbuseManager {
    pub fn new(config: AbuseConfig) -> Self {
        Self {
            config,
            state: Mutex::new(HashMap::new()),
            address_sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn try_open(
        self: &Arc<Self>,
        ip: IpAddr,
    ) -> std::result::Result<ConnectionPermit, AbuseReject> {
        let mut state = self.state.lock().expect("abuse state mutex poisoned");
        let entry = state.entry(ip).or_default();
        entry.expire_ban_if_needed(Instant::now());
        if entry.is_banned(Instant::now()) {
            return Err(AbuseReject::Banned);
        }
        if entry.active_connections >= self.config.max_connections_per_ip {
            return Err(AbuseReject::TooManyConnections);
        }
        entry.active_connections += 1;
        Ok(ConnectionPermit {
            manager: self.clone(),
            ip,
        })
    }

    pub fn try_open_address_session(
        self: &Arc<Self>,
        address: &str,
    ) -> std::result::Result<AddressSessionPermit, AbuseReject> {
        let mut sessions = self
            .address_sessions
            .lock()
            .expect("address session mutex poisoned");
        let active = sessions.entry(address.to_owned()).or_default();
        if *active >= self.config.max_sessions_per_address {
            return Err(AbuseReject::TooManyAddressSessions);
        }
        *active += 1;
        Ok(AddressSessionPermit {
            manager: self.clone(),
            address: address.to_owned(),
        })
    }

    pub fn record_malformed_frame(&self, ip: IpAddr) -> bool {
        self.record_offense(ip, OffenseKind::MalformedFrame)
    }

    pub fn record_auth_failure(&self, ip: IpAddr) -> bool {
        self.record_offense(ip, OffenseKind::AuthFailure)
    }

    pub fn record_invalid_share(&self, ip: IpAddr) -> bool {
        self.record_offense(ip, OffenseKind::InvalidShare)
    }

    pub fn is_banned(&self, ip: IpAddr) -> bool {
        let mut state = self.state.lock().expect("abuse state mutex poisoned");
        let entry = state.entry(ip).or_default();
        entry.expire_ban_if_needed(Instant::now());
        entry.is_banned(Instant::now())
    }

    fn close(&self, ip: IpAddr) {
        let mut state = self.state.lock().expect("abuse state mutex poisoned");
        if let Some(entry) = state.get_mut(&ip) {
            entry.active_connections = entry.active_connections.saturating_sub(1);
        }
    }

    fn close_address_session(&self, address: &str) {
        let mut sessions = self
            .address_sessions
            .lock()
            .expect("address session mutex poisoned");
        let Some(active) = sessions.get_mut(address) else {
            return;
        };
        *active = active.saturating_sub(1);
        if *active == 0 {
            sessions.remove(address);
        }
    }

    fn record_offense(&self, ip: IpAddr, kind: OffenseKind) -> bool {
        let mut state = self.state.lock().expect("abuse state mutex poisoned");
        let now = Instant::now();
        let entry = state.entry(ip).or_default();
        entry.expire_ban_if_needed(now);
        match kind {
            OffenseKind::MalformedFrame => entry.malformed_frames += 1,
            OffenseKind::AuthFailure => entry.auth_failures += 1,
            OffenseKind::InvalidShare => entry.invalid_shares += 1,
        }

        let should_ban = entry.malformed_frames >= self.config.malformed_frame_limit
            || entry.auth_failures >= self.config.auth_failure_limit
            || entry.invalid_shares >= self.config.invalid_share_limit;
        if should_ban {
            entry.banned_until = Some(now + Duration::from_secs(self.config.ban_secs));
        }
        should_ban
    }
}

#[derive(Debug)]
pub struct ConnectionPermit {
    manager: Arc<AbuseManager>,
    ip: IpAddr,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.manager.close(self.ip);
    }
}

#[derive(Debug)]
pub struct AddressSessionPermit {
    manager: Arc<AbuseManager>,
    address: String,
}

impl Drop for AddressSessionPermit {
    fn drop(&mut self) {
        self.manager.close_address_session(&self.address);
    }
}

#[derive(Debug, Default)]
struct IpAbuseState {
    active_connections: u32,
    malformed_frames: u32,
    auth_failures: u32,
    invalid_shares: u32,
    banned_until: Option<Instant>,
}

impl IpAbuseState {
    fn is_banned(&self, now: Instant) -> bool {
        self.banned_until
            .map(|banned_until| banned_until > now)
            .unwrap_or(false)
    }

    fn expire_ban_if_needed(&mut self, now: Instant) {
        if self
            .banned_until
            .map(|banned_until| banned_until <= now)
            .unwrap_or(false)
        {
            self.malformed_frames = 0;
            self.auth_failures = 0;
            self.invalid_shares = 0;
            self.banned_until = None;
        }
    }
}

enum OffenseKind {
    MalformedFrame,
    AuthFailure,
    InvalidShare,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, routing::post};
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    struct RotatingTemplateProvider {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TemplateProvider for RotatingTemplateProvider {
        async fn current_job(&self) -> csd_pool_node::Result<PoolJob> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(csd_pool_node::easy_static_job(if call == 0 {
                "job-initial"
            } else {
                "job-refreshed"
            }))
        }
    }

    struct TipAwareTemplateProvider {
        calls: AtomicUsize,
        tip_changed: AtomicBool,
    }

    #[async_trait::async_trait]
    impl TemplateProvider for TipAwareTemplateProvider {
        async fn current_job(&self) -> csd_pool_node::Result<PoolJob> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let mut job = csd_pool_node::easy_static_job(if call == 0 {
                "job-initial"
            } else {
                "job-refreshed"
            });
            if call > 0 {
                job.template.prev = [1; 32];
                job.notify.prev_hash_be_hex = "01".repeat(32);
            }
            Ok(job)
        }

        async fn current_tip(&self) -> csd_pool_node::Result<Option<[u8; 32]>> {
            Ok(Some(if self.tip_changed.load(Ordering::SeqCst) {
                [1; 32]
            } else {
                [0; 32]
            }))
        }
    }

    struct SameTipHeartbeatProvider {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TemplateProvider for SameTipHeartbeatProvider {
        async fn current_job(&self) -> csd_pool_node::Result<PoolJob> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let mut job = csd_pool_node::easy_static_job(format!("heartbeat-{call}"));
            job.notify.ntime_hex = format!("{:08x}", 0x6655_4400_u32 + call as u32);
            job.template.time = 0x6655_4400_u64 + call as u64;
            Ok(job)
        }

        async fn current_tip(&self) -> csd_pool_node::Result<Option<[u8; 32]>> {
            Ok(Some([0; 32]))
        }
    }

    struct LaggingTemplateProvider {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TemplateProvider for LaggingTemplateProvider {
        async fn current_job(&self) -> csd_pool_node::Result<PoolJob> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let mut job = csd_pool_node::easy_static_job(match call {
                0 => "job-initial",
                1 => "job-stale",
                _ => "job-fresh",
            });
            if call >= 2 {
                job.template.prev = [1; 32];
                job.notify.prev_hash_be_hex = "01".repeat(32);
            }
            Ok(job)
        }

        async fn current_tip(&self) -> csd_pool_node::Result<Option<[u8; 32]>> {
            Ok(Some([1; 32]))
        }
    }

    struct SentinelAwareTemplateProvider {
        state: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl TemplateProvider for SentinelAwareTemplateProvider {
        async fn current_job(&self) -> csd_pool_node::Result<PoolJob> {
            let caught_up = self.state.load(Ordering::SeqCst) >= 2;
            let mut job = csd_pool_node::easy_static_job(if caught_up {
                "sentinel-new"
            } else {
                "sentinel-old"
            });
            if caught_up {
                job.template.prev = [1; 32];
                job.notify.prev_hash_be_hex = "01".repeat(32);
            }
            Ok(job)
        }

        async fn current_tip(&self) -> csd_pool_node::Result<Option<[u8; 32]>> {
            Ok(Some(if self.state.load(Ordering::SeqCst) >= 2 {
                [1; 32]
            } else {
                [0; 32]
            }))
        }
    }

    struct ScriptedTipSentinel {
        state: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl JobTipSentinel for ScriptedTipSentinel {
        async fn observe(&self) -> std::result::Result<TipSentinelObservation, String> {
            let state = self.state.load(Ordering::SeqCst);
            let primary_new = state >= 2;
            let secondary_new = state >= 1;
            Ok(TipSentinelObservation {
                primary: ObservedNodeTip {
                    height: if primary_new { 11 } else { 10 },
                    tip_hex: if primary_new {
                        "01".repeat(32)
                    } else {
                        "00".repeat(32)
                    },
                    chainwork_hex: if primary_new { "11" } else { "10" }.to_owned(),
                },
                secondary: ObservedNodeTip {
                    height: if secondary_new { 11 } else { 10 },
                    tip_hex: if secondary_new {
                        "01".repeat(32)
                    } else {
                        "00".repeat(32)
                    },
                    chainwork_hex: if secondary_new { "11" } else { "10" }.to_owned(),
                },
                observed_unix_ms: unix_ms(),
            })
        }
    }

    struct FailingBlockSubmitter;
    struct ObservableBlockSubmitter;

    #[derive(Clone)]
    enum MockSubmitBehavior {
        Response(bool),
        LocalCanonicalRelayFailed(&'static str),
        Error(&'static str),
        DelayedResponse(Duration, bool),
    }

    struct MockCandidateNodeEndpoint {
        name: &'static str,
        health: NodeHealth,
        health_error: Option<&'static str>,
        behavior: MockSubmitBehavior,
        submit_calls: AtomicUsize,
        cached_submit_calls: AtomicUsize,
        full_submit_calls: AtomicUsize,
    }

    impl MockCandidateNodeEndpoint {
        fn healthy(name: &'static str, behavior: MockSubmitBehavior) -> Self {
            Self {
                name,
                health: NodeHealth {
                    height: Some(100),
                    tip: Some(format!("0x{}", "00".repeat(32))),
                    chainwork: Some("work-100".to_owned()),
                    peers: Some(8),
                    extra: serde_json::Value::Null,
                },
                health_error: None,
                behavior,
                submit_calls: AtomicUsize::new(0),
                cached_submit_calls: AtomicUsize::new(0),
                full_submit_calls: AtomicUsize::new(0),
            }
        }

        fn with_tip(mut self, tip: &str) -> Self {
            self.health.tip = Some(tip.to_owned());
            self
        }

        fn with_peers(mut self, peers: u64) -> Self {
            self.health.peers = Some(peers);
            self
        }
    }

    #[async_trait::async_trait]
    impl CandidateNodeEndpoint for MockCandidateNodeEndpoint {
        fn name(&self) -> &str {
            self.name
        }

        async fn health(&self) -> std::result::Result<NodeHealth, String> {
            self.health_error
                .map(|error| Err(error.to_owned()))
                .unwrap_or_else(|| Ok(self.health.clone()))
        }

        async fn submit_cached_candidate(
            &self,
            candidate: &BlockCandidateSubmitRequest,
        ) -> std::result::Result<SubmitBlockResponse, String> {
            self.cached_submit_calls.fetch_add(1, Ordering::SeqCst);
            self.submit_response(&candidate.hash_hex).await
        }

        async fn submit_full_candidate(
            &self,
            candidate: &StatelessBlockCandidateSubmitRequest<'_>,
        ) -> std::result::Result<SubmitBlockResponse, String> {
            self.full_submit_calls.fetch_add(1, Ordering::SeqCst);
            self.submit_response(&candidate.candidate.hash_hex).await
        }
    }

    impl MockCandidateNodeEndpoint {
        async fn submit_response(
            &self,
            candidate_hash: &str,
        ) -> std::result::Result<SubmitBlockResponse, String> {
            self.submit_calls.fetch_add(1, Ordering::SeqCst);
            let response = |ok| SubmitBlockResponse {
                ok,
                hash: Some(candidate_hash.to_owned()),
                extra: serde_json::json!({
                    "status": if ok { "accepted" } else { "rejected" },
                }),
            };
            match self.behavior {
                MockSubmitBehavior::Response(ok) => Ok(response(ok)),
                MockSubmitBehavior::LocalCanonicalRelayFailed(status) => Ok(SubmitBlockResponse {
                    ok: false,
                    hash: Some(candidate_hash.to_owned()),
                    extra: serde_json::json!({
                        "status": "accepted_local_relay_failed",
                        "error": status,
                        "node_observability": {
                            "local_canonical": true,
                            "relay_ack": {
                                "ok": false,
                                "status": status,
                            }
                        }
                    }),
                }),
                MockSubmitBehavior::Error(error) => Err(error.to_owned()),
                MockSubmitBehavior::DelayedResponse(delay, ok) => {
                    tokio::time::sleep(delay).await;
                    Ok(response(ok))
                }
            }
        }
    }

    fn mock_candidate() -> BlockCandidateSubmitRequest {
        BlockCandidateSubmitRequest {
            job_id: "job-1".to_owned(),
            worker_name: "worker-1".to_owned(),
            header_hex: "00".repeat(84),
            hash_hex: "11".repeat(32),
            coinbase_txid_hex: "22".repeat(32),
            coinbase_hex: "33".repeat(16),
            merkle_root_hex: "44".repeat(32),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: "66554433".to_owned(),
            nonce_hex: "00000001".to_owned(),
        }
    }

    fn candidate_material() -> CandidateTemplateMaterial {
        CandidateTemplateMaterial {
            block_template_hex: "00".repeat(128),
            block_template_sha256_hex: "55".repeat(32),
            block_template_bytes: 128,
        }
    }

    fn submission(candidate: BlockCandidateSubmitRequest) -> BlockCandidateSubmission {
        BlockCandidateSubmission {
            candidate,
            candidate_material: Some(Arc::new(candidate_material())),
        }
    }

    fn mock_submission() -> BlockCandidateSubmission {
        submission(mock_candidate())
    }

    const H58427_PARENT_DISPLAY: &str =
        "00000000000013c6d2db4280dd807f75740a3300e84c53d4114b944b9012d56b";

    fn h58427_candidate() -> BlockCandidateSubmitRequest {
        BlockCandidateSubmitRequest {
            job_id: "91005efd84e1d198c0006cbab54c83b95d0592c870d73bdc5056daec97572dd7"
                .to_owned(),
            worker_name: "V100-JN-20260719-1-22-CS".to_owned(),
            header_hex: "0100000000000000000013c6d2db4280dd807f75740a3300e84c53d4114b944b9012d56b5cde0ff8e025e5027b9ecfcf22dadd8c287c23504d689ec8d27fdce929690bbe70a35e6a00000000eb8d1d1aa71be588".to_owned(),
            hash_hex: "00000000000010bd7b85f08688ee8322818820686b6726426ce861dd851469fd"
                .to_owned(),
            coinbase_txid_hex:
                "5cde0ff8e025e5027b9ecfcf22dadd8c287c23504d689ec8d27fdce929690bbe"
                    .to_owned(),
            coinbase_hex: "00".repeat(64),
            merkle_root_hex:
                "5cde0ff8e025e5027b9ecfcf22dadd8c287c23504d689ec8d27fdce929690bbe"
                    .to_owned(),
            extranonce2_hex: "a29747fe".to_owned(),
            ntime_hex: "6a5ea370".to_owned(),
            nonce_hex: "88e51ba7".to_owned(),
        }
    }

    fn h58427_submission() -> BlockCandidateSubmission {
        submission(h58427_candidate())
    }

    #[derive(Clone)]
    struct ReplayNodeState {
        submit_calls: Arc<AtomicUsize>,
    }

    async fn replay_health() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "height": 100,
            "tip": format!("0x{}", "00".repeat(32)),
            "chainwork": "work-100",
            "peers": 8,
        }))
    }

    async fn replay_submit(
        State(state): State<ReplayNodeState>,
        Json(candidate): Json<BlockCandidateSubmitRequest>,
    ) -> Json<SubmitBlockResponse> {
        state.submit_calls.fetch_add(1, Ordering::SeqCst);
        Json(SubmitBlockResponse {
            ok: true,
            hash: Some(candidate.hash_hex),
            extra: serde_json::json!({"status": "accepted"}),
        })
    }

    async fn replay_submit_full(
        State(state): State<ReplayNodeState>,
        Json(candidate): Json<serde_json::Value>,
    ) -> Json<SubmitBlockResponse> {
        state.submit_calls.fetch_add(1, Ordering::SeqCst);
        Json(SubmitBlockResponse {
            ok: true,
            hash: candidate["hash_hex"].as_str().map(str::to_owned),
            extra: serde_json::json!({"status": "accepted"}),
        })
    }

    async fn spawn_replay_node() -> (String, Arc<AtomicUsize>) {
        let submit_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/health", axum::routing::get(replay_health))
            .route("/api/rpc/block/submit", post(replay_submit))
            .route("/api/rpc/block/submit-full", post(replay_submit_full))
            .with_state(ReplayNodeState {
                submit_calls: submit_calls.clone(),
            });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), submit_calls)
    }

    async fn parallel_submitter(
        primary: Arc<MockCandidateNodeEndpoint>,
        secondary: Arc<MockCandidateNodeEndpoint>,
        timeout_duration: Duration,
    ) -> ParallelNodeBlockSubmitter {
        let submitter = ParallelNodeBlockSubmitter::new(
            primary,
            secondary,
            ParallelSubmitConfig {
                primary_timeout: timeout_duration,
                secondary_timeout: timeout_duration,
                health_refresh: Duration::from_secs(10),
                health_max_age: Duration::from_secs(30),
                secondary_max_in_flight: 1,
                circuit_failure_threshold: 3,
                circuit_cooldown: Duration::from_secs(30),
            },
            SecondaryCandidateControl::OneShot {
                budget: 1,
                latch: test_candidate_latch(),
            },
        );
        submitter.refresh_health_once().await;
        submitter
    }

    fn parallel_submitter_without_health_watch(
        primary: Arc<MockCandidateNodeEndpoint>,
        secondary: Arc<MockCandidateNodeEndpoint>,
        latch: PersistentCandidateLatch,
    ) -> ParallelNodeBlockSubmitter {
        ParallelNodeBlockSubmitter {
            primary,
            secondary,
            primary_timeout: Duration::from_secs(1),
            secondary_timeout: Duration::from_secs(1),
            health_refresh: Duration::from_secs(10),
            health_max_age: Duration::from_secs(30),
            health_snapshot: Arc::new(RwLock::new(None)),
            secondary_control: SecondaryCandidateControl::OneShot { budget: 1, latch },
            secondary_in_flight: Arc::new(Semaphore::new(1)),
            secondary_circuit: SecondaryCircuitBreaker::new(3, Duration::from_secs(30)),
        }
    }

    fn continuous_submitter_without_health_watch(
        primary: Arc<MockCandidateNodeEndpoint>,
        secondary: Arc<MockCandidateNodeEndpoint>,
        journal: PersistentCandidateJournal,
        failure_threshold: u64,
        cooldown: Duration,
    ) -> ParallelNodeBlockSubmitter {
        ParallelNodeBlockSubmitter {
            primary,
            secondary,
            primary_timeout: Duration::from_secs(1),
            secondary_timeout: Duration::from_secs(1),
            health_refresh: Duration::from_secs(10),
            health_max_age: Duration::from_secs(30),
            health_snapshot: Arc::new(RwLock::new(None)),
            secondary_control: SecondaryCandidateControl::ContinuousBounded(journal),
            secondary_in_flight: Arc::new(Semaphore::new(1)),
            secondary_circuit: SecondaryCircuitBreaker::new(failure_threshold, cooldown),
        }
    }

    fn test_candidate_latch() -> PersistentCandidateLatch {
        let path = std::env::temp_dir().join(format!(
            "csd-pool-secondary-candidate-{}.latch",
            uuid::Uuid::new_v4()
        ));
        PersistentCandidateLatch::new(path).unwrap()
    }

    fn test_candidate_journal() -> (PersistentCandidateJournal, PathBuf) {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "csd-pool-secondary-candidates-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        (PersistentCandidateJournal::new(path.clone()).unwrap(), path)
    }

    #[test]
    fn extranonce1_is_serialized_in_verification_byte_order() {
        assert_eq!(extranonce1_for_session(0x01020304), "04030201");
        assert_eq!(extranonce1_for_session(1), "01000000");
    }

    #[async_trait::async_trait]
    impl BlockSubmitter for FailingBlockSubmitter {
        async fn submit_candidate(
            &self,
            _submission: &BlockCandidateSubmission,
        ) -> Result<SubmitBlockResponse> {
            Err(BridgeError::InvalidConfig("node unavailable".to_owned()))
        }
    }

    #[async_trait::async_trait]
    impl BlockSubmitter for ObservableBlockSubmitter {
        async fn submit_candidate(
            &self,
            submission: &BlockCandidateSubmission,
        ) -> Result<SubmitBlockResponse> {
            let candidate = &submission.candidate;
            Ok(SubmitBlockResponse {
                ok: true,
                hash: Some(candidate.hash_hex.clone()),
                extra: serde_json::json!({
                    "status": "accepted",
                    "node_observability": {
                        "accept_elapsed_us": 125,
                        "relay_enqueue_elapsed_us": 140,
                        "relay_queued": true,
                    },
                }),
            })
        }
    }

    #[tokio::test]
    async fn shared_job_watch_refreshes_all_subscribers() {
        let provider = Arc::new(RotatingTemplateProvider {
            calls: AtomicUsize::new(0),
        });
        let jobs = SharedJobWatch::start_with_refresh(provider, None, Duration::from_millis(10))
            .await
            .unwrap();
        let mut first = jobs.subscribe();
        let mut second = jobs.subscribe();
        assert_eq!(first.borrow().template.job_id, "job-initial");
        tokio::time::timeout(Duration::from_secs(1), first.changed())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), second.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.borrow().template.job_id, "job-refreshed");
        assert_eq!(second.borrow().template.job_id, "job-refreshed");
    }

    #[tokio::test]
    async fn shared_job_watch_fetches_template_only_after_tip_change() {
        let provider = Arc::new(TipAwareTemplateProvider {
            calls: AtomicUsize::new(0),
            tip_changed: AtomicBool::new(false),
        });
        let jobs = SharedJobWatch::start_with_policy(
            provider.clone(),
            None,
            SharedPoolState::new(),
            Duration::from_millis(10),
            Some(Duration::from_secs(1)),
            Duration::from_secs(8),
        )
        .await
        .unwrap();
        let mut receiver = jobs.subscribe();

        tokio::time::sleep(Duration::from_millis(35)).await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        provider.tip_changed.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receiver.borrow().template.job_id, "job-refreshed");
        assert!(receiver.borrow().notify.clean_jobs);
        assert!(jobs.retained_jobs().get("job-initial").is_some());
        assert!(jobs.retained_jobs().get_for_submit("job-initial").is_none());
        assert!(jobs.retained_jobs().get("job-refreshed").is_some());
        assert!(
            jobs.retained_jobs()
                .get_for_submit("job-refreshed")
                .is_some()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn clean_tip_churn_keeps_base_jobs_for_classification_but_not_acceptance() {
        let initial = Arc::new(csd_pool_node::easy_static_job("clean-job-0"));
        let retained = RetainedJobs::with_initial(initial.clone());
        let retention = Duration::from_secs(900);

        for index in 1..=12 {
            let mut next = csd_pool_node::easy_static_job(format!("clean-job-{index}"));
            next.template.prev = [index as u8; 32];
            next.notify.prev_hash_be_hex = hex::encode(next.template.prev);
            retained.publish(Arc::new(next), JobReason::TipChange, retention);
        }

        let old = retained.get("clean-job-0").expect("old base job retained");
        assert_eq!(old.template.job_id, initial.template.job_id);
        assert_eq!(old.template.share_target, initial.template.share_target);
        assert!(retained.get_for_submit("clean-job-0").is_none());
        assert!(retained.get_for_submit("clean-job-12").is_some());
        assert_eq!(retained.len(), 13);
    }

    #[test]
    fn invalidated_parent_remains_classifiable_but_cannot_accept_shares() {
        let job = Arc::new(csd_pool_node::easy_static_job("stale-parent-job"));
        let parent = job.template.prev;
        let retained = RetainedJobs::with_initial(job);

        assert!(retained.get_for_submit("stale-parent-job").is_some());
        assert_eq!(retained.invalidate_parent(&parent), 1);
        assert!(retained.get("stale-parent-job").is_some());
        assert!(retained.job_parent_is_invalidated("stale-parent-job"));
        assert!(retained.get_for_submit("stale-parent-job").is_none());
        assert!(!retained.parent_is_retained(&parent));
    }

    #[test]
    fn clean_generation_fence_distinguishes_delivery_from_miner_switch_lag() {
        let initial = Arc::new(csd_pool_node::easy_static_job("generation-1"));
        let retained = RetainedJobs::with_initial(initial.clone());
        assert_eq!(retained.clean_generation("generation-1"), Some(1));

        let mut heartbeat = csd_pool_node::easy_static_job("generation-1-heartbeat");
        heartbeat.notify.clean_jobs = false;
        retained.publish(
            Arc::new(heartbeat),
            JobReason::Heartbeat,
            Duration::from_secs(60),
        );
        assert_eq!(retained.clean_generation("generation-1-heartbeat"), Some(1));

        let mut replacement = csd_pool_node::easy_static_job("generation-2");
        replacement.template.prev = [2; 32];
        replacement.notify.prev_hash_be_hex = "02".repeat(32);
        replacement.notify.clean_jobs = true;
        retained.publish(
            Arc::new(replacement),
            JobReason::TipChange,
            Duration::from_secs(60),
        );
        assert_eq!(retained.clean_generation("generation-2"), Some(2));
        assert_eq!(
            retained.stale_generation_class("generation-1", 1),
            StaleGenerationClass::NotifyDeliveryLag
        );
        assert_eq!(
            retained.stale_generation_class("generation-1", 2),
            StaleGenerationClass::MinerSwitchLag
        );

        let current = Arc::new(csd_pool_node::easy_static_job("sentinel-invalidated"));
        let parent = current.template.prev;
        let sentinel_retained = RetainedJobs::with_initial(current);
        sentinel_retained.invalidate_parent(&parent);
        assert_eq!(
            sentinel_retained.stale_generation_class("sentinel-invalidated", 1),
            StaleGenerationClass::Unclassified
        );
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_client_write_times_out_instead_of_blocking_clean_job_delivery() {
        let (mut writer, _reader) = tokio::io::duplex(1);
        let task = tokio::spawn(async move {
            write_client_frame_with_deadline(
                &mut writer,
                "a frame larger than the one-byte transport",
                Duration::from_millis(20),
            )
            .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(21)).await;
        let result = task.await.unwrap();
        assert!(matches!(result, Err(BridgeError::ClientWriteTimeout)));
    }

    #[tokio::test]
    async fn clean_job_waiter_skips_heartbeat_and_returns_replacement() {
        let initial = Arc::new(csd_pool_node::easy_static_job("waiter-initial"));
        let (sender, mut receiver) = watch::channel(initial);
        let waiter = tokio::spawn(async move { next_clean_job(&mut receiver).await });

        let mut heartbeat = csd_pool_node::easy_static_job("waiter-heartbeat");
        heartbeat.notify.clean_jobs = false;
        sender.send_replace(Arc::new(heartbeat));
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        let mut clean = csd_pool_node::easy_static_job("waiter-clean");
        clean.notify.clean_jobs = true;
        sender.send_replace(Arc::new(clean));
        let observed = waiter.await.unwrap().expect("clean replacement");
        assert_eq!(observed.template.job_id, "waiter-clean");
    }

    #[tokio::test(start_paused = true)]
    async fn same_tip_heartbeat_refreshes_without_cleaning_and_retains_old_jobs() {
        let provider = Arc::new(SameTipHeartbeatProvider {
            calls: AtomicUsize::new(0),
        });
        let state = SharedPoolState::new();
        let jobs = SharedJobWatch::start_with_policy(
            provider.clone(),
            None,
            state.clone(),
            Duration::from_secs(2),
            Some(Duration::from_secs(120)),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
        let mut receiver = jobs.subscribe();
        let initial_id = receiver.borrow().template.job_id.clone();

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(119)).await;
        tokio::task::yield_now().await;
        assert_eq!(receiver.borrow().template.job_id, initial_id);

        tokio::time::advance(Duration::from_secs(3)).await;
        tokio::task::yield_now().await;
        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        let heartbeat_job = receiver.borrow().clone();
        assert_ne!(heartbeat_job.template.job_id, initial_id);
        assert_eq!(heartbeat_job.template.prev, [0; 32]);
        assert!(!heartbeat_job.notify.clean_jobs);
        assert!(jobs.retained_jobs().get(&initial_id).is_some());
        assert!(jobs.retained_jobs().get_for_submit(&initial_id).is_some());
        assert!(
            jobs.retained_jobs()
                .get_for_submit(&heartbeat_job.template.job_id)
                .is_some()
        );
        assert_eq!(jobs.retained_jobs().len(), 2);

        let totals = state.snapshot().totals;
        assert_eq!(totals.job_tip_change_count, 1);
        assert_eq!(totals.job_heartbeat_count, 1);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn long_same_tip_gap_keeps_legacy_and_v2_sessions_live_and_accepts_old_job() {
        type TestReader = BufReader<tokio::net::tcp::OwnedReadHalf>;
        type TestWriter = tokio::net::tcp::OwnedWriteHalf;

        async fn read_frame(reader: &mut TestReader) -> serde_json::Value {
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("frame read");
            serde_json::from_str(line.trim()).expect("valid json frame")
        }

        async fn connect_client(
            listener: &TcpListener,
            jobs: &SharedJobWatch,
            pool_state: &SharedPoolState,
            repository: Arc<csd_pool_db::InMemoryRepository>,
            user_agent: &str,
            worker_name: &str,
        ) -> (
            TestReader,
            TestWriter,
            tokio::task::JoinHandle<Result<()>>,
            [u8; 4],
        ) {
            let client = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (server_stream, peer) = listener.accept().await.unwrap();
            let abuse = Arc::new(AbuseManager::default());
            let permit = abuse.try_open(peer.ip()).unwrap();
            let repository: Arc<dyn MiningRepository> = repository;
            let session = tokio::spawn(handle_client(
                server_stream,
                peer,
                pool_state.clone(),
                jobs.subscribe(),
                jobs.retained_jobs(),
                Some(repository),
                None,
                abuse,
                VardiffConfig {
                    initial_difficulty: 1.0,
                    min_difficulty: 1.0,
                    max_difficulty: 1.0,
                    ..VardiffConfig::default()
                },
                permit,
            ));

            let (read_half, mut write_half) = client.into_split();
            let mut reader = BufReader::new(read_half);
            write_half
                .write_all(
                    format!(
                        "{{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"{user_agent}\"]}}\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let subscribe = read_frame(&mut reader).await;
            let extranonce1_hex = subscribe["result"][1].as_str().unwrap();
            let extranonce1 = parse_le_u32_hex_bytes("extranonce1", extranonce1_hex).unwrap();

            write_half
                .write_all(
                    format!(
                        "{{\"id\":2,\"method\":\"mining.authorize\",\"params\":[\"0123456789abcdef0123456789abcdef01234567.{worker_name}\",\"x\"]}}\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let authorize = read_frame(&mut reader).await;
            assert_eq!(authorize["result"], true);
            let difficulty = read_frame(&mut reader).await;
            assert_eq!(difficulty["method"], "mining.set_difficulty");
            let notify = read_frame(&mut reader).await;
            assert_eq!(notify["method"], "mining.notify");

            (reader, write_half, session, extranonce1)
        }

        async fn read_heartbeat(reader: &mut TestReader, previous_job_id: &str) -> String {
            let frame = read_frame(reader).await;
            assert_eq!(frame["method"], "mining.notify");
            assert_eq!(frame["params"][8], false);
            let job_id = frame["params"][0].as_str().unwrap().to_owned();
            assert_ne!(job_id, previous_job_id);
            job_id
        }

        async fn submit_old_job(
            reader: &mut TestReader,
            writer: &mut TestWriter,
            request_id: u64,
            worker_name: &str,
            job: &PoolJob,
            extranonce1: [u8; 4],
        ) {
            let submit = SubmitParams {
                worker_name: format!("0123456789abcdef0123456789abcdef01234567.{worker_name}"),
                job_id: job.template.job_id.clone(),
                extranonce2_hex: "01020304".to_owned(),
                ntime_hex: job.notify.ntime_hex.clone(),
                nonce_hex: "00000000".to_owned(),
            };
            verify_submit(&job.template, extranonce1, &submit, 1.0).unwrap();
            writer
                .write_all(
                    format!(
                        "{{\"id\":{request_id},\"method\":\"mining.submit\",\"params\":[\"{}\",\"{}\",\"{}\",\"{}\",\"{}\"]}}\n",
                        submit.worker_name,
                        submit.job_id,
                        submit.extranonce2_hex,
                        submit.ntime_hex,
                        submit.nonce_hex
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            loop {
                let response = read_frame(reader).await;
                if response["id"].as_u64() == Some(request_id) {
                    assert_eq!(response["result"], true);
                    break;
                }
            }
        }

        let provider = Arc::new(SameTipHeartbeatProvider {
            calls: AtomicUsize::new(0),
        });
        let repository = Arc::new(csd_pool_db::InMemoryRepository::new());
        let pool_state = SharedPoolState::new();
        let jobs = SharedJobWatch::start_with_policy(
            provider,
            Some(repository.clone()),
            pool_state.clone(),
            Duration::from_secs(2),
            Some(Duration::from_secs(120)),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
        let initial_job = jobs.sender.borrow().clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

        let (mut legacy_reader, mut legacy_writer, legacy_session, legacy_xn1) = connect_client(
            &listener,
            &jobs,
            &pool_state,
            repository.clone(),
            "csd-gpu-miner/0.2.3-r72",
            "legacy-r72",
        )
        .await;
        let (mut v2_reader, mut v2_writer, v2_session, v2_xn1) = connect_client(
            &listener,
            &jobs,
            &pool_state,
            repository.clone(),
            "csd-gpu-miner/0.2.3-liveness-v2",
            "liveness-v2",
        )
        .await;

        tokio::task::yield_now().await;
        let mut legacy_job_id = initial_job.template.job_id.clone();
        let mut v2_job_id = initial_job.template.job_id.clone();
        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(120)).await;
            for _ in 0..100 {
                tokio::task::yield_now().await;
                if jobs.sender.borrow().template.job_id != legacy_job_id {
                    break;
                }
            }
            assert_ne!(jobs.sender.borrow().template.job_id, legacy_job_id);
            legacy_job_id = read_heartbeat(&mut legacy_reader, &legacy_job_id).await;
            v2_job_id = read_heartbeat(&mut v2_reader, &v2_job_id).await;
        }
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;

        assert!(
            jobs.retained_jobs()
                .get(&initial_job.template.job_id)
                .is_some()
        );
        submit_old_job(
            &mut legacy_reader,
            &mut legacy_writer,
            91,
            "legacy-r72",
            &initial_job,
            legacy_xn1,
        )
        .await;
        submit_old_job(
            &mut v2_reader,
            &mut v2_writer,
            92,
            "liveness-v2",
            &initial_job,
            v2_xn1,
        )
        .await;

        assert_eq!(pool_state.snapshot().totals.stratum_connections, 2);
        let sessions = repository.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().all(|(_, active)| *active));
        let shares = repository.list_shares().unwrap();
        assert_eq!(shares.len(), 2);
        assert!(
            shares
                .iter()
                .all(|share| share.job_id == initial_job.template.job_id)
        );

        legacy_session.abort();
        v2_session.abort();
    }

    #[tokio::test]
    async fn shared_job_watch_discards_template_that_lags_live_tip() {
        let provider = Arc::new(LaggingTemplateProvider {
            calls: AtomicUsize::new(0),
        });
        let jobs =
            SharedJobWatch::start_with_refresh(provider.clone(), None, Duration::from_millis(10))
                .await
                .unwrap();
        let mut receiver = jobs.subscribe();

        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receiver.borrow().template.job_id, "job-fresh");
        assert!(provider.calls.load(Ordering::SeqCst) >= 3);
    }

    #[test]
    fn secondary_tip_sentinel_is_disabled_by_default() {
        assert!(!tip_sentinel_enabled(None));
        assert!(!tip_sentinel_enabled(Some("false")));
        assert!(tip_sentinel_enabled(Some("true")));
    }

    #[test]
    fn secondary_tip_sentinel_accepts_an_independent_observer_url() {
        assert_eq!(
            select_tip_sentinel_node_url(Some("http://127.0.0.1:8792"), || {
                panic!("independent sentinel URL must not resolve the submit fallback")
            })
            .unwrap(),
            "http://127.0.0.1:8792"
        );
    }

    #[test]
    fn secondary_tip_sentinel_defaults_to_the_secondary_submit_node() {
        for explicit in [None, Some("")] {
            assert_eq!(
                select_tip_sentinel_node_url(explicit, || {
                    Ok("http://127.0.0.1:8791".to_owned())
                })
                .unwrap(),
                "http://127.0.0.1:8791"
            );
        }
    }

    #[test]
    fn secondary_tip_sentinel_uses_an_independent_token_with_compatible_fallback() {
        assert_eq!(
            select_tip_sentinel_node_token(true, Some("node-c-token"), Some("node-a-token"))
                .unwrap(),
            Some("node-c-token".to_owned()),
        );
        assert_eq!(
            select_tip_sentinel_node_token(false, None, Some("node-a-token")).unwrap(),
            Some("node-a-token".to_owned()),
        );
        assert_eq!(
            select_tip_sentinel_node_token(false, Some(""), Some("")).unwrap(),
            None
        );
    }

    #[test]
    fn secondary_tip_sentinel_requires_a_dedicated_token_for_an_independent_url() {
        let error = select_tip_sentinel_node_token(true, None, Some("node-a-token")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("requires CSD_POOL_TIP_SENTINEL_NODE_TOKEN")
        );
    }

    #[test]
    fn secondary_tip_sentinel_ca_is_scoped_to_independent_https() {
        let path = std::env::temp_dir().join(format!(
            "csd-pool-sentinel-ca-{}-{}.pem",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"test certificate bytes").unwrap();
        let path_text = path.to_str().unwrap();

        assert_eq!(
            load_tip_sentinel_ca_pem(true, "https://127.0.0.1:18900", Some(path_text))
                .unwrap()
                .unwrap(),
            b"test certificate bytes"
        );
        assert!(
            load_tip_sentinel_ca_pem(false, "https://127.0.0.1:18900", Some(path_text)).is_err()
        );
        assert!(load_tip_sentinel_ca_pem(true, "http://127.0.0.1:18900", Some(path_text)).is_err());
        assert!(load_tip_sentinel_ca_pem(true, "https://public.example", None).is_err());
        assert!(
            load_tip_sentinel_ca_pem(false, "http://127.0.0.1:8791", None)
                .unwrap()
                .is_none()
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn secondary_tip_sentinel_ca_rejects_bad_paths() {
        assert!(
            load_tip_sentinel_ca_pem(true, "https://127.0.0.1:18900", Some("relative.pem"))
                .is_err()
        );
        let missing = std::env::temp_dir().join(format!(
            "missing-csd-pool-sentinel-ca-{}",
            uuid::Uuid::new_v4()
        ));
        assert!(
            load_tip_sentinel_ca_pem(true, "https://127.0.0.1:18900", missing.to_str()).is_err()
        );
    }

    #[test]
    fn secondary_tip_sentinel_does_not_invalidate_when_b_is_behind() {
        let current = [1; 32];
        let behind = TipSentinelObservation {
            primary: ObservedNodeTip {
                height: 11,
                tip_hex: "01".repeat(32),
                chainwork_hex: "11".to_owned(),
            },
            secondary: ObservedNodeTip {
                height: 10,
                tip_hex: "00".repeat(32),
                chainwork_hex: "10".to_owned(),
            },
            observed_unix_ms: 1,
        };
        assert!(!tip_sentinel_invalidates_parent(&current, &behind));
    }

    #[test]
    fn secondary_tip_sentinel_invalidates_equal_height_competing_tip() {
        let current = [1; 32];
        let behind = TipSentinelObservation {
            primary: ObservedNodeTip {
                height: 11,
                tip_hex: "01".repeat(32),
                chainwork_hex: "11".to_owned(),
            },
            secondary: ObservedNodeTip {
                height: 10,
                tip_hex: "00".repeat(32),
                chainwork_hex: "10".to_owned(),
            },
            observed_unix_ms: 1,
        };
        let competing = TipSentinelObservation {
            secondary: ObservedNodeTip {
                height: 11,
                tip_hex: "02".repeat(32),
                chainwork_hex: "11".to_owned(),
            },
            ..behind
        };
        assert!(tip_sentinel_invalidates_parent(&current, &competing));
    }

    #[tokio::test]
    async fn secondary_tip_sentinel_fences_old_jobs_until_a_template_catches_up() {
        let state = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(SentinelAwareTemplateProvider {
            state: state.clone(),
        });
        let sentinel = Arc::new(ScriptedTipSentinel {
            state: state.clone(),
        });
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(0x5566_7788);
        let jobs = SharedJobWatch::start_with_policy_and_sentinel_and_telemetry(
            provider,
            Some(sentinel),
            None,
            SharedPoolState::new(),
            Duration::from_millis(50),
            None,
            Duration::from_secs(60),
            Duration::from_millis(5),
            telemetry.clone(),
        )
        .await
        .unwrap();
        let mut receiver = jobs.subscribe();
        let old_job_id = receiver.borrow().template.job_id.clone();
        assert!(jobs.retained_jobs().get(&old_job_id).is_some());

        state.store(1, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !jobs.retained_jobs().job_parent_is_invalidated(&old_job_id) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(jobs.retained_jobs().get(&old_job_id).is_some());
        assert!(jobs.retained_jobs().get_for_submit(&old_job_id).is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(25), receiver.changed())
                .await
                .is_err(),
            "B must never provide or cause publication of a mining template"
        );

        state.store(2, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        let current = receiver.borrow().clone();
        assert_eq!(current.template.job_id, "sentinel-new");
        assert_eq!(current.template.prev, [1; 32]);
        assert!(current.notify.clean_jobs);
        assert!(jobs.retained_jobs().get(&old_job_id).is_some());
        assert!(jobs.retained_jobs().get_for_submit(&old_job_id).is_none());
        let summary = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(summary) = telemetry.take_completed().pop() {
                    break summary;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(summary.source, "independent_sentinel_ahead");
        assert_eq!(summary.reason, "zero_sessions");
        assert!(summary.observed_to_a_ready_ms.is_some());
        assert!(summary.a_ready_to_published_ms.is_some());
    }

    #[tokio::test]
    async fn authorized_tcp_client_receives_proactive_job_refresh() {
        let provider = Arc::new(RotatingTemplateProvider {
            calls: AtomicUsize::new(0),
        });
        let jobs = SharedJobWatch::start_with_refresh(provider, None, Duration::from_millis(200))
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).await.unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();
        let abuse = Arc::new(AbuseManager::default());
        let permit = abuse.try_open(peer.ip()).unwrap();
        let session = tokio::spawn(handle_client(
            server_stream,
            peer,
            SharedPoolState::new(),
            jobs.subscribe(),
            jobs.retained_jobs(),
            None,
            None,
            abuse,
            VardiffConfig::default(),
            permit,
        ));

        let (read_half, mut write_half) = client.into_split();
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(
                b"{\"id\":1,\"method\":\"mining.authorize\",\"params\":[\"0123456789abcdef0123456789abcdef01234567\",\"x\"]}\n",
            )
            .await
            .unwrap();

        let mut observed_jobs = Vec::new();
        for _ in 0..4 {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            if frame["method"] == "mining.notify" {
                observed_jobs.push(frame["params"][0].as_str().unwrap().to_owned());
                if observed_jobs.len() == 2 {
                    break;
                }
            }
        }

        assert_eq!(observed_jobs, ["job-initial", "job-refreshed"]);
        session.abort();
    }

    #[tokio::test]
    async fn queued_clean_job_precedes_a_ready_client_request() {
        let jobs = SharedJobWatch::start_with_policy(
            Arc::new(StaticTemplateProvider::easy_job("priority-initial")),
            None,
            SharedPoolState::new(),
            Duration::from_secs(60),
            None,
            Duration::from_secs(600),
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();
        let abuse = Arc::new(AbuseManager::default());
        let permit = abuse.try_open(peer.ip()).unwrap();
        let session = tokio::spawn(handle_client(
            server_stream,
            peer,
            SharedPoolState::new(),
            jobs.subscribe(),
            jobs.retained_jobs(),
            None,
            None,
            abuse,
            VardiffConfig::default(),
            permit,
        ));

        let (read_half, mut write_half) = client.into_split();
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(
                b"{\"id\":1,\"method\":\"mining.authorize\",\"params\":[\"0123456789abcdef0123456789abcdef01234567.priority\",\"x\"]}\n",
            )
            .await
            .unwrap();
        for _ in 0..3 {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
        }

        write_half
            .write_all(b"{\"id\":9,\"method\":\"unknown.ready.request\",\"params\":[]}\n")
            .await
            .unwrap();
        let mut replacement = csd_pool_node::easy_static_job("priority-clean");
        replacement.template.prev = [3; 32];
        replacement.notify.prev_hash_be_hex = "03".repeat(32);
        replacement.notify.clean_jobs = true;
        let replacement = Arc::new(replacement);
        jobs.retained_jobs().publish(
            replacement.clone(),
            JobReason::TipChange,
            Duration::from_secs(600),
        );
        jobs.sender.send_replace(replacement);

        let mut first_line = String::new();
        reader.read_line(&mut first_line).await.unwrap();
        let first: serde_json::Value = serde_json::from_str(first_line.trim()).unwrap();
        assert_eq!(first["method"], "mining.notify");
        assert_eq!(first["params"][0], "priority-clean");
        assert_eq!(first["params"][8], true);

        let mut second_line = String::new();
        reader.read_line(&mut second_line).await.unwrap();
        let second: serde_json::Value = serde_json::from_str(second_line.trim()).unwrap();
        assert_eq!(second["id"], 9);
        session.abort();
    }

    #[tokio::test]
    async fn replacement_latency_tracks_primary_tip_through_clean_session_fanout() {
        let provider = Arc::new(TipAwareTemplateProvider {
            calls: AtomicUsize::new(0),
            tip_changed: AtomicBool::new(false),
        });
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(0x1122_3344);
        let jobs = SharedJobWatch::start_with_policy_and_sentinel_and_telemetry(
            provider.clone(),
            None,
            None,
            SharedPoolState::new(),
            Duration::from_millis(20),
            None,
            Duration::from_secs(60),
            Duration::from_millis(20),
            telemetry.clone(),
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();
        let abuse = Arc::new(AbuseManager::default());
        let permit = abuse.try_open(peer.ip()).unwrap();
        let session = tokio::spawn(handle_client(
            server_stream,
            peer,
            SharedPoolState::new(),
            jobs.subscribe(),
            jobs.retained_jobs(),
            None,
            None,
            abuse,
            VardiffConfig::default(),
            permit,
        ));

        let (read_half, mut write_half) = client.into_split();
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(
                b"{\"id\":1,\"method\":\"mining.authorize\",\"params\":[\"0123456789abcdef0123456789abcdef01234567\",\"x\"]}\n",
            )
            .await
            .unwrap();
        for _ in 0..3 {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(telemetry.stats().active_session_total, 1);

        provider.tip_changed.store(true, Ordering::SeqCst);
        let replacement_job = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
                if frame["method"] == "mining.notify" {
                    break frame["params"][0].as_str().unwrap().to_owned();
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(replacement_job, "job-refreshed");

        let summary = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(summary) = telemetry.take_completed().pop() {
                    break summary;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(summary.source, "primary_already_ready");
        assert_eq!(summary.status, "complete");
        assert_eq!(summary.reason, "all_notified");
        assert_eq!(summary.target_sessions, 1);
        assert_eq!(summary.notify_success, 1);
        assert!(summary.observed_to_a_ready_ms.is_some());
        assert!(summary.a_ready_to_published_ms.is_some());
        assert!(summary.published_to_fanout_ms.is_some());
        session.abort();
    }

    #[tokio::test]
    async fn contended_replacement_latency_never_delays_tcp_clean_job() {
        let provider = Arc::new(TipAwareTemplateProvider {
            calls: AtomicUsize::new(0),
            tip_changed: AtomicBool::new(false),
        });
        let telemetry = ReplacementLatencyTelemetry::enabled_for_test(0x2233_4455);
        let jobs = SharedJobWatch::start_with_policy_and_sentinel_and_telemetry(
            provider.clone(),
            None,
            None,
            SharedPoolState::new(),
            Duration::from_millis(20),
            None,
            Duration::from_secs(60),
            Duration::from_millis(20),
            telemetry.clone(),
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();
        let abuse = Arc::new(AbuseManager::default());
        let permit = abuse.try_open(peer.ip()).unwrap();
        let session = tokio::spawn(handle_client(
            server_stream,
            peer,
            SharedPoolState::new(),
            jobs.subscribe(),
            jobs.retained_jobs(),
            None,
            None,
            abuse,
            VardiffConfig::default(),
            permit,
        ));

        let (read_half, mut write_half) = client.into_split();
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(
                b"{\"id\":1,\"method\":\"mining.authorize\",\"params\":[\"0123456789abcdef0123456789abcdef01234567\",\"x\"]}\n",
            )
            .await
            .unwrap();
        for _ in 0..3 {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
        }

        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let blocking_telemetry = telemetry.clone();
        let blocker = std::thread::spawn(move || {
            blocking_telemetry.hold_state_until_released_for_test(ready_tx, release_rx);
        });
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        provider.tip_changed.store(true, Ordering::SeqCst);
        let frame = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
                if frame["method"] == "mining.notify" {
                    break frame;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(frame["params"][0], "job-refreshed");
        assert_eq!(frame["params"].as_array().unwrap().last().unwrap(), true);

        release_tx.send(()).unwrap();
        blocker.join().unwrap();
        assert!(telemetry.stats().lock_contention_total > 0);
        assert!(telemetry.take_completed().is_empty());
        session.abort();
    }

    #[tokio::test]
    async fn first_difficulty_suggestion_sends_non_clean_notify_and_repeats_are_ignored() {
        let jobs = SharedJobWatch::start_with_policy(
            Arc::new(StaticTemplateProvider::easy_job("suggest-job")),
            Some(Arc::new(csd_pool_db::InMemoryRepository::new())),
            SharedPoolState::new(),
            Duration::from_secs(60),
            None,
            Duration::from_secs(600),
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();
        let abuse = Arc::new(AbuseManager::default());
        let permit = abuse.try_open(peer.ip()).unwrap();
        let session = tokio::spawn(handle_client(
            server_stream,
            peer,
            SharedPoolState::new(),
            jobs.subscribe(),
            jobs.retained_jobs(),
            None,
            None,
            abuse,
            VardiffConfig {
                initial_difficulty: 8.0,
                min_difficulty: 8.0,
                max_difficulty: 64.0,
                ..VardiffConfig::default()
            },
            permit,
        ));

        let (read_half, mut write_half) = client.into_split();
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(
                b"{\"id\":1,\"method\":\"mining.authorize\",\"params\":[\"0123456789abcdef0123456789abcdef01234567.suggest-test\",\"x\"]}\n",
            )
            .await
            .unwrap();
        for _ in 0..3 {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
        }

        write_half
            .write_all(b"{\"id\":2,\"method\":\"mining.suggest_difficulty\",\"params\":[16]}\n")
            .await
            .unwrap();
        let mut frames = Vec::new();
        for _ in 0..3 {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            frames.push(serde_json::from_str::<serde_json::Value>(line.trim()).unwrap());
        }
        assert_eq!(frames[0]["result"], true);
        assert_eq!(frames[1]["method"], "mining.set_difficulty");
        assert_eq!(frames[1]["params"][0], 16.0);
        assert_eq!(frames[2]["method"], "mining.notify");
        assert_eq!(frames[2]["params"][8], false);

        write_half
            .write_all(b"{\"id\":3,\"method\":\"mining.suggest_difficulty\",\"params\":[32]}\n")
            .await
            .unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let repeat: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(repeat["result"], true);
        let mut unexpected = String::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), reader.read_line(&mut unexpected))
                .await
                .is_err(),
            "a repeated suggestion must not send another difficulty update"
        );
        session.abort();
    }

    #[tokio::test]
    async fn difficulty_suggestion_after_first_accepted_share_is_acknowledged_but_ignored() {
        let job = csd_pool_node::easy_static_job("late-suggest-job");
        let jobs = SharedJobWatch::start_with_policy(
            Arc::new(StaticTemplateProvider::new(job.clone())),
            Some(Arc::new(csd_pool_db::InMemoryRepository::new())),
            SharedPoolState::new(),
            Duration::from_secs(60),
            None,
            Duration::from_secs(600),
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();
        let abuse = Arc::new(AbuseManager::default());
        let permit = abuse.try_open(peer.ip()).unwrap();
        let session = tokio::spawn(handle_client(
            server_stream,
            peer,
            SharedPoolState::new(),
            jobs.subscribe(),
            jobs.retained_jobs(),
            None,
            None,
            abuse,
            VardiffConfig {
                initial_difficulty: 1.0,
                min_difficulty: 1.0,
                max_difficulty: 64.0,
                ..VardiffConfig::default()
            },
            permit,
        ));

        let (read_half, mut write_half) = client.into_split();
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(
                b"{\"id\":1,\"method\":\"mining.authorize\",\"params\":[\"0123456789abcdef0123456789abcdef01234567.late-suggest\",\"x\"]}\n",
            )
            .await
            .unwrap();
        for _ in 0..3 {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
        }

        write_half
            .write_all(
                format!(
                    "{{\"id\":2,\"method\":\"mining.submit\",\"params\":[\"0123456789abcdef0123456789abcdef01234567.late-suggest\",\"{}\",\"01020304\",\"{}\",\"00000000\"]}}\n",
                    job.template.job_id, job.notify.ntime_hex
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let accepted: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(accepted["result"], true);

        write_half
            .write_all(b"{\"id\":3,\"method\":\"mining.suggest_difficulty\",\"params\":[16]}\n")
            .await
            .unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let suggestion: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(suggestion["result"], true);

        let mut unexpected = String::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), reader.read_line(&mut unexpected))
                .await
                .is_err(),
            "a late suggestion must not send a difficulty update or notify"
        );
        session.abort();
    }

    #[tokio::test]
    async fn persists_candidate_without_ending_session_when_node_transport_fails() {
        let job = csd_pool_node::easy_static_job("static-1");
        let submit = SubmitParams {
            worker_name: "0123456789abcdef0123456789abcdef01234567.rig-a".to_owned(),
            job_id: job.template.job_id.clone(),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: job.notify.ntime_hex.clone(),
            nonce_hex: "00000001".to_owned(),
        };
        let share = verify_submit(&job.template, [1, 0, 0, 0], &submit, 1.0).unwrap();
        let repository = csd_pool_db::InMemoryRepository::new();
        let submitter = FailingBlockSubmitter;
        let pool_state = SharedPoolState::new();

        let result = submit_block_candidate(
            Some(&submitter),
            Some(&repository),
            &job,
            &pool_state,
            "0123456789abcdef0123456789abcdef01234567",
            &submit,
            &share,
            [1, 0, 0, 0],
            1.0,
            CandidateObservation::now(),
        )
        .await;

        assert!(result.is_ok());
        let candidates = repository.list_block_candidates().unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].hash_hex, hex::encode(share.hash));
        assert_eq!(candidates[0].submit_response_json["retryable"], true);
        assert!(
            candidates[0].submit_response_json["pool_observability"]["candidate_detected_unix_ms"]
                .as_u64()
                .is_some()
        );
        assert!(
            candidates[0].submit_response_json["pool_observability"]["node_roundtrip_us"]
                .as_u64()
                .is_some()
        );
        assert_eq!(pool_state.snapshot().totals.candidate_propagation_count, 1);
    }

    #[tokio::test]
    async fn records_successful_candidate_propagation_metrics() {
        let job = csd_pool_node::easy_static_job("static-1");
        let submit = SubmitParams {
            worker_name: "0123456789abcdef0123456789abcdef01234567.rig-a".to_owned(),
            job_id: job.template.job_id.clone(),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: job.notify.ntime_hex.clone(),
            nonce_hex: "00000001".to_owned(),
        };
        let share = verify_submit(&job.template, [1, 0, 0, 0], &submit, 1.0).unwrap();
        let repository = csd_pool_db::InMemoryRepository::new();
        let pool_state = SharedPoolState::new();

        submit_block_candidate(
            Some(&ObservableBlockSubmitter),
            Some(&repository),
            &job,
            &pool_state,
            "0123456789abcdef0123456789abcdef01234567",
            &submit,
            &share,
            [1, 0, 0, 0],
            1.0,
            CandidateObservation::now(),
        )
        .await
        .unwrap();

        let totals = pool_state.snapshot().totals;
        assert_eq!(totals.candidate_propagation_count, 1);
        assert_eq!(totals.candidate_node_accept_count, 1);
        assert_eq!(totals.candidate_relay_enqueue_count, 1);
        assert!((totals.candidate_node_accept_seconds_sum - 0.000_125).abs() < f64::EPSILON);
        assert!((totals.candidate_relay_enqueue_seconds_sum - 0.000_140).abs() < f64::EPSILON);
        let candidates = repository.list_block_candidates().unwrap();
        assert_eq!(
            candidates[0].submit_response_json["node_observability"]["relay_queued"],
            true
        );
        assert!(
            candidates[0].submit_response_json["pool_observability"]["node_roundtrip_us"]
                .as_u64()
                .is_some()
        );
    }

    #[test]
    fn canonical_block_hash_uses_display_order_without_implicit_reversal() {
        let display =
            CanonicalBlockHash::from_display_hex(&format!("0x{}", H58427_PARENT_DISPLAY)).unwrap();
        let uppercase = CanonicalBlockHash::from_display_hex(&format!(
            "0X{}",
            H58427_PARENT_DISPLAY.to_ascii_uppercase()
        ))
        .unwrap();
        let candidate_parent =
            CanonicalBlockHash::from_candidate_header(&h58427_candidate()).unwrap();
        let mut reversed = hex::decode(H58427_PARENT_DISPLAY).unwrap();
        reversed.reverse();
        let reversed_display =
            CanonicalBlockHash::from_display_hex(&hex::encode(reversed)).unwrap();

        assert_eq!(display, uppercase);
        assert_eq!(display, candidate_parent);
        assert_ne!(display, reversed_display);
        assert!(CanonicalBlockHash::from_display_hex("00").is_none());

        let mut malformed = h58427_candidate();
        malformed.header_hex = "00".repeat(83);
        assert!(CanonicalBlockHash::from_candidate_header(&malformed).is_none());
    }

    #[tokio::test]
    async fn parallel_submit_real_h58427_parent_allows_secondary() {
        let primary = Arc::new(
            MockCandidateNodeEndpoint::healthy("node-a", MockSubmitBehavior::Response(true))
                .with_tip(&format!("0x{H58427_PARENT_DISPLAY}")),
        );
        let secondary = Arc::new(
            MockCandidateNodeEndpoint::healthy("node-b", MockSubmitBehavior::Response(true))
                .with_tip(&format!("0X{}", H58427_PARENT_DISPLAY.to_ascii_uppercase())),
        );
        let response =
            parallel_submitter(primary.clone(), secondary.clone(), Duration::from_secs(1))
                .await
                .submit_candidate(&h58427_submission())
                .await
                .unwrap();

        assert!(response.ok);
        assert_eq!(response.extra["parallel_submit"]["status"], "both_accepted");
        assert_eq!(primary.submit_calls.load(Ordering::SeqCst), 1);
        assert_eq!(secondary.submit_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn parallel_submit_parent_mismatch_does_not_claim_one_shot_budget() {
        let primary = Arc::new(
            MockCandidateNodeEndpoint::healthy("node-a", MockSubmitBehavior::Response(true))
                .with_tip(&format!("0x{H58427_PARENT_DISPLAY}")),
        );
        let secondary = Arc::new(
            MockCandidateNodeEndpoint::healthy("node-b", MockSubmitBehavior::Response(true))
                .with_tip(&format!("0x{H58427_PARENT_DISPLAY}")),
        );
        let submitter =
            parallel_submitter(primary.clone(), secondary.clone(), Duration::from_secs(1)).await;
        let mut mismatch = h58427_submission();
        let mut header = hex::decode(&mismatch.candidate.header_hex).unwrap();
        header[4] ^= 0x01;
        mismatch.candidate.header_hex = hex::encode(header);

        let first = submitter.submit_candidate(&mismatch).await.unwrap();
        assert_eq!(
            first.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "candidate_parent_mismatch"
        );
        assert_eq!(
            first.extra["parallel_submit"]["secondary_candidate_budget"]["claimed"],
            false
        );
        assert!(!submitter.one_shot_latch_path().exists());
        assert_eq!(secondary.submit_calls.load(Ordering::SeqCst), 0);

        let second = submitter
            .submit_candidate(&h58427_submission())
            .await
            .unwrap();
        assert_eq!(
            second.extra["parallel_submit"]["secondary_candidate_budget"]["claimed"],
            true
        );
        assert_eq!(primary.submit_calls.load(Ordering::SeqCst), 2);
        assert_eq!(secondary.submit_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn parallel_submit_malformed_candidate_parent_skips_secondary() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let mut candidate = mock_submission();
        candidate.candidate.header_hex = "00".repeat(83);
        let response = parallel_submitter(primary, secondary.clone(), Duration::from_secs(1))
            .await
            .submit_candidate(&candidate)
            .await
            .unwrap();

        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "candidate_parent_unavailable"
        );
        assert_eq!(secondary.submit_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn parallel_submit_records_both_successes_and_keeps_primary_authoritative() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let response =
            parallel_submitter(primary.clone(), secondary.clone(), Duration::from_secs(1))
                .await
                .submit_candidate(&mock_submission())
                .await
                .unwrap();

        assert!(response.ok);
        assert_eq!(response.extra["parallel_submit"]["status"], "both_accepted");
        assert_eq!(
            response.extra["parallel_submit"]["primary_submit"]["outcome"],
            "accepted"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["outcome"],
            "accepted"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_candidate_budget"]["claimed"],
            true
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_candidate_budget"]["remaining"],
            0
        );
        assert_eq!(primary.submit_calls.load(Ordering::SeqCst), 1);
        assert_eq!(secondary.submit_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn parallel_submit_one_shot_budget_allows_only_one_concurrent_secondary_call() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::DelayedResponse(Duration::from_millis(25), true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::DelayedResponse(Duration::from_millis(25), true),
        ));
        let submitter =
            parallel_submitter(primary.clone(), secondary.clone(), Duration::from_secs(1)).await;
        let mut first = mock_submission();
        first.candidate.job_id = "candidate-1".to_owned();
        first.candidate.hash_hex = "11".repeat(32);
        let mut second = mock_submission();
        second.candidate.job_id = "candidate-2".to_owned();
        second.candidate.hash_hex = "22".repeat(32);

        let (first_response, second_response) = tokio::join!(
            submitter.submit_candidate(&first),
            submitter.submit_candidate(&second)
        );
        let responses = [first_response.unwrap(), second_response.unwrap()];

        assert_eq!(primary.submit_calls.load(Ordering::SeqCst), 2);
        assert_eq!(secondary.submit_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            responses
                .iter()
                .filter(|response| {
                    response.extra["parallel_submit"]["secondary_candidate_budget"]["claimed"]
                        == true
                })
                .count(),
            1
        );
        assert_eq!(
            responses
                .iter()
                .filter(|response| {
                    response.extra["parallel_submit"]["secondary_submit"]["skip_reason"]
                        == "candidate_budget_exhausted"
                })
                .count(),
            1
        );
        assert!(responses.iter().all(|response| {
            response.extra["parallel_submit"]["secondary_candidate_budget"]["remaining"] == 0
        }));
    }

    #[tokio::test]
    async fn parallel_submit_keeps_primary_success_when_secondary_fails() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Error("node-b unavailable"),
        ));
        let response = parallel_submitter(primary, secondary, Duration::from_secs(1))
            .await
            .submit_candidate(&mock_submission())
            .await
            .unwrap();

        assert!(response.ok);
        assert_eq!(
            response.extra["parallel_submit"]["status"],
            "authority_accepted_secondary_failed"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["outcome"],
            "transport_error"
        );
    }

    #[tokio::test]
    async fn parallel_submit_reports_secondary_success_without_overriding_primary_failure() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(false),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let response = parallel_submitter(primary, secondary, Duration::from_secs(1))
            .await
            .submit_candidate(&mock_submission())
            .await
            .unwrap();

        assert!(response.ok);
        assert_eq!(
            response.extra["parallel_submit"]["status"],
            "secondary_accepted_authority_failed"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["outcome"],
            "accepted"
        );
    }

    #[tokio::test]
    async fn secondary_only_success_persists_status_and_operator_alert() {
        use csd_pool_db::{BlockRepository, MonitoringRepository};

        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(false),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let submission = mock_submission();
        let response = parallel_submitter(primary, secondary, Duration::from_secs(1))
            .await
            .submit_candidate(&submission)
            .await
            .unwrap();
        let repository = csd_pool_db::InMemoryRepository::new();
        let record = block_candidate_record(
            "0123456789abcdef0123456789abcdef01234567",
            &submission,
            &response,
            42.0,
        );

        assert!(repository.record_block_candidate(&record).await.unwrap());
        let blocks = BlockRepository::list_blocks_to_reconcile(&repository, 10)
            .await
            .unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].status, "submitted_secondary");
        let alerts = repository
            .list_block_submission_alerts(10, 10)
            .await
            .unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].status, "submitted_secondary");
        assert_eq!(alerts[0].reason, "authority_failed_secondary_accepted");
        assert_eq!(
            repository.list_block_candidates().unwrap()[0].submit_response_json["parallel_submit"]
                ["accepted_by"],
            "secondary"
        );
    }

    #[tokio::test]
    async fn parallel_submit_fails_when_both_nodes_are_local_only_without_broadcast() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::LocalCanonicalRelayFailed("insufficient_peers"),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::LocalCanonicalRelayFailed("relay_ack_timeout"),
        ));
        let response = parallel_submitter(primary, secondary, Duration::from_secs(1))
            .await
            .submit_candidate(&mock_submission())
            .await
            .unwrap();

        assert!(!response.ok);
        assert_eq!(
            response.extra["parallel_submit"]["status"],
            "both_local_canonical_relay_failed"
        );
        assert_eq!(response.extra["parallel_submit"]["accepted_by"], "none");
        assert_eq!(
            response.extra["parallel_submit"]["primary_submit"]["outcome"],
            "local_canonical_relay_failed"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["outcome"],
            "local_canonical_relay_failed"
        );
        assert_eq!(
            response.extra["parallel_submit"]["local_canonical_nodes"],
            serde_json::json!({"authority": true, "secondary": true})
        );
    }

    #[tokio::test]
    async fn secondary_broadcast_success_does_not_hide_authority_local_only_failure() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::LocalCanonicalRelayFailed("insufficient_peers"),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let response = parallel_submitter(primary, secondary, Duration::from_secs(1))
            .await
            .submit_candidate(&mock_submission())
            .await
            .unwrap();

        assert!(response.ok);
        assert_eq!(
            response.extra["parallel_submit"]["accepted_by"],
            "secondary"
        );
        assert_eq!(
            response.extra["parallel_submit"]["status"],
            "secondary_accepted_authority_local_canonical_relay_failed"
        );
        assert_eq!(
            response.extra["parallel_submit"]["primary_submit"]["outcome"],
            "local_canonical_relay_failed"
        );
    }

    #[tokio::test]
    async fn authority_broadcast_success_preserves_secondary_local_only_failure() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::LocalCanonicalRelayFailed("relay_ack_timeout"),
        ));
        let response = parallel_submitter(primary, secondary, Duration::from_secs(1))
            .await
            .submit_candidate(&mock_submission())
            .await
            .unwrap();

        assert!(response.ok);
        assert_eq!(
            response.extra["parallel_submit"]["accepted_by"],
            "authority"
        );
        assert_eq!(
            response.extra["parallel_submit"]["status"],
            "authority_accepted_secondary_local_canonical_relay_failed"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["outcome"],
            "local_canonical_relay_failed"
        );
    }

    #[tokio::test]
    async fn parallel_submit_bounds_both_node_timeouts() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::DelayedResponse(Duration::from_secs(1), true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::DelayedResponse(Duration::from_secs(1), true),
        ));
        let response = parallel_submitter(primary, secondary, Duration::from_millis(100))
            .await
            .submit_candidate(&mock_submission())
            .await
            .unwrap();

        assert!(!response.ok);
        assert_eq!(response.extra["parallel_submit"]["status"], "both_failed");
        assert_eq!(
            response.extra["parallel_submit"]["primary_submit"]["outcome"],
            "timeout"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["outcome"],
            "timeout"
        );
    }

    #[tokio::test]
    async fn parallel_submit_skips_secondary_when_chain_state_differs() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(
            MockCandidateNodeEndpoint::healthy("node-b", MockSubmitBehavior::Response(true))
                .with_tip(&format!("0x{}", "11".repeat(32))),
        );
        let response = parallel_submitter(primary, secondary.clone(), Duration::from_secs(1))
            .await
            .submit_candidate(&mock_submission())
            .await
            .unwrap();

        assert!(response.ok);
        assert_eq!(
            response.extra["parallel_submit"]["status"],
            "authority_accepted_secondary_skipped"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "chain_state_mismatch"
        );
        assert_eq!(secondary.submit_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn parallel_submit_skips_secondary_without_relay_peers_or_latch_claim() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(
            MockCandidateNodeEndpoint::healthy("node-b", MockSubmitBehavior::Response(true))
                .with_peers(0),
        );
        let submitter = parallel_submitter_without_health_watch(
            primary,
            secondary.clone(),
            test_candidate_latch(),
        );
        submitter.refresh_health_once().await;

        let response = submitter
            .submit_candidate(&mock_submission())
            .await
            .unwrap();

        assert!(response.ok);
        assert_eq!(
            response.extra["parallel_submit"]["status"],
            "authority_accepted_secondary_skipped"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "secondary_no_peers"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_candidate_budget"]["claimed"],
            false
        );
        assert!(!submitter.one_shot_latch_path().exists());
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn parallel_submit_health_mismatch_does_not_claim_and_next_healthy_candidate_can_claim() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(
            MockCandidateNodeEndpoint::healthy("node-b", MockSubmitBehavior::Response(true))
                .with_tip(&format!("0x{}", "11".repeat(32))),
        );
        let submitter = parallel_submitter_without_health_watch(
            primary,
            secondary.clone(),
            test_candidate_latch(),
        );
        submitter.refresh_health_once().await;

        let first = submitter
            .submit_candidate(&mock_submission())
            .await
            .unwrap();
        assert_eq!(
            first.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "chain_state_mismatch"
        );
        assert_eq!(
            first.extra["parallel_submit"]["secondary_candidate_budget"]["claimed"],
            false
        );
        assert!(!submitter.one_shot_latch_path().exists());

        {
            let mut health = submitter
                .health_snapshot
                .write()
                .expect("parallel health snapshot lock poisoned");
            let snapshot = health.as_mut().unwrap();
            snapshot.secondary.snapshot.as_mut().unwrap().tip =
                Some(format!("0x{}", "00".repeat(32)));
            snapshot.observed_at = Instant::now();
        }

        let second = submitter
            .submit_candidate(&mock_submission())
            .await
            .unwrap();
        assert_eq!(
            second.extra["parallel_submit"]["secondary_candidate_budget"]["claimed"],
            true
        );
        assert_eq!(secondary.submit_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn candidate_before_first_health_does_not_create_latch() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let latch = test_candidate_latch();
        let submitter =
            parallel_submitter_without_health_watch(primary.clone(), secondary.clone(), latch);

        let first = submitter
            .submit_candidate(&mock_submission())
            .await
            .unwrap();
        assert!(first.ok);
        assert_eq!(
            first.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "health_not_ready"
        );
        assert_eq!(
            first.extra["parallel_submit"]["secondary_candidate_budget"]["claimed"],
            false
        );
        assert!(!submitter.one_shot_latch_path().exists());
        assert_eq!(primary.cached_submit_calls.load(Ordering::SeqCst), 1);
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 0);

        submitter.refresh_health_once().await;
        let second = submitter
            .submit_candidate(&mock_submission())
            .await
            .unwrap();
        assert!(second.ok);
        assert_eq!(
            second.extra["parallel_submit"]["secondary_candidate_budget"]["claimed"],
            true
        );
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn continuous_bounded_submits_each_candidate_once_across_restart() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let (journal, directory) = test_candidate_journal();
        let submitter = continuous_submitter_without_health_watch(
            primary.clone(),
            secondary.clone(),
            journal,
            3,
            Duration::from_secs(30),
        );
        submitter.refresh_health_once().await;

        let first = mock_submission();
        let mut second = mock_submission();
        second.candidate.job_id = "candidate-2".to_owned();
        second.candidate.hash_hex = "22".repeat(32);
        assert!(submitter.submit_candidate(&first).await.unwrap().ok);
        assert!(submitter.submit_candidate(&second).await.unwrap().ok);
        let duplicate = submitter.submit_candidate(&first).await.unwrap();

        assert_eq!(primary.cached_submit_calls.load(Ordering::SeqCst), 3);
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            duplicate.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "candidate_already_submitted_to_secondary"
        );
        assert_eq!(
            duplicate.extra["parallel_submit"]["secondary_candidate_control"]["mode"],
            "continuous_bounded"
        );

        let restarted = continuous_submitter_without_health_watch(
            primary.clone(),
            secondary.clone(),
            PersistentCandidateJournal::new(directory.clone()).unwrap(),
            3,
            Duration::from_secs(30),
        );
        restarted.refresh_health_once().await;
        let replay = restarted.submit_candidate(&second).await.unwrap();
        assert!(replay.ok);
        assert_eq!(
            replay.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "candidate_already_submitted_to_secondary"
        );
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn continuous_bounded_slow_claim_does_not_delay_primary_start() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let (journal, directory) = test_candidate_journal();
        let submitter = continuous_submitter_without_health_watch(
            primary.clone(),
            secondary,
            journal.with_claim_delay(Duration::from_millis(150)),
            3,
            Duration::from_secs(30),
        );
        submitter.refresh_health_once().await;
        let pending =
            tokio::spawn(async move { submitter.submit_candidate(&mock_submission()).await });

        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(primary.cached_submit_calls.load(Ordering::SeqCst), 1);
        assert!(!pending.is_finished());
        assert!(pending.await.unwrap().unwrap().ok);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn continuous_bounded_limits_secondary_concurrency_without_blocking_primary() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::DelayedResponse(Duration::from_millis(100), true),
        ));
        let (journal, directory) = test_candidate_journal();
        let submitter = continuous_submitter_without_health_watch(
            primary.clone(),
            secondary.clone(),
            journal,
            3,
            Duration::from_secs(30),
        );
        submitter.refresh_health_once().await;
        let first = mock_submission();
        let mut second = mock_submission();
        second.candidate.job_id = "candidate-2".to_owned();
        second.candidate.hash_hex = "22".repeat(32);

        let (first, second) = tokio::join!(
            submitter.submit_candidate(&first),
            submitter.submit_candidate(&second)
        );
        let responses = [first.unwrap(), second.unwrap()];
        assert_eq!(primary.cached_submit_calls.load(Ordering::SeqCst), 2);
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            responses
                .iter()
                .filter(|response| {
                    response.extra["parallel_submit"]["secondary_submit"]["skip_reason"]
                        == "secondary_concurrency_limit"
                })
                .count(),
            1
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn continuous_bounded_circuit_breaker_isolated_from_primary_and_recovers() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Error("node-b unavailable"),
        ));
        let (journal, directory) = test_candidate_journal();
        let submitter = continuous_submitter_without_health_watch(
            primary.clone(),
            secondary.clone(),
            journal,
            2,
            Duration::from_millis(50),
        );
        submitter.refresh_health_once().await;

        for value in [0x11, 0x22] {
            let mut submission = mock_submission();
            submission.candidate.hash_hex = format!("{value:02x}").repeat(32);
            assert!(submitter.submit_candidate(&submission).await.unwrap().ok);
        }
        let mut blocked = mock_submission();
        blocked.candidate.hash_hex = "33".repeat(32);
        let blocked = submitter.submit_candidate(&blocked).await.unwrap();
        assert!(blocked.ok);
        assert_eq!(
            blocked.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "secondary_circuit_open"
        );
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 2);
        assert_eq!(primary.cached_submit_calls.load(Ordering::SeqCst), 3);

        tokio::time::sleep(Duration::from_millis(75)).await;
        let mut retry = mock_submission();
        retry.candidate.hash_hex = "44".repeat(32);
        assert!(submitter.submit_candidate(&retry).await.unwrap().ok);
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 3);
        assert_eq!(primary.cached_submit_calls.load(Ordering::SeqCst), 4);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn continuous_bounded_secondary_timeout_keeps_primary_success() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::DelayedResponse(Duration::from_secs(1), true),
        ));
        let (journal, directory) = test_candidate_journal();
        let mut submitter = continuous_submitter_without_health_watch(
            primary.clone(),
            secondary.clone(),
            journal,
            3,
            Duration::from_secs(30),
        );
        submitter.secondary_timeout = Duration::from_millis(250);
        submitter.refresh_health_once().await;

        let started = Instant::now();
        let response = submitter
            .submit_candidate(&mock_submission())
            .await
            .unwrap();
        assert!(response.ok);
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(
            response.extra["parallel_submit"]["accepted_by"],
            "authority"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["outcome"],
            "timeout"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_candidate_control"]["circuit"]["consecutive_failures"],
            1
        );
        assert_eq!(primary.cached_submit_calls.load(Ordering::SeqCst), 1);
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn continuous_candidate_journal_requires_private_real_directory() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let directory =
            std::env::temp_dir().join(format!("csd-pool-secondary-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = PersistentCandidateJournal::new(directory.clone())
            .err()
            .expect("broad state directory permissions must fail closed");
        assert!(error.to_string().contains("mode 0700"));
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn persistent_latch_is_mode_600_and_existing_file_is_fail_closed() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let latch = test_candidate_latch();
        let path = latch.path.as_ref().clone();
        let first = latch.claim(&"11".repeat(32), 1);
        assert!(first.claimed);
        assert!(first.error.is_none());
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let restarted = PersistentCandidateLatch::new(path.clone()).unwrap();
        let second = restarted.claim(&"22".repeat(32), 1);
        assert!(!second.claimed);
        assert!(second.error.is_none());
        assert_eq!(second.remaining, Some(0));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn persistent_latch_rejects_unsafe_writable_parent() {
        use std::os::unix::fs::PermissionsExt;

        let parent = std::env::temp_dir().join(format!(
            "csd-pool-secondary-candidate-unsafe-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = PersistentCandidateLatch::new(parent.join("candidate.latch"))
            .err()
            .expect("unsafe writable parent must fail closed");
        assert!(error.to_string().contains("group/world writable"));
        std::fs::remove_dir(parent).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn persistent_latch_rejects_existing_file_with_broad_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let parent = std::env::temp_dir().join(format!(
            "csd-pool-secondary-candidate-mode-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = parent.join("candidate.latch");
        std::fs::write(&path, b"claimed\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = PersistentCandidateLatch::new(path.clone())
            .err()
            .expect("broad latch permissions must fail closed");
        assert!(error.to_string().contains("mode 0600 or stricter"));
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(parent).unwrap();
    }

    #[tokio::test]
    async fn slow_latch_persistence_does_not_delay_primary_request_start() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let latch = test_candidate_latch().with_claim_delay(Duration::from_millis(150));
        let submitter = parallel_submitter_without_health_watch(primary.clone(), secondary, latch);
        submitter.refresh_health_once().await;
        let submission = mock_submission();
        let pending = tokio::spawn(async move { submitter.submit_candidate(&submission).await });

        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(primary.cached_submit_calls.load(Ordering::SeqCst), 1);
        assert!(!pending.is_finished());
        assert!(pending.await.unwrap().unwrap().ok);
    }

    #[tokio::test]
    async fn slow_latch_persistence_is_bounded_and_fails_closed() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let latch = test_candidate_latch().with_claim_delay(Duration::from_millis(350));
        let latch_path = latch.path.clone();
        let mut submitter =
            parallel_submitter_without_health_watch(primary, secondary.clone(), latch);
        submitter.secondary_timeout = Duration::from_millis(50);
        submitter.refresh_health_once().await;

        let started = Instant::now();
        let response = submitter
            .submit_candidate(&mock_submission())
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(response.ok);
        assert_eq!(
            response.extra["parallel_submit"]["status"],
            "authority_accepted_secondary_skipped"
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "candidate_budget_persistence_failed"
        );
        assert!(
            response.extra["parallel_submit"]["secondary_candidate_budget"]["error"]
                .as_str()
                .unwrap()
                .contains("timed out")
        );
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 0);

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(latch_path.exists());
    }

    #[tokio::test]
    async fn health_drift_after_claim_skips_secondary_and_consumes_latch() {
        let primary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-a",
            MockSubmitBehavior::Response(true),
        ));
        let secondary = Arc::new(MockCandidateNodeEndpoint::healthy(
            "node-b",
            MockSubmitBehavior::Response(true),
        ));
        let latch = test_candidate_latch().with_claim_delay(Duration::from_millis(100));
        let submitter = Arc::new(parallel_submitter_without_health_watch(
            primary,
            secondary.clone(),
            latch,
        ));
        submitter.refresh_health_once().await;
        let worker = submitter.clone();
        let pending =
            tokio::spawn(async move { worker.submit_candidate(&mock_submission()).await });

        while !submitter.one_shot_latch_path().exists() {
            tokio::task::yield_now().await;
        }
        {
            let mut health = submitter
                .health_snapshot
                .write()
                .expect("parallel health snapshot lock poisoned");
            let snapshot = health.as_mut().unwrap();
            snapshot.secondary.snapshot.as_mut().unwrap().tip =
                Some(format!("0x{}", "44".repeat(32)));
            snapshot.observed_at = Instant::now();
        }
        let response = pending.await.unwrap().unwrap();

        assert!(response.ok);
        assert_eq!(
            response.extra["parallel_submit"]["secondary_candidate_budget"]["claimed"],
            true
        );
        assert_eq!(
            response.extra["parallel_submit"]["secondary_submit"]["skip_reason"],
            "chain_state_mismatch"
        );
        assert_eq!(secondary.full_submit_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn parallel_submit_replays_over_two_real_http_node_clients() {
        let (primary_url, primary_calls) = spawn_replay_node().await;
        let (secondary_url, secondary_calls) = spawn_replay_node().await;
        let submitter = ParallelNodeBlockSubmitter::new(
            Arc::new(CsdCandidateNodeEndpoint::new(
                "node-a".to_owned(),
                CsdNodeClient::new(primary_url),
            )),
            Arc::new(CsdCandidateNodeEndpoint::new(
                "node-b".to_owned(),
                CsdNodeClient::new(secondary_url),
            )),
            ParallelSubmitConfig {
                primary_timeout: Duration::from_secs(1),
                secondary_timeout: Duration::from_secs(1),
                health_refresh: Duration::from_secs(10),
                health_max_age: Duration::from_secs(30),
                secondary_max_in_flight: 1,
                circuit_failure_threshold: 3,
                circuit_cooldown: Duration::from_secs(30),
            },
            SecondaryCandidateControl::OneShot {
                budget: 1,
                latch: test_candidate_latch(),
            },
        );
        submitter.refresh_health_once().await;

        let response = submitter
            .submit_candidate(&mock_submission())
            .await
            .unwrap();

        assert!(response.ok);
        assert_eq!(response.extra["parallel_submit"]["status"], "both_accepted");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(secondary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            response.extra["parallel_submit"]["primary_health"]["tip"],
            format!("0x{}", "00".repeat(32))
        );
    }

    #[test]
    fn parallel_candidate_submit_budget_is_fixed_to_one() {
        assert_eq!(parallel_candidate_submit_budget(None).unwrap(), 1);
        assert_eq!(parallel_candidate_submit_budget(Some("1")).unwrap(), 1);
        assert!(parallel_candidate_submit_budget(Some("0")).is_err());
        assert!(parallel_candidate_submit_budget(Some("2")).is_err());
        assert!(parallel_candidate_submit_budget(Some("invalid")).is_err());
    }

    #[test]
    fn live_template_tip_polling_is_bounded() {
        assert_eq!(bounded_template_refresh_secs(0, "live"), 1);
        assert_eq!(bounded_template_refresh_secs(2, "live"), 2);
        assert_eq!(bounded_template_refresh_secs(30, "LIVE"), 5);
        assert_eq!(bounded_template_refresh_secs(30, "static"), 30);
    }

    #[test]
    fn validates_addr20() {
        assert!(valid_addr20("0123456789abcdef0123456789abcdef01234567"));
        assert!(!valid_addr20("0x0123456789abcdef0123456789abcdef01234567"));
        assert!(!valid_addr20("xyz"));
    }

    #[test]
    fn parses_authorize_worker_with_prefix() {
        let params = serde_json::json!(["0xABCDEFabcdefABCDEFabcdefABCDEFabcdefABCD", "x"]);
        assert_eq!(
            parse_authorize_worker(&params).unwrap(),
            "abcdefabcdefabcdefabcdefabcdefabcdefabcd"
        );
    }

    #[test]
    fn parses_authorize_worker_suffix_and_rejects_invalid_names() {
        let params = serde_json::json!([
            format!("0123456789abcdef0123456789abcdef01234567.rig-01"),
            "x"
        ]);
        assert_eq!(
            parse_authorize_worker(&params).unwrap(),
            "0123456789abcdef0123456789abcdef01234567"
        );

        let invalid =
            serde_json::json!([format!("0123456789abcdef0123456789abcdef01234567/rig"), "x"]);
        assert!(parse_authorize_worker(&invalid).is_none());
    }

    #[test]
    fn submit_worker_must_belong_to_authorized_address() {
        let miner = "0123456789abcdef0123456789abcdef01234567";
        assert!(authorized_submit_worker(miner, miner));
        assert!(authorized_submit_worker(miner, &format!("{miner}.rig_01")));
        assert!(!authorized_submit_worker(
            miner,
            "89abcdef0123456789abcdef0123456789abcdef"
        ));
        assert!(!authorized_submit_worker(
            miner,
            &format!("{miner}.bad/name")
        ));
    }

    #[test]
    fn default_stratum_listen_is_localhost() {
        if std::env::var("CSD_POOL_CONFIG").is_err()
            && std::env::var("CSD_POOL_STRATUM_LISTEN").is_err()
        {
            assert_eq!(stratum_listen(), "127.0.0.1:3333");
        }
    }

    #[test]
    fn static_job_is_internally_consistent() {
        let job = csd_pool_node::easy_static_job("static-1");
        assert_eq!(job.notify.job_id, job.template.job_id);
        assert_eq!(job.template.share_target, [0xff; 32]);
    }

    #[test]
    fn verifies_submit_for_static_job_easy_target() {
        let job = csd_pool_node::easy_static_job("static-1");
        let submit = SubmitParams {
            worker_name: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            job_id: job.template.job_id.clone(),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: job.notify.ntime_hex.clone(),
            nonce_hex: "00000001".to_owned(),
        };
        let share = verify_submit(&job.template, [1, 0, 0, 0], &submit, 1.0).unwrap();
        assert_eq!(share.header.len(), 84);
    }

    #[test]
    fn maps_pool_job_to_job_record() {
        let job = csd_pool_node::easy_static_job("static-1");
        let record = job_record_from_pool_job(&job, JobReason::TipChange);
        assert_eq!(record.job_id, "static-1");
        assert_eq!(record.prev_hash_be_hex, "00".repeat(32));
        assert_eq!(record.version_hex, "20000000");
        assert_eq!(record.share_target, [0xff; 32]);
        assert!(record.clean_jobs);
    }

    #[test]
    fn maps_verified_submit_to_share_record() {
        let job = csd_pool_node::easy_static_job("static-1");
        let submit = SubmitParams {
            worker_name: "0123456789abcdef0123456789abcdef01234567.rig-a".to_owned(),
            job_id: job.template.job_id.clone(),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: job.notify.ntime_hex.clone(),
            nonce_hex: "00000001".to_owned(),
        };
        let share = verify_submit(&job.template, [1, 0, 0, 0], &submit, 1.0).unwrap();
        let record = share_record_from_submit(
            None,
            "0123456789abcdef0123456789abcdef01234567",
            &submit,
            &share,
            8.0,
        );

        assert_eq!(record.worker_name, "rig-a");
        assert_eq!(record.hash, share.hash);
        assert_eq!(record.extranonce2_hex, "01020304");
    }

    #[test]
    fn estimates_block_candidate_effort_from_current_round_work() {
        let mut job = csd_pool_node::easy_static_job("static-1");
        job.template.network_target =
            csd_pool_consensus::target_for_difficulty(&job.template.share_target, 4.0);
        let pool_state = SharedPoolState::new();
        pool_state.record_share_accepted("0123456789abcdef0123456789abcdef01234567", 1.0, false);

        let effort = block_effort_pct(&job, &pool_state, 1.0);

        assert!((effort - 50.0).abs() < 0.01);
    }

    #[test]
    fn maps_block_candidate_submit_request_and_record() {
        let job = csd_pool_node::easy_static_job("static-1");
        let submit = SubmitParams {
            worker_name: "0123456789abcdef0123456789abcdef01234567.rig-a".to_owned(),
            job_id: job.template.job_id.clone(),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: job.notify.ntime_hex.clone(),
            nonce_hex: "00000001".to_owned(),
        };
        let share = verify_submit(&job.template, [1, 0, 0, 0], &submit, 1.0).unwrap();
        let request = block_candidate_submit_request(
            &job,
            "0123456789abcdef0123456789abcdef01234567",
            &submit,
            &share,
            [1, 0, 0, 0],
        );
        assert_eq!(request.job_id, "static-1");
        assert_eq!(request.worker_name, "rig-a");
        assert_eq!(request.header_hex.len(), 168);
        assert_eq!(request.hash_hex, hex::encode(share.hash));
        assert_eq!(
            request.coinbase_hex,
            hex::encode(coinbase_bytes(
                &job.template.coinbase_prefix,
                compose_extranonce([1, 0, 0, 0], [1, 2, 3, 4]),
                &job.template.coinbase_suffix,
            ))
        );

        let response = SubmitBlockResponse {
            ok: true,
            hash: Some("22".repeat(32)),
            extra: serde_json::json!({"source": "test"}),
        };
        let submission = BlockCandidateSubmission {
            candidate: request,
            candidate_material: None,
        };
        let record = block_candidate_record(
            "0123456789abcdef0123456789abcdef01234567",
            &submission,
            &response,
            91.25,
        );
        assert_eq!(record.hash_hex, "22".repeat(32));
        assert_eq!(record.worker_name, "rig-a");
        assert_eq!(record.effort_pct, 91.25);
        assert_eq!(record.candidate_payload_json["job_id"], "static-1");
        assert_eq!(record.submit_response_json["ok"], true);
    }

    #[test]
    fn candidate_observability_preserves_node_response_fields() {
        let mut response = SubmitBlockResponse {
            ok: true,
            hash: Some("22".repeat(32)),
            extra: serde_json::json!({
                "status": "accepted",
                "node_observability": {
                    "accept_elapsed_us": 125,
                    "relay_queued": true,
                    "relay_ack_scope": "local_gossipsub_publish",
                    "relay_connected_peers_observed": 8,
                    "relay_remote_delivery_observed": false,
                },
            }),
        };
        let observation = CandidateObservation {
            detected_at: Instant::now(),
            detected_unix_ms: 123_456,
        };

        attach_candidate_observability(&mut response, observation, 42, 87);

        assert_eq!(response.extra["status"], "accepted");
        assert_eq!(
            response.extra["node_observability"]["accept_elapsed_us"],
            125
        );
        assert_eq!(
            response.extra["node_observability"]["relay_ack_scope"],
            "local_gossipsub_publish"
        );
        assert_eq!(
            response.extra["node_observability"]["relay_connected_peers_observed"],
            8
        );
        assert_eq!(
            response.extra["node_observability"]["relay_remote_delivery_observed"],
            false
        );
        assert_eq!(
            response.extra["pool_observability"]["candidate_detected_unix_ms"],
            123_456
        );
        assert_eq!(
            response.extra["pool_observability"]["candidate_detected_to_submit_start_us"],
            42
        );
        assert_eq!(
            response.extra["pool_observability"]["node_roundtrip_us"],
            87
        );
        assert_eq!(
            candidate_node_duration(&response, "accept_elapsed_us"),
            Some(Duration::from_micros(125))
        );
        assert_eq!(
            candidate_node_duration(&response, "relay_enqueue_elapsed_us"),
            None
        );
    }

    #[test]
    fn parallel_health_tracks_tip_age_without_claiming_global_freshness() {
        let audit = |tip: &str| HealthAudit {
            node: "node-a".to_owned(),
            started_unix_ms: 1,
            finished_unix_ms: 2,
            elapsed_us: 1_000,
            outcome: "ok",
            snapshot: Some(NodeHealth {
                height: Some(100),
                tip: Some(tip.to_owned()),
                chainwork: Some("1000".to_owned()),
                peers: Some(8),
                extra: serde_json::Value::Null,
            }),
            error: None,
        };
        let original = audit(&format!("0x{}", "00".repeat(32)));
        let same = audit(&format!("0x{}", "00".repeat(32)));
        let changed = audit(&format!("0x{}", "01".repeat(32)));
        assert_eq!(tip_observed_since(None, None, &original, 100), Some(100));
        assert_eq!(
            tip_observed_since(Some(&original), Some(100), &same, 250),
            Some(100)
        );
        assert_eq!(
            tip_observed_since(Some(&same), Some(100), &changed, 300),
            Some(300)
        );

        let snapshot = ParallelHealthSnapshot {
            primary: same.clone(),
            secondary: same,
            observed_at: Instant::now(),
            observed_unix_ms: 250,
            primary_tip_observed_since_unix_ms: Some(100),
            secondary_tip_observed_since_unix_ms: Some(125),
        };
        let value = candidate_parent_health_json(&snapshot, &mock_candidate());
        assert_eq!(value["primary_tip_age_ms"], 150);
        assert_eq!(value["secondary_tip_age_ms"], 125);
        assert_eq!(value["candidate_parent_matches_primary"], true);
        assert_eq!(value["candidate_parent_matches_secondary"], true);
        assert_eq!(value["global_freshness_proven"], false);
    }

    #[test]
    fn defaults_worker_name_when_submit_name_has_no_suffix() {
        assert_eq!(
            worker_name_from_submit(
                "0123456789abcdef0123456789abcdef01234567",
                "0123456789abcdef0123456789abcdef01234567"
            ),
            "default"
        );
    }

    #[test]
    fn maps_submit_to_share_quality_event() {
        let miner = "0123456789abcdef0123456789abcdef01234567";
        let submit = SubmitParams {
            worker_name: format!("{miner}.rig-a"),
            job_id: "job-1".to_owned(),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            nonce_hex: "00000001".to_owned(),
        };
        let event = share_event_from_submit(None, miner, &submit, "rejected", "low_difficulty");
        assert_eq!(event.miner, miner);
        assert_eq!(event.worker_name, "rig-a");
        assert_eq!(event.job_id.as_deref(), Some("job-1"));
        assert_eq!(event.kind, "rejected");
        assert_eq!(event.reason, "low_difficulty");
    }

    #[test]
    fn share_key_detects_duplicate_submit_tuple() {
        let submit = SubmitParams {
            worker_name: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            job_id: "job1".to_owned(),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            nonce_hex: "00000001".to_owned(),
        };
        let mut seen = HashSet::new();
        assert!(seen.insert(ShareKey::from_submit(&submit)));
        assert!(!seen.insert(ShareKey::from_submit(&submit)));
    }

    #[test]
    fn live_mode_rejects_missing_persistent_database() {
        let error = require_persistent_database(None, Some("live"), None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("persistent PostgreSQL is required")
        );
        assert!(
            require_persistent_database(None, Some("static"), None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn live_mode_rejects_disabled_candidate_submission() {
        let error = require_candidate_submission(false, Some("live")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("candidate block submission is required")
        );
        assert!(!require_candidate_submission(false, Some("static")).unwrap());
        assert!(require_candidate_submission(true, Some("live")).unwrap());
    }

    #[test]
    fn live_candidate_submission_requires_template_node_affinity() {
        assert!(
            require_template_submit_affinity("http://node-a:8789", "http://node-a:8789/").is_ok()
        );
        let error = require_template_submit_affinity("http://node-a:8789", "http://node-b:8789")
            .unwrap_err();
        assert!(error.to_string().contains("jobs are node-local"));
    }

    #[test]
    fn abuse_manager_limits_active_connections_per_ip() {
        let manager = Arc::new(AbuseManager::new(AbuseConfig {
            max_connections_per_ip: 1,
            max_sessions_per_address: 64,
            malformed_frame_limit: 8,
            auth_failure_limit: 5,
            invalid_share_limit: 16,
            ban_secs: 600,
        }));
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));

        let permit = manager.try_open(ip).unwrap();
        assert_eq!(
            manager.try_open(ip).unwrap_err(),
            AbuseReject::TooManyConnections
        );

        drop(permit);
        assert!(manager.try_open(ip).is_ok());
    }

    #[test]
    fn abuse_manager_limits_active_sessions_per_address() {
        let manager = Arc::new(AbuseManager::new(AbuseConfig {
            max_connections_per_ip: 4,
            max_sessions_per_address: 1,
            malformed_frame_limit: 8,
            auth_failure_limit: 5,
            invalid_share_limit: 16,
            ban_secs: 600,
        }));
        let address = "0123456789abcdef0123456789abcdef01234567";

        let permit = manager.try_open_address_session(address).unwrap();
        assert_eq!(
            manager.try_open_address_session(address).unwrap_err(),
            AbuseReject::TooManyAddressSessions
        );

        drop(permit);
        assert!(manager.try_open_address_session(address).is_ok());
    }

    #[test]
    fn abuse_manager_bans_after_malformed_frame_limit() {
        let manager = Arc::new(AbuseManager::new(AbuseConfig {
            max_connections_per_ip: 4,
            max_sessions_per_address: 64,
            malformed_frame_limit: 2,
            auth_failure_limit: 5,
            invalid_share_limit: 16,
            ban_secs: 600,
        }));
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 11));

        assert!(!manager.record_malformed_frame(ip));
        assert!(manager.record_malformed_frame(ip));
        assert_eq!(manager.try_open(ip).unwrap_err(), AbuseReject::Banned);
    }

    #[test]
    fn abuse_manager_bans_auth_and_invalid_share_offenders() {
        let manager = Arc::new(AbuseManager::new(AbuseConfig {
            max_connections_per_ip: 4,
            max_sessions_per_address: 64,
            malformed_frame_limit: 8,
            auth_failure_limit: 2,
            invalid_share_limit: 2,
            ban_secs: 600,
        }));
        let auth_ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 12));
        let share_ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 13));

        assert!(!manager.record_auth_failure(auth_ip));
        assert!(manager.record_auth_failure(auth_ip));
        assert!(manager.is_banned(auth_ip));

        assert!(!manager.record_invalid_share(share_ip));
        assert!(manager.record_invalid_share(share_ip));
        assert!(manager.is_banned(share_ip));
    }

    #[test]
    fn vardiff_ewma_and_hysteresis_prevent_jitter() {
        let mut vardiff = VardiffState::new(VardiffConfig {
            initial_difficulty: 8.0,
            min_difficulty: 8.0,
            max_difficulty: 64.0,
            target_share_secs: 20,
            ..VardiffConfig::default()
        });
        let start = Instant::now();

        assert_eq!(vardiff.record_accepted_share(start), None);
        for seconds in (15..=150).step_by(15) {
            assert_eq!(
                vardiff.record_accepted_share(start + Duration::from_secs(seconds)),
                None
            );
        }
        assert_eq!(vardiff.current_difficulty(), 8.0);
    }

    #[test]
    fn vardiff_rate_limits_and_caps_fast_adjustments() {
        let mut vardiff = VardiffState::new(VardiffConfig {
            initial_difficulty: 8.0,
            min_difficulty: 8.0,
            max_difficulty: 64.0,
            target_share_secs: 20,
            ..VardiffConfig::default()
        });
        let start = Instant::now();

        assert_eq!(vardiff.record_accepted_share(start), None);
        for seconds in (5..120).step_by(5) {
            assert_eq!(
                vardiff.record_accepted_share(start + Duration::from_secs(seconds)),
                None
            );
        }
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(120)),
            Some(16.0)
        );
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(125)),
            None
        );
        assert_eq!(vardiff.current_difficulty(), 16.0);
    }

    #[test]
    fn vardiff_rate_limits_and_caps_slow_adjustments() {
        let mut vardiff = VardiffState::new(VardiffConfig {
            initial_difficulty: 32.0,
            min_difficulty: 8.0,
            max_difficulty: 64.0,
            target_share_secs: 20,
            ..VardiffConfig::default()
        });
        let start = Instant::now();

        assert_eq!(vardiff.record_accepted_share(start), None);
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(50)),
            None
        );
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(100)),
            None
        );
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(150)),
            Some(16.0)
        );
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(300)),
            Some(8.0)
        );
    }

    #[test]
    fn vardiff_clamps_suggestions_and_grants_old_difficulty_grace() {
        let mut vardiff = VardiffState::new(VardiffConfig {
            initial_difficulty: 1.0,
            min_difficulty: 8.0,
            max_difficulty: 16.0,
            target_share_secs: 20,
            ..VardiffConfig::default()
        });
        let start = Instant::now();

        assert_eq!(vardiff.current_difficulty(), 8.0);
        assert_eq!(
            vardiff.apply_suggested_difficulty(1_000.0, start),
            Some(16.0)
        );
        assert_eq!(vardiff.previous_difficulty_in_grace(start), Some(8.0));
        assert_eq!(vardiff.apply_suggested_difficulty(f64::NAN, start), None);
        assert_eq!(vardiff.current_difficulty(), 16.0);
        assert_eq!(
            vardiff.previous_difficulty_in_grace(start + Duration::from_secs(121)),
            None
        );
    }

    #[test]
    fn vardiff_quantizes_fractional_difficulty_for_miner_compatibility() {
        let mut vardiff = VardiffState::new(VardiffConfig {
            initial_difficulty: 8.4,
            min_difficulty: 8.0,
            max_difficulty: 64.0,
            target_share_secs: 20,
            ..VardiffConfig::default()
        });
        let start = Instant::now();

        assert_eq!(vardiff.current_difficulty(), 8.0);
        assert_eq!(
            vardiff.apply_suggested_difficulty(9.444_256_455_205_316, start),
            Some(9.0)
        );
        assert_eq!(vardiff.current_difficulty(), 9.0);
        assert_eq!(vardiff.previous_difficulty_in_grace(start), Some(8.0));
    }

    #[test]
    fn vardiff_rate_limits_repeated_suggestions() {
        let mut vardiff = VardiffState::new(VardiffConfig {
            initial_difficulty: 8.0,
            min_difficulty: 8.0,
            max_difficulty: 64.0,
            target_share_secs: 20,
            ..VardiffConfig::default()
        });
        let start = Instant::now();

        assert_eq!(vardiff.apply_suggested_difficulty(64.0, start), Some(16.0));
        assert_eq!(
            vardiff.apply_suggested_difficulty(64.0, start + Duration::from_secs(1)),
            None
        );
        assert_eq!(vardiff.current_difficulty(), 16.0);
        assert_eq!(
            vardiff.apply_suggested_difficulty(64.0, start + Duration::from_secs(120)),
            Some(32.0)
        );
    }

    #[test]
    fn parses_numeric_and_string_suggested_difficulty() {
        assert_eq!(
            parse_suggested_difficulty(&serde_json::json!([16])),
            Some(16.0)
        );
        assert_eq!(
            parse_suggested_difficulty(&serde_json::json!(["8.5"])),
            Some(8.5)
        );
        assert_eq!(parse_suggested_difficulty(&serde_json::json!([0])), None);
    }

    #[test]
    fn vardiff_transition_accepts_old_difficulty_only_during_grace() {
        let job = csd_pool_node::easy_static_job("grace-job");
        let extranonce1_le = [1, 0, 0, 0];
        let mut submit = SubmitParams {
            worker_name: "0123456789abcdef0123456789abcdef01234567.rig-a".to_owned(),
            job_id: job.template.job_id.clone(),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: job.notify.ntime_hex.clone(),
            nonce_hex: "00000000".to_owned(),
        };
        let mut found = false;
        for nonce in 0..100_000_u32 {
            submit.nonce_hex = format!("{nonce:08x}");
            if verify_submit(&job.template, extranonce1_le, &submit, 8.0).is_ok()
                && verify_submit(&job.template, extranonce1_le, &submit, 16.0).is_err()
            {
                found = true;
                break;
            }
        }
        assert!(found, "expected a share between difficulty 8 and 16");

        let start = Instant::now();
        let mut vardiff = VardiffState::new(VardiffConfig {
            initial_difficulty: 8.0,
            min_difficulty: 8.0,
            max_difficulty: 64.0,
            target_share_secs: 20,
            ..VardiffConfig::default()
        });
        assert_eq!(vardiff.apply_suggested_difficulty(16.0, start), Some(16.0));
        let (_, accepted_difficulty) = verify_submit_for_vardiff(
            &job.template,
            extranonce1_le,
            &submit,
            &vardiff,
            start + Duration::from_secs(15),
        )
        .unwrap();
        assert_eq!(accepted_difficulty, 8.0);
        assert!(
            verify_submit_for_vardiff(
                &job.template,
                extranonce1_le,
                &submit,
                &vardiff,
                start + Duration::from_secs(121),
            )
            .is_err()
        );
    }
}
