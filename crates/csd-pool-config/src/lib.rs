use serde::{Deserialize, Serialize};
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid socket address in {field}: {value}")]
    InvalidSocketAddr { field: &'static str, value: String },
}

pub type Result<T> = std::result::Result<T, ConfigError>;

pub fn env_flag_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

pub fn persistent_database_required(
    template_mode: Option<&str>,
    explicit_requirement: Option<&str>,
) -> bool {
    env_flag_enabled(explicit_requirement)
        || template_mode.is_some_and(|mode| mode.trim().eq_ignore_ascii_case("live"))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PoolConfig {
    pub pool: PoolSection,
    pub stratum: StratumSection,
    #[serde(default)]
    pub abuse: AbuseSection,
    pub api: ApiSection,
    pub csd_nodes: Vec<CsdNodeSection>,
    pub database: DatabaseSection,
    pub redis: RedisSection,
    pub signer: SignerSection,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            pool: PoolSection::default(),
            stratum: StratumSection::default(),
            abuse: AbuseSection::default(),
            api: ApiSection::default(),
            csd_nodes: vec![CsdNodeSection::default()],
            database: DatabaseSection::default(),
            redis: RedisSection::default(),
            signer: SignerSection::default(),
        }
    }
}

impl PoolConfig {
    pub fn from_toml_str(value: &str) -> Result<Self> {
        Ok(toml::from_str(value)?)
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_toml_str(&fs::read_to_string(path)?)
    }

    pub fn stratum_listen_addr(&self) -> Result<SocketAddr> {
        parse_socket("stratum.listen", &self.stratum.listen)
    }

