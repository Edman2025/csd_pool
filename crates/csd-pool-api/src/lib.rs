#![allow(clippy::collapsible_if)] // Production remains on Rust 1.86, before stable let chains.

use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{Next, from_fn};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, extract::Request};
use csd_pool_accounting::{PayoutRecipient, PayoutSelection, select_payouts};
use csd_pool_db::{
    AlertEvent, ControlRepository, DashboardBlock, DashboardHistorySample, DashboardPayment,
    DashboardPoolStats, DashboardRepository, MonitoringRepository, NodeSampleRecord,
    PayoutAuditEvent, PayoutBatchRecord, PayoutRepository, PgRepository, RecentSession,
    SessionVersionSummary,
};
use csd_pool_node::CsdNodeClient;
use csd_pool_state::{SharedPoolState, TotalsSnapshot, WorkerSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info};

#[derive(Debug, thiserror::Error)]
pub enum ApiStartupError {
    #[error("configuration error: {0}")]
    Config(#[from] csd_pool_config::ConfigError),
    #[error("repository error: {0}")]
    Repository(#[from] csd_pool_db::RepositoryError),
    #[error(
        "persistent PostgreSQL is required in live mode; set CSD_POOL_DATABASE_URL or the [database].url_env variable"
    )]
    MissingDatabase,
}

#[derive(Clone, Debug)]
pub struct ApiSettings {
    pub pool_id: String,
    pub mining_address: String,
    pub pool_fee_pct: f64,
    pub payout_interval_secs: u64,
    pub next_payout_secs: u64,
    pub confirm_depth: u64,
    pub stratum_listen: String,
    pub api_listen: String,
    pub signer_listen: String,
    pub initial_difficulty: f64,
    pub min_difficulty: f64,
    pub max_difficulty: f64,
    pub target_share_secs: u64,
    pub vardiff_ewma_alpha: f64,
    pub vardiff_raise_ratio: f64,
    pub vardiff_lower_ratio: f64,
    pub vardiff_min_adjust_secs: u64,
    pub vardiff_max_adjust_factor: f64,
    pub vardiff_transition_grace_secs: u64,
    pub minimum_payout_base_units: Option<u128>,
    pub manual_payout_approval_base_units: Option<u128>,
    pub max_payout_batch_base_units: Option<u128>,
    pub max_daily_payout_base_units: Option<u128>,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            pool_id: "csd-main".to_owned(),
            mining_address: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            pool_fee_pct: 1.0,
            payout_interval_secs: 1800,
            next_payout_secs: 1800,
            confirm_depth: 10,
            stratum_listen: "127.0.0.1:3333".to_owned(),
            api_listen: "127.0.0.1:8080".to_owned(),
            signer_listen: "127.0.0.1:8890".to_owned(),
            initial_difficulty: 8.0,
            min_difficulty: 8.0,
            max_difficulty: 512.0,
            target_share_secs: 20,
            vardiff_ewma_alpha: 0.25,
            vardiff_raise_ratio: 0.70,
            vardiff_lower_ratio: 1.40,
            vardiff_min_adjust_secs: 120,
            vardiff_max_adjust_factor: 2.0,
            vardiff_transition_grace_secs: 120,
            minimum_payout_base_units: Some(100_000_000),
            manual_payout_approval_base_units: Some(25_000_000_000),
            max_payout_batch_base_units: Some(100_000_000_000),
            max_daily_payout_base_units: Some(500_000_000_000),
        }
    }
}

impl ApiSettings {
    fn from_env() -> Self {
        let Ok(path) = std::env::var("CSD_POOL_CONFIG") else {
            return Self::default();
        };
        let Ok(config) = csd_pool_config::PoolConfig::from_file(path) else {
            return Self::default();
        };
        let minimum_payout_base_units = parse_csd_base_units(&config.pool.minimum_payout_csd).ok();
        let manual_payout_approval_base_units =
            parse_csd_base_units(&config.pool.manual_payout_approval_csd).ok();
        let max_payout_batch_base_units =
            parse_csd_base_units(&config.pool.max_payout_batch_csd).ok();
        let max_daily_payout_base_units =
            parse_csd_base_units(&config.pool.max_daily_payout_csd).ok();
        Self {
            pool_id: config.pool.id,
            mining_address: config.pool.mining_address,
            pool_fee_pct: config.pool.fee_percent,
            payout_interval_secs: config.pool.payout_interval_secs,
            next_payout_secs: config.pool.payout_interval_secs,
            confirm_depth: config.pool.confirm_depth,
            stratum_listen: config.stratum.listen,
            api_listen: config.api.listen,
            signer_listen: config.signer.listen,
            initial_difficulty: config.stratum.initial_difficulty,
            min_difficulty: config.stratum.min_difficulty,
            max_difficulty: config.stratum.max_difficulty,
            target_share_secs: config.stratum.target_share_secs,
            vardiff_ewma_alpha: config.stratum.vardiff_ewma_alpha,
            vardiff_raise_ratio: config.stratum.vardiff_raise_ratio,
            vardiff_lower_ratio: config.stratum.vardiff_lower_ratio,
            vardiff_min_adjust_secs: config.stratum.vardiff_min_adjust_secs,
            vardiff_max_adjust_factor: config.stratum.vardiff_max_adjust_factor,
            vardiff_transition_grace_secs: config.stratum.vardiff_transition_grace_secs,
            minimum_payout_base_units,
            manual_payout_approval_base_units,
            max_payout_batch_base_units,
            max_daily_payout_base_units,
        }
    }
}

pub async fn run_api_server(
    listen: SocketAddr,
    pool_state: SharedPoolState,
) -> std::io::Result<()> {
    run_api_server_with_repository(listen, pool_state, None).await
}

pub async fn run_api_server_with_repository(
    listen: SocketAddr,
    pool_state: SharedPoolState,
    repository: Option<PgRepository>,
) -> std::io::Result<()> {
    let app = router_from_pool_state_and_repository(pool_state, repository);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    info!(%listen, "csd pool api listening");
    axum::serve(listener, app).await
}

pub fn router_from_pool_state(pool_state: SharedPoolState) -> Router {
    router_from_pool_state_and_repository(pool_state, None)
}

pub fn router_from_pool_state_and_repository(
    pool_state: SharedPoolState,
    repository: Option<PgRepository>,
) -> Router {
    router(Arc::new(AppState::from_pool_state(
        pool_state,
        ApiSettings::from_env(),
        repository,
    )))
}

pub fn api_listen() -> String {
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        if let Ok(config) = csd_pool_config::PoolConfig::from_file(path) {
            return config.api.listen;
        }
    }
    std::env::var("CSD_POOL_API_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into())
}

pub fn database_url() -> Option<String> {
    database_url_for_startup().ok().flatten()
}

fn database_url_for_startup() -> Result<Option<String>, ApiStartupError> {
    let env_name = match std::env::var("CSD_POOL_CONFIG") {
        Ok(path) => {
            csd_pool_config::PoolConfig::from_file(path)?
                .database
                .url_env
        }
        Err(_) => "CSD_POOL_DATABASE_URL".to_owned(),
    };
    Ok(std::env::var(env_name)
        .ok()
        .filter(|value| !value.is_empty()))
}

fn require_persistent_database(
    database_url: Option<String>,
    template_mode: Option<&str>,
    explicit_requirement: Option<&str>,
) -> Result<Option<String>, ApiStartupError> {
    if database_url.is_none()
        && csd_pool_config::persistent_database_required(template_mode, explicit_requirement)
    {
        return Err(ApiStartupError::MissingDatabase);
    }
    Ok(database_url)
}

pub async fn repository_from_env() -> Result<Option<PgRepository>, ApiStartupError> {
    let database_url = require_persistent_database(
        database_url_for_startup()?,
        std::env::var("CSD_POOL_TEMPLATE_MODE").ok().as_deref(),
        std::env::var("CSD_POOL_REQUIRE_DATABASE").ok().as_deref(),
    )?;
    let Some(url) = database_url else {
        return Ok(None);
    };
    let repo = PgRepository::connect(&url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    Ok(Some(repo))
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/getting-started", get(getting_started_page))
        .route("/health", get(health))
        .route("/status", get(status_page))
        .route("/metrics", get(prometheus_metrics))
        .route("/api/getting-started", get(getting_started))
        .route("/api/status", get(status))
        .route("/api/pool", get(pool))
        .route("/api/metrics", get(metrics))
        .route("/api/history", get(history))
        .route("/api/miner/{address}", get(miner))
        .route("/api/miner/{address}/workers", get(miner_workers))
        .route("/api/blocks", get(blocks))
        .route("/api/payments", get(payments))
        .route("/api/operator/payouts/status", get(operator_payout_status))
        .route("/api/operator/health", get(operator_health))
        .route("/api/operator/sessions", get(operator_sessions))
        .route("/api/operator/alerts", get(operator_alerts))
        .route(
            "/api/operator/alerts/{fingerprint}/resolve",
            post(operator_resolve_alert),
        )
        .route(
            "/api/operator/payouts/export.csv",
            get(operator_payout_export),
        )
        .route(
            "/api/operator/payouts/audit/export.csv",
            get(operator_payout_audit_export),
        )
        .route(
            "/api/operator/payouts/preview",
            get(operator_payout_preview),
        )
        .route("/api/operator/payouts/audit", get(operator_payout_audit))
        .route("/api/operator/payouts", get(operator_payouts))
        .route("/api/operator/payouts/pause", post(operator_pause_payouts))
        .route(
            "/api/operator/payouts/resume",
            post(operator_resume_payouts),
        )
        .route(
            "/api/operator/payouts/{batch_id}/retry",
            post(operator_retry_payout),
        )
        .route(
            "/api/operator/payouts/{batch_id}/approve",
            post(operator_approve_payout),
        )
        .route(
            "/api/operator/payouts/{batch_id}/cancel",
            post(operator_cancel_payout),
        )
        .with_state(state)
        .layer(from_fn(security_headers))
}

async fn security_headers(request: Request, next: Next) -> Response {
    let is_api_response = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self'",
        ),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    if is_api_response {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        service: "csd-pool-api",
        release: release_info(),
    })
}

async fn dashboard() -> Html<&'static str> {
    Html(dashboard_html())
}

async fn getting_started_page() -> Html<&'static str> {
    Html(getting_started_html())
}

async fn status_page() -> Html<&'static str> {
    Html(status_html())
}

async fn getting_started() -> Json<GettingStartedResponse> {
    Json(getting_started_response())
}

async fn status(State(state): State<Arc<AppState>>) -> Result<Json<StatusResponse>, ApiError> {
    Ok(Json(state.status_response().await?))
}

async fn pool(State(state): State<Arc<AppState>>) -> Result<Json<PoolResponse>, ApiError> {
    let db_stats = match state.repository.as_ref() {
        Some(repository) => Some(repository.dashboard_pool_stats().await?),
        None => None,
    };
    let network = network_telemetry_from_env().await;
    Ok(Json(
        state.pool_response(db_stats.as_ref(), network.as_ref()),
    ))
}

async fn metrics(State(state): State<Arc<AppState>>) -> Result<Json<MetricsResponse>, ApiError> {
    let snapshot = state.pool_state.snapshot();
    let db_stats = match state.repository.as_ref() {
        Some(repository) => Some(repository.dashboard_pool_stats().await?),
        None => None,
    };
    Ok(Json(MetricsResponse {
        workers: snapshot
            .workers
            .iter()
            .map(|(address, worker)| (address.clone(), worker_stats(worker)))
            .collect(),
        totals: Totals {
            workers_online: reported_worker_count(db_stats.as_ref(), &snapshot.totals),
            shares_accepted: snapshot.totals.shares_accepted,
            shares_rejected: snapshot.totals.shares_rejected,
            shares_stale: snapshot.totals.shares_stale,
            blocks_found: snapshot.totals.blocks_found,
        },
        fee_revenue_csd: db_stats
            .map(|stats| format_csd(stats.fee_revenue_base_units))
            .unwrap_or_else(|| "0.00000000".to_owned()),
    }))
}

async fn prometheus_metrics(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let db_stats = match state.repository.as_ref() {
        Some(repository) => Some(repository.dashboard_pool_stats().await?),
        None => None,
    };
    let node_samples = match state.repository.as_ref() {
        Some(repository) => repository.latest_node_samples(100).await?,
        None => vec![],
    };
    Ok((
        [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        state.prometheus_metrics(db_stats.as_ref(), &node_samples),
    ))
}

async fn history(
    Query(query): Query<HistoryQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<HistoryResponse>, ApiError> {
    let (range_secs, interval_secs) =
        history_window(query.range.as_deref(), state.history_interval_secs);
    let network_hashrate_hs = network_telemetry_from_env()
        .await
        .map(|network| network.hashrate_hs)
        .unwrap_or_default();
    let mut samples = match state.repository.as_ref() {
        Some(repository) => repository
            .dashboard_history(range_secs, interval_secs)
            .await?
            .into_iter()
            .map(history_sample_response)
            .collect(),
        None => vec![memory_history_sample(&state)],
    };
    if network_hashrate_hs > 0.0 {
        for sample in &mut samples {
            sample.net_hs = network_hashrate_hs;
        }
    }
    Ok(Json(HistoryResponse {
        interval_secs,
        samples,
    }))
}

async fn miner(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<MinerResponse>, ApiError> {
    let address = normalize_addr20(&address).ok_or(ApiError::InvalidAddress)?;
    let snapshot = state.pool_state.snapshot();
    let worker = snapshot.workers.get(&address);
    let db_miner = match state.repository.as_ref() {
        Some(repository) => repository.dashboard_miner_stats(&address).await?,
        None => None,
    };
    let payments = match state.repository.as_ref() {
        Some(repository) => repository
            .dashboard_recent_payments_for_miner(&address, 20)
            .await?
            .into_iter()
            .map(payment_response)
            .collect(),
        None => vec![],
    };
    let confirming_blocks = match state.repository.as_ref() {
        Some(repository) => repository
            .dashboard_recent_blocks(100)
            .await?
            .into_iter()
            .filter(|block| {
                block.finder == address
                    && matches!(
                        block.status.as_str(),
                        "submitted" | "seen_on_chain" | "immature"
                    )
            })
            .map(block_response)
            .collect(),
        None => vec![],
    };
    let db = db_miner.as_ref();

    Ok(Json(MinerResponse {
        address,
        online: worker.is_some(),
        workers_online: worker
            .map(|_| 1)
            .or_else(|| db.map(|stats| stats.workers_total))
            .unwrap_or_default(),
        hashrate_hs: worker.map(WorkerSnapshot::hashrate_hs).unwrap_or_default(),
        pending_csd: format_csd(
            db.map(|stats| stats.immature_base_units)
                .unwrap_or_default(),
        ),
        pending_base_units: db
            .map(|stats| stats.immature_base_units.to_string())
            .unwrap_or_else(|| "0".to_owned()),
        owed_csd: format_csd(
            db.map(|stats| stats.confirmed_base_units)
                .unwrap_or_default(),
        ),
        owed_base_units: db
            .map(|stats| stats.confirmed_base_units.to_string())
            .unwrap_or_else(|| "0".to_owned()),
        paid_lifetime_csd: format_csd(db.map(|stats| stats.paid_base_units).unwrap_or_default()),
        paid_lifetime_base_units: db
            .map(|stats| stats.paid_base_units.to_string())
            .unwrap_or_else(|| "0".to_owned()),
        eta_secs: None,
        // Session earnings and rate estimates require persisted session
        // boundaries. Do not emit fabricated zeroes until that data exists.
        csd_per_hour: None,
        csd_per_day: None,
        session_csd: None,
        session_secs: None,
        shares_accepted: worker
            .map(|w| w.shares_accepted)
            .or_else(|| db.map(|stats| stats.shares_accepted))
            .unwrap_or_default(),
        shares_rejected: worker
            .map(|w| w.shares_rejected)
            .or_else(|| db.map(|stats| stats.shares_rejected))
            .unwrap_or_default(),
        shares_stale: worker
            .map(|w| w.shares_stale)
            .or_else(|| db.map(|stats| stats.shares_stale))
            .unwrap_or_default(),
        last_difficulty: worker
            .map(|w| w.last_difficulty)
            .or_else(|| db.map(|stats| stats.last_difficulty))
            .unwrap_or_default(),
        last_seen_ts: worker
            .map(|w| w.last_seen_ts)
            .or_else(|| db.map(|stats| stats.last_seen_ts))
            .unwrap_or_default(),
        confirming_blocks,
        payments,
    }))
}

async fn miner_workers(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<MinerWorkersResponse>, ApiError> {
    let address = normalize_addr20(&address).ok_or(ApiError::InvalidAddress)?;
    if let Some(repository) = state.repository.as_ref() {
        let workers = repository
            .dashboard_workers_for_miner(&address)
            .await?
            .into_iter()
            .map(|stats| WorkerDetail {
                name: stats.name,
                online: stats.last_seen_ts > 0,
                hashrate_hs: 0.0,
                shares_accepted: stats.shares_accepted,
                shares_rejected: stats.shares_rejected,
                shares_stale: stats.shares_stale,
                blocks_found: stats.blocks_found,
                last_difficulty: stats.last_difficulty,
                connected_at: stats.connected_at,
                last_seen_at: stats.last_seen_at,
            })
            .collect();
        return Ok(Json(MinerWorkersResponse { address, workers }));
    }

    let snapshot = state.pool_state.snapshot();
    let workers = snapshot
        .workers
        .get(&address)
        .map(|stats| WorkerDetail {
            name: "default".to_owned(),
            online: true,
            hashrate_hs: stats.hashrate_hs(),
            shares_accepted: stats.shares_accepted,
            shares_rejected: stats.shares_rejected,
            shares_stale: stats.shares_stale,
            blocks_found: stats.blocks_found,
            last_difficulty: stats.last_difficulty,
            connected_at: None,
            last_seen_at: None,
        })
        .into_iter()
        .collect();
    Ok(Json(MinerWorkersResponse { address, workers }))
}

async fn blocks(State(state): State<Arc<AppState>>) -> Result<Json<BlocksResponse>, ApiError> {
    let blocks = match state.repository.as_ref() {
        Some(repository) => repository
            .dashboard_recent_blocks(100)
            .await?
            .into_iter()
            .map(block_response)
            .collect(),
        None => state.blocks.clone(),
    };
    Ok(Json(BlocksResponse { blocks }))
}

async fn payments(State(state): State<Arc<AppState>>) -> Result<Json<PaymentsResponse>, ApiError> {
    let payments = match state.repository.as_ref() {
        Some(repository) => repository
            .dashboard_recent_payments(100)
            .await?
            .into_iter()
            .map(payment_response)
            .collect(),
        None => state.payments.clone(),
    };
    Ok(Json(PaymentsResponse { payments }))
}

async fn operator_payout_status(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorPayoutStatusResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let payouts_enabled = repository.payouts_enabled().await?;
    Ok(Json(OperatorPayoutStatusResponse { payouts_enabled }))
}

async fn operator_health(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorHealthResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let samples = repository.latest_node_samples(100).await?;
    let ok = samples.iter().all(|sample| sample.ok);
    Ok(Json(OperatorHealthResponse { ok, samples }))
}

async fn operator_sessions(
    headers: HeaderMap,
    Query(query): Query<OperatorSessionsQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorSessionsResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let versions = repository.session_version_summaries().await?;
    let sessions = repository
        .recent_stratum_sessions(query.limit.unwrap_or(100).clamp(1, 500))
        .await?;
    Ok(Json(OperatorSessionsResponse {
        release: release_info(),
        active_sessions: versions.iter().map(|version| version.active_sessions).sum(),
        versions,
        sessions,
    }))
}

async fn operator_alerts(
    headers: HeaderMap,
    Query(query): Query<OperatorAlertsQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorAlertsResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let alerts = repository
        .list_alerts(query.status.as_deref(), query.limit.unwrap_or(100).min(500))
        .await?;
    Ok(Json(OperatorAlertsResponse { alerts }))
}

async fn operator_resolve_alert(
    headers: HeaderMap,
    Path(fingerprint): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorResolveAlertResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let resolved = repository.resolve_alert(&fingerprint).await?;
    Ok(Json(OperatorResolveAlertResponse { resolved }))
}

async fn operator_payouts(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorPayoutsResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let batches = repository
        .list_payout_batches_by_status(
            &[
                "needs_approval",
                "created",
                "signed",
                "submitted",
                "confirmed",
                "failed",
                "cancelled",
            ],
            100,
        )
        .await?
        .into_iter()
        .map(operator_payout_batch_response)
        .collect();
    Ok(Json(OperatorPayoutsResponse { batches }))
}

async fn operator_payout_export(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let batches = operator_payout_batch_records(repository).await?;
    let csv = payout_batches_csv(&batches);
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"csd-payout-batches.csv\"",
            ),
        ],
        csv,
    ))
}

