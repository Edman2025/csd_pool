use std::net::SocketAddr;

use csd_pool_state::SharedPoolState;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("csd-pool-api startup failed: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "csd_pool_api=info".into()))
        .init();

    let listen: SocketAddr = csd_pool_api::api_listen()
        .parse()
        .expect("valid CSD_POOL_API_LISTEN");
    let repository = csd_pool_api::repository_from_env().await?;
    csd_pool_api::run_api_server_with_repository(listen, SharedPoolState::new(), repository)
        .await?;
    Ok(())
}
