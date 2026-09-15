use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Extension, Json,
    extract::{ConnectInfo, Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{AppState, error::ApiError};

const SESSION_COOKIE: &str = "proxy_session";
const SESSION_SECONDS: u64 = 12 * 60 * 60;

#[derive(Clone)]
pub(crate) struct Identity {
    pub user_id: Uuid,
    pub username: String,
    pub token_hash: Vec<u8>,
    pub api_key: bool,
}

impl Identity {
    pub fn require_session(&self) -> Result<(), ApiError> {
        if self.api_key {
            Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "session_required",
                "A browser session is required",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
pub(crate) struct LoginLimiter(Mutex<HashMap<IpAddr, (Instant, u32)>>);

impl LoginLimiter {
    fn check(&self, ip: IpAddr) -> Result<(), ApiError> {
        let mut entries = self.0.lock().map_err(|_| ApiError::internal())?;
        let now = Instant::now();
        entries.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        if entries.len() >= 10_000 && !entries.contains_key(&ip) {
            return Err(rate_limited());
        }
        let (_, count) = entries.entry(ip).or_insert((now, 0));
        if *count >= 10 {
            return Err(rate_limited());
        }
        *count += 1;
        Ok(())
    }
}

fn rate_limited() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "Too many login attempts; retry in one minute",
    )
}

pub(crate) fn new_token(prefix: &str) -> String {
    let mut random = [0; 32];
    OsRng.fill_bytes(&mut random);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(random))
}

pub(crate) fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == SESSION_COOKIE && value.len() == 43).then_some(value)
        })
}

fn cookie_header(token: &str, secure: bool, max_age: u64) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    ))
    .map_err(|_| ApiError::internal())
}

pub(crate) async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());
    let (token, api_key) = if let Some(bearer) = bearer {
        let token = bearer
            .strip_prefix("Bearer ")
            .filter(|t| t.starts_with("px_") && t.len() == 46)
            .ok_or_else(ApiError::unauthorized)?;
        (token, true)
    } else {
        (
            cookie_token(request.headers()).ok_or_else(ApiError::unauthorized)?,
            false,
        )
    };
    if api_key && request.method() != Method::GET && request.method() != Method::HEAD {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "read_only_key",
            "API keys are read-only",
        ));
    }
    let hash = token_hash(token);
    let sql = if api_key {
        "SELECT u.id, u.username FROM api_keys k JOIN users u ON u.id = k.user_id WHERE k.token_hash = $1"
    } else {
        "SELECT u.id, u.username FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = $1 AND s.expires_at > now()"
    };
    let (user_id, username) = sqlx::query_as::<_, (Uuid, String)>(sql)
        .bind(&hash)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    request.extensions_mut().insert(Identity {
        user_id,
        username,
        token_hash: hash,
        api_key,
    });
    Ok(next.run(request).await)
}

// Browsers cannot add this header cross-origin without a preflight; no CORS is enabled.
// Origin is checked against configuration, never against untrusted forwarded headers.
pub(crate) async fn browser_boundary(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if request.uri().path().starts_with("/api/") {
        if let Some(origin) = request.headers().get(header::ORIGIN)
            && origin.to_str().ok() != Some(state.origin.as_str())
        {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "invalid_origin",
                "Origin is not allowed",
            ));
        }
        if !matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        ) && request
            .headers()
            .get("x-proxy-request")
            .and_then(|h| h.to_str().ok())
            != Some("1")
        {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "csrf_required",
                "X-Proxy-Request: 1 is required",
            ));
        }
    }
    Ok(next.run(request).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginInput {
    username: String,
    password: String,
}

pub(crate) async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<LoginInput>,
) -> Result<Response, ApiError> {
    state.login_limiter.check(peer.ip())?;
    if input.username.len() > 128 || input.password.len() > 1024 {
        return Err(invalid_login());
    }
    let permit = state
        .password_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| rate_limited())?;
    let user = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, username, password_hash FROM users WHERE username = $1",
    )
    .bind(input.username.trim())
    .fetch_optional(&state.pool)
    .await?;
    let hash = user
        .as_ref()
        .map(|u| u.2.clone())
        .unwrap_or_else(|| state.dummy_password_hash.as_ref().clone());
    let verified = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        PasswordHash::new(&hash).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(input.password.as_bytes(), &hash)
                .is_ok()
        })
    })
    .await
    .map_err(|_| ApiError::internal())?;
    if !verified {
        return Err(invalid_login());
    }
    let (user_id, username, _) = user.ok_or_else(invalid_login)?;
    let token = new_token("");
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM sessions WHERE expires_at <= now()")
        .execute(&mut *tx)
        .await?;
    if let Some(old_token) = cookie_token(&headers) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(token_hash(old_token))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '12 hours')")
        .bind(token_hash(&token)).bind(user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        [(
            header::SET_COOKIE,
            cookie_header(&token, state.secure_cookies, SESSION_SECONDS)?,
        )],
        Json(json!({"username": username})),
    )
        .into_response())
}

