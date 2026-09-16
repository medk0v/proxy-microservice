use std::{env, net::SocketAddr};

use anyhow::{Context, bail};
use base64::{Engine, engine::general_purpose::STANDARD};

pub struct Config {
    pub database_url: String,
    pub bind_addr: SocketAddr,
    pub origin: String,
    pub secure_cookies: bool,
    pub encryption_key: [u8; 32],
    pub telegram: Option<TelegramConfig>,
}

#[derive(Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
}

impl TelegramConfig {
    fn from_values(
        bot_token: Option<String>,
        chat_id: Option<String>,
    ) -> anyhow::Result<Option<Self>> {
        let bot_token = bot_token.filter(|value| !value.trim().is_empty());
        let chat_id = chat_id.filter(|value| !value.trim().is_empty());
        let (bot_token, chat_id) = match (bot_token, chat_id) {
            (None, None) => return Ok(None),
            (Some(bot_token), Some(chat_id)) => (bot_token, chat_id),
            _ => bail!("Set both TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID, or leave both empty"),
        };
        let valid_token = bot_token.split_once(':').is_some_and(|(id, secret)| {
            !id.is_empty()
                && id.bytes().all(|byte| byte.is_ascii_digit())
                && !secret.is_empty()
                && secret
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        });
        if !valid_token || bot_token.len() > 256 {
            bail!("Invalid TELEGRAM_BOT_TOKEN format");
        }
        let valid_chat = chat_id.parse::<i64>().is_ok_and(|id| id != 0)
            || chat_id.strip_prefix('@').is_some_and(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            });
        if !valid_chat || chat_id.len() > 128 {
            bail!("TELEGRAM_CHAT_ID must be a nonzero numeric chat ID or @channel_username");
        }
        Ok(Some(Self { bot_token, chat_id }))
    }
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
            telegram: TelegramConfig::from_values(
                env::var("TELEGRAM_BOT_TOKEN").ok(),
                env::var("TELEGRAM_CHAT_ID").ok(),
            )?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::TelegramConfig;

    #[test]
    fn telegram_settings_are_optional_but_must_be_paired() {
        assert!(TelegramConfig::from_values(None, None).unwrap().is_none());
        assert!(
            TelegramConfig::from_values(Some(String::new()), Some(String::new()))
                .unwrap()
                .is_none()
        );
        assert!(TelegramConfig::from_values(Some("123:test_token".into()), None).is_err());
        assert!(TelegramConfig::from_values(None, Some("-123".into())).is_err());
        assert!(
            TelegramConfig::from_values(Some("123:test_token".into()), Some("-123".into()))
                .unwrap()
                .is_some()
        );
        assert!(
            TelegramConfig::from_values(Some("123:test_token".into()), Some("@my_channel".into()))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn telegram_settings_reject_invalid_values_without_echoing_secrets() {
        for (token, chat) in [
            ("secret-token", "123"),
            ("123:secret\nurl = attacker", "123"),
            ("123:secret", "0"),
            ("123:secret", "@"),
            ("123:secret", "123\nurl = attacker"),
        ] {
            let error = TelegramConfig::from_values(Some(token.into()), Some(chat.into()))
                .err()
                .expect("invalid settings must fail")
                .to_string();
            assert!(!error.contains(token));
            if chat.len() > 1 {
                assert!(!error.contains(chat));
            }
        }
    }
}
