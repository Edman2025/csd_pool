#![allow(clippy::collapsible_if)] // Production remains on Rust 1.86, before stable let chains.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

#[derive(Clone, Debug)]
pub struct SignerSettings {
    pub token: Option<String>,
    pub mode: String,
    pub wallet_address: Option<String>,
}

impl SignerSettings {
    pub fn from_env() -> Self {
        Self {
            token: signer_token(),
            mode: std::env::var("CSD_POOL_SIGNER_MODE").unwrap_or_else(|_| "mock".to_owned()),
            wallet_address: signer_wallet_address(),
        }
    }
}

pub async fn run_signer_server(
    listen: SocketAddr,
    settings: SignerSettings,
) -> std::io::Result<()> {
    let app = router(settings);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    info!(%listen, "csd pool signer listening");
    axum::serve(listener, app).await
}

pub fn router(settings: SignerSettings) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/payout/sign", post(sign_payout))
        .with_state(Arc::new(settings))
}

pub fn signer_listen() -> String {
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        if let Ok(config) = csd_pool_config::PoolConfig::from_file(path) {
            return config.signer.listen;
        }
    }
    std::env::var("CSD_POOL_SIGNER_LISTEN").unwrap_or_else(|_| "127.0.0.1:8890".into())
}

fn signer_token() -> Option<String> {
    let env_name = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.signer.token_env)
            .unwrap_or_else(|| "CSD_POOL_SIGNER_TOKEN".to_owned())
    } else {
        "CSD_POOL_SIGNER_TOKEN".to_owned()
    };
    std::env::var(env_name)
        .ok()
        .filter(|value| !value.is_empty())
}

fn signer_wallet_address() -> Option<String> {
    std::env::var("CSD_POOL_SIGNER_WALLET_ADDRESS")
        .ok()
        .and_then(|value| normalize_addr20(&value))
}

async fn health(State(settings): State<Arc<SignerSettings>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        service: "csd-pool-signer",
        mode: settings.mode.clone(),
        wallet_address: settings.wallet_address.clone(),
    })
}

async fn sign_payout(
    headers: HeaderMap,
    State(settings): State<Arc<SignerSettings>>,
    Json(request): Json<SignPayoutRequest>,
) -> Result<Json<SignedPayoutResponse>, SignerError> {
    authorize(&headers, settings.token.as_deref())?;
    validate_sign_request(&request)?;
    let canonical = serde_json::to_vec(&request)?;
    let raw_tx_hex =
        hex::encode([b"csd-payout-mock-v1:".as_slice(), canonical.as_slice()].concat());
    let txid = sha256d_hex(raw_tx_hex.as_bytes());
    Ok(Json(SignedPayoutResponse {
        raw_tx_hex,
        node_tx: None,
        txid,
    }))
}

fn authorize(headers: &HeaderMap, token: Option<&str>) -> Result<(), SignerError> {
    let Some(expected) = token else {
        return Ok(());
    };
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(SignerError::Unauthorized);
    };
    let Ok(value) = value.to_str() else {
        return Err(SignerError::Unauthorized);
    };
    if value
        .strip_prefix("Bearer ")
        .map(|actual| constant_time_eq(actual.as_bytes(), expected.as_bytes()))
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(SignerError::Unauthorized)
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

fn validate_sign_request(request: &SignPayoutRequest) -> Result<(), SignerError> {
    if request.batch_id.trim().is_empty() {
        return Err(SignerError::BadRequest("batch_id is required"));
    }
    if request.outputs.is_empty() {
        return Err(SignerError::BadRequest("outputs are required"));
    }
    let total = request.outputs.iter().try_fold(0u128, |total, output| {
        if !is_addr20(&output.address) {
            return Err(SignerError::BadRequest(
                "output address must be 40 hex chars",
            ));
        }
        if output.amount_base_units == 0 {
            return Err(SignerError::BadRequest("output amount must be positive"));
        }
        total
            .checked_add(output.amount_base_units)
            .ok_or(SignerError::BadRequest("output total overflow"))
    })?;
    if total != request.total_base_units {
        return Err(SignerError::BadRequest(
            "output total does not match batch total",
        ));
    }
    Ok(())
}

fn is_addr20(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn normalize_addr20(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let normalized = trimmed
        .strip_prefix("0x")
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    if is_addr20(&normalized) {
        Some(normalized)
    } else {
        None
    }
}

fn sha256d_hex(bytes: &[u8]) -> String {
    let first = Sha256::digest(bytes);
    let second = Sha256::digest(first);
    hex::encode(second)
}

#[derive(Debug, thiserror::Error)]
enum SignerError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("{0}")]
    BadRequest(&'static str),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl IntoResponse for SignerError {
    fn into_response(self) -> axum::response::Response {
        match self {
            SignerError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: ErrorBody {
                        code: "unauthorized",
                        message: "signer token is missing or invalid",
                    },
                }),
            )
                .into_response(),
            SignerError::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: ErrorBody {
                        code: "bad_request",
                        message,
                    },
                }),
            )
                .into_response(),
            SignerError::Json(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: ErrorBody {
                        code: "signer_error",
                        message: "failed to build signed payout",
                    },
                }),
            )
                .into_response(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SignPayoutRequest {
    pub batch_id: String,
    pub total_base_units: u128,
    pub outputs: Vec<SignPayoutOutput>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SignPayoutOutput {
    pub address: String,
    pub amount_base_units: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SignedPayoutResponse {
    pub raw_tx_hex: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_tx: Option<serde_json::Value>,
    pub txid: String,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    service: &'static str,
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wallet_address: Option<String>,
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
    fn constant_time_eq_handles_equal_mismatch_and_length_difference() {
        assert!(constant_time_eq(b"signer-secret", b"signer-secret"));
        assert!(!constant_time_eq(b"signer-secret", b"signer-secreu"));
        assert!(!constant_time_eq(b"signer-secret", b"signer-secret-long"));
        assert!(!constant_time_eq(b"signer-secret-long", b"signer-secret"));
    }

    #[tokio::test]
    async fn signs_valid_request_deterministically() {
        let app = router(SignerSettings {
            token: None,
            mode: "mock".to_owned(),
            wallet_address: None,
        });
        let body = serde_json::json!({
            "batch_id": "batch-1",
            "total_base_units": 125000000u128,
            "outputs": [{
                "address": "0123456789abcdef0123456789abcdef01234567",
                "amount_base_units": 125000000u128
            }]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/payout/sign")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["raw_tx_hex"].as_str().unwrap().starts_with("637364"));
        assert_eq!(json["txid"].as_str().unwrap().len(), 64);
    }

    #[tokio::test]
    async fn rejects_bad_total_and_requires_token_when_configured() {
        let app = router(SignerSettings {
            token: Some("secret".to_owned()),
            mode: "mock".to_owned(),
            wallet_address: None,
        });
        let body = serde_json::json!({
            "batch_id": "batch-1",
            "total_base_units": 2u128,
            "outputs": [{
                "address": "0123456789abcdef0123456789abcdef01234567",
                "amount_base_units": 1u128
            }]
        });

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/payout/sign")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let wrong_token = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/payout/sign")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer secret-long")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong_token.status(), StatusCode::UNAUTHORIZED);

        let bad_request = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/payout/sign")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer secret")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_request.status(), StatusCode::BAD_REQUEST);
    }
}
