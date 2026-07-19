use std::net::SocketAddr;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "csd_pool_signer=info".into()),
        )
        .init();

    let listen: SocketAddr = csd_pool_signer::signer_listen()
        .parse()
        .expect("valid CSD_POOL_SIGNER_LISTEN");
    csd_pool_signer::run_signer_server(listen, csd_pool_signer::SignerSettings::from_env()).await
}
