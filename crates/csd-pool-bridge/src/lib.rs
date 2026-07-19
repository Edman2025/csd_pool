#![allow(clippy::collapsible_if)] // Production remains on Rust 1.86, before stable let chains.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csd_pool_consensus::{
    ConsensusError, SubmitSolution, VerifiedShare, WorkTemplate, coinbase_bytes,
    compose_extranonce, difficulty_for_target, parse_le_u32_hex_bytes, parse_u32_hex,
    verify_share_with_difficulty,
};
use csd_pool_db::{JobRecord, MiningRepository, PgRepository, ShareEventRecord, ShareRecord};
use csd_pool_node::{
    BlockCandidateSubmitRequest, CsdNodeClient, LiveTemplateProvider, PoolJob,
    StaticTemplateProvider, SubmitBlockResponse, TemplateProvider,
};
use csd_pool_protocol::{
    Request, Response, SubmitParams, notify, response_error, response_ok, serialize_line,
    set_difficulty,
};
use csd_pool_state::SharedPoolState;
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, info, warn};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

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
}

pub type Result<T> = std::result::Result<T, BridgeError>;

pub async fn run_stratum_server(listen: &str, pool_state: SharedPoolState) -> Result<()> {
    let provider = template_provider_from_env()?;
    let block_submitter = block_submitter_from_env()?;
    let repository = mining_repository_from_env().await?;
    let abuse = Arc::new(AbuseManager::new(abuse_config_from_env()?));
    let vardiff = vardiff_config_from_env()?;
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
    let jobs = SharedJobWatch::start(provider, repository.clone()).await?;
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
}

impl SharedJobWatch {
    async fn start(
        provider: Arc<dyn TemplateProvider>,
        repository: Option<Arc<dyn MiningRepository>>,
    ) -> Result<Self> {
        let requested_refresh_secs = env_u64("CSD_POOL_TEMPLATE_REFRESH_SECS", 2);
        let template_mode = std::env::var("CSD_POOL_TEMPLATE_MODE").unwrap_or_default();
        let refresh_secs =
            bounded_template_refresh_secs(requested_refresh_secs, template_mode.as_str());
        if refresh_secs != requested_refresh_secs.max(1) {
            warn!(
                requested_refresh_secs,
                refresh_secs, "limiting live template tip polling interval"
            );
        }
        info!(
            refresh_secs,
            template_mode = template_mode.as_str(),
            "configured mining template refresh"
        );
        Self::start_with_refresh(provider, repository, Duration::from_secs(refresh_secs)).await
    }

