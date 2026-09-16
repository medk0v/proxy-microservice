use std::{
    collections::HashMap,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
};
use uuid::Uuid;

use crate::config::TelegramConfig;

const COOLDOWN: Duration = Duration::from_secs(60);
const MAX_COOLDOWNS: usize = 4096;
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

#[derive(Clone)]
pub(crate) struct TelegramNotifier(Option<Arc<NotifierInner>>);

impl TelegramNotifier {
    pub(crate) fn new(config: Option<&TelegramConfig>) -> Self {
        Self(config.map(|config| {
            Arc::new(NotifierInner {
                config: config.clone(),
                cooldowns: Mutex::new(Cooldowns::default()),
                slots: Arc::new(Semaphore::new(2)),
            })
        }))
    }

    /// Best-effort delivery without delaying a proxy response. Each user can trigger
    /// one attempt per minute across all operations; at most two attempts run at once.
    pub(crate) fn no_proxies(
        &self,
        user_id: Uuid,
        operation: &'static str,
        protocol: Option<&str>,
        region: Option<&str>,
        country: Option<&str>,
    ) {
        let Some(inner) = &self.0 else { return };
        let Ok(slot) = inner.slots.clone().try_acquire_owned() else {
            return;
        };
        let Ok(mut cooldowns) = inner.cooldowns.lock() else {
            return;
        };
        if !cooldowns.reserve(user_id, Instant::now()) {
            return;
        }
        drop(cooldowns);

        let mut text = String::from("Нет доступных прокси для запроса.");
        // Only bounded, validated filter labels are included. Never include the
        // search term, proxy data, credentials, or requester identity.
        for (label, value) in [
            ("Операция", Some(operation)),
            ("Протокол", protocol),
            ("Регион", region),
            ("Страна", country),
        ] {
            if let Some(value) = value.filter(|value| {
                !value.is_empty()
                    && value.len() <= 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            }) {
                text.push_str(&format!("\n{label}: {value}"));
            }
        }

        let inner = inner.clone();
        tokio::spawn(async move {
            let _slot = slot;
            if !send_with_curl(Path::new("curl"), &inner.config, &text).await {
                tracing::warn!("Telegram no-proxy notification failed");
            }
        });
    }
}

struct NotifierInner {
    config: TelegramConfig,
    cooldowns: Mutex<Cooldowns>,
    slots: Arc<Semaphore>,
}

#[derive(Default)]
struct Cooldowns(HashMap<Uuid, Instant>);

impl Cooldowns {
    fn reserve(&mut self, user_id: Uuid, now: Instant) -> bool {
        self.0
            .retain(|_, sent| now.duration_since(*sent) < COOLDOWN);
        if self.0.contains_key(&user_id) || self.0.len() >= MAX_COOLDOWNS {
            return false;
        }
        self.0.insert(user_id, now);
        true
    }
}

fn curl_quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}

async fn send_with_curl(program: &Path, config: &TelegramConfig, text: &str) -> bool {
    let payload = serde_json::json!({ "chat_id": config.chat_id, "text": text }).to_string();
    let input = format!(
        "url = {}\nheader = \"Content-Type: application/json\"\ndata-raw = {}\n",
        curl_quote(&format!(
            "https://api.telegram.org/bot{}/sendMessage",
            config.bot_token
        )),
        curl_quote(&payload),
    );
    // -q must be first to disable .curlrc. Credentials and message stay off argv;
    // stderr and the Telegram response are never logged.
    let Ok(mut child) = Command::new(program)
        .args([
            "-q",
            "--silent",
            "--fail",
            "--connect-timeout",
            "3",
            "--max-time",
            "10",
            "--proto",
            "=https",
            "--config",
            "-",
        ])
        .env_remove("TELEGRAM_BOT_TOKEN")
        .env_remove("TELEGRAM_CHAT_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    else {
        return false;
    };

    let result = tokio::time::timeout(Duration::from_secs(12), async {
        let mut stdin = child.stdin.take().ok_or(())?;
        stdin.write_all(input.as_bytes()).await.map_err(|_| ())?;
        drop(stdin);
        let mut response = Vec::new();
        child
            .stdout
            .take()
            .ok_or(())?
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut response)
            .await
            .map_err(|_| ())?;
        if response.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(());
        }
        if !child.wait().await.map_err(|_| ())?.success() {
            return Err(());
        }
        let response: serde_json::Value = serde_json::from_slice(&response).map_err(|_| ())?;
        if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(());
        }
        Ok(())
    })
    .await;
    matches!(result, Ok(Ok(())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_is_per_user_and_expires() {
        let mut cooldowns = Cooldowns::default();
        let now = Instant::now();
        let user = Uuid::new_v4();
        assert!(cooldowns.reserve(user, now));
        assert!(!cooldowns.reserve(user, now + Duration::from_secs(59)));
        assert!(cooldowns.reserve(Uuid::new_v4(), now + Duration::from_secs(59)));
        assert!(cooldowns.reserve(user, now + COOLDOWN));
        assert_eq!(cooldowns.0.len(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn curl_receives_secrets_only_on_stdin_and_checks_response() {
        use std::os::unix::fs::PermissionsExt;

        let directory =
            std::env::temp_dir().join(format!("proxy-notification-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let program = directory.join("curl");
        let args_file = directory.join("args");
        let input_file = directory.join("input");
        let script = "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"${0%/*}/args\"\ncat > \"${0%/*}/input\"\nprintf '%s' '{\"ok\":true}'\n";
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = TelegramConfig {
            bot_token: "123:test_token".into(),
            chat_id: "-123456".into(),
        };
        let text = "Нет прокси\nСтрана: US";
        assert!(send_with_curl(&program, &config, text).await);
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert_eq!(
            args,
            "-q\n--silent\n--fail\n--connect-timeout\n3\n--max-time\n10\n--proto\n=https\n--config\n-\n"
        );
        assert!(!args.contains(&config.bot_token));
        assert!(!args.contains(&config.chat_id));
        let input = std::fs::read_to_string(&input_file).unwrap();
        assert!(
            input.contains("url = \"https://api.telegram.org/bot123:test_token/sendMessage\"\n")
        );
        assert!(input.contains("\\\"chat_id\\\":\\\"-123456\\\""));
        assert!(input.contains("Нет прокси\\\\nСтрана: US"));

        for (response, status) in [
            ("{\"ok\":false}", 0),
            ("invalid json", 0),
            ("{\"ok\":true}", 22),
        ] {
            std::fs::write(
                &program,
                format!("#!/bin/sh\ncat > /dev/null\nprintf '%s' '{response}'\nexit {status}\n"),
            )
            .unwrap();
            assert!(!send_with_curl(&program, &config, text).await);
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
