use std::net::SocketAddr;

use csd_pool_state::SharedPoolState;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
enum DaemonError {
    #[error("invalid API listen address: {0}")]
    ApiListenAddr(#[from] std::net::AddrParseError),
    #[error("api server error: {0}")]
    Api(#[from] std::io::Error),
    #[error("stratum server error: {0}")]
    Stratum(#[from] csd_pool_bridge::BridgeError),
    #[error("api startup error: {0}")]
    ApiStartup(#[from] csd_pool_api::ApiStartupError),
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("csd-pool-daemon startup failed: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), DaemonError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "csd_pool_daemon=info,csd_pool=info".into()),
        )
        .init();

    let pool_state = SharedPoolState::new();
    let api_listen: SocketAddr = csd_pool_api::api_listen().parse()?;
    let repository = csd_pool_api::repository_from_env().await?;
    let stratum_listen = csd_pool_bridge::stratum_listen();
    let release_name =
        std::env::var("CSD_POOL_RELEASE_NAME").unwrap_or_else(|_| "unknown".to_owned());
    let release_revision =
        std::env::var("CSD_POOL_RELEASE_REVISION").unwrap_or_else(|_| "unknown".to_owned());
    let release_timestamp =
        std::env::var("CSD_POOL_RELEASE_TIMESTAMP_UTC").unwrap_or_else(|_| "unknown".to_owned());

    info!(
        %api_listen,
        %stratum_listen,
        version = env!("CARGO_PKG_VERSION"),
        %release_name,
        %release_revision,
        %release_timestamp,
        "starting csd pool daemon"
    );

    tokio::select! {
        result = csd_pool_api::run_api_server_with_repository(api_listen, pool_state.clone(), repository) => {
            result.map_err(DaemonError::Api)
        }
        result = csd_pool_bridge::run_stratum_server(&stratum_listen, pool_state) => {
            result.map_err(DaemonError::Stratum)
        }
    }
}
