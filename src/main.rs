use proxy_microservice::{application, bootstrap_admin, config::Config, connect};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "proxy_microservice=info".into()),
        )
        .init();
    let config = Config::from_env()?;
    let pool = connect(&config).await?;
    let reset = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("reset-admin-password") => true,
        _ => anyhow::bail!("Usage: proxy-microservice [reset-admin-password]"),
    };
    bootstrap_admin(&pool, reset).await?;
    if reset {
        tracing::info!("Administrator password reset; sessions and API keys revoked");
        return Ok(());
    }
    let app = application(pool.clone(), &config).await?;
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    tracing::info!(address = %config.bind_addr, "Proxy service is ready");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    pool.close().await;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
