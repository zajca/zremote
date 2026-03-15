use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .json()
        .init();

    tracing::info!("MyRemote Agent v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("No server URL configured, exiting");
}

#[cfg(test)]
mod tests {
    #[test]
    fn agent_version_is_set() {
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}