fn invalid_login() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_login",
        "Invalid username or password",
    )
}

pub(crate) async fn me(Extension(identity): Extension<Identity>) -> Json<Value> {
    Json(
        json!({"username": identity.username, "authentication": if identity.api_key { "api_key" } else { "session" }}),
    )
}

pub(crate) async fn logout(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Response, ApiError> {
    identity.require_session()?;
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(identity.token_hash)
        .execute(&state.pool)
        .await?;
    Ok((
        [(
            header::SET_COOKIE,
            cookie_header("", state.secure_cookies, 0)?,
        )],
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub(crate) fn hash_password(password: &str) -> anyhow::Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|_| anyhow::anyhow!("Password hashing failed"))
}

pub async fn bootstrap_admin(pool: &PgPool, reset_password: bool) -> anyhow::Result<()> {
    let username = std::env::var("ADMIN_USERNAME").unwrap_or_else(|_| "admin".into());
    anyhow::ensure!(
        !username.trim().is_empty() && username.len() <= 128,
        "ADMIN_USERNAME must contain 1 to 128 bytes"
    );
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username = $1)")
        .bind(username.trim())
        .fetch_one(pool)
        .await?;
    if exists && !reset_password {
        return Ok(());
    }
    let password = std::env::var("ADMIN_PASSWORD").map_err(|_| {
        anyhow::anyhow!("ADMIN_PASSWORD is required to create or reset the administrator")
    })?;
    anyhow::ensure!(
        (12..=1024).contains(&password.len()),
        "ADMIN_PASSWORD must contain 12 to 1024 bytes"
    );
    let hash = tokio::task::spawn_blocking(move || hash_password(&password)).await??;
    let mut tx = pool.begin().await?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO users (id, username, password_hash) VALUES ($1, $2, $3) ON CONFLICT (username) DO UPDATE SET password_hash = EXCLUDED.password_hash RETURNING id")
        .bind(Uuid::new_v4()).bind(username.trim()).bind(hash).fetch_one(&mut *tx).await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM api_keys WHERE user_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Serialize, sqlx::FromRow)]
pub(crate) struct KeySummary {
    id: Uuid,
    name: String,
    suffix: String,
    created_at: String,
}

pub(crate) async fn list_keys(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<Vec<KeySummary>>, ApiError> {
    identity.require_session()?;
    Ok(Json(sqlx::query_as("SELECT id, name, suffix, created_at::text FROM api_keys WHERE user_id = $1 ORDER BY created_at DESC")
        .bind(identity.user_id).fetch_all(&state.pool).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateKey {
    name: String,
}

pub(crate) async fn create_key(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(input): Json<CreateKey>,
) -> Result<Json<Value>, ApiError> {
    identity.require_session()?;
    let name = input.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(ApiError::bad_request(
            "invalid_key_name",
            "Key name must contain 1 to 100 bytes",
        ));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(identity.user_id)
        .execute(&mut *tx)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys WHERE user_id = $1")
        .bind(identity.user_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= 20 {
        return Err(ApiError::bad_request(
            "too_many_keys",
            "Revoke an existing key before creating another",
        ));
    }
    let token = new_token("px_");
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO api_keys (id, user_id, name, token_hash, suffix) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(identity.user_id)
    .bind(name)
    .bind(token_hash(&token))
    .bind(&token[token.len() - 4..])
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"id": id, "token": token, "name": name})))
}

pub(crate) async fn revoke_key(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    identity.require_session()?;
    let deleted = sqlx::query("DELETE FROM api_keys WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(identity.user_id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "Key not found",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}
