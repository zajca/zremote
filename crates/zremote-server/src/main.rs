use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .json()
        .init();

    let token = std::env::var("ZREMOTE_TOKEN").unwrap_or_else(|_| {
        tracing::error!("ZREMOTE_TOKEN environment variable is required");
        std::process::exit(1);
    });
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:zremote.db".to_string());
    let port: u16 = std::env::var("ZREMOTE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);

    zremote_server::run_server(zremote_server::ServerConfig {
        token,
        database_url,
        port,
    })
    .await;
}