async fn operator_payout_preview(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorPayoutPreviewResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let minimum_payout_base_units = minimum_payout_base_units()?;
    let balances = repository
        .list_payable_balances(minimum_payout_base_units, 1_000)
        .await?;
    let selection = select_payouts(
        &balances,
        minimum_payout_base_units,
        payout_preview_recipient_limit(),
    );
    let max_payout_batch_base_units = max_payout_batch_base_units()?;
    let max_daily_payout_base_units = max_daily_payout_base_units()?;
    let manual_payout_approval_base_units = manual_payout_approval_base_units()?;
    let daily_payout_used_base_units = repository.active_payout_total_today().await?;
    Ok(Json(operator_payout_preview_response(
        minimum_payout_base_units,
        max_payout_batch_base_units,
        max_daily_payout_base_units,
        manual_payout_approval_base_units,
        daily_payout_used_base_units,
        selection,
    )))
}

async fn operator_payout_audit(
    headers: HeaderMap,
    Query(query): Query<OperatorPayoutAuditQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorPayoutAuditResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let events = repository
        .list_payout_audit_events(query.batch_id.as_deref(), limit)
        .await?
        .into_iter()
        .map(operator_payout_audit_event_response)
        .collect();
    Ok(Json(OperatorPayoutAuditResponse { events }))
}

async fn operator_payout_audit_export(
    headers: HeaderMap,
    Query(query): Query<OperatorPayoutAuditQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let limit = query.limit.unwrap_or(1_000).clamp(1, 10_000);
    let events = repository
        .list_payout_audit_events(query.batch_id.as_deref(), limit)
        .await?;
    let csv = payout_audit_events_csv(&events);
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"csd-payout-audit.csv\"",
            ),
        ],
        csv,
    ))
}

async fn operator_pause_payouts(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorPayoutStatusResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    repository.set_payouts_enabled(false).await?;
    append_payout_audit(
        repository,
        "pool",
        "operator",
        "pause_payouts",
        serde_json::json!({}),
    )
    .await?;
    Ok(Json(OperatorPayoutStatusResponse {
        payouts_enabled: false,
    }))
}

async fn operator_retry_payout(
    headers: HeaderMap,
    Path(batch_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorRetryPayoutResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let new_batch_id = retry_batch_id(&batch_id);
    let batch = repository
        .retry_failed_payout(&batch_id, &new_batch_id)
        .await?
        .map(operator_payout_batch_response);
    if batch.is_some() {
        append_payout_audit(
            repository,
            &new_batch_id,
            "operator",
            "retry",
            serde_json::json!({ "source_batch_id": batch_id }),
        )
        .await?;
    }
    Ok(Json(OperatorRetryPayoutResponse {
        retried: batch.is_some(),
        new_batch_id,
        batch,
    }))
}

async fn operator_approve_payout(
    headers: HeaderMap,
    Path(batch_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorApprovePayoutResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let approved = repository.mark_payout_approved(&batch_id).await?;
    if approved {
        append_payout_audit(
            repository,
            &batch_id,
            "operator",
            "approve",
            serde_json::json!({}),
        )
        .await?;
    }
    Ok(Json(OperatorApprovePayoutResponse { approved }))
}

async fn operator_cancel_payout(
    headers: HeaderMap,
    Path(batch_id): Path<String>,
    State(state): State<Arc<AppState>>,
    body: Option<Json<CancelPayoutRequest>>,
) -> Result<Json<OperatorCancelPayoutResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    let reason = body
        .as_ref()
        .and_then(|body| body.reason.as_deref())
        .unwrap_or("operator cancelled payout");
    let batch = repository
        .cancel_payout(&batch_id, reason)
        .await?
        .map(operator_payout_batch_response);
    if batch.is_some() {
        append_payout_audit(
            repository,
            &batch_id,
            "operator",
            "cancel",
            serde_json::json!({ "reason": reason }),
        )
        .await?;
    }
    Ok(Json(OperatorCancelPayoutResponse {
        cancelled: batch.is_some(),
        batch,
    }))
}

async fn operator_resume_payouts(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperatorPayoutStatusResponse>, ApiError> {
    authorize_operator(&headers)?;
    let repository = state
        .repository
        .as_ref()
        .ok_or(ApiError::RepositoryRequired)?;
    repository.set_payouts_enabled(true).await?;
    append_payout_audit(
        repository,
        "pool",
        "operator",
        "resume_payouts",
        serde_json::json!({}),
    )
    .await?;
    Ok(Json(OperatorPayoutStatusResponse {
        payouts_enabled: true,
    }))
}

#[derive(Clone)]
struct AppState {
    pool_state: SharedPoolState,
    settings: ApiSettings,
    repository: Option<PgRepository>,
    history_interval_secs: u64,
    blocks: Vec<BlockResponse>,
    payments: Vec<PaymentResponse>,
}

impl AppState {
    fn from_pool_state(
        pool_state: SharedPoolState,
        settings: ApiSettings,
        repository: Option<PgRepository>,
    ) -> Self {
        Self {
            pool_state,
            settings,
            repository,
            history_interval_secs: 60,
            blocks: vec![],
            payments: vec![],
        }
    }

    #[cfg(test)]
    fn demo() -> Self {
        let pool_state = SharedPoolState::new();
        pool_state.record_authorized_worker("0123456789abcdef0123456789abcdef01234567");
        Self::from_pool_state(pool_state, ApiSettings::default(), None)
    }

    fn pool_response(
        &self,
        db_stats: Option<&DashboardPoolStats>,
        network: Option<&NetworkTelemetry>,
    ) -> PoolResponse {
        let snapshot = self.pool_state.snapshot();
        let pool_hashrate_hs = snapshot.totals.pool_hashrate_hs;
        let network_hashrate_hs = network
            .map(|network| network.hashrate_hs)
            .unwrap_or_default();
        let network_share_pct = if pool_hashrate_hs > 0.0 && network_hashrate_hs > 0.0 {
            (pool_hashrate_hs / network_hashrate_hs) * 100.0
        } else {
            0.0
        };
        PoolResponse {
            pool_hashrate_hs,
            network_hashrate_hs,
            network_share_pct,
            round_effort_pct: round_effort_pct(snapshot.totals.round_share_difficulty_sum, network),
            expected_block_secs: network.and_then(|network| {
                if pool_hashrate_hs > 0.0 && network_hashrate_hs > 0.0 {
                    Some(network.target_block_secs * (network_hashrate_hs / pool_hashrate_hs))
                } else {
                    None
                }
            }),
            total_blocks: db_stats
                .map(|stats| stats.total_blocks)
                .unwrap_or(snapshot.totals.blocks_found),
            canonical_blocks: db_stats
                .map(|stats| stats.canonical_blocks)
                .unwrap_or_default(),
            immature_blocks: db_stats
                .map(|stats| stats.immature_blocks)
                .unwrap_or(snapshot.totals.blocks_found),
            orphaned_blocks: db_stats
                .map(|stats| stats.orphaned_blocks)
                .unwrap_or_default(),
            avg_block_effort_pct_24h: db_stats
                .map(|stats| stats.avg_block_effort_pct_24h)
                .unwrap_or_default(),
            avg_block_effort_pct_7d: db_stats
                .map(|stats| stats.avg_block_effort_pct_7d)
                .unwrap_or_default(),
            avg_block_effort_pct_lifetime: db_stats
                .map(|stats| stats.avg_block_effort_pct_lifetime)
                .unwrap_or_default(),
            block_luck_pct_24h: block_luck_pct(
                db_stats
                    .map(|stats| stats.avg_block_effort_pct_24h)
                    .unwrap_or_default(),
            ),
            block_luck_pct_7d: block_luck_pct(
                db_stats
                    .map(|stats| stats.avg_block_effort_pct_7d)
                    .unwrap_or_default(),
            ),
            block_luck_pct_lifetime: block_luck_pct(
                db_stats
                    .map(|stats| stats.avg_block_effort_pct_lifetime)
                    .unwrap_or_default(),
            ),
            workers_online: reported_worker_count(db_stats, &snapshot.totals),
            miners_online: reported_miner_count(db_stats, &snapshot.totals),
            shares_accepted: snapshot.totals.shares_accepted,
            shares_rejected: snapshot.totals.shares_rejected,
            shares_stale: snapshot.totals.shares_stale,
            pool_fee_pct: self.settings.pool_fee_pct,
            payout_interval_secs: self.settings.payout_interval_secs,
            next_payout_secs: next_payout_secs(&self.settings, db_stats),
            confirm_depth: self.settings.confirm_depth,
            updated_ts: snapshot.updated_ts.max(now_ts()),
        }
    }

    async fn status_response(&self) -> Result<StatusResponse, ApiError> {
        let snapshot = self.pool_state.snapshot();
        let mut active_alerts = 0_u64;
        let mut unhealthy_services = 0_u64;
        let mut node_count = 0_u64;
        let mut latest_sample_at = None;
        let mut payouts_enabled = None;
        let mut persistent_workers_online = None;
        let mut data_source = "memory";

        if let Some(repository) = self.repository.as_ref() {
            data_source = "postgres";
            let samples = repository.latest_node_samples(100).await?;
            node_count = samples
                .iter()
                .filter(|sample| sample.node_name.starts_with("node:"))
                .count() as u64;
            unhealthy_services = samples.iter().filter(|sample| !sample.ok).count() as u64;
            latest_sample_at = samples
                .iter()
                .filter_map(|sample| sample.sampled_at.clone())
                .max();
            active_alerts = repository.list_alerts(Some("active"), 500).await?.len() as u64;
            payouts_enabled = Some(repository.payouts_enabled().await?);
            persistent_workers_online =
                Some(repository.dashboard_pool_stats().await?.workers_online);
        }

        let status = if unhealthy_services > 0
            || active_alerts > 0
            || (self.repository.is_some() && node_count == 0)
        {
            "degraded"
        } else {
            "operational"
        };

        Ok(StatusResponse {
            status,
            service: "csd-pool",
            release: release_info(),
            config: self.config_summary_response(),
            data_source,
            api_ok: true,
            workers_online: persistent_workers_online
                .unwrap_or_default()
                .max(active_worker_count(&snapshot.totals)),
            shares_accepted: snapshot.totals.shares_accepted,
            shares_rejected: snapshot.totals.shares_rejected,
            shares_stale: snapshot.totals.shares_stale,
            active_alerts,
            unhealthy_services,
            node_count,
            payouts_enabled,
            latest_sample_at,
            updated_ts: snapshot.updated_ts.max(now_ts()),
        })
    }

    fn config_summary_response(&self) -> RuntimeConfigResponse {
        RuntimeConfigResponse {
            pool_id: self.settings.pool_id.clone(),
            mining_address: self.settings.mining_address.clone(),
            fee_percent: self.settings.pool_fee_pct,
            confirm_depth: self.settings.confirm_depth,
            stratum_listen: self.settings.stratum_listen.clone(),
            api_listen: self.settings.api_listen.clone(),
            signer_listen: self.settings.signer_listen.clone(),
            initial_difficulty: self.settings.initial_difficulty,
            min_difficulty: self.settings.min_difficulty,
            max_difficulty: self.settings.max_difficulty,
            target_share_secs: self.settings.target_share_secs,
            vardiff_ewma_alpha: self.settings.vardiff_ewma_alpha,
            vardiff_raise_ratio: self.settings.vardiff_raise_ratio,
            vardiff_lower_ratio: self.settings.vardiff_lower_ratio,
            vardiff_min_adjust_secs: self.settings.vardiff_min_adjust_secs,
            vardiff_max_adjust_factor: self.settings.vardiff_max_adjust_factor,
            vardiff_transition_grace_secs: self.settings.vardiff_transition_grace_secs,
            minimum_payout_base_units: self
                .settings
                .minimum_payout_base_units
                .map(|value| value.to_string()),
            manual_payout_approval_base_units: self
                .settings
                .manual_payout_approval_base_units
                .map(|value| value.to_string()),
            max_payout_batch_base_units: self
                .settings
                .max_payout_batch_base_units
                .map(|value| value.to_string()),
            max_daily_payout_base_units: self
                .settings
                .max_daily_payout_base_units
                .map(|value| value.to_string()),
        }
    }

    fn prometheus_metrics(
        &self,
        db_stats: Option<&DashboardPoolStats>,
        node_samples: &[NodeSampleRecord],
    ) -> String {
        let snapshot = self.pool_state.snapshot();
        let fee_revenue_base_units = db_stats
            .map(|stats| stats.fee_revenue_base_units)
            .unwrap_or_default();
        let next_payout_secs = next_payout_secs(&self.settings, db_stats);
        let job_age_secs = db_stats
            .and_then(|stats| latest_age_secs(stats.latest_job_created_ts))
            .unwrap_or_default();
        let total_blocks = db_stats
            .map(|stats| stats.total_blocks)
            .unwrap_or(snapshot.totals.blocks_found);
        let confirmed_blocks = db_stats
            .map(|stats| stats.canonical_blocks)
            .unwrap_or_default();
        let orphaned_blocks = db_stats
            .map(|stats| stats.orphaned_blocks)
            .unwrap_or_default();
        let updated_ts = snapshot.updated_ts.max(now_ts());
        let workers_online = reported_worker_count(db_stats, &snapshot.totals);

        let mut body = String::new();
        body.push_str(
            "# HELP csd_pool_workers_online Workers active in the last five minutes, with current sessions as an immediate floor.\n",
        );
        body.push_str("# TYPE csd_pool_workers_online gauge\n");
        body.push_str(&format!("csd_pool_workers_online {}\n", workers_online));
        body.push_str(
            "# HELP csd_pool_hashrate_hs Estimated pool hashrate from accepted Stratum shares.\n",
        );
        body.push_str("# TYPE csd_pool_hashrate_hs gauge\n");
        body.push_str(&format!(
            "csd_pool_hashrate_hs {:.6}\n",
            snapshot.totals.pool_hashrate_hs
        ));
        body.push_str(
            "# HELP csd_pool_round_share_difficulty Current round accepted share difficulty sum.\n",
        );
        body.push_str("# TYPE csd_pool_round_share_difficulty gauge\n");
        body.push_str(&format!(
            "csd_pool_round_share_difficulty {:.6}\n",
            snapshot.totals.round_share_difficulty_sum
        ));
        body.push_str("# HELP csd_pool_stratum_connections Current active Stratum TCP sessions.\n");
        body.push_str("# TYPE csd_pool_stratum_connections gauge\n");
        body.push_str(&format!(
            "csd_pool_stratum_connections {}\n",
            snapshot.totals.stratum_connections
        ));
        body.push_str("# HELP csd_pool_shares_total Stratum shares grouped by result.\n");
        body.push_str("# TYPE csd_pool_shares_total counter\n");
        body.push_str(&format!(
            "csd_pool_shares_total{{result=\"accepted\"}} {}\n",
            snapshot.totals.shares_accepted
        ));
        body.push_str(&format!(
            "csd_pool_shares_total{{result=\"rejected\"}} {}\n",
            snapshot.totals.shares_rejected
        ));
        body.push_str(&format!(
            "csd_pool_shares_total{{result=\"stale\"}} {}\n",
            snapshot.totals.shares_stale
        ));
        body.push_str("# HELP csd_pool_share_validation_seconds Share validation latency summary for this API/Stratum process.\n");
        body.push_str("# TYPE csd_pool_share_validation_seconds summary\n");
        body.push_str(&format!(
            "csd_pool_share_validation_seconds_sum {:.9}\n",
            snapshot.totals.share_validation_seconds_sum
        ));
        body.push_str(&format!(
            "csd_pool_share_validation_seconds_count {}\n",
            snapshot.totals.share_validation_count
        ));
        body.push_str("# HELP csd_pool_share_validation_seconds_avg Average share validation latency for this API/Stratum process.\n");
        body.push_str("# TYPE csd_pool_share_validation_seconds_avg gauge\n");
        let validation_avg = if snapshot.totals.share_validation_count == 0 {
            0.0
        } else {
            snapshot.totals.share_validation_seconds_sum
                / snapshot.totals.share_validation_count as f64
        };
        body.push_str(&format!(
            "csd_pool_share_validation_seconds_avg {validation_avg:.9}\n"
        ));
        body.push_str(
            "# HELP csd_pool_candidate_propagation_seconds Block-candidate hot-path timing summaries. relay_enqueue measures local broadcast-queue admission, not peer receipt.\n",
        );
        body.push_str("# TYPE csd_pool_candidate_propagation_seconds summary\n");
        for (phase, sum, count) in [
            (
                "detected_to_submit_start",
                snapshot.totals.candidate_detected_to_submit_seconds_sum,
                snapshot.totals.candidate_propagation_count,
            ),
            (
                "node_roundtrip",
                snapshot.totals.candidate_node_roundtrip_seconds_sum,
                snapshot.totals.candidate_propagation_count,
            ),
            (
                "candidate_record",
                snapshot.totals.candidate_record_seconds_sum,
                snapshot.totals.candidate_propagation_count,
            ),
            (
                "candidate_total",
                snapshot.totals.candidate_total_seconds_sum,
                snapshot.totals.candidate_propagation_count,
            ),
            (
                "node_accept",
                snapshot.totals.candidate_node_accept_seconds_sum,
                snapshot.totals.candidate_node_accept_count,
            ),
            (
                "relay_enqueue",
                snapshot.totals.candidate_relay_enqueue_seconds_sum,
                snapshot.totals.candidate_relay_enqueue_count,
            ),
        ] {
            body.push_str(&format!(
                "csd_pool_candidate_propagation_seconds_sum{{phase=\"{phase}\"}} {sum:.9}\n"
            ));
            body.push_str(&format!(
                "csd_pool_candidate_propagation_seconds_count{{phase=\"{phase}\"}} {count}\n"
            ));
        }
        body.push_str(
            "# HELP csd_pool_candidate_propagation_seconds_max Maximum observed block-candidate hot-path timing by phase.\n",
        );
        body.push_str("# TYPE csd_pool_candidate_propagation_seconds_max gauge\n");
        for (phase, max) in [
            (
                "detected_to_submit_start",
                snapshot.totals.candidate_detected_to_submit_seconds_max,
            ),
            (
                "node_roundtrip",
                snapshot.totals.candidate_node_roundtrip_seconds_max,
            ),
            (
                "candidate_record",
                snapshot.totals.candidate_record_seconds_max,
            ),
            (
                "candidate_total",
                snapshot.totals.candidate_total_seconds_max,
            ),
            (
                "node_accept",
                snapshot.totals.candidate_node_accept_seconds_max,
            ),
            (
                "relay_enqueue",
                snapshot.totals.candidate_relay_enqueue_seconds_max,
            ),
        ] {
            body.push_str(&format!(
                "csd_pool_candidate_propagation_seconds_max{{phase=\"{phase}\"}} {max:.9}\n"
            ));
        }
        body.push_str(
            "# HELP csd_pool_job_notify_total Mining job publications grouped by reason.\n",
        );
        body.push_str("# TYPE csd_pool_job_notify_total counter\n");
        body.push_str(&format!(
            "csd_pool_job_notify_total{{reason=\"tip_change\"}} {}\n",
            snapshot.totals.job_tip_change_count
        ));
        body.push_str(&format!(
            "csd_pool_job_notify_total{{reason=\"heartbeat\"}} {}\n",
            snapshot.totals.job_heartbeat_count
        ));
        body.push_str(
            "# HELP csd_pool_job_notify_age_seconds Age of the latest job publication by this process.\n",
        );
        body.push_str("# TYPE csd_pool_job_notify_age_seconds gauge\n");
        let notify_age_secs = snapshot
            .totals
            .last_job_notify_ts
            .map(|timestamp| now_ts().saturating_sub(timestamp))
            .unwrap_or_default();
        body.push_str(&format!(
            "csd_pool_job_notify_age_seconds {notify_age_secs}\n"
        ));
        body.push_str("# HELP csd_pool_blocks_found_total Block candidates found by the pool.\n");
        body.push_str("# TYPE csd_pool_blocks_found_total counter\n");
        body.push_str(&format!("csd_pool_blocks_found_total {total_blocks}\n"));
        body.push_str("# HELP csd_pool_blocks_submitted_total Submitted block candidates recorded by the pool.\n");
        body.push_str("# TYPE csd_pool_blocks_submitted_total counter\n");
        body.push_str(&format!("csd_pool_blocks_submitted_total {total_blocks}\n"));
        body.push_str(
            "# HELP csd_pool_blocks_confirmed_total Confirmed pool blocks recorded by the pool.\n",
        );
        body.push_str("# TYPE csd_pool_blocks_confirmed_total counter\n");
        body.push_str(&format!(
            "csd_pool_blocks_confirmed_total {confirmed_blocks}\n"
        ));
        body.push_str(
            "# HELP csd_pool_blocks_orphaned_total Orphaned pool blocks recorded by the pool.\n",
        );
        body.push_str("# TYPE csd_pool_blocks_orphaned_total counter\n");
        body.push_str(&format!(
            "csd_pool_blocks_orphaned_total {orphaned_blocks}\n"
        ));
        if let Some(stats) = db_stats {
            body.push_str(
                "# HELP csd_pool_jobs_created_total Mining jobs persisted by the bridge.\n",
            );
            body.push_str("# TYPE csd_pool_jobs_created_total counter\n");
            body.push_str(&format!(
                "csd_pool_jobs_created_total {}\n",
                stats.jobs_created
            ));
            body.push_str(
                "# HELP csd_pool_job_age_seconds Age of the latest persisted mining job.\n",
            );
            body.push_str("# TYPE csd_pool_job_age_seconds gauge\n");
            body.push_str(&format!("csd_pool_job_age_seconds {job_age_secs}\n"));
            body.push_str(
                "# HELP csd_pool_payout_batches_total Payout batches grouped by status.\n",
            );
            body.push_str("# TYPE csd_pool_payout_batches_total gauge\n");
            body.push_str(&format!(
                "csd_pool_payout_batches_total{{status=\"needs_approval\"}} {}\n",
                stats.payout_batches_needs_approval
            ));
            body.push_str(&format!(
                "csd_pool_payout_batches_total{{status=\"created\"}} {}\n",
                stats.payout_batches_created
            ));
            body.push_str(&format!(
                "csd_pool_payout_batches_total{{status=\"signed\"}} {}\n",
                stats.payout_batches_signed
            ));
            body.push_str(&format!(
                "csd_pool_payout_batches_total{{status=\"submitted\"}} {}\n",
                stats.payout_batches_submitted
            ));
            body.push_str(&format!(
                "csd_pool_payout_batches_total{{status=\"confirmed\"}} {}\n",
                stats.payout_batches_confirmed
            ));
            body.push_str(&format!(
                "csd_pool_payout_batches_total{{status=\"failed\"}} {}\n",
                stats.payout_batches_failed
            ));
            body.push_str(&format!(
                "csd_pool_payout_batches_total{{status=\"cancelled\"}} {}\n",
                stats.payout_batches_cancelled
            ));
            body.push_str("# HELP csd_pool_payout_amount_base_units_total Total submitted or confirmed payout batch amount in base units.\n");
            body.push_str("# TYPE csd_pool_payout_amount_base_units_total counter\n");
            body.push_str(&format!(
                "csd_pool_payout_amount_base_units_total {}\n",
                stats.payout_amount_base_units_total
            ));
        }
        body.push_str("# HELP csd_pool_fee_revenue_base_units Pool fee revenue in base units.\n");
        body.push_str("# TYPE csd_pool_fee_revenue_base_units gauge\n");
        body.push_str(&format!(
            "csd_pool_fee_revenue_base_units {fee_revenue_base_units}\n"
        ));
        if !node_samples.is_empty() {
            body.push_str("# HELP csd_pool_service_up Latest sampled service health, 1 for healthy and 0 for unhealthy.\n");
            body.push_str("# TYPE csd_pool_service_up gauge\n");
            body.push_str(
                "# HELP csd_node_rpc_latency_seconds Latest sampled CSD node RPC latency.\n",
            );
            body.push_str("# TYPE csd_node_rpc_latency_seconds gauge\n");
            body.push_str("# HELP csd_node_height Latest sampled CSD node height.\n");
            body.push_str("# TYPE csd_node_height gauge\n");
            body.push_str("# HELP csd_node_peers Latest sampled CSD node peer count.\n");
            body.push_str("# TYPE csd_node_peers gauge\n");
            for sample in node_samples {
                let service = prometheus_label_value(&sample.node_name);
                let up = u8::from(sample.ok);
                body.push_str(&format!(
                    "csd_pool_service_up{{service=\"{service}\"}} {up}\n"
                ));
                let Some(node_name) = sample.node_name.strip_prefix("node:") else {
                    continue;
                };
                let node = prometheus_label_value(node_name);
                if let Some(rpc_ms) = sample.rpc_ms {
                    body.push_str(&format!(
                        "csd_node_rpc_latency_seconds{{node=\"{node}\"}} {:.6}\n",
                        rpc_ms / 1000.0
                    ));
                }
                if let Some(height) = sample.height {
                    body.push_str(&format!("csd_node_height{{node=\"{node}\"}} {height}\n"));
                }
                if let Some(peers) = sample.peers {
                    body.push_str(&format!("csd_node_peers{{node=\"{node}\"}} {peers}\n"));
                }
            }
        }
        body.push_str(
            "# HELP csd_pool_next_payout_seconds Seconds until the next scheduled payout window.\n",
        );
        body.push_str("# TYPE csd_pool_next_payout_seconds gauge\n");
        body.push_str(&format!(
            "csd_pool_next_payout_seconds {next_payout_secs}\n"
        ));
        body.push_str("# HELP csd_pool_updated_timestamp_seconds Last in-memory pool state update timestamp.\n");
        body.push_str("# TYPE csd_pool_updated_timestamp_seconds gauge\n");
        body.push_str(&format!(
            "csd_pool_updated_timestamp_seconds {updated_ts}\n"
        ));
        body
    }
}

fn release_info() -> ReleaseInfo {
    ReleaseInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        name: std::env::var("CSD_POOL_RELEASE_NAME").unwrap_or_else(|_| "unknown".to_owned()),
        revision: std::env::var("CSD_POOL_RELEASE_REVISION")
            .unwrap_or_else(|_| "unknown".to_owned()),
        timestamp_utc: std::env::var("CSD_POOL_RELEASE_TIMESTAMP_UTC")
            .unwrap_or_else(|_| "unknown".to_owned()),
    }
}

#[derive(Debug)]
enum ApiError {
    InvalidAddress,
    InvalidAmount(String),
    Repository(csd_pool_db::RepositoryError),
    RepositoryRequired,
    Unauthorized,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match self {
            ApiError::InvalidAddress => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: ErrorBody {
                        code: "invalid_address",
                        message: "address must be 40 hex chars",
                    },
                }),
            )
                .into_response(),
            ApiError::Repository(err) => {
                error!(%err, "pool api repository read failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: ErrorBody {
                            code: "repository_error",
                            message: "failed to read pool database",
                        },
                    }),
                )
                    .into_response()
            }
            ApiError::InvalidAmount(_value) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: ErrorBody {
                        code: "invalid_amount",
                        message: "invalid configured CSD amount",
                    },
                }),
            )
                .into_response(),
            ApiError::RepositoryRequired => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: ErrorBody {
                        code: "repository_required",
                        message: "operator endpoint requires PostgreSQL",
                    },
                }),
            )
                .into_response(),
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: ErrorBody {
                        code: "unauthorized",
                        message: "operator token is missing or invalid",
                    },
                }),
            )
                .into_response(),
        }
    }
}

