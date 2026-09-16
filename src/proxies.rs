use crate::location::{Region, normalize_country, unknown_country};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use crate::{
    AppState,
    auth::Identity,
    error::ApiError,
    parser::{self, Format, ParseError, ParsedImport, Protocol, Proxy},
};

impl From<ParseError> for ApiError {
    fn from(error: ParseError) -> Self {
        Self::bad_request(error.code, error.message)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportInput {
    text: String,
    #[serde(default)]
    format: Format,
    #[serde(default)]
    protocol: Protocol,
    #[serde(default)]
    skip_invalid: bool,
    #[serde(default)]
    region: Region,
    #[serde(default = "unknown_country")]
    country: String,
    #[serde(default)]
    expires_at: Value,
}

fn parse_expiry(value: &Value) -> Result<Option<DateTime<Utc>>, ApiError> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => DateTime::parse_from_rfc3339(value)
            .map(|date| Some(date.with_timezone(&Utc)))
            .map_err(|_| invalid_expiry()),
        _ => Err(invalid_expiry()),
    }
}

fn invalid_expiry() -> ApiError {
    ApiError::bad_request(
        "invalid_expires_at",
        "expires_at must be an RFC 3339 timestamp with a timezone, or null",
    )
}

#[derive(Serialize)]
struct PreviewRow {
    protocol: Protocol,
    host: String,
    port: u16,
    username: String,
    authenticated: bool,
}

impl From<&Proxy> for PreviewRow {
    fn from(p: &Proxy) -> Self {
        Self {
            protocol: p.protocol,
            host: p.host.clone(),
            port: p.port,
            username: p.username.clone(),
            authenticated: !p.username.is_empty(),
        }
    }
}

async fn existing_count(
    state: &AppState,
    user_id: Uuid,
    parsed: &ParsedImport,
) -> Result<i64, ApiError> {
    let fingerprints: Vec<Vec<u8>> = parsed
        .proxies
        .iter()
        .map(|p| state.crypto.fingerprint(p))
        .collect();
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM proxies WHERE user_id = $1 AND fingerprint = ANY($2)",
    )
    .bind(user_id)
    .bind(fingerprints)
    .fetch_one(&state.pool)
    .await?)
}

pub(crate) async fn preview(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(input): Json<ImportInput>,
) -> Result<Json<Value>, ApiError> {
    let country = normalize_country(&input.country)?;
    let expires_at = parse_expiry(&input.expires_at)?;
    let parsed = parser::parse_import(&input.text, input.format, input.protocol)?;
    let existing = existing_count(&state, identity.user_id, &parsed).await? as usize;
    Ok(Json(json!({
        "total": parsed.total,
        "valid": parsed.proxies.len(),
        "new": parsed.proxies.len() - existing,
        "duplicates": parsed.duplicates + existing,
        "invalid": parsed.errors.len(),
        "region": input.region,
        "country": country,
        "expires_at": expires_at,
        "errors": parsed.errors,
        "preview": parsed.proxies.iter().take(10).map(PreviewRow::from).collect::<Vec<_>>()
    })))
}

