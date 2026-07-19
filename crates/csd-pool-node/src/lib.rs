use async_trait::async_trait;
use csd_pool_consensus::{
    Hash32, WorkTemplate, decode_hash32_hex, decode_prev_hash_from_stratum, parse_u32_hex,
};
use csd_pool_protocol::NotifyParams;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("consensus error: {0}")]
    Consensus(#[from] csd_pool_consensus::ConsensusError),
    #[error("invalid CSD node response: {0}")]
    InvalidResponse(&'static str),
}

pub type Result<T> = std::result::Result<T, NodeError>;

#[derive(Clone, Debug)]
pub struct CsdNodeClient {
    base_url: String,
    http: reqwest::Client,
    bearer_token: Option<String>,
}

impl CsdNodeClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            http: reqwest::Client::new(),
            bearer_token: None,
        }
    }

    pub fn from_env(base_url: impl Into<String>) -> Self {
        Self::new(base_url).with_bearer_token(
            std::env::var("CSD_POOL_NODE_TOKEN")
                .ok()
                .filter(|token| !token.is_empty()),
        )
    }

    pub fn with_bearer_token(mut self, token: Option<String>) -> Self {
        self.bearer_token = token;
        self
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.bearer_token.as_deref() {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    pub async fn health(&self) -> Result<NodeHealth> {
        let url = format!("{}/health", self.base_url);
        Ok(self
            .authorize(self.http.get(url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn network(&self) -> Result<NetworkSnapshot> {
        let url = format!("{}/api/network", self.base_url);
        Ok(self
            .authorize(self.http.get(url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn submit_raw_block(&self, raw_block_hex: &str) -> Result<SubmitBlockResponse> {
        let url = format!("{}/api/rpc/block/submit", self.base_url);
        let request = serde_json::json!({ "block": raw_block_hex });
        Ok(self
            .authorize(self.http.post(url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn submit_candidate(
        &self,
        candidate: &BlockCandidateSubmitRequest,
    ) -> Result<SubmitBlockResponse> {
        let url = format!("{}/api/rpc/block/submit", self.base_url);
        Ok(self
            .authorize(self.http.post(url))
            .json(candidate)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn mining_template(&self, pool_address: &str) -> Result<NodeMiningTemplate> {
        let url = format!("{}/api/rpc/mining/template", self.base_url);
        Ok(self
            .authorize(self.http.get(url))
            .query(&[("address", pool_address)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn block_status(&self, hash: &str) -> Result<BlockStatusResponse> {
        let url = format!("{}/api/rpc/block/status", self.base_url);
        Ok(self
            .authorize(self.http.get(url))
            .query(&[("hash", hash)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn submit_raw_transaction(&self, raw_tx_hex: &str) -> Result<SubmitTxResponse> {
        let url = format!("{}/api/rpc/tx/submit", self.base_url);
        let request = serde_json::json!({ "raw_tx_hex": raw_tx_hex });
        Ok(self
            .authorize(self.http.post(url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn submit_transaction(
        &self,
        node_tx: &serde_json::Value,
    ) -> Result<SubmitTxResponse> {
        let url = format!("{}/api/rpc/tx/submit", self.base_url);
        let request = serde_json::json!({ "tx": node_tx });
        Ok(self
            .authorize(self.http.post(url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn submit_official_transaction(
        &self,
        node_tx: &serde_json::Value,
    ) -> Result<SubmitTxResponse> {
        let url = format!("{}/tx/submit", self.base_url);
        let request = serde_json::json!({ "tx": node_tx });
        Ok(self
            .authorize(self.http.post(url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn transaction_status(&self, txid: &str) -> Result<TransactionStatusResponse> {
        let url = format!("{}/api/rpc/tx/status", self.base_url);
        Ok(self
            .authorize(self.http.get(url))
            .query(&[("txid", txid)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NodeHealth {
    pub height: Option<u64>,
    pub tip: Option<String>,
    pub chainwork: Option<String>,
    pub peers: Option<u64>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NetworkSnapshot {
    #[serde(default)]
    pub hashrate: f64,
    #[serde(default, rename = "hashrateGHs")]
    pub hashrate_ghs: f64,
    #[serde(default, rename = "targetBlockSecs")]
    pub target_block_secs: u64,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SubmitBlockResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct BlockCandidateSubmitRequest {
    pub job_id: String,
    pub worker_name: String,
    pub header_hex: String,
    pub hash_hex: String,
    pub coinbase_txid_hex: String,
    pub coinbase_hex: String,
    pub merkle_root_hex: String,
    pub extranonce2_hex: String,
    pub ntime_hex: String,
    pub nonce_hex: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct BlockStatusResponse {
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub height: Option<u64>,
    #[serde(default)]
    pub confirmations: u64,
    #[serde(default)]
    pub reward_base_units: Option<u128>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SubmitTxResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub txid: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TransactionStatusResponse {
    #[serde(default)]
    pub txid: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub confirmations: u64,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeMiningTemplate {
    pub job_id: String,
    pub prev_hash_be_hex: String,
    pub coinb1_hex: String,
    pub coinb2_hex: String,
    #[serde(default)]
    pub merkle_branches_hex: Vec<String>,
    pub version_hex: String,
    pub nbits_hex: String,
    pub ntime_hex: String,
    #[serde(default = "default_clean_jobs")]
    pub clean_jobs: bool,
    pub share_target_hex: String,
    pub network_target_hex: String,
}

impl NodeMiningTemplate {
    pub fn into_pool_job(self) -> Result<PoolJob> {
        let share_target = decode_hash32_hex("share_target_hex", &self.share_target_hex)?;
        let network_target = decode_hash32_hex("network_target_hex", &self.network_target_hex)?;
        let notify = NotifyParams {
            job_id: self.job_id,
            prev_hash_be_hex: self.prev_hash_be_hex,
            coinb1_hex: self.coinb1_hex,
            coinb2_hex: self.coinb2_hex,
            merkle_branches_hex: self.merkle_branches_hex,
            version_hex: self.version_hex,
            nbits_hex: self.nbits_hex,
            ntime_hex: self.ntime_hex,
            clean_jobs: self.clean_jobs,
        };
        job_from_notify(notify, share_target, network_target)
    }
}

fn default_clean_jobs() -> bool {
    true
}

#[derive(Clone, Debug)]
pub struct PoolJob {
    pub notify: NotifyParams,
    pub template: WorkTemplate,
}

#[async_trait]
pub trait TemplateProvider: Send + Sync {
    async fn current_job(&self) -> Result<PoolJob>;
}

#[derive(Clone, Debug)]
pub struct StaticTemplateProvider {
    job: PoolJob,
}

#[derive(Clone, Debug)]
pub struct LiveTemplateProvider {
    node: CsdNodeClient,
    pool_address: String,
}

impl LiveTemplateProvider {
    pub fn new(node: CsdNodeClient, pool_address: impl Into<String>) -> Self {
        Self {
            node,
            pool_address: pool_address.into(),
        }
    }
}

#[async_trait]
impl TemplateProvider for LiveTemplateProvider {
    async fn current_job(&self) -> Result<PoolJob> {
        self.node
            .mining_template(&self.pool_address)
            .await?
            .into_pool_job()
    }
}

impl StaticTemplateProvider {
    pub fn new(job: PoolJob) -> Self {
        Self { job }
    }

    pub fn easy_job(job_id: impl Into<String>) -> Self {
        Self::new(easy_static_job(job_id))
    }
}

#[async_trait]
impl TemplateProvider for StaticTemplateProvider {
    async fn current_job(&self) -> Result<PoolJob> {
        Ok(self.job.clone())
    }
}

pub fn easy_static_job(job_id: impl Into<String>) -> PoolJob {
    let notify = NotifyParams {
        job_id: job_id.into(),
        prev_hash_be_hex: "00".repeat(32),
        coinb1_hex: "aa".to_owned(),
        coinb2_hex: "bb".to_owned(),
        merkle_branches_hex: vec![],
        version_hex: "20000000".to_owned(),
        nbits_hex: "207fffff".to_owned(),
        ntime_hex: "665544cc".to_owned(),
        clean_jobs: true,
    };
    job_from_notify(notify, [0xff; 32], [0; 32]).expect("static job is valid")
}

pub fn job_from_notify(
    notify: NotifyParams,
    share_target: Hash32,
    network_target: Hash32,
) -> Result<PoolJob> {
    let version = parse_u32_hex("version_hex", &notify.version_hex)?;
    let prev = decode_prev_hash_from_stratum(&notify.prev_hash_be_hex)?;
    let time = parse_u32_hex("ntime_hex", &notify.ntime_hex)? as u64;
    let bits = parse_u32_hex("nbits_hex", &notify.nbits_hex)?;
    let coinbase_prefix =
        hex::decode(&notify.coinb1_hex).map_err(|_| NodeError::InvalidResponse("coinb1 hex"))?;
    let coinbase_suffix =
        hex::decode(&notify.coinb2_hex).map_err(|_| NodeError::InvalidResponse("coinb2 hex"))?;
    let merkle_branch = notify
        .merkle_branches_hex
        .iter()
        .map(|hash| decode_hash32_hex("merkle_branch", hash))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let template = WorkTemplate {
        job_id: notify.job_id.clone(),
        version,
        prev,
        time,
        bits,
        share_target,
        network_target,
        coinbase_prefix,
        coinbase_suffix,
        merkle_branch,
    };
    Ok(PoolJob { notify, template })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::HeaderMap, routing::get};

    #[tokio::test]
    async fn sends_bearer_token_to_adapter_endpoints() {
        async fn network(headers: HeaderMap) -> axum::Json<NetworkSnapshot> {
            assert_eq!(
                headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer node-secret")
            );
            axum::Json(NetworkSnapshot {
                target_block_secs: 60,
                ..NetworkSnapshot::default()
            })
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/api/network", get(network)))
                .await
                .unwrap();
        });

        let snapshot = CsdNodeClient::new(format!("http://{address}"))
            .with_bearer_token(Some("node-secret".to_owned()))
            .network()
            .await
            .unwrap();
        assert_eq!(snapshot.target_block_secs, 60);
        server.abort();
    }

    #[test]
    fn builds_easy_static_job() {
        let job = easy_static_job("job1");
        assert_eq!(job.notify.job_id, "job1");
        assert_eq!(job.template.job_id, "job1");
        assert_eq!(job.template.share_target, [0xff; 32]);
    }

    #[test]
    fn maps_notify_to_template() {
        let notify = NotifyParams {
            job_id: "job1".to_owned(),
            prev_hash_be_hex: "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
                .to_owned(),
            coinb1_hex: "aa".to_owned(),
            coinb2_hex: "bb".to_owned(),
            merkle_branches_hex: vec![],
            version_hex: "20000000".to_owned(),
            nbits_hex: "207fffff".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            clean_jobs: true,
        };
        let job = job_from_notify(notify, [0xff; 32], [0; 32]).unwrap();
        assert_eq!(job.template.prev[0], 31);
        assert_eq!(job.template.time, 0x665544cc);
    }

    #[test]
    fn maps_node_template_to_pool_job() {
        let template = NodeMiningTemplate {
            job_id: "job1".to_owned(),
            prev_hash_be_hex: "00".repeat(32),
            coinb1_hex: "aa".to_owned(),
            coinb2_hex: "bb".to_owned(),
            merkle_branches_hex: vec![],
            version_hex: "20000000".to_owned(),
            nbits_hex: "207fffff".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            clean_jobs: true,
            share_target_hex: "ff".repeat(32),
            network_target_hex: "00".repeat(32),
        };
        let job = template.into_pool_job().unwrap();
        assert_eq!(job.template.job_id, "job1");
        assert_eq!(job.template.share_target, [0xff; 32]);
        assert_eq!(job.template.network_target, [0; 32]);
    }

    #[test]
    fn serializes_candidate_submit_request_contract() {
        let request = BlockCandidateSubmitRequest {
            job_id: "job1".to_owned(),
            worker_name: "worker".to_owned(),
            header_hex: "aa".repeat(84),
            hash_hex: "11".repeat(32),
            coinbase_txid_hex: "22".repeat(32),
            coinbase_hex: "44".repeat(80),
            merkle_root_hex: "33".repeat(32),
            extranonce2_hex: "01020304".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            nonce_hex: "00000001".to_owned(),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["job_id"], "job1");
        assert_eq!(json["header_hex"].as_str().unwrap().len(), 168);
        assert_eq!(json["coinbase_hex"].as_str().unwrap().len(), 160);
    }

    #[test]
    fn parses_block_status_response_contract() {
        let status: BlockStatusResponse = serde_json::from_value(serde_json::json!({
            "hash": "11",
            "status": "confirmed",
            "height": 42,
            "confirmations": 12,
            "reward_base_units": 5000000000u64
        }))
        .unwrap();

        assert_eq!(status.status, "confirmed");
        assert_eq!(status.height, Some(42));
        assert_eq!(status.confirmations, 12);
        assert_eq!(status.reward_base_units, Some(5_000_000_000));
    }
}