impl From<csd_pool_db::RepositoryError> for ApiError {
    fn from(value: csd_pool_db::RepositoryError) -> Self {
        ApiError::Repository(value)
    }
}

fn normalize_addr20(value: &str) -> Option<String> {
    let stripped = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if stripped.len() == 40 && stripped.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(stripped.to_ascii_lowercase())
    } else {
        None
    }
}

fn worker_stats(stats: &WorkerSnapshot) -> WorkerStats {
    WorkerStats {
        hashrate_hs: stats.hashrate_hs(),
        shares_accepted: stats.shares_accepted,
        shares_rejected: stats.shares_rejected,
        shares_stale: stats.shares_stale,
        blocks_found: stats.blocks_found,
        last_difficulty: stats.last_difficulty,
        last_seen_ts: stats.last_seen_ts,
    }
}

fn active_worker_count(totals: &TotalsSnapshot) -> u64 {
    totals.stratum_connections.max(totals.workers_online)
}

fn reported_worker_count(db_stats: Option<&DashboardPoolStats>, totals: &TotalsSnapshot) -> u64 {
    db_stats
        .map(|stats| stats.workers_online)
        .unwrap_or_default()
        .max(active_worker_count(totals))
}

fn reported_miner_count(db_stats: Option<&DashboardPoolStats>, totals: &TotalsSnapshot) -> u64 {
    db_stats
        .map(|stats| stats.miners_online)
        .unwrap_or_default()
        .max(totals.workers_online)
}

fn history_window(range: Option<&str>, default_interval_secs: u64) -> (u64, u64) {
    match range {
        Some("24h") => (24 * 60 * 60, 60),
        Some("7d") => (7 * 24 * 60 * 60, 15 * 60),
        Some("30d") => (30 * 24 * 60 * 60, 60 * 60),
        _ => (12 * 60 * 60, default_interval_secs.max(1)),
    }
}

fn memory_history_sample(state: &AppState) -> HistorySample {
    let snapshot = state.pool_state.snapshot();
    HistorySample {
        ts: snapshot.updated_ts.max(now_ts()),
        pool_hs: snapshot.totals.pool_hashrate_hs,
        net_hs: 0.0,
        workers: active_worker_count(&snapshot.totals),
        shares_accepted: snapshot.totals.shares_accepted,
        shares_rejected: snapshot.totals.shares_rejected,
        shares_stale: snapshot.totals.shares_stale,
    }
}

fn history_sample_response(sample: DashboardHistorySample) -> HistorySample {
    HistorySample {
        ts: sample.ts,
        pool_hs: sample.pool_hs,
        net_hs: sample.net_hs,
        workers: sample.workers,
        shares_accepted: sample.shares_accepted,
        shares_rejected: sample.shares_rejected,
        shares_stale: sample.shares_stale,
    }
}

fn getting_started_response() -> GettingStartedResponse {
    let stratum_endpoint = public_stratum_endpoint();
    let payout = public_payout_rules();
    GettingStartedResponse {
        pool_name: "CSD Pool",
        stratum_endpoint: stratum_endpoint.clone(),
        username_format: "<addr20>.<worker>",
        address_format: "40 lowercase hex characters; optional 0x prefix accepted by dashboard lookup",
        worker_name_rules: "letters, numbers, dash, underscore, and dot; keep names short and stable",
        port_tiers: public_port_tiers(&stratum_endpoint),
        commands: vec![
            CommandExample {
                label: "Generic Stratum miner",
                command: format!(
                    "csd-pool-miner --url stratum+tcp://{} --user <addr20>.rig-01 --pass x",
                    stratum_endpoint
                ),
            },
            CommandExample {
                label: "CUDA worker",
                command: format!(
                    "csd-pool-miner --backend cuda --url stratum+tcp://{} --user <addr20>.cuda-01 --pass x",
                    stratum_endpoint
                ),
            },
            CommandExample {
                label: "CPU fallback",
                command: format!(
                    "csd-pool-miner --backend cpu --url stratum+tcp://{} --user <addr20>.cpu-01 --pass x",
                    stratum_endpoint
                ),
            },
        ],
        payout,
        public_endpoints: vec![
            "/".to_owned(),
            "/getting-started".to_owned(),
            "/status".to_owned(),
            "/api/pool".to_owned(),
            "/api/miner/<addr20>".to_owned(),
            "/api/miner/<addr20>/workers".to_owned(),
            "/api/blocks".to_owned(),
            "/api/payments".to_owned(),
        ],
    }
}

fn public_stratum_endpoint() -> String {
    std::env::var("CSD_POOL_PUBLIC_STRATUM_ADDR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "127.0.0.1:3333".to_owned())
}

fn public_port_tiers(default_endpoint: &str) -> Vec<PortTier> {
    if let Ok(value) = std::env::var("CSD_POOL_PUBLIC_PORT_TIERS") {
        if !value.trim().is_empty() {
            let tiers: Vec<PortTier> = value.split(',').filter_map(parse_port_tier).collect();
            if !tiers.is_empty() {
                return tiers;
            }
        }
    }
    let port = default_endpoint
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .unwrap_or(3333);
    vec![PortTier {
        port,
        label: "standard".to_owned(),
        starting_difficulty: 8.0,
        enabled: true,
    }]
}

fn parse_port_tier(value: &str) -> Option<PortTier> {
    let mut parts = value.split(':').map(str::trim);
    let port = parts.next()?.parse::<u16>().ok()?;
    let label = parts
        .next()
        .filter(|label| !label.is_empty())
        .unwrap_or("custom");
    let starting_difficulty = parts
        .next()
        .and_then(|difficulty| difficulty.parse::<f64>().ok())
        .unwrap_or(8.0);
    let enabled = !matches!(parts.next(), Some("disabled" | "off" | "0" | "false"));
    Some(PortTier {
        port,
        label: label.to_owned(),
        starting_difficulty,
        enabled,
    })
}

fn public_payout_rules() -> PayoutRules {
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        if let Ok(config) = csd_pool_config::PoolConfig::from_file(path) {
            return PayoutRules {
                minimum_payout_csd: config.pool.minimum_payout_csd,
                payout_interval_secs: config.pool.payout_interval_secs,
                confirm_depth: config.pool.confirm_depth,
                fee_percent: config.pool.fee_percent,
            };
        }
    }
    PayoutRules {
        minimum_payout_csd: "1.0".to_owned(),
        payout_interval_secs: 1800,
        confirm_depth: 10,
        fee_percent: 1.0,
    }
}

fn round_effort_pct(round_share_difficulty_sum: f64, network: Option<&NetworkTelemetry>) -> f64 {
    let Some(network) = network else {
        return 0.0;
    };
    let expected_network_difficulty =
        network.hashrate_hs * network.target_block_secs / 4_294_967_296.0;
    if round_share_difficulty_sum <= 0.0 || expected_network_difficulty <= 0.0 {
        return 0.0;
    }
    (round_share_difficulty_sum / expected_network_difficulty) * 100.0
}