    async fn start_with_refresh(
        provider: Arc<dyn TemplateProvider>,
        repository: Option<Arc<dyn MiningRepository>>,
        refresh: Duration,
    ) -> Result<Self> {
        let initial = Arc::new(provider.current_job().await?);
        if let Some(repository) = repository.as_deref() {
            repository
                .upsert_job(&job_record_from_pool_job(&initial))
                .await?;
        }
        let (sender, _) = watch::channel(initial);
        let refresh_sender = sender.clone();
        tokio::spawn(async move {
            let mut ticker = interval(refresh);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let current = refresh_sender.borrow().clone();
                let observed_tip = match provider.current_tip().await {
                    Ok(Some(tip)) if tip == current.template.prev => continue,
                    Ok(tip) => tip,
                    Err(err) => {
                        warn!(%err, "mining tip refresh failed; retaining current job");
                        continue;
                    }
                };
                let refresh_started = Instant::now();
                let next = match provider.current_job().await {
                    Ok(job) => Arc::new(job),
                    Err(err) => {
                        warn!(%err, "mining template refresh failed; retaining current job");
                        continue;
                    }
                };
                let latest_tip = match provider.current_tip().await {
                    Ok(tip) => tip.or(observed_tip),
                    Err(err) => {
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
                if next.template.job_id == refresh_sender.borrow().template.job_id {
                    continue;
                }
                if let Some(repository) = repository.as_deref() {
                    if let Err(err) = repository
                        .upsert_job(&job_record_from_pool_job(&next))
                        .await
                    {
                        warn!(%err, job_id = next.template.job_id, "refusing unpersisted mining job");
                        continue;
                    }
                }
                info!(
                    job_id = next.template.job_id,
                    refresh_ms = refresh_started.elapsed().as_millis(),
                    "broadcasting refreshed mining job"
                );
                refresh_sender.send_replace(next);
            }
        });
        Ok(Self { sender })
    }

    fn subscribe(&self) -> watch::Receiver<Arc<PoolJob>> {
        self.sender.subscribe()
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
    Ok(Arc::new(LiveTemplateProvider::new(
        CsdNodeClient::from_env(node_url),
        pool_address,
    )))
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
        initial_difficulty: env_f64("CSD_POOL_INITIAL_DIFFICULTY", 16.0),
        min_difficulty: env_f64("CSD_POOL_MIN_DIFFICULTY", 8.0),
        max_difficulty: env_f64("CSD_POOL_MAX_DIFFICULTY", 512.0),
        target_share_secs: env_u64("CSD_POOL_TARGET_SHARE_SECS", 20),
        retarget_secs: env_u64("CSD_POOL_VARDIFF_RETARGET_SECS", 120),
        ewma_alpha: env_f64("CSD_POOL_VARDIFF_EWMA_ALPHA", 0.25),
        fast_share_ratio: env_f64("CSD_POOL_VARDIFF_FAST_SHARE_RATIO", 0.75),
        slow_share_ratio: env_f64("CSD_POOL_VARDIFF_SLOW_SHARE_RATIO", 1.5),
        max_adjustment_factor: env_f64("CSD_POOL_VARDIFF_MAX_ADJUSTMENT_FACTOR", 1.5),
        transition_grace_secs: env_u64("CSD_POOL_VARDIFF_TRANSITION_GRACE_SECS", 15),
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
    Ok(Some(Arc::new(CsdNodeBlockSubmitter::new(
        CsdNodeClient::from_env(node_url),
    ))))
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

#[allow(clippy::too_many_arguments)]
async fn handle_client(
    stream: TcpStream,
    peer: SocketAddr,
    pool_state: SharedPoolState,
    mut job_rx: watch::Receiver<Arc<PoolJob>>,
    repository: Option<Arc<dyn MiningRepository>>,
    block_submitter: Option<Arc<dyn BlockSubmitter>>,
    abuse: Arc<AbuseManager>,
    vardiff_config: VardiffConfig,
    _permit: ConnectionPermit,
) -> Result<()> {
    let _connection_guard = pool_state.connection_guard();
    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let extranonce1_le = (session_id as u32).to_le_bytes();
    // Stratum carries extranonce1 as the raw four-byte value encoded as hex.
    // The official CSD v0.2.3 miner decodes those bytes and interprets them as
    // a little-endian u32, so the wire value must be the LE byte encoding (for
    // session 1 this is "01000000", not the integer formatting "00000001").
    let extranonce1 = hex::encode(extranonce1_le);
    let mut authorized_worker: Option<String> = None;
    let mut _address_permit: Option<AddressSessionPermit> = None;
    let mut seen_shares = HashSet::new();
    let mut vardiff = VardiffState::new(vardiff_config);

    info!(%peer, session_id, "client connected");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    loop {
        line.clear();
        let n = if authorized_worker.is_some() {
            tokio::select! {
                result = reader.read_line(&mut line) => result?,
                changed = job_rx.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    let current_job = job_rx.borrow_and_update().clone();
                    write_half
                        .write_all(serialize_line(&notify(&current_job.notify))?.as_bytes())
                        .await?;
                    continue;
                }
            }
        } else {
            reader.read_line(&mut line).await?
        };
        if n == 0 {
            info!(%peer, session_id, "client disconnected");
            return Ok(());
        }

        let request: Request = match serde_json::from_str(line.trim()) {
            Ok(req) => req,
            Err(err) => {
                warn!(%peer, session_id, %err, "malformed json frame");
                if abuse.record_malformed_frame(peer.ip()) {
                    warn!(%peer, session_id, "closing session after malformed frame ban");
                    return Ok(());
                }
                continue;
            }
        };

        debug!(%peer, session_id, method = request.method, "request");
        let mut pending_difficulty: Option<f64> = None;
        let response = match request.method.as_str() {
            "mining.subscribe" => subscribe_response(request.id, &extranonce1),
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
                                    authorized_worker = Some(worker);
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
                if authorized_worker.is_none() {
                    response_error(request.id.unwrap_or(0), 24, "unauthorized")
                } else {
                    match parse_suggested_difficulty(&request.params) {
                        Some(suggested_difficulty) => {
                            pending_difficulty = vardiff
                                .apply_suggested_difficulty(suggested_difficulty, Instant::now());
                            response_ok(request.id.unwrap_or(0))
                        }
                        None => response_error(request.id.unwrap_or(0), 20, "invalid difficulty"),
                    }
                }
            }
            "mining.submit" => {
                if authorized_worker.is_none() {
                    response_error(request.id.unwrap_or(0), 24, "unauthorized")
                } else {
                    let worker_address = authorized_worker.as_deref().unwrap_or_default();
                    match SubmitParams::parse(&request.params) {
                        Ok(submit) => {
                            let current_job = job_rx.borrow().clone();
                            if !authorized_submit_worker(worker_address, &submit.worker_name) {
                                pool_state.record_share_rejected(worker_address);
                                abuse.record_invalid_share(peer.ip());
                                persist_share_event(
                                    repository.as_deref(),
                                    &share_event_from_submit(
                                        worker_address,
                                        &submit,
                                        "rejected",
                                        "unauthorized_worker",
                                    ),
                                )
                                .await?;
                                response_error(request.id.unwrap_or(0), 20, "invalid worker")
                            } else if submit.job_id != current_job.template.job_id {
                                pool_state.record_share_stale(worker_address);
                                persist_share_event(
                                    repository.as_deref(),
                                    &share_event_from_submit(
                                        worker_address,
                                        &submit,
                                        "stale",
                                        "unknown_job",
                                    ),
                                )
                                .await?;
                                response_error(request.id.unwrap_or(0), 21, "unknown job")
                            } else if !seen_shares.insert(ShareKey::from_submit(&submit)) {
                                pool_state.record_share_rejected(worker_address);
                                persist_share_event(
                                    repository.as_deref(),
                                    &share_event_from_submit(
                                        worker_address,
                                        &submit,
                                        "rejected",
                                        "duplicate_share",
                                    ),
                                )
                                .await?;
                                response_error(request.id.unwrap_or(0), 22, "duplicate share")
                            } else {
                                let validation_started = Instant::now();
                                let validation_result = verify_submit_with_vardiff(
                                    &current_job.template,
                                    extranonce1_le,
                                    &submit,
                                    &vardiff,
                                    validation_started,
                                );
                                pool_state.record_share_validation(validation_started.elapsed());
                                match validation_result {
                                    Ok((share, accepted_difficulty)) => {
                                        let persisted = persist_accepted_share(
                                            repository.as_deref(),
                                            &current_job,
                                            worker_address,
                                            &submit,
                                            &share,
                                            accepted_difficulty,
                                        )
                                        .await?;
                                        if persisted {
                                            if share.is_block_candidate {
                                                submit_block_candidate(
                                                    block_submitter.as_deref(),
                                                    repository.as_deref(),
                                                    &current_job,
                                                    &pool_state,
                                                    worker_address,
                                                    &submit,
                                                    &share,
                                                    extranonce1_le,
                                                    accepted_difficulty,
                                                )
                                                .await?;
                                            }
                                            pool_state.record_share_accepted(
                                                worker_address,
                                                accepted_difficulty,
                                                share.is_block_candidate,
                                            );
                                            pending_difficulty =
                                                vardiff.record_accepted_share(validation_started);
                                            info!(
                                                %peer,
                                                session_id,
                                                worker = submit.worker_name,
                                                job_id = submit.job_id,
                                                hash = hex::encode(share.hash),
                                                candidate = share.is_block_candidate,
                                                "share accepted"
                                            );
                                            response_ok(request.id.unwrap_or(0))
                                        } else {
                                            pool_state.record_share_rejected(worker_address);
                                            persist_share_event(
                                                repository.as_deref(),
                                                &share_event_from_submit(
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
                                                worker_address,
                                                &submit,
                                                "rejected",
                                                "low_difficulty",
                                            ),
                                        )
                                        .await?;
                                        debug!(%peer, session_id, %err, "low difficulty share rejected");
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

        write_half
            .write_all(serialize_line(&response)?.as_bytes())
            .await?;

        if abuse.is_banned(peer.ip()) {
            warn!(%peer, session_id, "closing banned stratum session");
            return Ok(());
        }

        if request.method == "mining.authorize" && authorized_worker.is_some() {
            write_half
                .write_all(
                    serialize_line(&set_difficulty(vardiff.current_difficulty()))?.as_bytes(),
                )
                .await?;
            let current_job = job_rx.borrow_and_update().clone();
            write_half
                .write_all(serialize_line(&notify(&current_job.notify))?.as_bytes())
                .await?;
        } else if let Some(difficulty) = pending_difficulty {
            write_half
                .write_all(serialize_line(&set_difficulty(difficulty))?.as_bytes())
                .await?;
        }
    }
}

#[async_trait::async_trait]
pub trait BlockSubmitter: Send + Sync {
    async fn submit_candidate(
        &self,
        candidate: &BlockCandidateSubmitRequest,
    ) -> Result<SubmitBlockResponse>;
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
        candidate: &BlockCandidateSubmitRequest,
    ) -> Result<SubmitBlockResponse> {
        Ok(self.node.submit_candidate(candidate).await?)
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
    let request = block_candidate_submit_request(job, miner, submit, share, extranonce1_le);
    let response = match submitter.submit_candidate(&request).await {
        Ok(response) => response,
        Err(err) => {
            // Keep the solved header auditable and queryable by the block
            // reconciler even when the node call fails before returning HTTP.
            if let Some(repository) = repository {
                let failed_response = SubmitBlockResponse {
                    ok: false,
                    hash: Some(request.hash_hex.clone()),
                    extra: serde_json::json!({
                        "transport_error": err.to_string(),
                        "retryable": true,
                    }),
                };
                repository
                    .record_block_candidate(&block_candidate_record(
                        miner,
                        &request,
                        &failed_response,
                        effort_pct,
                    ))
                    .await?;
            }
            warn!(
                job_id = request.job_id,
                hash = request.hash_hex,
                %err,
                "block candidate submission transport failed; keeping miner session active"
            );
            return Ok(());
        }
    };
    if let Some(repository) = repository {
        repository
            .record_block_candidate(&block_candidate_record(
                miner, &request, &response, effort_pct,
            ))
            .await?;
    }
    if response.ok {
        info!(
            job_id = request.job_id,
            hash = request.hash_hex,
            effort_pct,
            "block candidate submitted"
        );
    } else {
        warn!(
            job_id = request.job_id,
            hash = request.hash_hex,
            effort_pct,
            response = %serde_json::to_string(&response).unwrap_or_default(),
            "block candidate rejected; keeping miner session active"
        );
    }
    Ok(())
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
    request: &BlockCandidateSubmitRequest,
    response: &SubmitBlockResponse,
    effort_pct: f64,
) -> csd_pool_db::BlockCandidateRecord {
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
        candidate_payload_json: serde_json::to_value(request).unwrap_or_else(|_| {
            serde_json::json!({
                "job_id": request.job_id,
                "hash_hex": request.hash_hex,
            })
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
) -> Result<bool> {
    let Some(repository) = repository else {
        return Ok(true);
    };

    repository
        .upsert_job(&job_record_from_pool_job(job))
        .await?;
    repository
        .insert_share(&share_record_from_submit(miner, submit, share, difficulty))
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
    miner: &str,
    submit: &SubmitParams,
    kind: &str,
    reason: &str,
) -> ShareEventRecord {
    ShareEventRecord {
        miner: miner.to_owned(),
        worker_name: worker_name_from_submit(miner, &submit.worker_name),
        job_id: Some(submit.job_id.clone()),
        kind: kind.to_owned(),
        reason: reason.to_owned(),
    }
}

fn job_record_from_pool_job(job: &PoolJob) -> JobRecord {
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
    }
}

fn share_record_from_submit(
    miner: &str,
    submit: &SubmitParams,
    share: &VerifiedShare,
    difficulty: f64,
) -> ShareRecord {
    ShareRecord {
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

fn parse_suggested_difficulty(params: &Value) -> Option<f64> {
    let values = params.as_array()?;
    if values.len() != 1 {
        return None;
    }
    let difficulty = values.first()?.as_f64()?;
    (difficulty.is_finite() && difficulty > 0.0).then_some(difficulty)
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

fn verify_submit_with_vardiff(
    template: &WorkTemplate,
    extranonce1_le: [u8; 4],
    submit: &SubmitParams,
    vardiff: &VardiffState,
    now: Instant,
) -> std::result::Result<(VerifiedShare, f64), ConsensusError> {
    let current_difficulty = vardiff.current_difficulty();
    match verify_submit(template, extranonce1_le, submit, current_difficulty) {
        Ok(share) => Ok((share, current_difficulty)),
        Err(ConsensusError::LowDifficultyShare) => {
            let Some(previous_difficulty) = vardiff.previous_difficulty(now) else {
                return Err(ConsensusError::LowDifficultyShare);
            };
            verify_submit(template, extranonce1_le, submit, previous_difficulty)
                .map(|share| (share, previous_difficulty))
        }
        Err(error) => Err(error),
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
    pub retarget_secs: u64,
    pub ewma_alpha: f64,
    pub fast_share_ratio: f64,
    pub slow_share_ratio: f64,
    pub max_adjustment_factor: f64,
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
            retarget_secs: value.vardiff_retarget_secs,
            ewma_alpha: value.vardiff_ewma_alpha,
            fast_share_ratio: value.vardiff_fast_share_ratio,
            slow_share_ratio: value.vardiff_slow_share_ratio,
            max_adjustment_factor: value.vardiff_max_adjustment_factor,
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
        self.min_difficulty = self.min_difficulty.ceil();
        if !self.max_difficulty.is_finite() || self.max_difficulty < self.min_difficulty {
            self.max_difficulty = self.min_difficulty;
        }
        self.max_difficulty = self.max_difficulty.floor().max(self.min_difficulty);
        if !self.initial_difficulty.is_finite() || self.initial_difficulty <= 0.0 {
            self.initial_difficulty = self.min_difficulty;
        }
        self.initial_difficulty = self
            .initial_difficulty
            .ceil()
            .clamp(self.min_difficulty, self.max_difficulty);
        if self.target_share_secs == 0 {
            self.target_share_secs = 20;
        }
        self.retarget_secs = self.retarget_secs.max(120);
        if !self.ewma_alpha.is_finite() || self.ewma_alpha <= 0.0 || self.ewma_alpha > 1.0 {
            self.ewma_alpha = 0.25;
        }
        if !self.fast_share_ratio.is_finite()
            || self.fast_share_ratio <= 0.0
            || self.fast_share_ratio >= 1.0
        {
            self.fast_share_ratio = 0.75;
        }
        if !self.slow_share_ratio.is_finite() || self.slow_share_ratio <= 1.0 {
            self.slow_share_ratio = 1.5;
        }
        if !self.max_adjustment_factor.is_finite() || self.max_adjustment_factor <= 1.0 {
            self.max_adjustment_factor = 1.5;
        }
        self.transition_grace_secs = self.transition_grace_secs.clamp(1, 60);
        self
    }
}

#[derive(Clone, Copy, Debug)]
struct PreviousDifficulty {
    difficulty: f64,
    expires_at: Instant,
}

#[derive(Debug)]
pub struct VardiffState {
    config: VardiffConfig,
    current_difficulty: f64,
    last_share_at: Option<Instant>,
    last_retarget_at: Option<Instant>,
    ewma_share_secs: f64,
    previous_difficulty: Option<PreviousDifficulty>,
    suggestion_received: bool,
}

impl VardiffState {
    pub fn new(config: VardiffConfig) -> Self {
        let config = config.normalized();
        let current_difficulty = config.initial_difficulty;
        let ewma_share_secs = config.target_share_secs as f64;
        Self {
            current_difficulty,
            config,
            last_share_at: None,
            last_retarget_at: None,
            ewma_share_secs,
            previous_difficulty: None,
            suggestion_received: false,
        }
    }

    pub fn current_difficulty(&self) -> f64 {
        self.current_difficulty
    }

    pub fn previous_difficulty(&self, now: Instant) -> Option<f64> {
        self.previous_difficulty
            .filter(|previous| previous.expires_at >= now)
            .map(|previous| previous.difficulty)
    }

    pub fn apply_suggested_difficulty(
        &mut self,
        suggested_difficulty: f64,
        now: Instant,
    ) -> Option<f64> {
        if self.suggestion_received
            || self.last_share_at.is_some()
            || !suggested_difficulty.is_finite()
            || suggested_difficulty <= 0.0
        {
            return None;
        }
        self.suggestion_received = true;

        let suggested_difficulty = suggested_difficulty
            .ceil()
            .clamp(self.config.min_difficulty, self.config.max_difficulty);
        let changed = self.transition_to(suggested_difficulty, now);
        if changed.is_some() {
            self.last_retarget_at = Some(now);
            self.ewma_share_secs = self.config.target_share_secs as f64;
        }
        changed
    }

    pub fn record_accepted_share(&mut self, now: Instant) -> Option<f64> {
        let Some(last_share_at) = self.last_share_at.replace(now) else {
            self.last_retarget_at.get_or_insert(now);
            return None;
        };
        let elapsed_secs = now
            .checked_duration_since(last_share_at)
            .unwrap_or_default()
            .as_secs_f64();
        let target = self.config.target_share_secs as f64;
        let bounded_sample = elapsed_secs.clamp(target * 0.25, target * 4.0);
        self.ewma_share_secs = self.config.ewma_alpha * bounded_sample
            + (1.0 - self.config.ewma_alpha) * self.ewma_share_secs;

        let last_retarget_at = self.last_retarget_at.get_or_insert(now);
        if now
            .checked_duration_since(*last_retarget_at)
            .unwrap_or_default()
            < Duration::from_secs(self.config.retarget_secs)
        {
            return None;
        }

        let too_fast = self.ewma_share_secs < target * self.config.fast_share_ratio;
        let too_slow = self.ewma_share_secs > target * self.config.slow_share_ratio;
        if !too_fast && !too_slow {
            return None;
        }

        let desired = self.current_difficulty * target / self.ewma_share_secs;
        let next = if desired > self.current_difficulty {
            desired
                .min(self.current_difficulty * self.config.max_adjustment_factor)
                .floor()
        } else {
            desired
                .max(self.current_difficulty / self.config.max_adjustment_factor)
                .ceil()
        }
        .clamp(self.config.min_difficulty, self.config.max_difficulty);

        let changed = self.transition_to(next, now);
        if changed.is_some() {
            self.last_retarget_at = Some(now);
            self.ewma_share_secs = target;
        }
        changed
    }

    fn transition_to(&mut self, next: f64, now: Instant) -> Option<f64> {
        if (next - self.current_difficulty).abs() < f64::EPSILON {
            return None;
        }
        if next > self.current_difficulty {
            self.previous_difficulty = Some(PreviousDifficulty {
                difficulty: self.current_difficulty,
                expires_at: now + Duration::from_secs(self.config.transition_grace_secs),
            });
        } else {
            self.previous_difficulty = None;
        }
        self.current_difficulty = next;
        Some(next)
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

    struct FailingBlockSubmitter;

    #[test]
    fn extranonce1_is_serialized_in_verification_byte_order() {
        assert_eq!(extranonce1_for_session(0x01020304), "04030201");
        assert_eq!(extranonce1_for_session(1), "01000000");
    }

    #[async_trait::async_trait]
    impl BlockSubmitter for FailingBlockSubmitter {
        async fn submit_candidate(
            &self,
            _candidate: &BlockCandidateSubmitRequest,
        ) -> Result<SubmitBlockResponse> {
            Err(BridgeError::InvalidConfig("node unavailable".to_owned()))
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
        let jobs =
            SharedJobWatch::start_with_refresh(provider.clone(), None, Duration::from_millis(10))
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
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
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
    async fn authorized_tcp_client_can_suggest_a_safely_clamped_difficulty() {
        let provider = Arc::new(StaticTemplateProvider::easy_job("static-suggest"));
        let jobs = SharedJobWatch::start(provider, None).await.unwrap();
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
            None,
            None,
            abuse,
            test_vardiff_config(16.0),
            permit,
        ));

        let (read_half, mut write_half) = client.into_split();
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(
                b"{\"id\":1,\"method\":\"mining.authorize\",\"params\":[\"0123456789abcdef0123456789abcdef01234567.rig-a\",\"x\"]}\n",
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

        write_half
            .write_all(b"{\"id\":2,\"method\":\"mining.suggest_difficulty\",\"params\":[1.0]}\n")
            .await
            .unwrap();
        let mut response_line = String::new();
        let mut difficulty_line = String::new();
        tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut response_line))
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            reader.read_line(&mut difficulty_line),
        )
        .await
        .unwrap()
        .unwrap();

        let response: Value = serde_json::from_str(response_line.trim()).unwrap();
        let difficulty: Value = serde_json::from_str(difficulty_line.trim()).unwrap();
        assert_eq!(response["id"], 2);
        assert_eq!(response["result"], true);
        assert_eq!(difficulty["method"], "mining.set_difficulty");
        assert_eq!(difficulty["params"][0], 8.0);
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

        let result = submit_block_candidate(
            Some(&submitter),
            Some(&repository),
            &job,
            &SharedPoolState::new(),
            "0123456789abcdef0123456789abcdef01234567",
            &submit,
            &share,
            [1, 0, 0, 0],
            1.0,
        )
        .await;

        assert!(result.is_ok());
        let candidates = repository.list_block_candidates().unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].hash_hex, hex::encode(share.hash));
        assert_eq!(candidates[0].submit_response_json["retryable"], true);
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
    fn parses_only_one_finite_positive_suggested_difficulty() {
        assert_eq!(
            parse_suggested_difficulty(&serde_json::json!([14.25])),
            Some(14.25)
        );
        assert_eq!(parse_suggested_difficulty(&serde_json::json!([0.0])), None);
        assert_eq!(parse_suggested_difficulty(&serde_json::json!([-1.0])), None);
        assert_eq!(
            parse_suggested_difficulty(&serde_json::json!([14.0, 16.0])),
            None
        );
        assert_eq!(parse_suggested_difficulty(&serde_json::json!(["14"])), None);
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
        let record = job_record_from_pool_job(&job);
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
        let record = block_candidate_record(
            "0123456789abcdef0123456789abcdef01234567",
            &request,
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
        let event = share_event_from_submit(miner, &submit, "rejected", "low_difficulty");
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

    fn test_vardiff_config(initial_difficulty: f64) -> VardiffConfig {
        VardiffConfig {
            initial_difficulty,
            min_difficulty: 8.0,
            max_difficulty: 64.0,
            target_share_secs: 20,
            retarget_secs: 120,
            ewma_alpha: 0.25,
            fast_share_ratio: 0.75,
            slow_share_ratio: 1.5,
            max_adjustment_factor: 1.5,
            transition_grace_secs: 15,
        }
    }

    #[test]
    fn vardiff_hysteresis_does_not_chase_alternating_share_jitter() {
        let mut vardiff = VardiffState::new(test_vardiff_config(16.0));
        let start = Instant::now();
        let mut elapsed = 0;

        assert_eq!(vardiff.record_accepted_share(start), None);
        for interval_secs in [14_u64, 26].into_iter().cycle().take(20) {
            elapsed += interval_secs;
            assert_eq!(
                vardiff.record_accepted_share(start + Duration::from_secs(elapsed)),
                None
            );
        }
        assert_eq!(vardiff.current_difficulty(), 16.0);
    }

    #[test]
    fn vardiff_fast_shares_wait_for_retarget_window_and_are_step_limited() {
        let mut vardiff = VardiffState::new(test_vardiff_config(8.0));
        let start = Instant::now();

        assert_eq!(vardiff.record_accepted_share(start), None);
        for sample in 1..24 {
            assert_eq!(
                vardiff.record_accepted_share(start + Duration::from_secs(sample * 5)),
                None
            );
        }
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(120)),
            Some(12.0)
        );
        assert_eq!(vardiff.current_difficulty(), 12.0);

        for sample in 25..48 {
            assert_eq!(
                vardiff.record_accepted_share(start + Duration::from_secs(sample * 5)),
                None
            );
        }
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(240)),
            Some(18.0)
        );
    }

    #[test]
    fn vardiff_slow_shares_are_smoothed_and_step_limited() {
        let mut vardiff = VardiffState::new(test_vardiff_config(32.0));
        let start = Instant::now();

        assert_eq!(vardiff.record_accepted_share(start), None);
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(45)),
            None
        );
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(90)),
            None
        );
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(135)),
            Some(22.0)
        );
        assert_eq!(vardiff.current_difficulty(), 22.0);
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(180)),
            None
        );
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(225)),
            None
        );
        assert_eq!(
            vardiff.record_accepted_share(start + Duration::from_secs(270)),
            Some(15.0)
        );
    }

