use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use proxy_microservice::{application, config::Config};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::net::SocketAddr;
use tower::ServiceExt;
use uuid::Uuid;

async fn setup(pool: &PgPool) -> (Router, Uuid, String) {
    let user_id = Uuid::new_v4();
    let token = "a".repeat(43);
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, 'admin', 'unused-in-this-test')").bind(user_id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '1 hour')")
        .bind(Sha256::digest(token.as_bytes()).to_vec()).bind(user_id).execute(pool).await.unwrap();
    let config = Config {
        database_url: String::new(),
        bind_addr: "127.0.0.1:8080".parse().unwrap(),
        origin: "http://localhost:8080".into(),
        secure_cookies: false,
        encryption_key: [3; 32],
    };
    (
        application(pool.clone(), &config).await.unwrap(),
        user_id,
        format!("proxy_session={token}"),
    )
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    credential: &str,
    data: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .extension(ConnectInfo(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
        ))
        .header("X-Proxy-Request", "1");
    if credential.starts_with("px_") {
        request = request.header("Authorization", format!("Bearer {credential}"));
    } else if !credential.is_empty() {
        request = request.header("Cookie", credential);
    }
    let body = if let Some(data) = data {
        request = request.header("Content-Type", "application/json");
        Body::from(data.to_string())
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8(bytes.to_vec()).unwrap())),
    )
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires a local PostgreSQL DATABASE_URL"]
async fn bulk_import_filters_random_and_deduplication(pool: PgPool) {
    let (app, _, cookie) = setup(&pool).await;
    let invalid =
        json!({"text": "u:p@proxy.example:8000\nnot-valid", "region": "europe", "country": "de"});
    let (status, preview) = request(
        &app,
        "POST",
        "/api/proxies/preview",
        &cookie,
        Some(invalid.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (preview["new"].as_u64(), preview["invalid"].as_u64()),
        (Some(1), Some(1))
    );
    assert!(!preview.to_string().contains("password"));
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/proxies/import",
            &cookie,
            Some(invalid.clone())
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        request(&app, "GET", "/api/proxies", &cookie, None).await.1["total"],
        0
    );
    let mut partial = invalid;
    partial["skip_invalid"] = true.into();
    assert_eq!(
        request(&app, "POST", "/api/proxies/import", &cookie, Some(partial))
            .await
            .1["inserted"],
        1
    );
    let text = (10000..11000)
        .map(|port| format!("http://alice:synthetic-password@proxy.example:{port}"))
        .collect::<Vec<_>>()
        .join("\n");
    let input = json!({"text": text, "region": "oceania", "country": "AU"});
    let (first, second) = tokio::join!(
        request(
            &app,
            "POST",
            "/api/proxies/import",
            &cookie,
            Some(input.clone())
        ),
        request(
            &app,
            "POST",
            "/api/proxies/import",
            &cookie,
            Some(input.clone())
        )
    );
    assert_eq!((first.0, second.0), (StatusCode::OK, StatusCode::OK));
    assert_eq!(
        first.1["inserted"].as_u64().unwrap() + second.1["inserted"].as_u64().unwrap(),
        1000
    );
    assert_eq!(
        first.1["duplicates"].as_u64().unwrap() + second.1["duplicates"].as_u64().unwrap(),
        1000
    );
    let (_, preview) = request(&app, "POST", "/api/proxies/preview", &cookie, Some(input)).await;
    assert_eq!(preview["new"], 0);
    assert_eq!(preview["duplicates"], 1000);
    let (_, list) = request(
        &app,
        "GET",
        "/api/proxies?region=oceania&country=au&per_page=10&page=2",
        &cookie,
        None,
    )
    .await;
    assert_eq!(list["total"], 1000);
    assert_eq!(list["items"].as_array().unwrap().len(), 10);
    assert!(list["items"][0].get("password").is_none());
    let (_, any) = request(&app, "GET", "/api/proxies/random", &cookie, None).await;
    assert!(any["url"].as_str().unwrap().starts_with("http://"));
    for query in ["region=europe", "country=DE", "country=de&region=europe"] {
        let (status, proxy) = request(
            &app,
            "GET",
            &format!("/api/proxies/random?{query}"),
            &cookie,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(proxy["country"], "DE");
        assert_eq!(proxy["region"], "europe");
    }
    assert_eq!(
        request(&app, "GET", "/api/proxies/random?country=US", &cookie, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &app,
            "GET",
            "/api/proxies/random?region=europe&country=AU",
            &cookie,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&app, "GET", "/api/proxies/random?country=ZZ", &cookie, None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (_, export) = request(
        &app,
        "GET",
        "/api/proxies/export?country=AU&format=url",
        &cookie,
        None,
    )
    .await;
    assert_eq!(export.as_str().unwrap().lines().count(), 1000);
    let ciphertext: Vec<u8> = sqlx::query_scalar(
        "SELECT password_encrypted FROM proxies WHERE username = 'alice' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        !ciphertext
            .windows(18)
            .any(|part| part == b"synthetic-password")
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires a local PostgreSQL DATABASE_URL"]
async fn authentication_keys_revocation_and_owner_isolation(pool: PgPool) {
    let (app, first_user, cookie) = setup(&pool).await;
    for path in [
        "/api/proxies",
        "/api/proxies/random",
        "/api/proxies/export",
        "/api/keys",
    ] {
        assert_eq!(
            request(&app, "GET", path, "", None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    request(
        &app,
        "POST",
        "/api/proxies/import",
        &cookie,
        Some(json!({"text": "socks5://u:p@one.example:1080"})),
    )
    .await;
    let (_, proxy) = request(&app, "GET", "/api/proxies/random", &cookie, None).await;
    assert_eq!(proxy["region"], "unknown");
    assert_eq!(proxy["country"], "unknown");
    let (_, key) = request(
        &app,
        "POST",
        "/api/keys",
        &cookie,
        Some(json!({"name": "integration"})),
    )
    .await;
    let token = key["token"].as_str().unwrap();
    assert_eq!(
        request(
            &app,
            "GET",
            "/api/proxies/random?country=unknown",
            token,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", "/api/keys", token, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/proxies/import",
            token,
            Some(json!({"text": "two.example:80"}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let stored_key: Vec<u8> =
        sqlx::query_scalar("SELECT token_hash FROM api_keys WHERE user_id = $1")
            .bind(first_user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_key, Sha256::digest(token.as_bytes()).to_vec());
    let second_user = Uuid::new_v4();
    let second_token = "b".repeat(43);
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, 'second', 'unused')")
        .bind(second_user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '1 hour')")
        .bind(Sha256::digest(second_token.as_bytes()).to_vec()).bind(second_user).execute(&pool).await.unwrap();
    let second_cookie = format!("proxy_session={second_token}");
    assert_eq!(
        request(
            &app,
            "GET",
            &format!("/api/proxies/{}", proxy["id"].as_str().unwrap()),
            &second_cookie,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&app, "GET", "/api/proxies", &second_cookie, None)
            .await
            .1["total"],
        0
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/proxies/delete",
            &second_cookie,
            Some(json!({"ids": [proxy["id"]]}))
        )
        .await
        .1["deleted"],
        0
    );
    assert_eq!(
        request(
            &app,
            "DELETE",
            &format!("/api/keys/{}", key["id"].as_str().unwrap()),
            &cookie,
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&app, "GET", "/api/proxies", token, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, "POST", "/api/auth/logout", &cookie, None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&app, "GET", "/api/proxies", &cookie, None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires a local PostgreSQL DATABASE_URL"]
async fn login_cookie_csrf_expiry_and_throttling(pool: PgPool) {
    use argon2::{
        Argon2, PasswordHasher,
        password_hash::{SaltString, rand_core::OsRng},
    };
    let (app, user_id, _) = setup(&pool).await;
    let password = "test-only-password";
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .unwrap()
        .to_string();
    sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
        .bind(hash)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    for (header, value) in [
        ("Origin", "https://untrusted.example"),
        ("X-Proxy-Request", "0"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header(header, value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header("X-Proxy-Request", "1")
        .header("Origin", "http://localhost:8080")
        .header("Content-Type", "application/json")
        .extension(ConnectInfo(
            "127.0.0.2:12345".parse::<SocketAddr>().unwrap(),
        ))
        .body(Body::from(
            json!({"username": "admin", "password": password}).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let set_cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    let cookie = set_cookie.split(';').next().unwrap();
    assert_eq!(
        self::request(&app, "GET", "/api/auth/me", cookie, None)
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        self::request(&app, "GET", "/api/auth/me", cookie, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..10 {
        assert_eq!(
            self::request(
                &app,
                "POST",
                "/api/auth/login",
                "",
                Some(json!({"username": "admin", "password": "incorrect"}))
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        self::request(
            &app,
            "POST",
            "/api/auth/login",
            "",
            Some(json!({"username": "admin", "password": password}))
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[test]
#[ignore = "set PROXY_IMPORT_FIXTURE to a private local file; never commit the fixture"]
fn private_import_fixture() {
    use proxy_microservice::parser::{Format, Protocol, parse_import};
    let path = std::env::var("PROXY_IMPORT_FIXTURE").unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    let parsed = parse_import(&text, Format::Auto, Protocol::Http).unwrap();
    assert!(parsed.errors.is_empty());
    assert_eq!(parsed.proxies.len(), 1000);
    assert_eq!(parsed.duplicates, 0);
}