fn block_luck_pct(avg_effort_pct: f64) -> f64 {
    if avg_effort_pct > 0.0 {
        10_000.0 / avg_effort_pct
    } else {
        0.0
    }
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug)]
struct NetworkTelemetry {
    hashrate_hs: f64,
    target_block_secs: f64,
}

async fn network_telemetry_from_env() -> Option<NetworkTelemetry> {
    let base_url = network_url_from_env()?;
    let timeout_secs = env_u64("CSD_POOL_NETWORK_TIMEOUT_SECS", 5).max(1);
    let network_token = std::env::var("CSD_POOL_NETWORK_TOKEN")
        .ok()
        .filter(|token| !token.is_empty());
    match tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        CsdNodeClient::new(base_url.clone())
            .with_bearer_token(network_token)
            .network(),
    )
    .await
    {
        Ok(Ok(snapshot)) => {
            let hashrate_hs = if snapshot.hashrate > 0.0 {
                snapshot.hashrate
            } else {
                snapshot.hashrate_ghs * 1_000_000_000.0
            };
            let target_block_secs = if snapshot.target_block_secs > 0 {
                snapshot.target_block_secs as f64
            } else {
                120.0
            };
            Some(NetworkTelemetry {
                hashrate_hs,
                target_block_secs,
            })
        }
        Ok(Err(error)) => {
            debug!(%base_url, %error, "network telemetry unavailable");
            None
        }
        Err(_) => {
            debug!(%base_url, timeout_secs, "network telemetry timed out");
            None
        }
    }
}

fn network_url_from_env() -> Option<String> {
    std::env::var("CSD_POOL_NETWORK_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn next_payout_secs(settings: &ApiSettings, db_stats: Option<&DashboardPoolStats>) -> u64 {
    let Some(stats) = db_stats else {
        return settings.next_payout_secs;
    };
    if stats.latest_payout_created_ts == 0 {
        return settings.next_payout_secs.min(settings.payout_interval_secs);
    }
    let elapsed = now_ts().saturating_sub(stats.latest_payout_created_ts);
    settings.payout_interval_secs.saturating_sub(elapsed)
}

fn latest_age_secs(latest_ts: u64) -> Option<u64> {
    if latest_ts == 0 {
        return None;
    }
    Some(now_ts().saturating_sub(latest_ts))
}

fn prometheus_label_value(value: &str) -> String {
    value
        .replace('\\', r"\\")
        .replace('"', r#"\""#)
        .replace('\n', r"\n")
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

async fn operator_payout_batch_records(
    repository: &PgRepository,
) -> Result<Vec<PayoutBatchRecord>, ApiError> {
    Ok(repository
        .list_payout_batches_by_status(
            &[
                "needs_approval",
                "created",
                "signed",
                "submitted",
                "confirmed",
                "failed",
                "cancelled",
            ],
            1_000,
        )
        .await?)
}

fn operator_token() -> Option<String> {
    let env_name = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.api.operator_token_env)
            .unwrap_or_else(|| "CSD_POOL_OPERATOR_TOKEN".to_owned())
    } else {
        "CSD_POOL_OPERATOR_TOKEN".to_owned()
    };
    std::env::var(env_name)
        .ok()
        .filter(|value| !value.is_empty())
}

fn authorize_operator(headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(expected) = operator_token() else {
        return Err(ApiError::Unauthorized);
    };
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(ApiError::Unauthorized);
    };
    let Ok(value) = value.to_str() else {
        return Err(ApiError::Unauthorized);
    };
    if value
        .strip_prefix("Bearer ")
        .map(|actual| constant_time_eq(actual.as_bytes(), expected.as_bytes()))
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(left_byte ^ right_byte);
    }
    diff == 0
}

fn format_csd(base_units: u128) -> String {
    format!(
        "{}.{:08}",
        base_units / 100_000_000,
        base_units % 100_000_000
    )
}

fn block_response(block: DashboardBlock) -> BlockResponse {
    BlockResponse {
        height: block.height,
        hash: block.hash,
        finder: block.finder,
        worker: block.worker,
        reward_csd: format_csd(block.reward_base_units),
        status: block.status,
        confirmations: block.confirmations,
        effort_pct: block.effort_pct,
        found_at: block.found_at,
        confirmed_at: block.confirmed_at,
    }
}

fn payment_response(payment: DashboardPayment) -> PaymentResponse {
    PaymentResponse {
        batch_id: payment.batch_id,
        address: payment.address,
        amount_csd: format_csd(payment.amount_base_units),
        amount_base_units: payment.amount_base_units.to_string(),
        txid: payment.txid,
        status: payment.status,
        created_at: payment.created_at,
        confirmed_at: payment.confirmed_at,
    }
}

fn operator_payout_batch_response(batch: PayoutBatchRecord) -> OperatorPayoutBatchResponse {
    OperatorPayoutBatchResponse {
        batch_id: batch.batch_id,
        status: batch.status,
        total_base_units: batch.total_base_units.to_string(),
        total_csd: format_csd(batch.total_base_units),
        txid: batch.txid,
        raw_tx_hash: batch.raw_tx_hash,
        recipients: batch
            .recipients
            .into_iter()
            .map(|recipient| OperatorPayoutRecipientResponse {
                miner: recipient.miner,
                address: recipient.address,
                amount_base_units: recipient.amount_base_units.to_string(),
                amount_csd: format_csd(recipient.amount_base_units),
            })
            .collect(),
    }
}

fn operator_payout_audit_event_response(
    event: PayoutAuditEvent,
) -> OperatorPayoutAuditEventResponse {
    OperatorPayoutAuditEventResponse {
        batch_id: event.batch_id,
        actor: event.actor,
        action: event.action,
        details: event.details,
        created_at: event.created_at,
    }
}

async fn append_payout_audit(
    repository: &PgRepository,
    batch_id: &str,
    actor: &str,
    action: &str,
    details: serde_json::Value,
) -> Result<(), ApiError> {
    repository
        .append_payout_audit_event(&PayoutAuditEvent {
            batch_id: batch_id.to_owned(),
            actor: actor.to_owned(),
            action: action.to_owned(),
            details,
            created_at: None,
        })
        .await?;
    Ok(())
}

fn retry_batch_id(batch_id: &str) -> String {
    format!("retry-{batch_id}-{}", now_millis())
}

fn minimum_payout_base_units() -> Result<u128, ApiError> {
    let value = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.pool.minimum_payout_csd)
            .unwrap_or_else(|| "1.0".to_owned())
    } else {
        std::env::var("CSD_POOL_MINIMUM_PAYOUT_CSD").unwrap_or_else(|_| "1.0".to_owned())
    };
    parse_csd_base_units(&value)
}

fn max_payout_batch_base_units() -> Result<u128, ApiError> {
    let value = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.pool.max_payout_batch_csd)
            .unwrap_or_else(|| "1000.0".to_owned())
    } else {
        std::env::var("CSD_POOL_MAX_PAYOUT_BATCH_CSD").unwrap_or_else(|_| "1000.0".to_owned())
    };
    parse_csd_base_units(&value)
}

fn max_daily_payout_base_units() -> Result<u128, ApiError> {
    let value = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.pool.max_daily_payout_csd)
            .unwrap_or_else(|| "5000.0".to_owned())
    } else {
        std::env::var("CSD_POOL_MAX_DAILY_PAYOUT_CSD").unwrap_or_else(|_| "5000.0".to_owned())
    };
    parse_csd_base_units(&value)
}

fn manual_payout_approval_base_units() -> Result<u128, ApiError> {
    let value = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.pool.manual_payout_approval_csd)
            .unwrap_or_else(|| "250.0".to_owned())
    } else {
        std::env::var("CSD_POOL_MANUAL_PAYOUT_APPROVAL_CSD").unwrap_or_else(|_| "250.0".to_owned())
    };
    parse_csd_base_units(&value)
}

fn payout_preview_recipient_limit() -> usize {
    std::env::var("CSD_POOL_PAYOUT_PREVIEW_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100)
}

fn parse_csd_base_units(value: &str) -> Result<u128, ApiError> {
    let trimmed = value.trim();
    let mut parts = trimmed.split('.');
    let whole = parts.next().unwrap_or_default();
    let frac = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || frac.len() > 8
    {
        return Err(ApiError::InvalidAmount(trimmed.to_owned()));
    }
    let whole_units = whole
        .parse::<u128>()
        .map_err(|_| ApiError::InvalidAmount(trimmed.to_owned()))?
        .checked_mul(100_000_000)
        .ok_or_else(|| ApiError::InvalidAmount(trimmed.to_owned()))?;
    let mut frac_padded = frac.to_owned();
    while frac_padded.len() < 8 {
        frac_padded.push('0');
    }
    let frac_units = if frac_padded.is_empty() {
        0
    } else {
        frac_padded
            .parse::<u128>()
            .map_err(|_| ApiError::InvalidAmount(trimmed.to_owned()))?
    };
    Ok(whole_units + frac_units)
}

fn operator_payout_preview_response(
    minimum_payout_base_units: u128,
    max_payout_batch_base_units: u128,
    max_daily_payout_base_units: u128,
    manual_payout_approval_base_units: u128,
    daily_payout_used_base_units: u128,
    selection: PayoutSelection,
) -> OperatorPayoutPreviewResponse {
    let cap_exceeded = selection.total_base_units > max_payout_batch_base_units;
    let daily_cap_exceeded = daily_payout_used_base_units
        .saturating_add(selection.total_base_units)
        > max_daily_payout_base_units;
    let daily_remaining_base_units =
        max_daily_payout_base_units.saturating_sub(daily_payout_used_base_units);
    let manual_approval_required = selection.total_base_units > manual_payout_approval_base_units;
    OperatorPayoutPreviewResponse {
        minimum_payout_base_units: minimum_payout_base_units.to_string(),
        minimum_payout_csd: format_csd(minimum_payout_base_units),
        max_payout_batch_base_units: max_payout_batch_base_units.to_string(),
        max_payout_batch_csd: format_csd(max_payout_batch_base_units),
        max_daily_payout_base_units: max_daily_payout_base_units.to_string(),
        max_daily_payout_csd: format_csd(max_daily_payout_base_units),
        manual_payout_approval_base_units: manual_payout_approval_base_units.to_string(),
        manual_payout_approval_csd: format_csd(manual_payout_approval_base_units),
        daily_payout_used_base_units: daily_payout_used_base_units.to_string(),
        daily_payout_used_csd: format_csd(daily_payout_used_base_units),
        daily_remaining_base_units: daily_remaining_base_units.to_string(),
        daily_remaining_csd: format_csd(daily_remaining_base_units),
        recipient_count: selection.recipients.len(),
        total_base_units: selection.total_base_units.to_string(),
        total_csd: format_csd(selection.total_base_units),
        would_create_batch: !selection.recipients.is_empty()
            && !cap_exceeded
            && !daily_cap_exceeded
            && !manual_approval_required,
        cap_exceeded,
        daily_cap_exceeded,
        manual_approval_required,
        recipients: selection
            .recipients
            .into_iter()
            .map(operator_payout_recipient_response)
            .collect(),
    }
}

fn operator_payout_recipient_response(
    recipient: PayoutRecipient,
) -> OperatorPayoutRecipientResponse {
    OperatorPayoutRecipientResponse {
        miner: recipient.miner,
        address: recipient.address,
        amount_base_units: recipient.amount_base_units.to_string(),
        amount_csd: format_csd(recipient.amount_base_units),
    }
}

fn payout_batches_csv(batches: &[PayoutBatchRecord]) -> String {
    let mut csv =
        "batch_id,status,txid,recipient,amount_base_units,amount_csd,total_base_units,total_csd\n"
            .to_owned();
    for batch in batches {
        for recipient in &batch.recipients {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{}\n",
                csv_cell(&batch.batch_id),
                csv_cell(&batch.status),
                csv_cell(batch.txid.as_deref().unwrap_or_default()),
                csv_cell(&recipient.address),
                recipient.amount_base_units,
                format_csd(recipient.amount_base_units),
                batch.total_base_units,
                format_csd(batch.total_base_units),
            ));
        }
    }
    csv
}

fn payout_audit_events_csv(events: &[PayoutAuditEvent]) -> String {
    let mut csv = "created_at,batch_id,actor,action,details_json\n".to_owned();
    for event in events {
        csv.push_str(&format!(
            "{},{},{},{},{}\n",
            csv_cell(event.created_at.as_deref().unwrap_or_default()),
            csv_cell(&event.batch_id),
            csv_cell(&event.actor),
            csv_cell(&event.action),
            csv_cell(&event.details.to_string()),
        ));
    }
    csv
}

