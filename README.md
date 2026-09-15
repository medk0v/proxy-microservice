# Proxy microservice

Небольшой сервис на Rust + PostgreSQL: один администратор, массовый импорт прокси, регионы и страны, API для получения реквизитов. Веб-панель встроена в Rust-бинарник; отдельная сборка фронтенда и Node.js не нужны.

Стек: [Axum 0.8](https://docs.rs/axum/0.8.9/axum/), [SQLx 0.8](https://docs.rs/sqlx/0.8.6/sqlx/), PostgreSQL 17. Сервис хранит и выдаёт прокси, но не пересылает через себя трафик и не проверяет их доступность или фактическую геолокацию.

## Локальный запуск

Нужны Rust 1.88+ и Docker Compose.

```bash
python3 scripts/setup.py
docker compose up -d db --wait
cargo run --locked
```

Откройте **http://localhost:8080**. Логин — `ADMIN_USERNAME`, пароль — `ADMIN_PASSWORD` в `.env`. Скрипт создаёт случайные пароли и ключ шифрования; существующий `.env` не перезаписывает. PostgreSQL доступна только на `127.0.0.1:55473`, данные сохраняются в отдельном Docker volume. Миграции применяются при запуске сервиса.

Полный запуск через Docker:

```bash
python3 scripts/setup.py  # только при первом запуске
docker compose --profile full up --build -d
```

Остановка: `docker compose --profile full down`. Данные при этом сохраняются; `down -v` удаляет базу. Если порт БД занят, измените `POSTGRES_PORT` и порт в `DATABASE_URL` одновременно. Если занят порт приложения, согласованно измените `BIND_ADDR`, `APP_PORT` и `APP_ORIGIN`.

## Импорт

«Добавить прокси» → вставьте список или загрузите TXT/CSV в UTF-8 → выберите регион и страну → «Проверить список» → «Добавить».

- До 10 000 строк / 2 МиБ за один импорт, одна запись на строку.
- Регион и страна назначаются всем новым записям в этом импорте. По умолчанию оба поля `unknown`.
- Пустые строки, UTF-8 BOM и комментарии с `#` поддерживаются.
- Автоматически распознаются смешанные форматы. Для неоднозначных строк выберите формат вручную.
- Ошибки показываются с номерами строк, без повторения паролей. По умолчанию импорт при наличии ошибок не выполняется; можно явно включить пропуск ошибочных строк.
- Дубли проверяются по протоколу, нормализованному хосту, порту, логину и паролю. Повторный и одновременный импорт не создают повторных записей. География существующих дублей не меняется.

Поддерживаемые форматы:

```text
login:password@host:port
host,port,login,password
http://login:password@host:port
https://login:password@host:port
socks4://login:password@host:port
socks5://login:password@host:port
login:password:host:port
host:port:login:password
host:port
```

Схема в URL имеет приоритет над выбранным протоколом по умолчанию. IPv6 в строках с двоеточиями указывается в квадратных скобках: `socks5://user:pass@[2001:db8::1]:1080`. В CSV IPv6 может быть без скобок. Спецсимволы в URL-креденшелах можно передавать через percent-encoding, например `%40` вместо `@`. CSV поддерживает кавычки и запятые внутри полей, без заголовка и многострочных полей.

Регионы: `custom`, `world_mix`, `europe`, `asia`, `northern_america`, `latin_america_and_caribbean`, `africa`, `oceania`, `unknown`. Страны — двухбуквенные коды из списка в админке (включая `XK` для Kosovo), либо `unknown`. «Все страны / регионы» в фильтрах означает отсутствие фильтра; `world_mix` и `custom` — конкретные метки.

## API

В админке откройте **«API и ключи»**. Создайте ключ, скопируйте его один раз. Ключ разрешает чтение, включая пароли прокси, и может быть отозван там же. Краткая документация и примеры `curl` находятся в этом же окне.

```bash
# Адрес вашего экземпляра и ключ из админки
PROXY_SERVICE_URL='http://localhost:8080'
PROXY_API_KEY='px_ваш_ключ'

# Любой случайный прокси
curl --fail-with-body "$PROXY_SERVICE_URL/api/proxies/random" \
  -H "Authorization: Bearer $PROXY_API_KEY"

# По региону
curl --fail-with-body "$PROXY_SERVICE_URL/api/proxies/random?region=europe" \
  -H "Authorization: Bearer $PROXY_API_KEY"

# По стране
curl --fail-with-body "$PROXY_SERVICE_URL/api/proxies/random?country=DE" \
  -H "Authorization: Bearer $PROXY_API_KEY"

# Все фильтры одновременно
curl --fail-with-body "$PROXY_SERVICE_URL/api/proxies/random?region=europe&country=DE&protocol=http" \
  -H "Authorization: Bearer $PROXY_API_KEY"
```

Ответ:

```json
{
  "id": "ac687bb2-3c90-4128-8e5b-a4c36d3f83d4",
  "protocol": "http",
  "host": "proxy.example",
  "port": 10000,
  "username": "login",
  "password": "password",
  "url": "http://login:password@proxy.example:10000",
  "region": "europe",
  "country": "DE"
}
```

Без фильтров возвращается случайная запись из всего вашего списка. Фильтры объединяются через AND. `country` принимает код без учёта регистра или `unknown`. `region` и `protocol` — значения в нижнем регистре. Случайный выбор не резервирует запись: повторные вызовы могут вернуть один и тот же прокси.

| Метод | Назначение |
| --- | --- |
| `GET /api/proxies/random` | Случайный прокси с паролем и готовым URL |
| `GET /api/proxies` | Список без паролей: `{items, total, page, per_page}` |
| `GET /api/proxies/{id}` | Реквизиты одного прокси с паролем |
| `GET /api/proxies/export` | TXT со всеми подходящими прокси, до 50 000 строк |
| `GET /api/countries` | Поддерживаемые двухбуквенные коды стран |
| `GET /api/auth/me` | Текущая учётная запись и тип авторизации |

Для `/random`, списка и экспорта: `region`, `country`, `protocol`, `search` (часть хоста или логина). Для списка: `page` от 1, `per_page` от 1 до 200 (по умолчанию 50). Экспорт: `format=url` по умолчанию, также `credentials_at`, `csv`, `credentials_first`, `host_first`. URL сохраняет протокол и кодирует спецсимволы; остальные форматы не содержат протокол и при повторном импорте требуют его выбора. Экспорт всегда скачивается как `.txt`.

HTTP-статусы: `200` — успех, `400` — неверные параметры, `401` — нет действующего ключа/сессии, `403` — запрещённая операция, `404` — запись не найдена (включая отсутствие прокси под фильтры), `429` — лимит входа, `500` — ошибка сервера. Ошибки бизнес-логики содержат `{"error":{"code":"not_found","message":"Proxy not found"}}`; ошибки схемы JSON/query могут быть обычным текстом Axum.

### Операции админки

Доступны только с браузерной сессией. Все изменяющие запросы требуют `X-Proxy-Request: 1`. Bearer-ключи не разрешают импорт, удаление и управление ключами.

| Метод | Тело / назначение |
| --- | --- |
| `POST /api/auth/login` | `{"username":"admin","password":"..."}` → HttpOnly cookie |
| `POST /api/auth/logout` | Отзыв текущей сессии |
| `POST /api/proxies/preview` | Проверка импорта без записи |
| `POST /api/proxies/import` | Импорт с транзакцией |
| `POST /api/proxies/delete` | `{"ids":["uuid"]}`, до 1000 ID |
| `GET /api/keys` | Метаданные ключей; секреты не возвращаются |
| `POST /api/keys` | `{"name":"my-service"}` → ключ, показываемый один раз |
| `DELETE /api/keys/{id}` | Немедленный отзыв ключа |

Тело проверки/импорта:

```json
{
  "text": "http://login:password@proxy.example:10000",
  "format": "auto",
  "protocol": "http",
  "region": "europe",
  "country": "DE",
  "skip_invalid": false
}
```

`format`: `auto`, `url`, `http_url`, `socks5_url`, `credentials_at`, `csv`, `credentials_first`, `host_first`, `host_port`. При ошибочных строках строгий импорт возвращает `422` и ничего не записывает.

## Авторизация и настройки

- Пароль администратора хешируется через Argon2id. При первом запуске учётная запись создаётся из `.env`, затем пароль берётся из базы.
- Сессии действуют 12 часов, cookie `HttpOnly`, `SameSite=Strict`; выход отзывает сессию в базе. API-ключи не истекают автоматически, работают до отзыва. В базе хранятся только хеши случайных токенов.
- Пароли прокси зашифрованы AES-256-GCM. **Сохраните `PROXY_ENCRYPTION_KEY` вместе с резервной копией базы**. Замена или потеря ключа делает существующие пароли нечитаемыми; механизм ротации ключа не реализован.
- `APP_ORIGIN` должен точно совпадать с адресом в браузере, без завершающего `/`. Для внешнего домена нужны HTTPS и `COOKIE_SECURE=true`; HTTP допускается только для localhost. Compose публикует приложение и БД на loopback. TLS завершается на вашем reverse proxy.
- Ограничение входа: 10 попыток за 60 секунд на IP, не более 4 одновременных проверок пароля. Forwarded-заголовки не используются; за reverse proxy лимит приходится на адрес прокси.
- Лимиты: JSON до 4 МиБ, импортированный текст до 2 МиБ, запрос до 30 секунд, SQL до 15 секунд. Пароли, тела запросов и ключи не записываются в логи.
- Учётная запись имеет доступ только к своим прокси и ключам. Публичной регистрации нет.

Для смены пароля измените `ADMIN_PASSWORD` в `.env` и выполните:

```bash
cargo run --locked -- reset-admin-password
# или, при запуске в Docker:
docker compose --profile full run --rm app reset-admin-password
```

Команда отзывает все сессии и API-ключи этого администратора.

## Проверки

```bash
cargo fmt --all -- --check
cargo test --locked
cargo clippy --all-targets --all-features --locked -- -D warnings

# PostgreSQL должна быть запущена; тестам нужны права CREATE DATABASE.
# SQLx создаёт отдельные временные базы, не использует рабочие таблицы.
python3 scripts/test_integration.py

# Необязательно: проверить собственный файл из 1000 строк без его сохранения в БД
python3 scripts/test_integration.py --fixture /absolute/path/to/MyList.txt
```

Интеграционные тесты проверяют строгий/частичный импорт, одновременный импорт 1000 строк, дедупликацию, фильтры и случайную выдачу, запреты для анонимного запроса, разделение пользователей, отзыв и права API-ключей, CSRF, срок сессии и ограничение входа. Приватные списки прокси в проект не копируются.
