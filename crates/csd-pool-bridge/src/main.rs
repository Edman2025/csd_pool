use csd_pool_state::SharedPoolState;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("csd-pool-bridge startup failed: {err}");
        std::process::exit(1);
    }
}

async fn run() -> csd_pool_bridge::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "csd_pool_bridge=info".to_owned()),
        )
        .init();

    csd_pool_bridge::run_stratum_server(&csd_pool_bridge::stratum_listen(), SharedPoolState::new())
        .await
}
