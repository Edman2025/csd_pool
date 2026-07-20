use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use csd_pool_node::{
    BlockStatusResponse, NetworkSnapshot, NodeHealth, NodeMiningTemplate, SubmitBlockResponse,
    SubmitTxResponse, TransactionStatusResponse,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::info;

const DEFAULT_REWARD_BASE_UNITS: u128 = 5_000_000_000;

#[derive(Clone, Default)]
struct MockState {
    blocks: Arc<Mutex<BTreeMap<String, BlockStatusResponse>>>,
    txs: Arc<Mutex<BTreeMap<String, TransactionStatusResponse>>>,
    easy_candidates: bool,
}

pub async fn run_mock_node_server(listen: SocketAddr) -> std::io::Result<()> {
    let app = router();
    let listener = tokio::net::TcpListener::bind(listen).await?;
    info!(%listen, "csd pool mock node listening");
    axum::serve(listener, app).await
}

pub fn router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/network", get(network))
        .route("/api/rpc/mining/template", get(mining_template))
        .route("/api/rpc/block/submit", post(submit_block))
        .route("/api/rpc/block/status", get(block_status))
        .route("/api/rpc/tx/submit", post(submit_tx))
        .route("/tx/submit", post(submit_tx))
        .route("/api/rpc/tx/status", get(tx_status))
        .with_state(MockState {
            easy_candidates: std::env::var("CSD_POOL_MOCK_NODE_EASY_CANDIDATES").as_deref()
                == Ok("true"),
            ..MockState::default()
        })
}

pub fn mock_node_listen() -> String {
    std::env::var("CSD_POOL_MOCK_NODE_LISTEN").unwrap_or_else(|_| "127.0.0.1:8790".into())
}

async fn health() -> Json<NodeHealth> {
    Json(NodeHealth {
        height: Some(42),
        tip: Some("11".repeat(32)),
        chainwork: Some("ff".repeat(16)),
        peers: Some(8),
        extra: serde_json::json!({ "service": "csd-pool-mock-node" }),
    })
}

async fn network() -> Json<NetworkSnapshot> {
    Json(NetworkSnapshot {
        hashrate: 1_000_000_000_000.0,
        hashrate_ghs: 1_000.0,
        target_block_secs: 120,
        extra: serde_json::json!({}),
    })
}

async fn mining_template(
    State(state): State<MockState>,
    Query(query): Query<TemplateQuery>,
) -> Json<NodeMiningTemplate> {
    Json(NodeMiningTemplate {
        job_id: format!("mock-job-{}", &query.address[..query.address.len().min(8)]),
        prev_hash_be_hex: "00".repeat(32),
        coinb1_hex: "aa".to_owned(),
        coinb2_hex: "bb".to_owned(),
        merkle_branches_hex: Vec::new(),
        version_hex: "20000000".to_owned(),
        nbits_hex: "207fffff".to_owned(),
        ntime_hex: "665544cc".to_owned(),
        clean_jobs: true,
        share_target_hex: "ff".repeat(32),
        network_target_hex: if state.easy_candidates {
            "ff".repeat(32)
        } else {
            "00".repeat(32)
        },
    })
}

async fn submit_block(
    State(state): State<MockState>,
    Json(payload): Json<serde_json::Value>,
) -> Json<SubmitBlockResponse> {
    let request_started = Instant::now();
    let request_received_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    let hash = payload
        .get("hash_hex")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .or_else(|| {
            payload
                .get("block")
                .and_then(|value| value.as_str())
                .map(sha256d_hex)
        })
        .unwrap_or_else(|| sha256d_hex(&payload.to_string()));

    let status = BlockStatusResponse {
        hash: hash.clone(),
        status: "confirmed".to_owned(),
        height: Some(43),
        confirmations: 12,
        reward_base_units: Some(DEFAULT_REWARD_BASE_UNITS),
        extra: serde_json::json!({ "source": "csd-pool-mock-node" }),
    };
    state
        .blocks
        .lock()
        .expect("block lock")
        .insert(hash.clone(), status);
    let accept_elapsed_us: u64 = request_started
        .elapsed()
        .as_micros()
        .try_into()
        .unwrap_or(u64::MAX);
    let relay_enqueue_elapsed_us: u64 = request_started
        .elapsed()
        .as_micros()
        .try_into()
        .unwrap_or(u64::MAX);
    Json(SubmitBlockResponse {
        ok: true,
        hash: Some(hash),
        extra: serde_json::json!({
            "source": "csd-pool-mock-node",
            "node_observability": {
                "request_received_unix_ms": request_received_unix_ms,
                "accept_elapsed_us": accept_elapsed_us,
                "relay_enqueue_elapsed_us": relay_enqueue_elapsed_us,
                "relay_queued": true,
            },
        }),
    })
}

