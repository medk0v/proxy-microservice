use std::{env, net::SocketAddr};

use anyhow::{Context, bail};
use base64::{Engine, engine::general_purpose::STANDARD};

pub struct Config {
    pub database_url: String,
    pub bind_addr: SocketAddr,
    pub origin: String,
    pub secure_cookies: bool,
    pub encryption_key: [u8; 32],
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = env::var("DATABASE_URL").context("DATABASE_URL is required")?;
        let bind_addr = env::var("BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8080".into())
            .parse()
            .context("Invalid BIND_ADDR")?;
        let origin = env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:8080".into());
        let url = url::Url::parse(&origin).context("Invalid APP_ORIGIN")?;
        if !matches!(url.scheme(), "http" | "https") || url.origin().ascii_serialization() != origin
        {
            bail!("APP_ORIGIN must be an HTTP(S) origin, without a path or trailing slash");
        }
        let secure_cookies = match env::var("COOKIE_SECURE").as_deref() {
            Ok("true") => true,
            Ok("false") => false,
            Err(_) => url.scheme() == "https",
            _ => bail!("COOKIE_SECURE must be true or false"),
        };
        let is_loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !is_loopback && (url.scheme() != "https" || !secure_cookies) {
            bail!("Non-local APP_ORIGIN requires HTTPS and COOKIE_SECURE=true");
        }
        if secure_cookies && url.scheme() != "https" {
            bail!("Secure cookies require an HTTPS APP_ORIGIN");
        }
        let encryption_key = STANDARD
            .decode(env::var("PROXY_ENCRYPTION_KEY").context("PROXY_ENCRYPTION_KEY is required")?)
            .context("PROXY_ENCRYPTION_KEY must be base64")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("PROXY_ENCRYPTION_KEY must contain exactly 32 bytes"))?;
        Ok(Self {
            database_url,
            bind_addr,
            origin,
            secure_cookies,
            encryption_key,
        })
    }
}