fn csv_cell(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn dashboard_html() -> &'static str {
    r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>CSD Pool Dashboard</title>
  <link rel="icon" href='data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="%230b0f12"/><path d="M16 5l9.5 5.5v11L16 27l-9.5-5.5v-11L16 5z" fill="none" stroke="%2331d0aa" stroke-width="2.2"/><path d="M11 16h10M16 11v10" stroke="%23e0a642" stroke-width="2.2" stroke-linecap="round"/></svg>'>
  <style>
    :root {
      color-scheme: dark;
      --bg: #0b0f12;
      --panel: #11181d;
      --panel-2: #151d23;
      --line: #223039;
      --text: #edf5f3;
      --muted: #8da19d;
      --soft: #b8c8c4;
      --teal: #31d0aa;
      --amber: #e0a642;
      --red: #e06161;
      --green: #62d48e;
      --shadow: 0 18px 60px rgba(0, 0, 0, .28);
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }

    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      background: radial-gradient(circle at 20% -10%, rgba(49, 208, 170, .12), transparent 28%),
                  linear-gradient(180deg, #0d1215 0%, var(--bg) 44%, #080b0d 100%);
      color: var(--text);
    }

    .shell { max-width: 1480px; margin: 0 auto; padding: 22px 28px 34px; }
    header {
      display: grid;
      grid-template-columns: minmax(180px, 1fr) auto minmax(180px, 1fr);
      align-items: center;
      gap: 18px;
      margin-bottom: 18px;
    }

    .brand { display: flex; align-items: center; gap: 12px; min-width: 0; }
    .mark {
      width: 34px; height: 34px; display: grid; place-items: center;
      border: 1px solid rgba(49, 208, 170, .45);
      border-radius: 8px;
      background: linear-gradient(135deg, rgba(49, 208, 170, .18), rgba(224, 166, 66, .08));
      box-shadow: inset 0 0 20px rgba(49, 208, 170, .08);
    }
    .mark svg { width: 21px; height: 21px; color: var(--teal); }
    h1 { margin: 0; font-size: 18px; line-height: 1.2; font-weight: 700; letter-spacing: 0; }
    .sub { color: var(--muted); font-size: 12px; margin-top: 2px; }

    nav { display: flex; align-items: center; border: 1px solid var(--line); background: rgba(17, 24, 29, .72); border-radius: 8px; padding: 3px; }
    nav button {
      appearance: none; border: 0; color: var(--muted); background: transparent;
      padding: 8px 13px; border-radius: 6px; font-size: 13px; font-weight: 650; cursor: pointer;
    }
    nav button[aria-selected="true"] { color: var(--text); background: #1b252c; box-shadow: inset 0 0 0 1px rgba(255,255,255,.04); }
    .network { justify-self: end; display: flex; gap: 14px; align-items: center; color: var(--soft); font-size: 13px; }
    .dot { width: 8px; height: 8px; border-radius: 50%; display: inline-block; background: var(--green); box-shadow: 0 0 18px var(--green); margin-right: 7px; }

    .kpis { display: grid; grid-template-columns: repeat(6, minmax(130px, 1fr)); gap: 10px; margin-bottom: 12px; }
    .tile, .panel {
      background: linear-gradient(180deg, rgba(21, 29, 35, .94), rgba(15, 21, 25, .94));
      border: 1px solid var(--line);
      border-radius: 8px;
      box-shadow: var(--shadow);
    }
    .tile { padding: 13px 14px; min-height: 88px; }
    .label { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: .08em; font-weight: 750; }
    .value { font-size: 23px; line-height: 1.1; font-weight: 760; margin-top: 10px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .delta { color: var(--soft); font-size: 12px; margin-top: 8px; }
    .accent { color: var(--teal); }
    .warn { color: var(--amber); }

    .grid { display: grid; grid-template-columns: minmax(0, 1.55fr) minmax(320px, .75fr); gap: 12px; align-items: start; }
    .stack { display: grid; gap: 12px; }
    .panel { padding: 16px; overflow: hidden; }
    .panel-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 14px; }
    h2 { margin: 0; font-size: 14px; line-height: 1.25; font-weight: 750; }
    .hint { color: var(--muted); font-size: 12px; }
    .range-tabs { display: flex; align-items: center; border: 1px solid var(--line); background: #0c1114; border-radius: 8px; padding: 3px; }
    .range-tabs button {
      appearance: none; border: 0; background: transparent; color: var(--muted);
      min-width: 44px; height: 28px; border-radius: 6px; font-size: 12px; font-weight: 720; cursor: pointer;
    }
    .range-tabs button[aria-selected="true"] { color: var(--text); background: #1b252c; box-shadow: inset 0 0 0 1px rgba(255,255,255,.05); }
    .chart { height: 238px; width: 100%; display: block; border-radius: 6px; background: #0c1114; border: 1px solid rgba(255,255,255,.04); }
    .miner-search { display: grid; grid-template-columns: minmax(260px, 1fr) auto; gap: 10px; align-items: end; margin-bottom: 12px; }
    .field label { display: block; color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: .08em; font-weight: 750; margin-bottom: 8px; }
    .field input {
      width: 100%; height: 38px; border: 1px solid var(--line); border-radius: 7px;
      background: #0c1114; color: var(--text); padding: 0 11px; font-size: 13px;
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
      outline: none;
    }
    .field input:focus { border-color: rgba(49, 208, 170, .62); box-shadow: 0 0 0 3px rgba(49, 208, 170, .08); }
    .operator-auth { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 8px; margin-bottom: 12px; }
    .operator-auth input {
      width: 100%; height: 34px; border: 1px solid var(--line); border-radius: 7px;
      background: #0c1114; color: var(--text); padding: 0 10px; font-size: 12px; outline: none;
    }
    .operator-auth button { height: 34px; }
    .command {
      height: 38px; border: 1px solid rgba(49, 208, 170, .42); border-radius: 7px;
      background: rgba(49, 208, 170, .12); color: var(--text); padding: 0 13px;
      font-size: 13px; font-weight: 750; cursor: pointer;
    }
    .miner-result { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 10px; }
    .mini { border-top: 1px solid rgba(255,255,255,.055); padding-top: 12px; min-width: 0; }
    .mini .value { font-size: 18px; margin-top: 7px; }
    .miner-message { color: var(--muted); font-size: 13px; margin-top: 11px; }

    .tables { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
    .table-scroll { overflow-x: auto; }
    table { width: 100%; border-collapse: collapse; font-size: 12px; }
    th { color: var(--muted); text-align: left; font-size: 11px; text-transform: uppercase; letter-spacing: .07em; font-weight: 750; padding: 0 8px 9px 0; }
    td { color: var(--soft); padding: 10px 8px 10px 0; border-top: 1px solid rgba(255,255,255,.055); vertical-align: middle; }
    td:first-child, th:first-child { padding-left: 0; }
    .mono { font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace; }
    .status { color: var(--teal); font-weight: 700; }
    .empty { color: var(--muted); padding: 18px 0; font-size: 13px; }

    .rail { display: grid; gap: 12px; }
    .service { display: flex; justify-content: space-between; align-items: center; padding: 11px 0; border-top: 1px solid rgba(255,255,255,.055); }
    .service:first-of-type { border-top: 0; }
    .service strong { font-size: 13px; }
    .badge { color: var(--green); border: 1px solid rgba(98, 212, 142, .35); background: rgba(98, 212, 142, .08); padding: 4px 7px; border-radius: 6px; font-size: 11px; font-weight: 750; }
    .badge.bad { color: var(--red); border-color: rgba(224, 97, 97, .35); background: rgba(224, 97, 97, .08); }
    .alert-row { color: var(--soft); padding: 10px 0; border-top: 1px solid rgba(255,255,255,.055); font-size: 12px; }
    .alert-row:first-child { border-top: 0; }
    .batch-actions { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 8px; }
    .mini-command {
      border: 1px solid var(--line);
      border-radius: 6px;
      background: #141d22;
      color: var(--text);
      cursor: pointer;
      font: inherit;
      font-size: 12px;
      padding: 5px 8px;
    }
    .mini-command:hover { border-color: rgba(49,208,170,.4); }
    footer { color: var(--muted); font-size: 12px; margin-top: 14px; display: flex; justify-content: space-between; gap: 12px; }

    @media (max-width: 1060px) {
      header { grid-template-columns: 1fr; }
      nav { justify-self: start; overflow-x: auto; max-width: 100%; }
      .network { justify-self: start; }
      .kpis { grid-template-columns: repeat(3, minmax(0, 1fr)); }
      .grid, .tables { grid-template-columns: 1fr; }
      .miner-result { grid-template-columns: repeat(2, minmax(0, 1fr)); }
    }

    @media (max-width: 620px) {
      .shell { padding: 16px; }
      .kpis { grid-template-columns: 1fr 1fr; }
      .miner-search { grid-template-columns: 1fr; }
      .operator-auth { grid-template-columns: 1fr; }
      .value { font-size: 19px; }
      nav button { padding: 8px 10px; }
      footer { flex-direction: column; }
    }
  </style>
</head>
<body>
  <div class="shell">
    <header>
      <div class="brand">
        <div class="mark" aria-hidden="true">
          <svg viewBox="0 0 24 24" fill="none">
            <path d="M12 3l7.8 4.5v9L12 21l-7.8-4.5v-9L12 3z" stroke="currentColor" stroke-width="1.8"/>
            <path d="M8 12h8M12 8v8" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>
          </svg>
        </div>
        <div>
          <h1>CSD Pool</h1>
          <div class="sub">Compute Substrate mining operations</div>
        </div>
      </div>
      <nav aria-label="Dashboard sections">
        <button type="button" aria-selected="true">Overview</button>
        <button type="button" data-nav-href="/getting-started">Start</button>
        <button type="button">Miners</button>
        <button type="button">Blocks</button>
        <button type="button">Payouts</button>
      </nav>
      <div class="network"><span><span class="dot"></span><span id="api-status">API online</span></span><span id="last-updated">syncing</span></div>
    </header>

    <section class="kpis" aria-label="Pool KPIs">
      <div class="tile"><div class="label">Pool Hashrate</div><div class="value" id="pool-hashrate">0 H/s</div><div class="delta">reported by pool state</div></div>
      <div class="tile"><div class="label">Network Hashrate</div><div class="value" id="network-hashrate">0 H/s</div><div class="delta">network telemetry</div></div>
      <div class="tile"><div class="label">Workers Online</div><div class="value accent" id="workers-online">0</div><div class="delta">authorized workers</div></div>
      <div class="tile"><div class="label">Round Effort</div><div class="value" id="round-effort">0%</div><div class="delta">current round</div></div>
      <div class="tile"><div class="label">Blocks Found</div><div class="value warn" id="blocks-found">0</div><div class="delta" id="block-luck">24h luck 0%</div></div>
      <div class="tile"><div class="label">Fee Revenue</div><div class="value" id="fee-revenue">0 CSD</div><div class="delta">pool operator ledger</div></div>
    </section>

    <section class="panel" id="miner-panel" aria-label="Miner lookup">
      <form class="miner-search" id="miner-form">
        <div class="field">
          <label for="miner-address">Miner Address</label>
          <input id="miner-address" name="address" inputmode="text" autocomplete="off" spellcheck="false" placeholder="addr20 / 40 hex chars">
        </div>
        <button class="command" type="submit">Lookup Miner</button>
      </form>
      <div class="miner-result" id="miner-result">
        <div class="mini"><div class="label">Status</div><div class="value" id="miner-online">-</div></div>
        <div class="mini"><div class="label">Confirmed Owed</div><div class="value" id="miner-owed">0 CSD</div></div>
        <div class="mini"><div class="label">Pending</div><div class="value" id="miner-pending">0 CSD</div></div>
        <div class="mini"><div class="label">Accepted Shares</div><div class="value" id="miner-accepted">0</div></div>
      </div>
      <div class="miner-message" id="miner-message">Enter a CSD addr20 mining address to inspect balances, shares, payments, and workers.</div>
    </section>

    <main class="grid">
      <div class="stack">
        <section class="panel">
          <div class="panel-head"><h2>Shares And Pool Activity</h2><div class="range-tabs" aria-label="History range"><button type="button" data-history-range="12h" aria-selected="true">12h</button><button type="button" data-history-range="24h" aria-selected="false">24h</button><button type="button" data-history-range="7d" aria-selected="false">7d</button></div></div>
          <div class="hint" id="share-summary">accepted 0, rejected 0, stale 0</div>
          <canvas class="chart" id="activity-chart" width="960" height="260"></canvas>
        </section>
        <div class="tables">
          <section class="panel">
            <div class="panel-head"><h2>Recent Blocks</h2><span class="hint">latest 6</span></div>
            <div class="table-scroll">
              <table>
                <thead><tr><th>Hash</th><th>Finder</th><th>Status</th><th>Effort</th><th>Conf</th><th>Reward</th></tr></thead>
                <tbody id="blocks-body"><tr><td colspan="6" class="empty">No blocks yet</td></tr></tbody>
              </table>
            </div>
          </section>
          <section class="panel">
            <div class="panel-head"><h2>Recent Payments</h2><span class="hint">latest 6</span></div>
            <table>
              <thead><tr><th>Tx</th><th>Recipient</th><th>Amount</th></tr></thead>
              <tbody id="payments-body"><tr><td colspan="3" class="empty">No payments yet</td></tr></tbody>
            </table>
          </section>
        </div>
      </div>

      <aside class="rail" aria-label="Operator health summary">
        <section class="panel">
          <div class="panel-head"><h2>Service Health</h2><span class="hint" id="operator-status">operator token optional</span></div>
          <form class="operator-auth" id="operator-form">
            <input id="operator-token" type="password" autocomplete="off" placeholder="Operator bearer token">
            <button class="command" type="submit">Load</button>
          </form>
          <div id="health-list">
            <div class="service"><strong>node-a</strong><span class="badge">unknown</span></div>
            <div class="service"><strong>node-b</strong><span class="badge">unknown</span></div>
            <div class="service"><strong>signer</strong><span class="badge">unknown</span></div>
          </div>
        </section>
        <section class="panel">
          <div class="panel-head"><h2>Share Quality</h2><span class="hint" id="quality-rate">0% reject</span></div>
          <table>
            <tbody>
              <tr><td>Accepted</td><td class="mono" id="shares-accepted">0</td></tr>
              <tr><td>Rejected</td><td class="mono" id="shares-rejected">0</td></tr>
              <tr><td>Stale</td><td class="mono" id="shares-stale">0</td></tr>
            </tbody>
          </table>
        </section>
        <section class="panel">
          <div class="panel-head"><h2>Payout Preview</h2><span class="hint" id="payout-preview-summary">requires token</span></div>
          <div id="payout-preview-list"><div class="empty">No operator token configured in browser</div></div>
        </section>
        <section class="panel">
          <div class="panel-head"><h2>Payout Control</h2><span class="hint" id="payout-control-summary">requires token</span></div>
          <div id="payout-control-list"><div class="empty">No operator token configured in browser</div></div>
        </section>
        <section class="panel">
          <div class="panel-head"><h2>Payout Batches</h2><span class="hint" id="payout-batches-summary">requires token</span></div>
          <div id="payout-batches-list"><div class="empty">No operator token configured in browser</div></div>
        </section>
        <section class="panel">
          <div class="panel-head"><h2>Payout Audit</h2><button class="mini-command" type="button" id="payout-audit-export">Export</button><span class="hint" id="payout-audit-summary">requires token</span></div>
          <div id="payout-audit-list"><div class="empty">No operator token configured in browser</div></div>
        </section>
        <section class="panel">
          <div class="panel-head"><h2>Active Alerts</h2><span class="hint" id="alerts-summary">requires token</span></div>
          <div id="alerts-list"><div class="empty">No operator token configured in browser</div></div>
        </section>
      </aside>
    </main>

    <footer>
      <span>Public dashboard reads /api/pool, /api/metrics, /api/history, /api/blocks, and /api/payments.</span>
      <span class="mono">Stratum v1 compatible CSD pool</span>
    </footer>
  </div>

  <script>
    const state = { history: [], historyRange: "12h" };
    const qs = (id) => document.getElementById(id);
    const fmt = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
    const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (char) => ({
      "&": "&amp;",
      "<": "&lt;",
      ">": "&gt;",
      '"': "&quot;",
      "'": "&#39;"
    }[char]));

    function shortHash(value) {
      if (!value) return "-";
      value = String(value);
      return value.length > 14 ? value.slice(0, 7) + "..." + value.slice(-6) : value;
    }

    function hashrate(value) {
      const units = ["H/s", "KH/s", "MH/s", "GH/s", "TH/s", "PH/s"];
      let v = Number(value || 0);
      let i = 0;
      while (v >= 1000 && i < units.length - 1) { v /= 1000; i++; }
      return `${fmt.format(v)} ${units[i]}`;
    }

    async function getJson(path, options = {}) {
      const response = await fetch(path, options);
      if (!response.ok) throw new Error(`${path} ${response.status}`);
      return response.json();
    }

    function operatorHeaders() {
      const token = localStorage.getItem("csd_pool_operator_token");
      return token ? { Authorization: `Bearer ${token}` } : null;
    }

    function renderHealth(samples) {
      const list = qs("health-list");
      if (!samples || samples.length === 0) {
        list.innerHTML = `<div class="empty">No health samples recorded</div>`;
        return;
      }
      list.innerHTML = samples.slice(0, 6).map(sample => {
        const badge = sample.ok ? "badge" : "badge bad";
        const label = sample.ok ? "healthy" : "failing";
        const height = sample.height == null ? "-" : fmt.format(sample.height);
        const rpc = sample.rpc_ms == null ? "-" : `${fmt.format(sample.rpc_ms)} ms`;
        return `<div class="service"><div><strong>${esc(sample.node_name)}</strong><div class="hint">height ${esc(height)} · rpc ${esc(rpc)}</div></div><span class="${badge}">${label}</span></div>`;
      }).join("");
    }

    function renderAlerts(alerts) {
      qs("alerts-summary").textContent = `${alerts.length} active`;
      if (!alerts || alerts.length === 0) {
        qs("alerts-list").innerHTML = `<div class="empty">No active alerts</div>`;
        return;
      }
      qs("alerts-list").innerHTML = alerts.slice(0, 6).map(alert =>
        `<div class="alert-row"><strong>${esc(alert.severity)}</strong> · ${esc(alert.kind)}<br><span class="hint">${esc(alert.subject)}: ${esc(alert.message)}</span><div class="batch-actions"><button class="mini-command" type="button" data-alert-resolve="${esc(alert.fingerprint)}">Resolve</button></div></div>`
      ).join("");
    }

    function renderPayoutPreview(preview) {
      qs("payout-preview-summary").textContent = `${preview.recipient_count || 0} recipients`;
      if (!preview.would_create_batch && !(preview.recipients || []).length) {
        qs("payout-preview-list").innerHTML = `<div class="empty">No balances above ${esc(preview.minimum_payout_csd)} CSD</div>`;
        return;
      }
      const rows = (preview.recipients || []).slice(0, 5).map(recipient =>
        `<div class="alert-row"><strong>${esc(recipient.amount_csd)} CSD</strong><br><span class="hint">${esc(shortHash(recipient.address))}</span></div>`
      ).join("");
      const badge = preview.cap_exceeded ? "cap" : preview.daily_cap_exceeded ? "daily" : preview.manual_approval_required ? "review" : preview.recipient_count;
      const capHint = preview.cap_exceeded ? `cap ${preview.max_payout_batch_csd} CSD exceeded` : `cap ${preview.max_payout_batch_csd} CSD`;
      const dailyHint = preview.daily_cap_exceeded ? `daily cap exceeded, ${preview.daily_remaining_csd} CSD left` : `${preview.daily_remaining_csd} CSD left today`;
      const approvalHint = preview.manual_approval_required ? `manual approval above ${preview.manual_payout_approval_csd} CSD` : `approval ${preview.manual_payout_approval_csd} CSD`;
      qs("payout-preview-list").innerHTML =
        `<div class="service"><div><strong>${esc(preview.total_csd)} CSD</strong><div class="hint">minimum ${esc(preview.minimum_payout_csd)} CSD · ${esc(capHint)} · ${esc(dailyHint)} · ${esc(approvalHint)}</div></div><span class="badge">${esc(badge)}</span></div>${rows}`;
    }

    function renderPayoutControl(status) {
      const enabled = !!status.payouts_enabled;
      qs("payout-control-summary").textContent = enabled ? "enabled" : "paused";
      const badgeClass = enabled ? "badge" : "badge bad";
      const badge = enabled ? "enabled" : "paused";
      const action = enabled ? "pause" : "resume";
      const label = enabled ? "Pause" : "Resume";
      qs("payout-control-list").innerHTML =
        `<div class="service"><div><strong>Automatic payouts</strong><div class="hint">${enabled ? "create, sign, and submit workers may run" : "create, sign, and submit workers are stopped"}</div></div><span class="${badgeClass}">${badge}</span></div><div class="batch-actions"><button class="mini-command" type="button" data-payout-control="${action}">${label}</button></div>`;
    }

    function renderPayoutBatches(batches) {
      qs("payout-batches-summary").textContent = `${batches.length} batches`;
      if (!batches || batches.length === 0) {
        qs("payout-batches-list").innerHTML = `<div class="empty">No payout batches yet</div>`;
        return;
      }
      qs("payout-batches-list").innerHTML = batches.slice(0, 6).map(batch => {
        const canApprove = batch.status === "needs_approval";
        const canCancel = ["needs_approval", "created", "signed"].includes(batch.status);
        const canRetry = ["failed", "cancelled"].includes(batch.status);
        const actions = [
          canApprove ? `<button class="mini-command" type="button" data-payout-action="approve" data-batch-id="${esc(batch.batch_id)}">Approve</button>` : "",
          canCancel ? `<button class="mini-command" type="button" data-payout-action="cancel" data-batch-id="${esc(batch.batch_id)}">Cancel</button>` : "",
          canRetry ? `<button class="mini-command" type="button" data-payout-action="retry" data-batch-id="${esc(batch.batch_id)}">Retry</button>` : ""
        ].filter(Boolean).join("");
        const tx = batch.txid ? ` · tx ${esc(shortHash(batch.txid))}` : "";
        return `<div class="alert-row"><strong>${esc(batch.total_csd)} CSD</strong> · <span class="status">${esc(batch.status)}</span><br><span class="hint">${esc(shortHash(batch.batch_id))} · ${esc(batch.recipients.length)} recipients${tx}</span>${actions ? `<div class="batch-actions">${actions}</div>` : ""}</div>`;
      }).join("");
    }

    function renderPayoutAudit(events) {
      qs("payout-audit-summary").textContent = `${events.length} events`;
      if (!events || events.length === 0) {
        qs("payout-audit-list").innerHTML = `<div class="empty">No payout audit events yet</div>`;
        return;
      }
      qs("payout-audit-list").innerHTML = events.slice(0, 6).map(event => {
        const created = event.created_at ? new Date(event.created_at).toLocaleString() : "";
        const reason = event.details && event.details.reason ? ` · ${event.details.reason}` : "";
        return `<div class="alert-row"><strong>${esc(event.action)}</strong> · ${esc(event.actor)}<br><span class="hint">${esc(shortHash(event.batch_id))}${esc(reason)}${created ? ` · ${esc(created)}` : ""}</span></div>`;
      }).join("");
    }

    async function payoutAction(action, batchId) {
      const headers = operatorHeaders();
      if (!headers) return;
      const options = { method: "POST", headers };
      if (action === "cancel") {
        options.headers = { ...headers, "Content-Type": "application/json" };
        options.body = JSON.stringify({ reason: "operator dashboard action" });
      }
      await getJson(`/api/operator/payouts/${encodeURIComponent(batchId)}/${action}`, options);
      await refreshOperator();
    }

    async function payoutControl(action) {
      const headers = operatorHeaders();
      if (!headers) return;
      await getJson(`/api/operator/payouts/${action}`, { method: "POST", headers });
      await refreshOperator();
    }

    async function resolveAlert(fingerprint) {
      const headers = operatorHeaders();
      if (!headers) return;
      await getJson(`/api/operator/alerts/${encodeURIComponent(fingerprint)}/resolve`, { method: "POST", headers });
      await refreshOperator();
    }

    async function exportPayoutAudit() {
      const headers = operatorHeaders();
      if (!headers) return;
      const response = await fetch("/api/operator/payouts/audit/export.csv?limit=1000", { headers });
      if (!response.ok) throw new Error(`audit export ${response.status}`);
      const blob = await response.blob();
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = "csd-payout-audit.csv";
      document.body.appendChild(link);
      link.click();
      link.remove();
      URL.revokeObjectURL(url);
    }

    async function refreshOperator() {
      const headers = operatorHeaders();
      if (!headers) {
        qs("operator-status").textContent = "operator token optional";
        return;
      }
      try {
        const [health, alerts, preview, payoutStatus, payouts, audit] = await Promise.all([
          getJson("/api/operator/health", { headers }),
          getJson("/api/operator/alerts?status=active&limit=20", { headers }),
          getJson("/api/operator/payouts/preview", { headers }),
          getJson("/api/operator/payouts/status", { headers }),
          getJson("/api/operator/payouts", { headers }),
          getJson("/api/operator/payouts/audit?limit=20", { headers })
        ]);
        qs("operator-status").textContent = health.ok ? "all healthy" : "attention needed";
        renderHealth(health.samples || []);
        renderAlerts(alerts.alerts || []);
        renderPayoutPreview(preview);
        renderPayoutControl(payoutStatus);
        renderPayoutBatches(payouts.batches || []);
        renderPayoutAudit(audit.events || []);
      } catch (error) {
        qs("operator-status").textContent = "operator API unavailable";
        qs("alerts-summary").textContent = "requires token";
        qs("alerts-list").innerHTML = `<div class="empty">Operator data unavailable. Check token or database-backed API.</div>`;
        qs("payout-preview-summary").textContent = "requires token";
        qs("payout-preview-list").innerHTML = `<div class="empty">Payout preview unavailable</div>`;
        qs("payout-control-summary").textContent = "requires token";
        qs("payout-control-list").innerHTML = `<div class="empty">Payout control unavailable</div>`;
        qs("payout-batches-summary").textContent = "requires token";
        qs("payout-batches-list").innerHTML = `<div class="empty">Payout batches unavailable</div>`;
        qs("payout-audit-summary").textContent = "requires token";
        qs("payout-audit-list").innerHTML = `<div class="empty">Payout audit unavailable</div>`;
        console.error(error);
      }
    }

    function chartPoints(history, metrics) {
      const samples = history && Array.isArray(history.samples) ? history.samples : [];
      if (samples.length > 0) {
        return samples.slice(-96).map(sample => ({
          accepted: sample.shares_accepted || 0,
          rejected: sample.shares_rejected || 0,
          stale: sample.shares_stale || 0
        }));
      }
      state.history.push({
        accepted: metrics.totals.shares_accepted || 0,
        rejected: metrics.totals.shares_rejected || 0,
        stale: metrics.totals.shares_stale || 0
      });
      state.history = state.history.slice(-48);
      return state.history;
    }

    function drawChart(history, metrics) {
      const canvas = qs("activity-chart");
      const ctx = canvas.getContext("2d");
      const w = canvas.width, h = canvas.height;
      ctx.clearRect(0, 0, w, h);
      ctx.fillStyle = "#0c1114";
      ctx.fillRect(0, 0, w, h);
      ctx.strokeStyle = "rgba(255,255,255,.08)";
      ctx.lineWidth = 1;
      for (let y = 40; y < h; y += 44) {
        ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(w, y); ctx.stroke();
      }
      const points = chartPoints(history, metrics);
      const max = Math.max(1, ...points.map(p => p.accepted + p.rejected + p.stale));
      const draw = (key, color) => {
        ctx.strokeStyle = color;
        ctx.lineWidth = 2;
        ctx.beginPath();
        points.forEach((point, index) => {
          const x = points.length === 1 ? 0 : index * (w / (points.length - 1));
          const y = h - 24 - (point[key] / max) * (h - 52);
          if (index === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
        });
        ctx.stroke();
      };
      draw("accepted", "#31d0aa");
      draw("rejected", "#e06161");
      draw("stale", "#e0a642");
    }

    function renderRows(id, rows, empty, renderer, colspan = 3) {
      const body = qs(id);
      if (!rows || rows.length === 0) {
        body.innerHTML = `<tr><td colspan="${colspan}" class="empty">${empty}</td></tr>`;
        return;
      }
      body.innerHTML = rows.slice(0, 6).map(renderer).join("");
    }

    function setHistoryRange(range) {
      state.historyRange = range;
      state.history = [];
      document.querySelectorAll("[data-history-range]").forEach(button => {
        button.setAttribute("aria-selected", button.dataset.historyRange === range ? "true" : "false");
      });
      refresh();
    }

    function normalizeAddress(value) {
      const address = String(value || "").trim().replace(/^0x/i, "").toLowerCase();
      return /^[0-9a-f]{40}$/.test(address) ? address : null;
    }

    async function lookupMiner(address) {
      const normalized = normalizeAddress(address);
      if (!normalized) {
        qs("miner-message").textContent = "Address must be 40 hex characters, with optional 0x prefix.";
        return;
      }
      qs("miner-message").textContent = "Loading miner profile...";
      try {
        const [miner, workers] = await Promise.all([
          getJson(`/api/miner/${normalized}`),
          getJson(`/api/miner/${normalized}/workers`)
        ]);
        qs("miner-online").textContent = miner.online ? "online" : "offline";
        qs("miner-online").className = miner.online ? "value accent" : "value";
        qs("miner-owed").textContent = `${miner.owed_csd} CSD`;
        qs("miner-pending").textContent = `${miner.pending_csd} CSD`;
        qs("miner-accepted").textContent = fmt.format(miner.shares_accepted || 0);
        qs("miner-message").textContent =
          `${shortHash(miner.address)} · workers ${workers.workers.length} · paid ${miner.paid_lifetime_csd} CSD · payments ${miner.payments.length}`;
      } catch (error) {
        qs("miner-message").textContent = "Miner lookup failed; check the address and API status.";
        console.error(error);
      }
    }

    async function refresh() {
      try {
        const [pool, metrics, history, blocks, payments] = await Promise.all([
          getJson("/api/pool"),
          getJson("/api/metrics"),
          getJson(`/api/history?range=${encodeURIComponent(state.historyRange)}`).catch(() => null),
          getJson("/api/blocks"),
          getJson("/api/payments")
        ]);

        qs("api-status").textContent = "API online";
        qs("pool-hashrate").textContent = hashrate(pool.pool_hashrate_hs);
        qs("network-hashrate").textContent = hashrate(pool.network_hashrate_hs);
        qs("workers-online").textContent = fmt.format(pool.workers_online || 0);
        qs("round-effort").textContent = `${fmt.format(pool.round_effort_pct || 0)}%`;
        qs("blocks-found").textContent = fmt.format(pool.total_blocks || 0);
        qs("block-luck").textContent = `24h luck ${fmt.format(pool.block_luck_pct_24h || 0)}% · effort ${fmt.format(pool.avg_block_effort_pct_24h || 0)}%`;
        qs("fee-revenue").textContent = `${metrics.fee_revenue_csd || "0.00000000"} CSD`;
        qs("last-updated").textContent = `updated ${new Date((pool.updated_ts || 0) * 1000).toLocaleTimeString()}`;

        const accepted = metrics.totals.shares_accepted || 0;
        const rejected = metrics.totals.shares_rejected || 0;
        const stale = metrics.totals.shares_stale || 0;
        const total = Math.max(1, accepted + rejected + stale);
        qs("share-summary").textContent = `${state.historyRange} · accepted ${fmt.format(accepted)}, rejected ${fmt.format(rejected)}, stale ${fmt.format(stale)}`;
        qs("shares-accepted").textContent = fmt.format(accepted);
        qs("shares-rejected").textContent = fmt.format(rejected);
        qs("shares-stale").textContent = fmt.format(stale);
        qs("quality-rate").textContent = `${fmt.format((rejected / total) * 100)}% reject`;
        drawChart(history, metrics);

        renderRows("blocks-body", blocks.blocks, "No blocks yet", block =>
          `<tr><td class="mono">${esc(shortHash(block.hash))}</td><td class="mono">${esc(block.worker || shortHash(block.finder))}</td><td><span class="status">${esc(block.status)}</span></td><td>${fmt.format(block.effort_pct || 0)}%</td><td>${fmt.format(block.confirmations || 0)}</td><td>${esc(block.reward_csd || "0.00000000")} CSD</td></tr>`,
          6
        );
        renderRows("payments-body", payments.payments, "No payments yet", payment =>
          `<tr><td class="mono">${esc(shortHash(payment.txid))}</td><td class="mono">${esc(shortHash(payment.address))}</td><td>${esc(payment.amount_csd || "0.00000000")} CSD</td></tr>`
        );
        refreshOperator();
      } catch (error) {
        qs("api-status").textContent = "API degraded";
        console.error(error);
      }
    }

    document.getElementById("miner-form").addEventListener("submit", (event) => {
      event.preventDefault();
      lookupMiner(qs("miner-address").value);
    });
    document.getElementById("operator-token").value = localStorage.getItem("csd_pool_operator_token") || "";
    document.getElementById("operator-form").addEventListener("submit", (event) => {
      event.preventDefault();
      const token = qs("operator-token").value.trim();
      if (token) {
        localStorage.setItem("csd_pool_operator_token", token);
      } else {
        localStorage.removeItem("csd_pool_operator_token");
      }
      refreshOperator();
    });
    document.getElementById("payout-control-list").addEventListener("click", (event) => {
      const button = event.target.closest("button[data-payout-control]");
      if (!button) return;
      payoutControl(button.dataset.payoutControl).catch(error => {
        qs("payout-control-summary").textContent = "action failed";
        console.error(error);
      });
    });
    document.getElementById("payout-batches-list").addEventListener("click", (event) => {
      const button = event.target.closest("button[data-payout-action]");
      if (!button) return;
      payoutAction(button.dataset.payoutAction, button.dataset.batchId).catch(error => {
        qs("payout-batches-summary").textContent = "action failed";
        console.error(error);
      });
    });
    document.getElementById("alerts-list").addEventListener("click", (event) => {
      const button = event.target.closest("button[data-alert-resolve]");
      if (!button) return;
      resolveAlert(button.dataset.alertResolve).catch(error => {
        qs("alerts-summary").textContent = "resolve failed";
        console.error(error);
      });
    });
    document.getElementById("payout-audit-export").addEventListener("click", () => {
      exportPayoutAudit().catch(error => {
        qs("payout-audit-summary").textContent = "export failed";
        console.error(error);
      });
    });
    document.querySelectorAll("[data-history-range]").forEach((button) => {
      button.addEventListener("click", () => setHistoryRange(button.dataset.historyRange || "12h"));
    });
    document.querySelectorAll("nav button").forEach((button) => {
      button.addEventListener("click", () => {
        document.querySelectorAll("nav button").forEach((item) => item.setAttribute("aria-selected", "false"));
        button.setAttribute("aria-selected", "true");
        if (button.dataset.navHref) window.location.href = button.dataset.navHref;
        if (button.textContent === "Miners") qs("miner-address").focus();
        if (button.textContent === "Blocks") document.getElementById("blocks-body").scrollIntoView({ behavior: "smooth", block: "center" });
        if (button.textContent === "Payouts") document.getElementById("payments-body").scrollIntoView({ behavior: "smooth", block: "center" });
      });
    });

    refresh();
    setInterval(refresh, 15000);
  </script>
</body>
</html>"##
}

fn getting_started_html() -> &'static str {
    r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>CSD Pool Getting Started</title>
  <style>
    :root { color-scheme: dark; --bg: #0b1013; --panel: #11191d; --line: #223038; --text: #edf5f2; --muted: #91a09b; --teal: #31d0aa; --amber: #e0a642; }
    * { box-sizing: border-box; }
    body { margin: 0; background: var(--bg); color: var(--text); font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    .shell { width: min(1060px, calc(100vw - 32px)); margin: 0 auto; padding: 26px 0 34px; }
    header { display: flex; justify-content: space-between; gap: 16px; align-items: flex-start; border-bottom: 1px solid var(--line); padding-bottom: 18px; margin-bottom: 18px; }
    h1 { margin: 0; font-size: 28px; letter-spacing: 0; }
    .sub, .hint { color: var(--muted); font-size: 13px; line-height: 1.55; }
    a { color: var(--teal); text-decoration: none; }
    .grid { display: grid; grid-template-columns: minmax(0, 1.2fr) minmax(300px, .8fr); gap: 12px; align-items: start; }
    .panel { border: 1px solid var(--line); background: var(--panel); border-radius: 8px; padding: 16px; min-width: 0; }
    .stack { display: grid; gap: 12px; }
    h2 { margin: 0 0 12px; font-size: 15px; }
    .endpoint { font-size: 24px; color: var(--teal); font-weight: 800; word-break: break-word; }
    .row { display: flex; justify-content: space-between; gap: 12px; border-top: 1px solid rgba(255,255,255,.06); padding: 11px 0; }
    .row:first-child { border-top: 0; padding-top: 0; }
    .mono, code { font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace; }
    pre { margin: 0; overflow-x: auto; white-space: pre-wrap; word-break: break-word; }
    .cmd { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 10px; align-items: center; border-top: 1px solid rgba(255,255,255,.06); padding: 12px 0; }
    .cmd:first-of-type { border-top: 0; padding-top: 0; }
    button { border: 1px solid rgba(49,208,170,.4); border-radius: 7px; background: rgba(49,208,170,.12); color: var(--text); height: 34px; padding: 0 10px; cursor: pointer; font-weight: 750; }
    .badge { color: var(--teal); border: 1px solid rgba(49,208,170,.35); border-radius: 6px; padding: 4px 7px; font-size: 11px; font-weight: 800; }
    .badge.off { color: var(--muted); border-color: var(--line); }
    footer { color: var(--muted); font-size: 12px; margin-top: 14px; display: flex; justify-content: space-between; gap: 12px; }
    @media (max-width: 820px) { .grid, header { grid-template-columns: 1fr; display: grid; } .cmd { grid-template-columns: 1fr; } }
  </style>
</head>
<body>
  <div class="shell">
    <header>
      <div>
        <h1>CSD Pool Getting Started</h1>
        <div class="sub">Copy a command, replace <span class="mono">&lt;addr20&gt;</span>, and point miners at the pool.</div>
      </div>
      <nav><a href="/">Dashboard</a> · <a href="/status">Status</a> · <a href="/api/getting-started">JSON</a></nav>
    </header>

    <main class="grid">
      <section class="panel stack">
        <div>
          <h2>Stratum Endpoint</h2>
          <div class="endpoint mono" id="endpoint">loading...</div>
          <div class="hint">Worker username format: <span class="mono" id="username-format">&lt;addr20&gt;.&lt;worker&gt;</span></div>
        </div>
        <div>
          <h2>Copy Commands</h2>
          <div id="commands"><div class="hint">Loading command examples...</div></div>
        </div>
      </section>

      <aside class="stack">
        <section class="panel">
          <h2>Port Tiers</h2>
          <div id="ports"><div class="hint">Loading ports...</div></div>
        </section>
        <section class="panel">
          <h2>Payout Rules</h2>
          <div id="payout"><div class="hint">Loading payout rules...</div></div>
        </section>
        <section class="panel">
          <h2>Address And Worker Rules</h2>
          <div class="row"><span>Address</span><span class="hint" id="address-format"></span></div>
          <div class="row"><span>Worker</span><span class="hint" id="worker-rules"></span></div>
        </section>
      </aside>
    </main>

    <footer>
      <span>Miner and payment stats are available from the public dashboard.</span>
      <span class="mono">/api/miner/&lt;addr20&gt;</span>
    </footer>
  </div>
  <script>
    const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[char]));
    async function copyCommand(text, button) {
      await navigator.clipboard.writeText(text);
      const old = button.textContent;
      button.textContent = "Copied";
      setTimeout(() => button.textContent = old, 1200);
    }
    async function load() {
      const response = await fetch("/api/getting-started");
      const data = await response.json();
      document.getElementById("endpoint").textContent = data.stratum_endpoint;
      document.getElementById("username-format").textContent = data.username_format;
      document.getElementById("address-format").textContent = data.address_format;
      document.getElementById("worker-rules").textContent = data.worker_name_rules;
      document.getElementById("commands").innerHTML = data.commands.map((item, index) =>
        `<div class="cmd"><div><strong>${esc(item.label)}</strong><pre class="mono" id="cmd-${index}">${esc(item.command)}</pre></div><button type="button" data-copy="${index}">Copy</button></div>`
      ).join("");
      document.querySelectorAll("[data-copy]").forEach(button => {
        button.addEventListener("click", () => copyCommand(data.commands[Number(button.dataset.copy)].command, button));
      });
      document.getElementById("ports").innerHTML = data.port_tiers.map(port => {
        const badge = port.enabled ? "badge" : "badge off";
        const label = port.enabled ? "enabled" : "disabled";
        return `<div class="row"><span><strong>${esc(port.port)}</strong> · ${esc(port.label)}<br><span class="hint">starting difficulty ${esc(port.starting_difficulty)}</span></span><span class="${badge}">${label}</span></div>`;
      }).join("");
      document.getElementById("payout").innerHTML =
        `<div class="row"><span>Minimum payout</span><strong>${esc(data.payout.minimum_payout_csd)} CSD</strong></div>` +
        `<div class="row"><span>Payout interval</span><strong>${esc(data.payout.payout_interval_secs)} sec</strong></div>` +
        `<div class="row"><span>Confirmation depth</span><strong>${esc(data.payout.confirm_depth)}</strong></div>` +
        `<div class="row"><span>Pool fee</span><strong>${esc(data.payout.fee_percent)}%</strong></div>`;
    }
    load().catch(() => {
      document.getElementById("commands").innerHTML = `<div class="hint">Getting started data unavailable.</div>`;
    });
  </script>
</body>
</html>"##
}

fn status_html() -> &'static str {
    r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>CSD Pool Status</title>
  <style>
    :root { color-scheme: dark; --bg: #101418; --panel: #171d23; --line: #2a333d; --text: #f1f5f9; --muted: #9aa6b2; --ok: #31d0aa; --warn: #e0a642; --bad: #e06161; }
    * { box-sizing: border-box; }
    body { margin: 0; min-height: 100vh; font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: var(--bg); color: var(--text); }
    main { width: min(900px, calc(100% - 32px)); margin: 0 auto; padding: 42px 0; }
    header { display: flex; align-items: flex-start; justify-content: space-between; gap: 18px; border-bottom: 1px solid var(--line); padding-bottom: 24px; }
    h1 { margin: 0; font-size: 30px; line-height: 1.1; }
    .subtitle { margin-top: 8px; color: var(--muted); }
    .badge { border: 1px solid var(--line); border-radius: 999px; padding: 8px 12px; font-weight: 700; text-transform: uppercase; letter-spacing: .08em; font-size: 12px; white-space: nowrap; }
    .operational { color: var(--ok); border-color: rgba(49,208,170,.55); }
    .degraded { color: var(--warn); border-color: rgba(224,166,66,.6); }
    .down { color: var(--bad); border-color: rgba(224,97,97,.6); }
    .grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; margin-top: 24px; }
    .metric { background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 16px; min-height: 96px; }
    .label { color: var(--muted); font-size: 13px; }
    .value { margin-top: 12px; font-size: 26px; font-weight: 800; line-height: 1; overflow-wrap: anywhere; }
    section { margin-top: 24px; background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 18px; }
    dl { display: grid; grid-template-columns: 180px 1fr; gap: 10px 18px; margin: 0; }
    dt { color: var(--muted); }
    dd { margin: 0; overflow-wrap: anywhere; }
    @media (max-width: 720px) { header { display: block; } .badge { display: inline-block; margin-top: 16px; } .grid { grid-template-columns: repeat(2, minmax(0, 1fr)); } dl { grid-template-columns: 1fr; } }
  </style>
</head>
<body>
  <main>
    <header>
      <div>
        <h1>CSD Pool Status</h1>
        <div class="subtitle" id="summary">Loading public pool status...</div>
      </div>
      <div class="badge" id="badge">checking</div>
    </header>
    <div class="grid">
      <div class="metric"><div class="label">Workers Online</div><div class="value" id="workers">0</div></div>
      <div class="metric"><div class="label">Accepted Shares</div><div class="value" id="accepted">0</div></div>
      <div class="metric"><div class="label">Active Alerts</div><div class="value" id="alerts">0</div></div>
      <div class="metric"><div class="label">Unhealthy Services</div><div class="value" id="unhealthy">0</div></div>
    </div>
    <section>
      <dl>
        <dt>API</dt><dd id="api">unknown</dd>
        <dt>CSD nodes sampled</dt><dd id="nodes">0</dd>
        <dt>Payouts</dt><dd id="payouts">unknown</dd>
        <dt>Latest health sample</dt><dd id="sample">none</dd>
        <dt>Data source</dt><dd id="source">unknown</dd>
        <dt>Last updated</dt><dd id="updated">unknown</dd>
      </dl>
    </section>
  </main>
  <script>
    const qs = id => document.getElementById(id);
    const fmt = new Intl.NumberFormat();
    function setStatus(status) {
      const badge = qs("badge");
      badge.textContent = status || "unknown";
      badge.className = `badge ${status || "down"}`;
    }
    async function refresh() {
      try {
        const response = await fetch("/api/status", { cache: "no-store" });
        if (!response.ok) throw new Error(`status ${response.status}`);
        const status = await response.json();
        setStatus(status.status);
        qs("summary").textContent = status.status === "operational" ? "All public pool systems are reporting healthy." : "One or more pool systems need operator attention.";
        qs("workers").textContent = fmt.format(status.workers_online || 0);
        qs("accepted").textContent = fmt.format(status.shares_accepted || 0);
        qs("alerts").textContent = fmt.format(status.active_alerts || 0);
        qs("unhealthy").textContent = fmt.format(status.unhealthy_services || 0);
        qs("api").textContent = status.api_ok ? "online" : "offline";
        qs("nodes").textContent = fmt.format(status.node_count || 0);
        qs("payouts").textContent = status.payouts_enabled === null || status.payouts_enabled === undefined ? "not connected" : (status.payouts_enabled ? "enabled" : "paused");
        qs("sample").textContent = status.latest_sample_at || "none";
        qs("source").textContent = status.data_source || "unknown";
        qs("updated").textContent = new Date((status.updated_ts || 0) * 1000).toLocaleString();
      } catch (error) {
        setStatus("down");
        qs("summary").textContent = "Status API is unavailable.";
        console.error(error);
      }
    }
    refresh();
    setInterval(refresh, 30000);
  </script>
</body>
</html>"##
}

#[derive(Deserialize)]
struct HistoryQuery {
    range: Option<String>,
}

#[derive(Deserialize)]
struct OperatorAlertsQuery {
    status: Option<String>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
struct OperatorSessionsQuery {
    limit: Option<i64>,
}

#[derive(Deserialize)]
struct OperatorPayoutAuditQuery {
    batch_id: Option<String>,
    limit: Option<i64>,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    service: &'static str,
    release: ReleaseInfo,
}

#[derive(Serialize)]
struct OperatorSessionsResponse {
    release: ReleaseInfo,
    active_sessions: u64,
    versions: Vec<SessionVersionSummary>,
    sessions: Vec<RecentSession>,
}

#[derive(Clone, Serialize)]
struct ReleaseInfo {
    version: String,
    name: String,
    revision: String,
    timestamp_utc: String,
}

#[derive(Serialize)]
struct StatusResponse {
    status: &'static str,
    service: &'static str,
    release: ReleaseInfo,
    config: RuntimeConfigResponse,
    data_source: &'static str,
    api_ok: bool,
    workers_online: u64,
    shares_accepted: u64,
    shares_rejected: u64,
    shares_stale: u64,
    active_alerts: u64,
    unhealthy_services: u64,
    node_count: u64,
    payouts_enabled: Option<bool>,
    latest_sample_at: Option<String>,
    updated_ts: u64,
}

#[derive(Serialize)]
struct RuntimeConfigResponse {
    pool_id: String,
    mining_address: String,
    fee_percent: f64,
    confirm_depth: u64,
    stratum_listen: String,
    api_listen: String,
    signer_listen: String,
    initial_difficulty: f64,
    min_difficulty: f64,
    max_difficulty: f64,
    target_share_secs: u64,
    vardiff_ewma_alpha: f64,
    vardiff_raise_ratio: f64,
    vardiff_lower_ratio: f64,
    vardiff_min_adjust_secs: u64,
    vardiff_max_adjust_factor: f64,
    vardiff_transition_grace_secs: u64,
    minimum_payout_base_units: Option<String>,
    manual_payout_approval_base_units: Option<String>,
    max_payout_batch_base_units: Option<String>,
    max_daily_payout_base_units: Option<String>,
}

#[derive(Serialize)]
struct GettingStartedResponse {
    pool_name: &'static str,
    stratum_endpoint: String,
    username_format: &'static str,
    address_format: &'static str,
    worker_name_rules: &'static str,
    port_tiers: Vec<PortTier>,
    commands: Vec<CommandExample>,
    payout: PayoutRules,
    public_endpoints: Vec<String>,
}

#[derive(Serialize)]
struct PortTier {
    port: u16,
    label: String,
    starting_difficulty: f64,
    enabled: bool,
}

#[derive(Serialize)]
struct CommandExample {
    label: &'static str,
    command: String,
}

#[derive(Serialize)]
struct PayoutRules {
    minimum_payout_csd: String,
    payout_interval_secs: u64,
    confirm_depth: u64,
    fee_percent: f64,
}

#[derive(Clone, Serialize)]
struct PoolResponse {
    pool_hashrate_hs: f64,
    network_hashrate_hs: f64,
    network_share_pct: f64,
    round_effort_pct: f64,
    expected_block_secs: Option<f64>,
    total_blocks: u64,
    canonical_blocks: u64,
    immature_blocks: u64,
    orphaned_blocks: u64,
    avg_block_effort_pct_24h: f64,
    avg_block_effort_pct_7d: f64,
    avg_block_effort_pct_lifetime: f64,
    block_luck_pct_24h: f64,
    block_luck_pct_7d: f64,
    block_luck_pct_lifetime: f64,
    workers_online: u64,
    miners_online: u64,
    shares_accepted: u64,
    shares_rejected: u64,
    shares_stale: u64,
    pool_fee_pct: f64,
    payout_interval_secs: u64,
    next_payout_secs: u64,
    confirm_depth: u64,
    updated_ts: u64,
}

#[derive(Clone, Serialize)]
struct WorkerStats {
    hashrate_hs: f64,
    shares_accepted: u64,
    shares_rejected: u64,
    shares_stale: u64,
    blocks_found: u64,
    last_difficulty: f64,
    last_seen_ts: u64,
}

#[derive(Clone, Serialize)]
struct Totals {
    workers_online: u64,
    shares_accepted: u64,
    shares_rejected: u64,
    shares_stale: u64,
    blocks_found: u64,
}

#[derive(Serialize)]
struct MetricsResponse {
    workers: BTreeMap<String, WorkerStats>,
    totals: Totals,
    fee_revenue_csd: String,
}

#[derive(Clone, Serialize)]
struct HistoryResponse {
    interval_secs: u64,
    samples: Vec<HistorySample>,
}

#[derive(Clone, Serialize)]
struct HistorySample {
    ts: u64,
    pool_hs: f64,
    net_hs: f64,
    workers: u64,
    shares_accepted: u64,
    shares_rejected: u64,
    shares_stale: u64,
}

#[derive(Serialize)]
struct MinerResponse {
    address: String,
    online: bool,
    workers_online: u64,
    hashrate_hs: f64,
    pending_csd: String,
    pending_base_units: String,
    owed_csd: String,
    owed_base_units: String,
    paid_lifetime_csd: String,
    paid_lifetime_base_units: String,
    eta_secs: Option<u64>,
    csd_per_hour: Option<String>,
    csd_per_day: Option<String>,
    session_csd: Option<String>,
    session_secs: Option<u64>,
    shares_accepted: u64,
    shares_rejected: u64,
    shares_stale: u64,
    last_difficulty: f64,
    last_seen_ts: u64,
    confirming_blocks: Vec<BlockResponse>,
    payments: Vec<PaymentResponse>,
}

#[derive(Serialize)]
struct MinerWorkersResponse {
    address: String,
    workers: Vec<WorkerDetail>,
}

#[derive(Serialize)]
struct WorkerDetail {
    name: String,
    online: bool,
    hashrate_hs: f64,
    shares_accepted: u64,
    shares_rejected: u64,
    shares_stale: u64,
    blocks_found: u64,
    last_difficulty: f64,
    connected_at: Option<String>,
    last_seen_at: Option<String>,
}

#[derive(Clone, Serialize)]
struct BlockResponse {
    height: u64,
    hash: String,
    finder: String,
    worker: String,
    reward_csd: String,
    status: String,
    confirmations: u64,
    effort_pct: f64,
    found_at: String,
    confirmed_at: Option<String>,
}

#[derive(Clone, Serialize)]
struct PaymentResponse {
    batch_id: String,
    address: String,
    amount_csd: String,
    amount_base_units: String,
    txid: String,
    status: String,
    created_at: String,
    confirmed_at: Option<String>,
}

#[derive(Serialize)]
struct PaymentsResponse {
    payments: Vec<PaymentResponse>,
}

#[derive(Serialize)]
struct BlocksResponse {
    blocks: Vec<BlockResponse>,
}

#[derive(Serialize)]
struct OperatorPayoutStatusResponse {
    payouts_enabled: bool,
}

#[derive(Serialize)]
struct OperatorPayoutsResponse {
    batches: Vec<OperatorPayoutBatchResponse>,
}

#[derive(Serialize)]
struct OperatorPayoutAuditResponse {
    events: Vec<OperatorPayoutAuditEventResponse>,
}

#[derive(Serialize)]
struct OperatorPayoutAuditEventResponse {
    batch_id: String,
    actor: String,
    action: String,
    details: serde_json::Value,
    created_at: Option<String>,
}

#[derive(Serialize)]
struct OperatorPayoutPreviewResponse {
    minimum_payout_base_units: String,
    minimum_payout_csd: String,
    max_payout_batch_base_units: String,
    max_payout_batch_csd: String,
    max_daily_payout_base_units: String,
    max_daily_payout_csd: String,
    manual_payout_approval_base_units: String,
    manual_payout_approval_csd: String,
    daily_payout_used_base_units: String,
    daily_payout_used_csd: String,
    daily_remaining_base_units: String,
    daily_remaining_csd: String,
    recipient_count: usize,
    total_base_units: String,
    total_csd: String,
    would_create_batch: bool,
    cap_exceeded: bool,
    daily_cap_exceeded: bool,
    manual_approval_required: bool,
    recipients: Vec<OperatorPayoutRecipientResponse>,
}

#[derive(Serialize)]
struct OperatorHealthResponse {
    ok: bool,
    samples: Vec<NodeSampleRecord>,
}

#[derive(Serialize)]
struct OperatorAlertsResponse {
    alerts: Vec<AlertEvent>,
}

#[derive(Serialize)]
struct OperatorResolveAlertResponse {
    resolved: bool,
}

#[derive(Deserialize)]
struct CancelPayoutRequest {
    reason: Option<String>,
}

#[derive(Serialize)]
struct OperatorRetryPayoutResponse {
    retried: bool,
    new_batch_id: String,
    batch: Option<OperatorPayoutBatchResponse>,
}

#[derive(Serialize)]
struct OperatorApprovePayoutResponse {
    approved: bool,
}

#[derive(Serialize)]
struct OperatorCancelPayoutResponse {
    cancelled: bool,
    batch: Option<OperatorPayoutBatchResponse>,
}

#[derive(Serialize)]
struct OperatorPayoutBatchResponse {
    batch_id: String,
    status: String,
    total_base_units: String,
    total_csd: String,
    txid: Option<String>,
    raw_tx_hash: Option<String>,
    recipients: Vec<OperatorPayoutRecipientResponse>,
}

#[derive(Serialize)]
struct OperatorPayoutRecipientResponse {
    miner: String,
    address: String,
    amount_base_units: String,
    amount_csd: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    #[test]
    fn normalizes_addr20() {
        assert_eq!(
            normalize_addr20("0xABCDEFabcdefABCDEFabcdefABCDEFabcdefABCD").unwrap(),
            "abcdefabcdefabcdefabcdefabcdefabcdefabcd"
        );
        assert!(normalize_addr20("bad").is_none());
    }

    #[test]
    fn constant_time_eq_handles_equal_mismatch_and_length_difference() {
        assert!(constant_time_eq(b"operator-secret", b"operator-secret"));
        assert!(!constant_time_eq(b"operator-secret", b"operator-secreu"));
        assert!(!constant_time_eq(
            b"operator-secret",
            b"operator-secret-long"
        ));
        assert!(!constant_time_eq(
            b"operator-secret-long",
            b"operator-secret"
        ));
    }

    #[test]
    fn demo_state_matches_public_api_contract() {
        let state = AppState::demo();
        let pool = state.pool_response(None, None);
        assert_eq!(pool.confirm_depth, 10);
        assert_eq!(pool.payout_interval_secs, 1800);
        assert_eq!(pool.workers_online, 1);
    }

    #[test]
    fn pool_response_reports_active_stratum_sessions_as_workers() {
        let pool_state = SharedPoolState::new();
        pool_state.record_authorized_worker("0123456789abcdef0123456789abcdef01234567");
        let _first = pool_state.connection_guard();
        let _second = pool_state.connection_guard();
        let state = AppState::from_pool_state(pool_state, ApiSettings::default(), None);

        let pool = state.pool_response(None, None);

        assert_eq!(pool.workers_online, 2);
        assert_eq!(pool.miners_online, 1);
    }

    #[test]
    fn pool_response_uses_persisted_recent_worker_counts() {
        let state = AppState::demo();
        let pool = state.pool_response(
            Some(&DashboardPoolStats {
                workers_online: 220,
                miners_online: 1,
                ..DashboardPoolStats::default()
            }),
            None,
        );

        assert_eq!(pool.workers_online, 220);
        assert_eq!(pool.miners_online, 1);
    }

    #[test]
    fn pool_response_can_use_persistent_block_stats() {
        let state = AppState::demo();
        let pool = state.pool_response(
            Some(&DashboardPoolStats {
                total_blocks: 3,
                canonical_blocks: 2,
                immature_blocks: 0,
                orphaned_blocks: 1,
                fee_revenue_base_units: 50_000_000,
                latest_payout_created_ts: 0,
                avg_block_effort_pct_24h: 50.0,
                avg_block_effort_pct_7d: 125.0,
                avg_block_effort_pct_lifetime: 100.0,
                ..DashboardPoolStats::default()
            }),
            None,
        );

        assert_eq!(pool.total_blocks, 3);
        assert_eq!(pool.canonical_blocks, 2);
        assert_eq!(pool.orphaned_blocks, 1);
        assert_eq!(pool.avg_block_effort_pct_24h, 50.0);
        assert_eq!(pool.block_luck_pct_24h, 200.0);
        assert_eq!(pool.block_luck_pct_7d, 80.0);
        assert_eq!(pool.block_luck_pct_lifetime, 100.0);
    }

    #[test]
    fn pool_response_uses_latest_payout_for_countdown() {
        let state = AppState::demo();
        let latest_payout_created_ts = now_ts().saturating_sub(600);
        let pool = state.pool_response(
            Some(&DashboardPoolStats {
                total_blocks: 0,
                canonical_blocks: 0,
                immature_blocks: 0,
                orphaned_blocks: 0,
                fee_revenue_base_units: 0,
                latest_payout_created_ts,
                ..DashboardPoolStats::default()
            }),
            None,
        );

        assert!(pool.next_payout_secs <= 1200);
        assert!(pool.next_payout_secs >= 1198);
    }

    #[test]
    fn pool_response_countdown_reaches_zero_after_interval() {
        let state = AppState::demo();
        let latest_payout_created_ts = now_ts().saturating_sub(2000);
        let pool = state.pool_response(
            Some(&DashboardPoolStats {
                total_blocks: 0,
                canonical_blocks: 0,
                immature_blocks: 0,
                orphaned_blocks: 0,
                fee_revenue_base_units: 0,
                latest_payout_created_ts,
                ..DashboardPoolStats::default()
            }),
            None,
        );

        assert_eq!(pool.next_payout_secs, 0);
    }

    #[test]
    fn pool_response_uses_network_telemetry_when_available() {
        let state = AppState::demo();
        let pool = state.pool_response(
            None,
            Some(&NetworkTelemetry {
                hashrate_hs: 2_500_000_000_000.0,
                target_block_secs: 120.0,
            }),
        );

        assert_eq!(pool.network_hashrate_hs, 2_500_000_000_000.0);
        assert_eq!(pool.expected_block_secs, None);
    }

    #[test]
    fn pool_response_estimates_current_round_effort() {
        let state = AppState::demo();
        state.pool_state.record_share_accepted(
            "0123456789abcdef0123456789abcdef01234567",
            10.0,
            false,
        );
        let pool = state.pool_response(
            None,
            Some(&NetworkTelemetry {
                hashrate_hs: 4_294_967_296.0,
                target_block_secs: 100.0,
            }),
        );

        assert_eq!(pool.round_effort_pct, 10.0);
    }

    #[test]
    fn round_effort_requires_network_telemetry() {
        assert_eq!(round_effort_pct(10.0, None), 0.0);
        assert_eq!(
            round_effort_pct(
                10.0,
                Some(&NetworkTelemetry {
                    hashrate_hs: 0.0,
                    target_block_secs: 120.0,
                }),
            ),
            0.0
        );
    }

    #[test]
    fn prometheus_metrics_exports_persistent_operational_counters() {
        let state = AppState::demo();
        state.pool_state.record_share_accepted(
            "0123456789abcdef0123456789abcdef01234567",
            8.0,
            false,
        );
        state.pool_state.record_job_notify("tip_change");
        state.pool_state.record_job_notify("heartbeat");
        let text = state.prometheus_metrics(
            Some(&DashboardPoolStats {
                total_blocks: 7,
                canonical_blocks: 5,
                immature_blocks: 1,
                orphaned_blocks: 1,
                fee_revenue_base_units: 25_000_000,
                latest_payout_created_ts: now_ts().saturating_sub(300),
                jobs_created: 42,
                latest_job_created_ts: now_ts().saturating_sub(15),
                payout_batches_needs_approval: 1,
                payout_batches_created: 2,
                payout_batches_signed: 3,
                payout_batches_submitted: 4,
                payout_batches_confirmed: 5,
                payout_batches_failed: 6,
                payout_batches_cancelled: 7,
                payout_amount_base_units_total: 123_000_000,
                ..DashboardPoolStats::default()
            }),
            &[],
        );

        assert!(text.contains("csd_pool_blocks_submitted_total 7"));
        assert!(text.contains("csd_pool_blocks_confirmed_total 5"));
        assert!(text.contains("csd_pool_blocks_orphaned_total 1"));
        assert!(text.contains("csd_pool_jobs_created_total 42"));
        assert!(text.contains("csd_pool_job_age_seconds "));
        assert!(text.contains("csd_pool_payout_batches_total{status=\"needs_approval\"} 1"));
        assert!(text.contains("csd_pool_payout_batches_total{status=\"submitted\"} 4"));
        assert!(text.contains("csd_pool_payout_amount_base_units_total 123000000"));
        assert!(text.contains("csd_pool_fee_revenue_base_units 25000000"));
        assert!(text.contains("csd_pool_round_share_difficulty 8.000000"));
        assert!(text.contains("csd_pool_job_notify_total{reason=\"tip_change\"} 1"));
        assert!(text.contains("csd_pool_job_notify_total{reason=\"heartbeat\"} 1"));
        assert!(text.contains("csd_pool_job_notify_age_seconds "));
    }

    #[test]
    fn prometheus_metrics_exports_latest_service_health_samples() {
        let state = AppState::demo();
        let text = state.prometheus_metrics(
            None,
            &[
                NodeSampleRecord {
                    node_name: "node:node-a".to_owned(),
                    height: Some(123),
                    chainwork: None,
                    peers: Some(8),
                    mempool_size: None,
                    rpc_ms: Some(42.5),
                    ok: true,
                    sampled_at: Some("2026-06-16 10:00:00+00".to_owned()),
                },
                NodeSampleRecord {
                    node_name: "signer\"primary".to_owned(),
                    height: None,
                    chainwork: None,
                    peers: None,
                    mempool_size: None,
                    rpc_ms: Some(12.0),
                    ok: false,
                    sampled_at: Some("2026-06-16 10:00:00+00".to_owned()),
                },
            ],
        );

        assert!(text.contains("csd_pool_service_up{service=\"node:node-a\"} 1"));
        assert!(text.contains("csd_node_rpc_latency_seconds{node=\"node-a\"} 0.042500"));
        assert!(text.contains("csd_node_height{node=\"node-a\"} 123"));
        assert!(text.contains("csd_node_peers{node=\"node-a\"} 8"));
        assert!(text.contains("csd_pool_service_up{service=\"signer\\\"primary\"} 0"));
    }

    #[test]
    fn operator_payout_response_formats_exact_amounts() {
        let response = operator_payout_batch_response(PayoutBatchRecord {
            batch_id: "batch-1".to_owned(),
            status: "created".to_owned(),
            total_base_units: 125_000_000,
            txid: None,
            raw_tx_hash: None,
            recipients: vec![csd_pool_accounting::PayoutRecipient {
                miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                address: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                amount_base_units: 125_000_000,
            }],
        });

        assert_eq!(response.total_csd, "1.25000000");
        assert_eq!(response.recipients[0].amount_base_units, "125000000");
    }

    #[test]
    fn operator_payout_preview_response_formats_selection() {
        let response = operator_payout_preview_response(
            100_000_000,
            1_000_000_000,
            5_000_000_000,
            1_000_000_000,
            1_000_000_000,
            PayoutSelection {
                recipients: vec![PayoutRecipient {
                    miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    address: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    amount_base_units: 250_000_000,
                }],
                total_base_units: 250_000_000,
            },
        );

        assert_eq!(response.minimum_payout_csd, "1.00000000");
        assert_eq!(response.max_payout_batch_csd, "10.00000000");
        assert_eq!(response.max_daily_payout_csd, "50.00000000");
        assert_eq!(response.manual_payout_approval_csd, "10.00000000");
        assert_eq!(response.daily_remaining_csd, "40.00000000");
        assert_eq!(response.recipient_count, 1);
        assert_eq!(response.total_csd, "2.50000000");
        assert!(response.would_create_batch);
        assert!(!response.cap_exceeded);
        assert!(!response.manual_approval_required);
        assert_eq!(response.recipients[0].amount_base_units, "250000000");

        let capped = operator_payout_preview_response(
            100_000_000,
            200_000_000,
            5_000_000_000,
            1_000_000_000,
            1_000_000_000,
            PayoutSelection {
                recipients: vec![PayoutRecipient {
                    miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    address: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    amount_base_units: 250_000_000,
                }],
                total_base_units: 250_000_000,
            },
        );
        assert!(capped.cap_exceeded);
        assert!(!capped.would_create_batch);

        let daily_capped = operator_payout_preview_response(
            100_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            900_000_000,
            PayoutSelection {
                recipients: vec![PayoutRecipient {
                    miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    address: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    amount_base_units: 250_000_000,
                }],
                total_base_units: 250_000_000,
            },
        );
        assert!(daily_capped.daily_cap_exceeded);
        assert!(!daily_capped.would_create_batch);

        let approval_required = operator_payout_preview_response(
            100_000_000,
            1_000_000_000,
            5_000_000_000,
            200_000_000,
            0,
            PayoutSelection {
                recipients: vec![PayoutRecipient {
                    miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    address: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    amount_base_units: 250_000_000,
                }],
                total_base_units: 250_000_000,
            },
        );
        assert!(approval_required.manual_approval_required);
        assert!(!approval_required.would_create_batch);
    }

    #[test]
    fn payout_csv_exports_one_row_per_recipient() {
        let csv = payout_batches_csv(&[PayoutBatchRecord {
            batch_id: "batch-1".to_owned(),
            status: "created".to_owned(),
            total_base_units: 125_000_000,
            txid: Some("tx,quoted".to_owned()),
            raw_tx_hash: None,
            recipients: vec![csd_pool_accounting::PayoutRecipient {
                miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                address: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                amount_base_units: 125_000_000,
            }],
        }]);

        assert!(csv.starts_with("batch_id,status,txid,recipient"));
        assert!(csv.contains("\"tx,quoted\""));
        assert!(csv.contains("1.25000000"));
    }

    #[test]
    fn payout_audit_csv_exports_event_details() {
        let csv = payout_audit_events_csv(&[PayoutAuditEvent {
            batch_id: "batch-1".to_owned(),
            actor: "operator".to_owned(),
            action: "cancel".to_owned(),
            details: serde_json::json!({ "reason": "manual, quoted" }),
            created_at: Some("2026-06-16 10:00:00+00".to_owned()),
        }]);

        assert!(csv.starts_with("created_at,batch_id,actor,action,details_json"));
        assert!(csv.contains("batch-1,operator,cancel"));
        assert!(csv.contains("\"{\"\"reason\"\":\"\"manual, quoted\"\"}\""));
    }

    #[tokio::test]
    async fn pool_endpoint_reads_shared_pool_state() {
        let state = SharedPoolState::new();
        state.record_authorized_worker("0123456789abcdef0123456789abcdef01234567");
        state.record_share_accepted("0123456789abcdef0123456789abcdef01234567", 8.0, false);
        let app = router_from_pool_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/pool")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["workers_online"], 1);
        assert_eq!(json["shares_accepted"], 1);
    }

    #[tokio::test]
    async fn history_endpoint_reads_shared_pool_state_without_database() {
        let state = SharedPoolState::new();
        state.record_authorized_worker("0123456789abcdef0123456789abcdef01234567");
        state.record_share_accepted("0123456789abcdef0123456789abcdef01234567", 8.0, false);
        state.record_share_rejected("0123456789abcdef0123456789abcdef01234567");
        state.record_share_stale("0123456789abcdef0123456789abcdef01234567");
        let app = router_from_pool_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/history?range=12h")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["interval_secs"], 60);
        assert_eq!(json["samples"][0]["workers"], 1);
        assert_eq!(json["samples"][0]["shares_accepted"], 1);
        assert_eq!(json["samples"][0]["shares_rejected"], 1);
        assert_eq!(json["samples"][0]["shares_stale"], 1);
    }

    #[test]
    fn history_window_selects_operational_buckets() {
        assert_eq!(history_window(Some("12h"), 60), (12 * 60 * 60, 60));
        assert_eq!(history_window(Some("24h"), 30), (24 * 60 * 60, 60));
        assert_eq!(history_window(Some("7d"), 60), (7 * 24 * 60 * 60, 15 * 60));
        assert_eq!(
            history_window(Some("30d"), 60),
            (30 * 24 * 60 * 60, 60 * 60)
        );
    }

    #[tokio::test]
    async fn prometheus_metrics_endpoint_exports_aggregate_counters() {
        let state = SharedPoolState::new();
        state.record_authorized_worker("0123456789abcdef0123456789abcdef01234567");
        state.record_share_accepted("0123456789abcdef0123456789abcdef01234567", 8.0, true);
        state.record_share_rejected("0123456789abcdef0123456789abcdef01234567");
        state.record_share_validation(std::time::Duration::from_millis(20));
        state.record_share_validation(std::time::Duration::from_millis(30));
        state.record_candidate_propagation(
            std::time::Duration::from_millis(2),
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(3),
            std::time::Duration::from_millis(25),
            Some(std::time::Duration::from_millis(15)),
            Some(std::time::Duration::from_millis(16)),
        );
        let app = router_from_pool_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            "text/plain; version=0.0.4; charset=utf-8"
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("csd_pool_workers_online 1"));
        assert!(text.contains("csd_pool_stratum_connections 0"));
        assert!(text.contains("csd_pool_shares_total{result=\"accepted\"} 1"));
        assert!(text.contains("csd_pool_shares_total{result=\"rejected\"} 1"));
        assert!(text.contains("csd_pool_share_validation_seconds_sum 0.050000000"));
        assert!(text.contains("csd_pool_share_validation_seconds_count 2"));
        assert!(text.contains("csd_pool_share_validation_seconds_avg 0.025000000"));
        assert!(text.contains(
            "csd_pool_candidate_propagation_seconds_sum{phase=\"detected_to_submit_start\"} 0.002000000"
        ));
        assert!(
            text.contains(
                "csd_pool_candidate_propagation_seconds_count{phase=\"candidate_total\"} 1"
            )
        );
        assert!(text.contains(
            "csd_pool_candidate_propagation_seconds_max{phase=\"relay_enqueue\"} 0.016000000"
        ));
        assert!(text.contains("csd_pool_blocks_found_total 1"));
        assert!(text.contains("csd_pool_next_payout_seconds 1800"));
    }

    #[tokio::test]
    async fn public_status_endpoint_does_not_require_operator_token() {
        let state = SharedPoolState::new();
        state.record_authorized_worker("0123456789abcdef0123456789abcdef01234567");
        state.record_share_accepted("0123456789abcdef0123456789abcdef01234567", 8.0, false);
        let app = router_from_pool_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "operational");
        assert_eq!(json["data_source"], "memory");
        assert_eq!(json["release"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(json["release"]["name"].is_string());
        assert!(json["release"]["revision"].is_string());
        assert_eq!(json["config"]["pool_id"], "csd-main");
        assert_eq!(
            json["config"]["mining_address"],
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(json["config"]["stratum_listen"], "127.0.0.1:3333");
        assert_eq!(json["config"]["api_listen"], "127.0.0.1:8080");
        assert_eq!(json["config"]["minimum_payout_base_units"], "100000000");
        assert_eq!(json["workers_online"], 1);
        assert_eq!(json["shares_accepted"], 1);
    }

    #[tokio::test]
    async fn status_page_serves_public_status_ui() {
        let app = router_from_pool_state(SharedPoolState::new());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(html.contains("CSD Pool Status"));
        assert!(html.contains("/api/status"));
        assert!(html.contains("Workers Online"));
    }

    #[tokio::test]
    async fn api_responses_include_security_headers() {
        let app = router_from_pool_state(SharedPoolState::new());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/pool")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["x-frame-options"], "DENY");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert_eq!(
            response.headers()["permissions-policy"],
            "camera=(), microphone=(), geolocation=()"
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            response.headers()["content-security-policy"],
            "default-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self'"
        );
    }

    #[tokio::test]
    async fn dashboard_route_serves_public_pool_ui() {
        let app = router_from_pool_state(SharedPoolState::new());
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(html.contains("<title>CSD Pool Dashboard</title>"));
        assert!(html.contains("data-nav-href=\"/getting-started\""));
        assert!(html.contains("Shares And Pool Activity"));
        assert!(html.contains("Miner Address"));
        assert!(html.contains("lookupMiner"));
        assert!(html.contains("Operator bearer token"));
        assert!(html.contains("refreshOperator"));
        assert!(html.contains("csd_pool_operator_token"));
        assert!(html.contains("data-history-range=\"12h\""));
        assert!(html.contains("data-history-range=\"24h\""));
        assert!(html.contains("data-history-range=\"7d\""));
        assert!(html.contains("/api/history?range=${encodeURIComponent(state.historyRange)}"));
        assert!(html.contains("setHistoryRange"));
        assert!(html.contains("history.samples"));
        assert!(html.contains("<th>Effort</th>"));
        assert!(html.contains("<th>Conf</th>"));
        assert!(html.contains("block.effort_pct"));
        assert!(html.contains("block.confirmations"));
        assert!(html.contains("block.worker || shortHash(block.finder)"));
        assert!(html.contains("const esc = (value)"));
        assert!(html.contains("${esc(sample.node_name)}"));
        assert!(html.contains("${esc(alert.message)}"));
        assert!(html.contains("${esc(shortHash(payment.address))}"));
        assert!(html.contains("data-alert-resolve"));
        assert!(html.contains("/api/operator/alerts/${encodeURIComponent(fingerprint)}/resolve"));
        assert!(html.contains("Payout Control"));
        assert!(html.contains("/api/operator/payouts/status"));
        assert!(html.contains("data-payout-control"));
        assert!(html.contains("Payout Batches"));
        assert!(html.contains("data-payout-action=\"approve\""));
        assert!(html.contains("Payout Audit"));
        assert!(html.contains("/api/operator/payouts/audit?limit=20"));
        assert!(html.contains("/api/operator/payouts/audit/export.csv?limit=1000"));
        assert!(html.contains("/api/operator/payouts/${encodeURIComponent(batchId)}/${action}"));
        assert!(html.contains("getJson(\"/api/pool\")"));
        assert!(html.contains("getJson(\"/api/payments\")"));
        assert!(html.contains("shortHash(payment.address)"));
    }

    #[tokio::test]
    async fn getting_started_routes_serve_copyable_miner_setup() {
        let app = router_from_pool_state(SharedPoolState::new());
        let html_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/getting-started")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(html_response.status(), StatusCode::OK);
        let html_body = axum::body::to_bytes(html_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = std::str::from_utf8(&html_body).unwrap();
        assert!(html.contains("<title>CSD Pool Getting Started</title>"));
        assert!(html.contains("/api/getting-started"));
        assert!(html.contains("data-copy"));
        assert!(html.contains("Worker username format"));

        let json_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/getting-started")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(json_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["stratum_endpoint"], "127.0.0.1:3333");
        assert_eq!(json["username_format"], "<addr20>.<worker>");
        assert!(
            json["commands"][0]["command"]
                .as_str()
                .unwrap()
                .contains("--user <addr20>.rig-01")
        );
        assert_eq!(json["port_tiers"][0]["port"], 3333);
        assert_eq!(json["payout"]["minimum_payout_csd"], "1.0");
    }

    #[test]
    fn default_api_listen_is_localhost() {
        if std::env::var("CSD_POOL_CONFIG").is_err()
            && std::env::var("CSD_POOL_API_LISTEN").is_err()
        {
            assert_eq!(api_listen(), "127.0.0.1:8080");
        }
    }

    #[test]
    fn live_mode_rejects_missing_persistent_database() {
        let error = require_persistent_database(None, Some("live"), None).unwrap_err();
        assert!(matches!(error, ApiStartupError::MissingDatabase));
        assert!(
            require_persistent_database(None, Some("static"), None)
                .unwrap()
                .is_none()
        );
    }
}