async fn block_status(
    State(state): State<MockState>,
    Query(query): Query<HashQuery>,
) -> Json<BlockStatusResponse> {
    let status = state
        .blocks
        .lock()
        .expect("block lock")
        .get(&query.hash)
        .cloned()
        .unwrap_or_else(|| BlockStatusResponse {
            hash: query.hash,
            status: "submitted".to_owned(),
            height: None,
            confirmations: 0,
            reward_base_units: None,
            extra: serde_json::json!({ "source": "csd-pool-mock-node" }),
        });
    Json(status)
}

async fn submit_tx(
    State(state): State<MockState>,
    Json(payload): Json<SubmitTxPayload>,
) -> Json<SubmitTxResponse> {
    let tx_material = payload
        .tx
        .map(|tx| tx.to_string())
        .or(payload.raw_tx_hex)
        .unwrap_or_default();
    let txid = sha256d_hex(&tx_material);
    let status = TransactionStatusResponse {
        txid: txid.clone(),
        status: "confirmed".to_owned(),
        confirmations: 6,
        extra: serde_json::json!({ "source": "csd-pool-mock-node" }),
    };
    state
        .txs
        .lock()
        .expect("tx lock")
        .insert(txid.clone(), status);
    Json(SubmitTxResponse {
        ok: true,
        txid: Some(txid),
        extra: serde_json::json!({ "source": "csd-pool-mock-node" }),
    })
}

async fn tx_status(
    State(state): State<MockState>,
    Query(query): Query<TxQuery>,
) -> Json<TransactionStatusResponse> {
    let status = state
        .txs
        .lock()
        .expect("tx lock")
        .get(&query.txid)
        .cloned()
        .unwrap_or_else(|| TransactionStatusResponse {
            txid: query.txid,
            status: "submitted".to_owned(),
            confirmations: 0,
            extra: serde_json::json!({ "source": "csd-pool-mock-node" }),
        });
    Json(status)
}

fn sha256d_hex(value: &str) -> String {
    let first = Sha256::digest(value.as_bytes());
    let second = Sha256::digest(first);
    hex::encode(second)
}

#[derive(Deserialize)]
struct TemplateQuery {
    address: String,
}

#[derive(Deserialize)]
struct HashQuery {
    hash: String,
}

#[derive(Deserialize)]
struct TxQuery {
    txid: String,
}

#[derive(Deserialize)]
struct SubmitTxPayload {
    #[serde(default)]
    raw_tx_hex: Option<String>,
    #[serde(default)]
    tx: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_template_and_block_lifecycle_contract() {
        let app = router();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(
                        "/api/rpc/mining/template?address=0123456789abcdef0123456789abcdef01234567",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let template: NodeMiningTemplate = serde_json::from_slice(&body).unwrap();
        assert_eq!(template.share_target_hex, "ff".repeat(32));

        let submit = serde_json::json!({
            "job_id": template.job_id,
            "worker_name": "rig-a",
            "header_hex": "aa".repeat(84),
            "hash_hex": "22".repeat(32),
            "coinbase_txid_hex": "33".repeat(32),
            "merkle_root_hex": "44".repeat(32),
            "extranonce2_hex": "01020304",
            "ntime_hex": "665544cc",
            "nonce_hex": "00000001"
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/rpc/block/submit")
                    .header("content-type", "application/json")
                    .body(Body::from(submit.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let submitted: SubmitBlockResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(submitted.hash, Some("22".repeat(32)));
        assert_eq!(submitted.extra["node_observability"]["relay_queued"], true);
        assert!(
            submitted.extra["node_observability"]["request_received_unix_ms"]
                .as_u64()
                .is_some()
        );
        assert!(
            submitted.extra["node_observability"]["accept_elapsed_us"]
                .as_u64()
                .is_some()
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/rpc/block/status?hash={}",
                        submitted.hash.unwrap()
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: BlockStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.status, "confirmed");
        assert_eq!(status.reward_base_units, Some(DEFAULT_REWARD_BASE_UNITS));
    }

    #[tokio::test]
    async fn serves_transaction_submit_and_status_contract() {
        let app = router();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tx/submit")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "tx": {
                                "version": 1,
                                "inputs": [],
                                "outputs": [],
                                "locktime": 0,
                                "app": "None"
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let submitted: SubmitTxResponse = serde_json::from_slice(&body).unwrap();
        let txid = submitted.txid.unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/rpc/tx/status?txid={txid}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: TransactionStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.status, "confirmed");
        assert_eq!(status.confirmations, 6);
    }
}
