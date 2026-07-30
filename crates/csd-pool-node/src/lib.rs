use async_trait::async_trait;
use csd_pool_consensus::{
    Hash32, WorkTemplate, decode_hash32_hex, decode_prev_hash_from_stratum, parse_u32_hex,
};
use csd_pool_protocol::NotifyParams;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Semaphore;

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

pub const MAX_CSD_BLOCK_BYTES: usize = 2 * 1024 * 1024;
const MAX_CSD_BLOCK_TEMPLATE_HEX_LEN: usize = MAX_CSD_BLOCK_BYTES * 2;
const MAX_STATELESS_TEMPLATE_RESPONSE_BYTES: usize = MAX_CSD_BLOCK_TEMPLATE_HEX_LEN + (512 * 1024);
const MAX_STATELESS_TEMPLATE_CACHE_ENTRIES: usize = 8;
const STATELESS_TEMPLATE_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

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

    pub fn with_root_certificate_pem(mut self, pem: &[u8]) -> Result<Self> {
        let certificates = reqwest::Certificate::from_pem_bundle(pem)?;
        if certificates.is_empty() {
            return Err(NodeError::InvalidResponse(
                "root certificate PEM contains no certificates",
            ));
        }
        let mut builder = reqwest::Client::builder();
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
        self.http = builder.build()?;
        Ok(self)
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
        parse_submit_block_response(
            self.authorize(self.http.post(url))
                .json(&request)
                .send()
                .await?,
        )
        .await
    }

    pub async fn submit_candidate(
        &self,
        candidate: &BlockCandidateSubmitRequest,
    ) -> Result<SubmitBlockResponse> {
        let url = format!("{}/api/rpc/block/submit", self.base_url);
        parse_submit_block_response(
            self.authorize(self.http.post(url))
                .json(candidate)
                .send()
                .await?,
        )
        .await
    }

    pub async fn submit_full_candidate(
        &self,
        candidate: &StatelessBlockCandidateSubmitRequest<'_>,
    ) -> Result<SubmitBlockResponse> {
        let url = format!("{}/api/rpc/block/submit-full", self.base_url);
        parse_submit_block_response(
            self.authorize(self.http.post(url))
                .json(candidate)
                .send()
                .await?,
        )
        .await
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

    pub async fn mining_template_material(
        &self,
        job_id: &str,
    ) -> Result<StatelessNodeMiningTemplate> {
        let url = format!("{}/api/rpc/mining/template-material", self.base_url);
        let mut response = self
            .authorize(self.http.get(url))
            .query(&[("job_id", job_id)])
            .send()
            .await?
            .error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_STATELESS_TEMPLATE_RESPONSE_BYTES as u64)
        {
            return Err(NodeError::InvalidResponse(
                "stateless template response exceeds the configured body limit",
            ));
        }
        let mut body = Vec::with_capacity(
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or_default()
                .min(MAX_STATELESS_TEMPLATE_RESPONSE_BYTES),
        );
        while let Some(chunk) = response.chunk().await? {
            if body.len().saturating_add(chunk.len()) > MAX_STATELESS_TEMPLATE_RESPONSE_BYTES {
                return Err(NodeError::InvalidResponse(
                    "stateless template response exceeds the configured body limit",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&body)?)
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

async fn parse_submit_block_response(response: reqwest::Response) -> Result<SubmitBlockResponse> {
    let status = response.status();
    let body = response.bytes().await?;
    let mut parsed: SubmitBlockResponse = serde_json::from_slice(&body)?;
    if !status.is_success() {
        let mut extra = parsed.extra.as_object().cloned().unwrap_or_default();
        extra.insert(
            "http_status".to_owned(),
            serde_json::Value::from(status.as_u16()),
        );
        parsed.extra = serde_json::Value::Object(extra);
    }
    Ok(parsed)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NodeHealth {
    pub height: Option<u64>,
    pub tip: Option<String>,
    pub chainwork: Option<String>,
    #[serde(alias = "peer_count")]
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

#[derive(Debug, Serialize)]
pub struct StatelessBlockCandidateSubmitRequest<'a> {
    #[serde(flatten)]
    pub candidate: &'a BlockCandidateSubmitRequest,
    #[serde(flatten)]
    pub candidate_material: &'a CandidateTemplateMaterial,
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StatelessNodeMiningTemplate {
    #[serde(flatten)]
    pub template: NodeMiningTemplate,
    pub block_template_hex: String,
    pub block_template_sha256_hex: String,
    pub block_template_bytes: u64,
}

impl StatelessNodeMiningTemplate {
    pub fn into_pool_job(self) -> Result<PoolJob> {
        let (template, candidate_material) = self.into_parts()?;
        let job = template.into_pool_job()?;
        *job.candidate_material
            .write()
            .expect("candidate material slot lock poisoned") = Some(Arc::new(candidate_material));
        Ok(job)
    }

    fn into_parts(self) -> Result<(NodeMiningTemplate, CandidateTemplateMaterial)> {
        let candidate_material = candidate_template_material(
            Some(&self.block_template_hex),
            Some(&self.block_template_sha256_hex),
            Some(self.block_template_bytes),
        )?
        .ok_or(NodeError::InvalidResponse(
            "stateless block template material is missing",
        ))?;
        Ok((self.template, candidate_material))
    }
}

fn candidate_template_material(
    block_template_hex: Option<&str>,
    block_template_sha256_hex: Option<&str>,
    block_template_bytes: Option<u64>,
) -> Result<Option<CandidateTemplateMaterial>> {
    let (Some(block_template_hex), Some(expected_sha256), Some(expected_bytes)) = (
        block_template_hex,
        block_template_sha256_hex,
        block_template_bytes,
    ) else {
        if block_template_hex.is_none()
            && block_template_sha256_hex.is_none()
            && block_template_bytes.is_none()
        {
            return Ok(None);
        }
        return Err(NodeError::InvalidResponse(
            "stateless block template fields are incomplete",
        ));
    };
    if block_template_hex.len() > MAX_CSD_BLOCK_TEMPLATE_HEX_LEN
        || block_template_hex.len() % 2 != 0
    {
        return Err(NodeError::InvalidResponse(
            "block_template_hex exceeds the encoded block limit or has odd length",
        ));
    }
    if expected_bytes > MAX_CSD_BLOCK_BYTES as u64 {
        return Err(NodeError::InvalidResponse(
            "block_template_bytes exceeds the consensus block limit",
        ));
    }
    let bytes = hex::decode(block_template_hex)
        .map_err(|_| NodeError::InvalidResponse("block_template_hex is invalid hex"))?;
    if u64::try_from(bytes.len()).ok() != Some(expected_bytes) {
        return Err(NodeError::InvalidResponse(
            "block_template_bytes does not match decoded template length",
        ));
    }
    let actual_sha256 = hex::encode(Sha256::digest(&bytes));
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        return Err(NodeError::InvalidResponse(
            "block_template_sha256_hex does not match template bytes",
        ));
    }
    Ok(Some(CandidateTemplateMaterial {
        block_template_hex: block_template_hex.to_ascii_lowercase(),
        block_template_sha256_hex: actual_sha256,
        block_template_bytes: expected_bytes,
    }))
}

fn default_clean_jobs() -> bool {
    true
}

#[derive(Clone, Debug)]
pub struct PoolJob {
    pub notify: NotifyParams,
    pub template: WorkTemplate,
    pub candidate_material: CandidateMaterialSlot,
}

pub type CandidateMaterialSlot = Arc<RwLock<Option<Arc<CandidateTemplateMaterial>>>>;
type MaterialFetchTask = (String, tokio::task::JoinHandle<()>);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CandidateTemplateMaterial {
    pub block_template_hex: String,
    pub block_template_sha256_hex: String,
    pub block_template_bytes: u64,
}

#[async_trait]
pub trait TemplateProvider: Send + Sync {
    async fn current_job(&self) -> Result<PoolJob>;

    async fn current_tip(&self) -> Result<Option<Hash32>> {
        Ok(None)
    }
}

#[derive(Clone, Debug)]
pub struct StaticTemplateProvider {
    job: PoolJob,
}

#[derive(Clone, Debug)]
pub struct LiveTemplateProvider {
    node: CsdNodeClient,
    pool_address: String,
    stateless_full: bool,
    material_cache: Arc<Mutex<TemplateMaterialCache>>,
    material_fetch: Arc<Mutex<Option<MaterialFetchTask>>>,
    material_fetch_limit: Arc<Semaphore>,
}

#[derive(Debug)]
struct TemplateMaterialCacheEntry {
    template: NodeMiningTemplate,
    slot: CandidateMaterialSlot,
    fetch_started: bool,
}

#[derive(Debug, Default)]
struct TemplateMaterialCache {
    entries: HashMap<String, TemplateMaterialCacheEntry>,
    order: VecDeque<String>,
}

impl TemplateMaterialCache {
    fn slot_for(&mut self, template: &NodeMiningTemplate) -> (CandidateMaterialSlot, bool) {
        if let Some(entry) = self.entries.get_mut(&template.job_id) {
            if entry.template == *template {
                let should_fetch = !entry.fetch_started
                    && entry
                        .slot
                        .read()
                        .expect("candidate material slot lock poisoned")
                        .is_none();
                entry.fetch_started = true;
                return (entry.slot.clone(), should_fetch);
            }
            if let Some(replaced) = self.entries.remove(&template.job_id) {
                *replaced
                    .slot
                    .write()
                    .expect("candidate material slot lock poisoned") = None;
            }
            self.order.retain(|job_id| job_id != &template.job_id);
        }

        let slot = Arc::new(RwLock::new(None));
        self.entries.insert(
            template.job_id.clone(),
            TemplateMaterialCacheEntry {
                template: template.clone(),
                slot: slot.clone(),
                fetch_started: true,
            },
        );
        self.order.push_back(template.job_id.clone());
        while self.order.len() > MAX_STATELESS_TEMPLATE_CACHE_ENTRIES {
            if let Some(oldest) = self.order.pop_front()
                && let Some(evicted) = self.entries.remove(&oldest)
            {
                *evicted
                    .slot
                    .write()
                    .expect("candidate material slot lock poisoned") = None;
            }
        }
        (slot, true)
    }

    fn cancel_fetch(&mut self, job_id: &str) {
        if let Some(entry) = self.entries.get_mut(job_id) {
            entry.fetch_started = false;
        }
    }

    fn finish_fetch(
        &mut self,
        template: &NodeMiningTemplate,
        slot: &CandidateMaterialSlot,
        material: Option<CandidateTemplateMaterial>,
    ) {
        let Some(entry) = self.entries.get_mut(&template.job_id) else {
            return;
        };
        if entry.template != *template || !Arc::ptr_eq(&entry.slot, slot) {
            return;
        }
        entry.fetch_started = false;
        if let Some(material) = material {
            *entry
                .slot
                .write()
                .expect("candidate material slot lock poisoned") = Some(Arc::new(material));
        }
    }
}

impl LiveTemplateProvider {
    pub fn new(node: CsdNodeClient, pool_address: impl Into<String>) -> Self {
        Self {
            node,
            pool_address: pool_address.into(),
            stateless_full: false,
            material_cache: Arc::new(Mutex::new(TemplateMaterialCache::default())),
            material_fetch: Arc::new(Mutex::new(None)),
            material_fetch_limit: Arc::new(Semaphore::new(1)),
        }
    }

    pub fn new_stateless(node: CsdNodeClient, pool_address: impl Into<String>) -> Self {
        Self {
            node,
            pool_address: pool_address.into(),
            stateless_full: true,
            material_cache: Arc::new(Mutex::new(TemplateMaterialCache::default())),
            material_fetch: Arc::new(Mutex::new(None)),
            material_fetch_limit: Arc::new(Semaphore::new(1)),
        }
    }
}

#[async_trait]
impl TemplateProvider for LiveTemplateProvider {
    async fn current_job(&self) -> Result<PoolJob> {
        let template = self.node.mining_template(&self.pool_address).await?;
        let mut job = template.clone().into_pool_job()?;
        if !self.stateless_full {
            return Ok(job);
        }

        let (slot, should_fetch) = self
            .material_cache
            .lock()
            .expect("template material cache lock poisoned")
            .slot_for(&template);
        job.candidate_material = slot.clone();
        if should_fetch {
            let node = self.node.clone();
            let job_id = template.job_id.clone();
            let fetch_limit = self.material_fetch_limit.clone();
            let material_cache = self.material_cache.clone();
            let mut in_flight = self
                .material_fetch
                .lock()
                .expect("template material fetch lock poisoned");
            if let Some((previous_job_id, previous)) = in_flight.as_ref()
                && previous_job_id != &job_id
            {
                previous.abort();
                self.material_cache
                    .lock()
                    .expect("template material cache lock poisoned")
                    .cancel_fetch(previous_job_id);
            }
            let task_job_id = job_id.clone();
            let task = tokio::spawn(async move {
                let material = match fetch_limit.acquire_owned().await {
                    Ok(_permit) => match tokio::time::timeout(
                        STATELESS_TEMPLATE_FETCH_TIMEOUT,
                        node.mining_template_material(&task_job_id),
                    )
                    .await
                    {
                        Ok(Ok(full_template)) => match full_template.into_parts() {
                            Ok((full_base, material)) if full_base == template => Some(material),
                            _ => None,
                        },
                        _ => None,
                    },
                    _ => None,
                };
                material_cache
                    .lock()
                    .expect("template material cache lock poisoned")
                    .finish_fetch(&template, &slot, material);
            });
            *in_flight = Some((job_id, task));
        }
        Ok(job)
    }

    async fn current_tip(&self) -> Result<Option<Hash32>> {
        self.node
            .health()
            .await?
            .tip
            .as_deref()
            .map(|tip| {
                decode_hash32_hex("node_tip", tip.strip_prefix("0x").unwrap_or(tip))
                    .map_err(NodeError::from)
            })
            .transpose()
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
    Ok(PoolJob {
        notify,
        template,
        candidate_material: Arc::new(RwLock::new(None)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::{get, post},
    };
    use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::{TlsAcceptor, rustls::ServerConfig};

    async fn spawn_self_signed_health_server(
        connection_count: usize,
    ) -> (std::net::SocketAddr, String) {
        let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])
            .expect("self-signed test certificate");
        let certificate_der = certificate.serialize_der().expect("certificate DER");
        let private_key_der = certificate.serialize_private_key_der();
        let certificate_pem = certificate.serialize_pem().expect("certificate PEM");
        let tls = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate_der)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(private_key_der)),
            )
            .expect("TLS server config");
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS test listener");
        let address = listener.local_addr().expect("TLS test address");
        tokio::spawn(async move {
            for _ in 0..connection_count {
                let (stream, _) = listener.accept().await.expect("TLS test accept");
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let mut request = [0_u8; 2048];
                    let _ = stream.read(&mut request).await;
                    let body = r#"{"height":42,"tip":"00","chainwork":"100","peer_count":3}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream
                        .write_all(response.as_bytes())
                        .await
                        .expect("TLS test response");
                });
            }
        });
        (address, certificate_pem)
    }

    #[test]
    fn node_health_accepts_official_peer_count_field() {
        let health: NodeHealth = serde_json::from_value(serde_json::json!({
            "height": 59092,
            "tip": format!("0x{}", "11".repeat(32)),
            "chainwork": "160000",
            "peer_count": 8,
        }))
        .unwrap();

        assert_eq!(health.peers, Some(8));
        assert_eq!(health.extra["peer_count"], serde_json::Value::Null);
    }

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

    #[tokio::test]
    async fn instance_root_certificate_trusts_only_the_configured_self_signed_server() {
        let (address, certificate_pem) = spawn_self_signed_health_server(4).await;
        let url = format!("https://{address}");

        let untrusted = CsdNodeClient::new(url.clone()).health().await;
        assert!(untrusted.is_err());

        let wrong_certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])
            .unwrap()
            .serialize_pem()
            .unwrap();
        let wrong_trust = CsdNodeClient::new(url.clone())
            .with_root_certificate_pem(wrong_certificate.as_bytes())
            .unwrap()
            .health()
            .await;
        assert!(wrong_trust.is_err());

        let health = CsdNodeClient::new(url)
            .with_root_certificate_pem(certificate_pem.as_bytes())
            .unwrap()
            .health()
            .await
            .unwrap();
        assert_eq!(health.height, Some(42));
        assert_eq!(health.peers, Some(3));

        let fresh_untrusted = CsdNodeClient::new(format!("https://{address}"))
            .health()
            .await;
        assert!(fresh_untrusted.is_err());
    }

    #[test]
    fn rejects_invalid_instance_root_certificate_pem() {
        let error = CsdNodeClient::new("https://127.0.0.1")
            .with_root_certificate_pem(b"not a certificate")
            .unwrap_err();
        assert!(matches!(error, NodeError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn preserves_structured_candidate_rejection() {
        async fn reject() -> (StatusCode, Json<serde_json::Value>) {
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "ok": false,
                    "error": "stale or unknown pool job"
                })),
            )
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/api/rpc/block/submit", post(reject)),
            )
            .await
            .unwrap();
        });
        let candidate = BlockCandidateSubmitRequest {
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

        let response = CsdNodeClient::new(format!("http://{address}"))
            .submit_candidate(&candidate)
            .await
            .unwrap();

        assert!(!response.ok);
        assert_eq!(response.extra["http_status"], 409);
        assert_eq!(response.extra["error"], "stale or unknown pool job");
        server.abort();
    }

    #[tokio::test]
    async fn cached_candidate_submit_keeps_the_legacy_endpoint_and_body() {
        async fn cached(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
            assert_eq!(body["job_id"], "job1");
            assert!(body.get("block_template_hex").is_none());
            Json(serde_json::json!({"ok": true, "hash": body["hash_hex"]}))
        }
        async fn full() -> StatusCode {
            panic!("legacy candidate submit must not call the full endpoint")
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/api/rpc/block/submit", post(cached))
                    .route("/api/rpc/block/submit-full", post(full)),
            )
            .await
            .unwrap();
        });
        let candidate = BlockCandidateSubmitRequest {
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

        let response = CsdNodeClient::new(format!("http://{address}"))
            .submit_candidate(&candidate)
            .await
            .unwrap();
        assert!(response.ok);
        server.abort();
    }

    #[tokio::test]
    async fn nonstateless_provider_only_fetches_the_compact_template() {
        async fn compact() -> Json<NodeMiningTemplate> {
            Json(NodeMiningTemplate {
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
            })
        }
        async fn material() -> StatusCode {
            panic!("nonstateless provider must not request template material")
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/api/rpc/mining/template", get(compact))
                    .route("/api/rpc/mining/template-material", get(material)),
            )
            .await
            .unwrap();
        });

        let provider = LiveTemplateProvider::new(
            CsdNodeClient::new(format!("http://{address}")),
            "00".repeat(20),
        );
        let job = provider.current_job().await.unwrap();
        assert_eq!(job.template.job_id, "job1");
        assert!(job.candidate_material.read().unwrap().is_none());
        server.abort();
    }

    #[tokio::test]
    async fn stateless_provider_retries_material_after_transient_failure() {
        #[derive(Clone)]
        struct MaterialState {
            calls: Arc<AtomicUsize>,
        }

        async fn compact() -> Json<NodeMiningTemplate> {
            Json(NodeMiningTemplate {
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
            })
        }

        async fn material(
            State(state): State<MaterialState>,
        ) -> std::result::Result<Json<StatelessNodeMiningTemplate>, StatusCode> {
            if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
            let bytes = b"official-template-material";
            Ok(Json(StatelessNodeMiningTemplate {
                template: compact().await.0,
                block_template_hex: hex::encode(bytes),
                block_template_sha256_hex: hex::encode(Sha256::digest(bytes)),
                block_template_bytes: bytes.len() as u64,
            }))
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn({
            let calls = calls.clone();
            async move {
                axum::serve(
                    listener,
                    Router::new()
                        .route("/api/rpc/mining/template", get(compact))
                        .route("/api/rpc/mining/template-material", get(material))
                        .with_state(MaterialState { calls }),
                )
                .await
                .unwrap();
            }
        });

        let provider = LiveTemplateProvider::new_stateless(
            CsdNodeClient::new(format!("http://{address}")),
            "00".repeat(20),
        );
        let first = provider.current_job().await.unwrap();
        assert!(first.candidate_material.read().unwrap().is_none());

        let recovered = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let job = provider.current_job().await.unwrap();
                if job.candidate_material.read().unwrap().is_some() {
                    break job;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("transient material failure should be retried");

        let material = recovered
            .candidate_material
            .read()
            .unwrap()
            .clone()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(material.block_template_bytes, 26);
        server.abort();
    }

    #[tokio::test]
    async fn chunked_template_material_response_is_stopped_at_the_body_limit() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .unwrap();
            let chunk = vec![b'a'; 1024 * 1024];
            for _ in 0..5 {
                if stream
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
                if stream.write_all(&chunk).await.is_err()
                    || stream.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n").await;
        });

        let error = CsdNodeClient::new(format!("http://{address}"))
            .mining_template_material("job1")
            .await
            .unwrap_err();
        assert!(matches!(error, NodeError::InvalidResponse(_)));
        server.await.unwrap();
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
        assert!(json.get("block_template_hex").is_none());
    }

    #[test]
    fn validates_opaque_candidate_template_material() {
        let bytes = b"official-consensus-bincode-is-opaque-to-the-pool";
        let digest = hex::encode(Sha256::digest(bytes));
        let template = StatelessNodeMiningTemplate {
            template: NodeMiningTemplate {
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
            },
            block_template_hex: hex::encode(bytes),
            block_template_sha256_hex: digest.clone(),
            block_template_bytes: bytes.len() as u64,
        };

        let job = template.into_pool_job().unwrap();
        let material = job.candidate_material.read().unwrap().clone().unwrap();
        assert_eq!(material.block_template_sha256_hex, digest);
        assert_eq!(material.block_template_bytes, bytes.len() as u64);
    }

    #[test]
    fn tip_churn_keeps_base_jobs_but_bounds_material_slots() {
        let template = |job_id: String| NodeMiningTemplate {
            job_id,
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
        let mut cache = TemplateMaterialCache::default();
        let mut retained_jobs = Vec::new();
        for index in 0..=12 {
            let node_template = template(format!("job-{index}"));
            let (slot, _) = cache.slot_for(&node_template);
            *slot.write().unwrap() = Some(Arc::new(CandidateTemplateMaterial {
                block_template_hex: format!("{index:02x}"),
                block_template_sha256_hex: hex::encode(Sha256::digest([index as u8])),
                block_template_bytes: 1,
            }));
            let mut retained_job = node_template.into_pool_job().unwrap();
            retained_job.candidate_material = slot;
            retained_jobs.push(retained_job);
        }

        assert_eq!(retained_jobs.len(), 13);
        assert!(
            retained_jobs[0]
                .candidate_material
                .read()
                .unwrap()
                .is_none()
        );
        assert_eq!(retained_jobs[0].template.job_id, "job-0");
        assert_eq!(retained_jobs[0].template.share_target, [0xff; 32]);
        assert_eq!(
            retained_jobs
                .iter()
                .filter(|job| job.candidate_material.read().unwrap().is_some())
                .count(),
            MAX_STATELESS_TEMPLATE_CACHE_ENTRIES
        );
        assert!(!cache.entries.contains_key("job-0"));
        assert_eq!(cache.entries.len(), MAX_STATELESS_TEMPLATE_CACHE_ENTRIES);
    }

    #[test]
    fn rejects_partial_or_mismatched_candidate_template_material() {
        assert!(candidate_template_material(Some("00"), None, Some(1)).is_err());
        let template = StatelessNodeMiningTemplate {
            template: NodeMiningTemplate {
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
            },
            block_template_hex: "00".to_owned(),
            block_template_sha256_hex: "11".repeat(32),
            block_template_bytes: 1,
        };
        assert!(template.into_pool_job().is_err());
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
