use std::net::SocketAddr;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "csd_pool_mock_node=info".into()),
        )
        .init();

    let listen: SocketAddr = csd_pool_mock_node::mock_node_listen()
        .parse()
        .expect("valid CSD_POOL_MOCK_NODE_LISTEN");
    csd_pool_mock_node::run_mock_node_server(listen).await
}
