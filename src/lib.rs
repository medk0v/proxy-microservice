mod auth;
pub mod config;
mod crypto;
mod error;
mod location;
mod notifications;
pub mod parser;
mod proxies;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{StatusCode, header},
    middleware,
    response::Html,
    routing::{delete, get, post},
};
use config::Config;
use crypto::SecretStore;
use sqlx::{
    ConnectOptions, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tower_http::{limit::RequestBodyLimitLayer, timeout::TimeoutLayer};

#[derive(Clone)]
pub(crate) struct AppState {
    pool: PgPool,
    crypto: SecretStore,
    origin: Arc<String>,
    secure_cookies: bool,
    dummy_password_hash: Arc<String>,
    login_limiter: Arc<auth::LoginLimiter>,
    password_slots: Arc<Semaphore>,
    notifications: notifications::TelegramNotifier,
}

pub async fn connect(config: &Config) -> anyhow::Result<PgPool> {
    let options: PgConnectOptions = config.database_url.parse()?;
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout = '15s'")
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SET lock_timeout = '5s'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options.disable_statement_logging())
        .await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

pub use auth::bootstrap_admin;

pub async fn application(pool: PgPool, config: &Config) -> anyhow::Result<Router> {
    let dummy_password_hash =
        tokio::task::spawn_blocking(|| auth::hash_password(&auth::new_token(""))).await??;
    Ok(router(AppState {
        pool,
        crypto: SecretStore::new(&config.encryption_key),
        origin: Arc::new(config.origin.clone()),
        secure_cookies: config.secure_cookies,
        dummy_password_hash: Arc::new(dummy_password_hash),
        login_limiter: Arc::default(),
        password_slots: Arc::new(Semaphore::new(4)),
        notifications: notifications::TelegramNotifier::new(config.telegram.as_ref()),
    }))
}

fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/auth/me", get(auth::me))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/proxies", get(proxies::list))
        .route("/api/proxies/random", get(proxies::random))
        .route("/api/proxies/export", get(proxies::export))
        .route("/api/proxies/preview", post(proxies::preview))
        .route("/api/proxies/import", post(proxies::import))
        .route("/api/proxies/delete", post(proxies::delete_selected))
        .route(
            "/api/proxies/{id}",
            get(proxies::detail).patch(proxies::update_expiry),
        )
        .route("/api/keys", get(auth::list_keys).post(auth::create_key))
        .route(
            "/api/countries",
            get(|| async {
                axum::Json(
                    location::COUNTRY_CODES
                        .split_whitespace()
                        .collect::<Vec<_>>(),
                )
            }),
        )
        .route("/api/keys/{id}", delete(auth::revoke_key))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));
    Router::new()
        .merge(protected)
        .route("/api/auth/login", post(auth::login).layer(DefaultBodyLimit::max(16 * 1024)))
        .route("/health", get(|| async { "ok" }))
        .route("/", get(|| async { Html(include_str!("../web/index.html")) }))
        .route("/app.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../web/app.js")) }))
        .route("/style.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("../web/style.css")) }))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .layer(RequestBodyLimitLayer::new(4 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), auth::browser_boundary))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30)))
        .layer(middleware::map_response(|mut response: axum::response::Response| async {
            let headers = response.headers_mut();
            headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
            headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
            headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
            headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
            response
        }))
        .with_state(state)
}