    pub fn api_listen_addr(&self) -> Result<SocketAddr> {
        parse_socket("api.listen", &self.api.listen)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PoolSection {
    pub id: String,
    pub mining_address: String,
    pub fee_percent: f64,
    pub confirm_depth: u64,
    pub payout_interval_secs: u64,
    pub minimum_payout_csd: String,
    #[serde(default = "default_max_payout_batch_csd")]
    pub max_payout_batch_csd: String,
    #[serde(default = "default_max_daily_payout_csd")]
    pub max_daily_payout_csd: String,
    #[serde(default = "default_manual_payout_approval_csd")]
    pub manual_payout_approval_csd: String,
}

impl Default for PoolSection {
    fn default() -> Self {
        Self {
            id: "csd-main".to_owned(),
            mining_address: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            fee_percent: 1.0,
            confirm_depth: 10,
            payout_interval_secs: 1800,
            minimum_payout_csd: "1.0".to_owned(),
            max_payout_batch_csd: default_max_payout_batch_csd(),
            max_daily_payout_csd: default_max_daily_payout_csd(),
            manual_payout_approval_csd: default_manual_payout_approval_csd(),
        }
    }
}

fn default_max_payout_batch_csd() -> String {
    "1000.0".to_owned()
}

fn default_max_daily_payout_csd() -> String {
    "5000.0".to_owned()
}

fn default_manual_payout_approval_csd() -> String {
    "250.0".to_owned()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct StratumSection {
    pub listen: String,
    pub initial_difficulty: f64,
    pub min_difficulty: f64,
    pub max_difficulty: f64,
    pub target_share_secs: u64,
}

impl Default for StratumSection {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:3333".to_owned(),
            initial_difficulty: 8.0,
            min_difficulty: 8.0,
            max_difficulty: 512.0,
            target_share_secs: 20,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AbuseSection {
    pub max_connections_per_ip: u32,
    #[serde(default = "default_max_sessions_per_address")]
    pub max_sessions_per_address: u32,
    pub malformed_frame_limit: u32,
    pub auth_failure_limit: u32,
    pub invalid_share_limit: u32,
    pub ban_secs: u64,
}

impl Default for AbuseSection {
    fn default() -> Self {
        Self {
            max_connections_per_ip: 32,
            max_sessions_per_address: 64,
            malformed_frame_limit: 8,
            auth_failure_limit: 5,
            invalid_share_limit: 16,
            ban_secs: 600,
        }
    }
}

fn default_max_sessions_per_address() -> u32 {
    64
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ApiSection {
    pub listen: String,
    #[serde(default = "default_operator_token_env")]
    pub operator_token_env: String,
}

impl Default for ApiSection {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".to_owned(),
            operator_token_env: default_operator_token_env(),
        }
    }
}

fn default_operator_token_env() -> String {
    "CSD_POOL_OPERATOR_TOKEN".to_owned()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct CsdNodeSection {
    pub name: String,
    pub rpc_url: String,
    pub role: String,
}

impl Default for CsdNodeSection {
    fn default() -> Self {
        Self {
            name: "local".to_owned(),
            rpc_url: "http://127.0.0.1:8790".to_owned(),
            role: "template,submit,watch".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct DatabaseSection {
    pub url_env: String,
}

impl Default for DatabaseSection {
    fn default() -> Self {
        Self {
            url_env: "CSD_POOL_DATABASE_URL".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RedisSection {
    pub url_env: String,
}

impl Default for RedisSection {
    fn default() -> Self {
        Self {
            url_env: "CSD_POOL_REDIS_URL".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SignerSection {
    pub url_env: String,
    #[serde(default = "default_signer_listen")]
    pub listen: String,
    #[serde(default = "default_signer_token_env")]
    pub token_env: String,
}

impl Default for SignerSection {
    fn default() -> Self {
        Self {
            url_env: "CSD_POOL_SIGNER_URL".to_owned(),
            listen: default_signer_listen(),
            token_env: default_signer_token_env(),
        }
    }
}

fn default_signer_listen() -> String {
    "127.0.0.1:8890".to_owned()
}

fn default_signer_token_env() -> String {
    "CSD_POOL_SIGNER_TOKEN".to_owned()
}

fn parse_socket(field: &'static str, value: &str) -> Result<SocketAddr> {
    value.parse().map_err(|_| ConfigError::InvalidSocketAddr {
        field,
        value: value.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_ports() {
        let config = PoolConfig::default();
        assert_eq!(config.stratum_listen_addr().unwrap().port(), 3333);
        assert_eq!(config.api_listen_addr().unwrap().port(), 8080);
        assert_eq!(config.pool.confirm_depth, 10);
    }

    #[test]
    fn live_mode_requires_persistent_database() {
        assert!(persistent_database_required(Some("live"), None));
        assert!(persistent_database_required(Some("static"), Some("true")));
        assert!(!persistent_database_required(Some("static"), None));
        assert!(!persistent_database_required(None, Some("false")));
    }

    #[test]
    fn parses_toml_config() {
        let config = PoolConfig::from_toml_str(
            r#"
            [pool]
            id = "csd-test"
            mining_address = "0123456789abcdef0123456789abcdef01234567"
            fee_percent = 0.5
            confirm_depth = 10
            payout_interval_secs = 1800
            minimum_payout_csd = "1.0"
            max_payout_batch_csd = "1000.0"
            max_daily_payout_csd = "5000.0"
            manual_payout_approval_csd = "250.0"

            [stratum]
            listen = "0.0.0.0:3333"
            initial_difficulty = 8.0
            min_difficulty = 8.0
            max_difficulty = 512.0
            target_share_secs = 20

            [abuse]
            max_connections_per_ip = 32
            max_sessions_per_address = 64
            malformed_frame_limit = 8
            auth_failure_limit = 5
            invalid_share_limit = 16
            ban_secs = 600

            [api]
            listen = "0.0.0.0:8080"
            operator_token_env = "CSD_POOL_OPERATOR_TOKEN"

            [[csd_nodes]]
            name = "node-a"
            rpc_url = "http://10.0.0.11:8790"
            role = "template,submit,watch"

            [database]
            url_env = "CSD_POOL_DATABASE_URL"

            [redis]
            url_env = "CSD_POOL_REDIS_URL"

            [signer]
            url_env = "CSD_POOL_SIGNER_URL"
            listen = "127.0.0.1:8890"
            token_env = "CSD_POOL_SIGNER_TOKEN"
            "#,
        )
        .unwrap();
        assert_eq!(config.pool.id, "csd-test");
        assert_eq!(config.pool.max_payout_batch_csd, "1000.0");
        assert_eq!(config.pool.max_daily_payout_csd, "5000.0");
        assert_eq!(config.pool.manual_payout_approval_csd, "250.0");
        assert_eq!(config.csd_nodes[0].name, "node-a");
        assert_eq!(config.api.operator_token_env, "CSD_POOL_OPERATOR_TOKEN");
        assert_eq!(config.abuse.max_connections_per_ip, 32);
        assert_eq!(config.abuse.max_sessions_per_address, 64);
        assert_eq!(config.abuse.ban_secs, 600);
        assert_eq!(config.signer.listen, "127.0.0.1:8890");
        assert_eq!(config.signer.token_env, "CSD_POOL_SIGNER_TOKEN");
    }

    #[test]
    fn parses_private_beta_ops_config() {
        let config =
            PoolConfig::from_toml_str(include_str!("../../../ops/config.private-beta.toml"))
                .unwrap();
        assert_eq!(config.stratum.listen, "127.0.0.1:33330");
        assert_eq!(config.api.listen, "127.0.0.1:8080");
        assert_eq!(config.pool.max_payout_batch_csd, "1000.0");
        assert_eq!(config.pool.max_daily_payout_csd, "5000.0");
        assert_eq!(config.pool.manual_payout_approval_csd, "250.0");
        assert_eq!(config.abuse.max_connections_per_ip, 32);
        assert_eq!(config.abuse.max_sessions_per_address, 64);
    }
}