pub(crate) async fn import(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(input): Json<ImportInput>,
) -> Result<Response, ApiError> {
    let country = normalize_country(&input.country)?;
    let expires_at = parse_expiry(&input.expires_at)?;
    let parsed = parser::parse_import(&input.text, input.format, input.protocol)?;
    if !parsed.errors.is_empty() && !input.skip_invalid {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error": {"code": "invalid_lines", "message": "Fix invalid lines or explicitly enable skip_invalid"}, "errors": parsed.errors}))).into_response());
    }
    if parsed.proxies.is_empty() {
        return Err(ApiError::bad_request(
            "no_valid_proxies",
            "No valid proxies to import",
        ));
    }
    let mut tx = state.pool.begin().await?;
    let mut inserted = 0;
    for chunk in parsed.proxies.chunks(500) {
        let rows = chunk
            .iter()
            .map(|proxy| {
                Ok((
                    proxy,
                    state.crypto.encrypt(&proxy.password)?,
                    state.crypto.fingerprint(proxy),
                ))
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO proxies (id, user_id, protocol, host, port, username, region, country, password_encrypted, fingerprint, expires_at) ",
        );
        query.push_values(rows, |mut row, (proxy, encrypted, fingerprint)| {
            row.push_bind(Uuid::new_v4())
                .push_bind(identity.user_id)
                .push_bind(proxy.protocol.as_str())
                .push_bind(&proxy.host)
                .push_bind(i32::from(proxy.port))
                .push_bind(&proxy.username)
                .push_bind(input.region.as_str())
                .push_bind(&country)
                .push_bind(encrypted)
                .push_bind(fingerprint)
                .push_bind(expires_at);
        });
        query.push(" ON CONFLICT (user_id, fingerprint) DO NOTHING");
        inserted += query.build().execute(&mut *tx).await?.rows_affected() as usize;
    }
    tx.commit().await?;
    Ok(Json(json!({"inserted": inserted, "duplicates": parsed.duplicates + parsed.proxies.len() - inserted, "invalid": parsed.errors.len(), "errors": parsed.errors})).into_response())
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListQuery {
    #[serde(default)]
    search: String,
    protocol: Option<Protocol>,
    region: Option<Region>,
    country: Option<String>,
    page: Option<i64>,
    per_page: Option<i64>,
    format: Option<Format>,
}

impl ListQuery {
    fn validate(&self) -> Result<(i64, i64), ApiError> {
        let page = self.page.unwrap_or(1);
        let per_page = self.per_page.unwrap_or(50);
        if self.search.len() > 255
            || !(1..=100_000).contains(&page)
            || !(1..=200).contains(&per_page)
        {
            return Err(ApiError::bad_request(
                "invalid_filter",
                "Invalid search or pagination parameters",
            ));
        }
        Ok((page, per_page))
    }
}

#[derive(Serialize, sqlx::FromRow)]
struct ProxySummary {
    id: Uuid,
    protocol: String,
    host: String,
    port: i32,
    username: String,
    region: String,
    country: String,
    created_at: String,
    expires_at: Option<DateTime<Utc>>,
    expired: bool,
}

pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let (page, per_page) = query.validate()?;
    let protocol = query.protocol.map(Protocol::as_str);
    let country = query
        .country
        .as_deref()
        .map(normalize_country)
        .transpose()?;
    let region = query.region.map(Region::as_str);
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM proxies WHERE user_id = $1 AND ($2::text IS NULL OR protocol = $2) AND strpos(lower(host || ' ' || username), lower($3)) > 0 AND ($4::text IS NULL OR region = $4) AND ($5::text IS NULL OR country = $5) AND (NOT $6::boolean OR expires_at IS NULL OR expires_at > statement_timestamp())")
        .bind(identity.user_id).bind(protocol).bind(query.search.trim()).bind(region).bind(&country).bind(identity.api_key).fetch_one(&state.pool).await?;
    let items = sqlx::query_as::<_, ProxySummary>("SELECT id, protocol, host, port, username, region, country, created_at::text, expires_at, COALESCE(expires_at <= statement_timestamp(), false) AS expired FROM proxies WHERE user_id = $1 AND ($2::text IS NULL OR protocol = $2) AND strpos(lower(host || ' ' || username), lower($3)) > 0 AND ($4::text IS NULL OR region = $4) AND ($5::text IS NULL OR country = $5) AND (NOT $6::boolean OR expires_at IS NULL OR expires_at > statement_timestamp()) ORDER BY created_at DESC, id DESC LIMIT $7 OFFSET $8")
        .bind(identity.user_id).bind(protocol).bind(query.search.trim()).bind(region).bind(&country).bind(identity.api_key).bind(per_page).bind((page - 1) * per_page).fetch_all(&state.pool).await?;
    if identity.api_key && total == 0 {
        state.notifications.no_proxies(
            identity.user_id,
            "list",
            protocol,
            region,
            country.as_deref(),
        );
    }
    Ok(Json(
        json!({"items": items, "total": total, "page": page, "per_page": per_page}),
    ))
}

#[derive(sqlx::FromRow)]
struct StoredProxy {
    id: Uuid,
    protocol: String,
    host: String,
    port: i32,
    username: String,
    region: String,
    country: String,
    password_encrypted: Vec<u8>,
    expires_at: Option<DateTime<Utc>>,
}

impl StoredProxy {
    fn decrypt(&self, state: &AppState) -> Result<Proxy, ApiError> {
        Ok(Proxy {
            protocol: Protocol::parse(&self.protocol)?,
            host: self.host.clone(),
            port: self.port as u16,
            username: self.username.clone(),
            password: state.crypto.decrypt(&self.password_encrypted)?,
        })
    }

    fn response(&self, state: &AppState) -> Result<Json<Value>, ApiError> {
        let p = self.decrypt(state)?;
        Ok(Json(
            json!({"id": self.id, "protocol": p.protocol, "host": p.host, "port": p.port,
            "username": p.username, "password": p.password, "region": self.region, "country": self.country, "expires_at": self.expires_at, "url": p.export(Format::Url)?}),
        ))
    }
}

pub(crate) async fn detail(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    sqlx::query_as::<_, StoredProxy>("SELECT id, protocol, host, port, username, region, country, password_encrypted, expires_at FROM proxies WHERE id = $1 AND user_id = $2 AND (expires_at IS NULL OR expires_at > statement_timestamp())")
        .bind(id).bind(identity.user_id).fetch_optional(&state.pool).await?.ok_or_else(not_found)?.response(&state)
}

pub(crate) async fn random(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    query.validate()?;
    let country = query
        .country
        .as_deref()
        .map(normalize_country)
        .transpose()?;
    let proxy = sqlx::query_as::<_, StoredProxy>("SELECT id, protocol, host, port, username, region, country, password_encrypted, expires_at FROM proxies WHERE user_id = $1 AND ($2::text IS NULL OR protocol = $2) AND strpos(lower(host || ' ' || username), lower($3)) > 0 AND ($4::text IS NULL OR region = $4) AND ($5::text IS NULL OR country = $5) AND (expires_at IS NULL OR expires_at > statement_timestamp()) ORDER BY random() LIMIT 1")
        .bind(identity.user_id).bind(query.protocol.map(Protocol::as_str)).bind(query.search.trim())
        .bind(query.region.map(Region::as_str)).bind(&country).fetch_optional(&state.pool).await?;
    match proxy {
        Some(proxy) => proxy.response(&state),
        None => {
            state.notifications.no_proxies(
                identity.user_id,
                "random",
                query.protocol.map(Protocol::as_str),
                query.region.map(Region::as_str),
                country.as_deref(),
            );
            Err(not_found())
        }
    }
}

pub(crate) async fn export(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(query): Query<ListQuery>,
) -> Result<Response, ApiError> {
    query.validate()?;
    let country = query
        .country
        .as_deref()
        .map(normalize_country)
        .transpose()?;
    let rows = sqlx::query_as::<_, StoredProxy>("SELECT id, protocol, host, port, username, region, country, password_encrypted, expires_at FROM proxies WHERE user_id = $1 AND ($2::text IS NULL OR protocol = $2) AND strpos(lower(host || ' ' || username), lower($3)) > 0 AND ($4::text IS NULL OR region = $4) AND ($5::text IS NULL OR country = $5) AND (expires_at IS NULL OR expires_at > statement_timestamp()) ORDER BY host, port, id LIMIT 50001")
        .bind(identity.user_id).bind(query.protocol.map(Protocol::as_str)).bind(query.search.trim())
        .bind(query.region.map(Region::as_str)).bind(&country).fetch_all(&state.pool).await?;
    if rows.is_empty() {
        state.notifications.no_proxies(
            identity.user_id,
            "export",
            query.protocol.map(Protocol::as_str),
            query.region.map(Region::as_str),
            country.as_deref(),
        );
    }
    if rows.len() > 50_000 {
        return Err(ApiError::bad_request(
            "export_too_large",
            "Narrow the filters to export up to 50000 proxies",
        ));
    }
    let mut text = String::new();
    for row in rows {
        text.push_str(
            &row.decrypt(&state)?
                .export(query.format.unwrap_or(Format::Url))?,
        );
        text.push('\n');
    }
    Ok((
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=proxies.txt",
            ),
        ],
        text,
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpiryInput {
    expires_at: Value,
}

pub(crate) async fn update_expiry(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<Uuid>,
    Json(input): Json<ExpiryInput>,
) -> Result<Json<Value>, ApiError> {
    identity.require_session()?;
    let expires_at = parse_expiry(&input.expires_at)?;
    let proxy = sqlx::query_as::<_, ProxySummary>("UPDATE proxies SET expires_at = $1 WHERE id = $2 AND user_id = $3 RETURNING id, protocol, host, port, username, region, country, created_at::text, expires_at, COALESCE(expires_at <= statement_timestamp(), false) AS expired")
        .bind(expires_at).bind(id).bind(identity.user_id).fetch_optional(&state.pool).await?.ok_or_else(not_found)?;
    Ok(Json(json!(proxy)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteInput {
    ids: Vec<Uuid>,
}

pub(crate) async fn delete_selected(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(input): Json<DeleteInput>,
) -> Result<Json<Value>, ApiError> {
    if input.ids.is_empty() || input.ids.len() > 1000 {
        return Err(ApiError::bad_request(
            "invalid_selection",
            "Select between 1 and 1000 proxies",
        ));
    }
    let deleted = sqlx::query("DELETE FROM proxies WHERE user_id = $1 AND id = ANY($2)")
        .bind(identity.user_id)
        .bind(input.ids)
        .execute(&state.pool)
        .await?
        .rows_affected();
    Ok(Json(json!({"deleted": deleted})))
}

fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "Proxy not found")
}
