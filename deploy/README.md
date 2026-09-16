# GitHub Actions → prx.azimuthglobal.co

Цель: Ubuntu 24.04 ARM64, `95.216.192.41`, существующие PostgreSQL 16 и Nginx. Приложение слушает `127.0.0.1:8084`. Веб-панель и миграции встроены в бинарник.

Сборка выполняется **на GitHub-hosted runner** `ubuntu-24.04-arm`. Сервер получает готовый бинарник по SSH: Rust, Git, GitHub-токены и Docker для приложения на сервере не нужны.

## 1. Настроить GitHub

Открыть [Repository secrets](https://github.com/medk0v/proxy-microservice/settings/secrets/actions) → **New repository secret**:

| Secret | Значение |
| --- | --- |
| `DEPLOY_HOST` | `95.216.192.41` — исходный IP, SSH-порт 22 |
| `DEPLOY_USER` | `proxy-deploy` |
| `DEPLOY_SSH_KEY` | Полный приватный Ed25519-ключ без passphrase, с BEGIN/END-строками |
| `DEPLOY_KNOWN_HOSTS` | Проверенная строка SSH host key для `95.216.192.41` из локального `known_hosts` |

Для этого репозитория локально подготовлен отдельный ключ `artifacts/github-deploy/id_ed25519`, публичная часть — файл с `.pub`. Папка игнорируется Git. Копирование приватной части в буфер обмена на Mac из корня проекта:

```bash
pbcopy < artifacts/github-deploy/id_ed25519
```

Получить сохранённую ранее строку сервера **локально, без SSH-подключения**:

```bash
ssh-keygen -F 95.216.192.41 | awk '$2 == "ssh-ed25519"'
```

Не использовать `ssh-keyscan` в workflow как замену проверке сервера. При смене host key сначала независимо проверить новый ключ.

Автодеплой включён по умолчанию: дополнительных variables не требуется. Для временной остановки установить в [Actions variables](https://github.com/medk0v/proxy-microservice/settings/variables/actions) `DEPLOY_ENABLED=false`. Вернуть автодеплой можно значением `true` или удалением этой переменной. Environment `production` используется для истории деплоев; обязательные reviewers можно настроить отдельно, если нужен ручной допуск.

## 2. Подготовить сервер один раз

Этот раздел меняет production. Выполняется оператором после разрешения на первый запуск; сам workflow его не запускает.

Передать папку `deploy/` и **только публичный** файл `id_ed25519.pub` на сервер от существующего администратора `dev`. На сервере из каталога с этими файлами:

```bash
sudo bash deploy/bootstrap.sh ./id_ed25519.pub
```

Скрипт создаёт:

- пользователя приложения `proxy-microservice` и отдельного SSH-пользователя `proxy-deploy`;
- `/opt/proxy-microservice/{incoming,releases}` и systemd unit;
- роль PostgreSQL `proxy_service`, базу `proxy_microservice` и `/etc/proxy-microservice.env` с режимом `0600`;
- случайный пароль БД, пароль администратора и ключ шифрования **только если конфигурации ещё нет**;
- sudo-доступ deploy-пользователю только к `restart`, `stop`, `reset-failed` конкретного сервиса.

Существующий `.env` с Mac не копируется. Повторный bootstrap сохраняет секреты; без конфигурационного файла скрипт отказывается использовать уже существующую одноимённую БД/роль. Порт 8084 проверяется на занятость. Сервис включается для старта при загрузке, но до первого релиза не запускается.

### Nginx и HTTPS

Убедиться, что A-запись `prx.azimuthglobal.co` ведёт на `95.216.192.41`. Проверить, что новый site не заменяет уже существующий. Команды ниже выполняются **на сервере**, по очереди; если команда завершилась ошибкой, сначала устранить её:

```bash
sudo install -d -m 755 /var/www/letsencrypt
sudo install -m 644 deploy/nginx-http.conf /etc/nginx/sites-available/prx.azimuthglobal.co
sudo ln -s /etc/nginx/sites-available/prx.azimuthglobal.co /etc/nginx/sites-enabled/prx.azimuthglobal.co
sudo nginx -t
sudo systemctl reload nginx

sudo certbot certonly --webroot -w /var/www/letsencrypt -d prx.azimuthglobal.co --non-interactive

sudo install -m 644 deploy/nginx.conf /etc/nginx/sites-available/prx.azimuthglobal.co
sudo nginx -t
sudo systemctl reload nginx
```

Команда Certbot предполагает уже зарегистрированный ACME account на этом сервере. Если его нет, сначала зарегистрировать с email владельца. Для webroot challenge путь `/.well-known/acme-challenge/` должен быть доступен извне; Cloudflare redirect/WAF rules не должны мешать выдаче. После выдачи сертификата использовать Cloudflare **Full (strict)**.

Установить hook, который перезагружает Nginx после продления именно этого сертификата, и проверить автопродление:

```bash
sudo install -o root -g root -m 755 deploy/certbot-deploy-hook.sh /etc/letsencrypt/renewal-hooks/deploy/proxy-nginx-reload
sudo systemctl is-enabled certbot.timer
sudo certbot renew --cert-name prx.azimuthglobal.co --dry-run --run-deploy-hooks --non-interactive
```

## 3. Включить автодеплой

Сначала настроить сервер и добавить четыре secrets, затем отправить workflow в `master`. Автодеплой включён по умолчанию. Если ранее установлено `DEPLOY_ENABLED=false`, заменить значение на `true`.

- Push в `master`: fmt, Clippy, JS syntax, тесты деплоя, unit-тесты и интеграционные тесты с PostgreSQL 16, затем ARM64 release build и deploy.
- Pull request в `master`: только проверки, без production secrets и деплоя.
- Ручной запуск: **Actions → Check and deploy → Run workflow → master**. Он тоже выполняет проверки и сборку. Устаревший коммит не деплоится: сравнивается с текущим HEAD `master` через GitHub API на runner.
- Все официальные actions закреплены на commit SHA; Dependabot предлагает обновления. Артефакт скачивается только из того же workflow run, который прошёл проверки.

Релиз имеет имя `<commit SHA>-<run ID>-<attempt>`. Контрольная сумма проверяется до переключения `current`. Деплои сериализованы и в Actions, и серверным `flock`. Обновление требует короткого рестарта приложения. PostgreSQL и Nginx при обычном деплое не перезапускаются.

Если новая версия не стартовала или не отвечает `ok` на локальном `/health`, переключается предыдущий бинарник и проверяется его запуск; workflow остаётся красным. Если не удался самый первый релиз, сервис останавливается. Затем runner отдельно проверяет публичный HTTPS `/health`; ошибка DNS/Cloudflare/TLS помечает workflow красным, но не откатывает исправный локальный процесс.

**Откат бинарника не откатывает миграции БД.** Миграции запускаются приложением до открытия порта. Новые миграции должны быть совместимы с предыдущим релизом; перед изменением схемы нужна резервная копия. `/health` подтверждает HTTP-процесс после успешной инициализации; это не постоянная проверка подключения к БД.

## Обслуживание

На сервере от администратора:

```bash
sudo systemctl status proxy-microservice.service
sudo journalctl -u proxy-microservice.service -n 100 --no-pager
readlink /opt/proxy-microservice/current
# Секреты просматривать только в своём терминале, не публиковать в Actions logs:
sudo cat /etc/proxy-microservice.env
```

Логин админки — `ADMIN_USERNAME`, первоначальный пароль — `ADMIN_PASSWORD` в серверном файле. Изменение этой переменной само по себе не меняет уже созданного пользователя: для сброса есть команда приложения `reset-admin-password`.

Хранить резервную копию базы **вместе с неизменным `PROXY_ENCRYPTION_KEY`**. Без ключа пароли сохранённых прокси не расшифровать. Release-каталоги сохраняются для диагностики; старые неиспользуемые релизы можно удалять отдельно после проверки текущего symlink. Для возврата к старому коду с повторными проверками сделать `git revert` и push в `master`.