    #[test]
    fn suggested_difficulty_is_clamped_and_only_applies_before_first_share() {
        let start = Instant::now();
        let mut low = VardiffState::new(test_vardiff_config(16.0));
        assert_eq!(low.apply_suggested_difficulty(1.0, start), Some(8.0));
        assert_eq!(low.current_difficulty(), 8.0);
        assert_eq!(low.apply_suggested_difficulty(32.0, start), None);
        assert_eq!(low.apply_suggested_difficulty(f64::INFINITY, start), None);

        let mut invalid = VardiffState::new(test_vardiff_config(16.0));
        assert_eq!(
            invalid.apply_suggested_difficulty(f64::INFINITY, start),
            None
        );
        assert_eq!(invalid.apply_suggested_difficulty(14.0, start), Some(14.0));

        let mut high = VardiffState::new(test_vardiff_config(16.0));
        assert_eq!(high.apply_suggested_difficulty(10_000.0, start), Some(64.0));
        assert_eq!(high.current_difficulty(), 64.0);
        assert_eq!(high.record_accepted_share(start), None);
        assert_eq!(
            high.apply_suggested_difficulty(8.0, start + Duration::from_secs(1)),
            None
        );
        assert_eq!(high.current_difficulty(), 64.0);
    }

    #[test]
    fn previous_difficulty_is_accepted_only_during_transition_grace() {
        let start = Instant::now();
        let mut vardiff = VardiffState::new(test_vardiff_config(8.0));
        assert_eq!(vardiff.apply_suggested_difficulty(16.0, start), Some(16.0));
        let job = csd_pool_node::easy_static_job("transition-job");
        let mut submit = SubmitParams {
            worker_name: "0123456789abcdef0123456789abcdef01234567.rig-a".to_owned(),
            job_id: job.template.job_id.clone(),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: job.notify.ntime_hex.clone(),
            nonce_hex: String::new(),
        };
        submit.nonce_hex = (0_u32..10_000)
            .map(|nonce| format!("{nonce:08x}"))
            .find(|nonce_hex| {
                let mut candidate = submit.clone();
                candidate.nonce_hex = nonce_hex.clone();
                verify_submit(&job.template, [1, 0, 0, 0], &candidate, 8.0).is_ok()
                    && matches!(
                        verify_submit(&job.template, [1, 0, 0, 0], &candidate, 16.0),
                        Err(ConsensusError::LowDifficultyShare)
                    )
            })
            .expect("find a share valid at difficulty 8 but not 16");

        let (_, accepted_difficulty) = verify_submit_with_vardiff(
            &job.template,
            [1, 0, 0, 0],
            &submit,
            &vardiff,
            start + Duration::from_secs(14),
        )
        .unwrap();
        assert_eq!(accepted_difficulty, 8.0);
        assert!(matches!(
            verify_submit_with_vardiff(
                &job.template,
                [1, 0, 0, 0],
                &submit,
                &vardiff,
                start + Duration::from_secs(16),
            ),
            Err(ConsensusError::LowDifficultyShare)
        ));
    }
}
