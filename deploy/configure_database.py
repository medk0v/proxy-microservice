#!/usr/bin/env python3
"""Called by bootstrap.sh as root on the server; never reads a developer .env."""
import base64
import os
from pathlib import Path
import secrets
import subprocess
from urllib.parse import urlsplit

CONFIG = Path('/etc/proxy-microservice.env')
DATABASE = 'proxy_microservice'
ROLE = 'proxy_service'


def postgres(sql):
    result = subprocess.run(
        ['runuser', '-u', 'postgres', '--', 'psql', '-XqAt', '-v', 'ON_ERROR_STOP=1', '-d', 'postgres'],
        input=sql, text=True, capture_output=True, check=False,
    )
    if result.returncode:
        # SQL errors can contain the statement with the generated database password.
        raise SystemExit('PostgreSQL setup failed; no SQL or credentials printed. Inspect the server locally.')
    return result.stdout.strip()


def configure():
    if os.geteuid() != 0:
        raise SystemExit('Run via bootstrap.sh as root on the production server.')
    if CONFIG.is_symlink():
        raise SystemExit('Refusing a symlink for the production environment file.')
    if not CONFIG.exists():
        existing = postgres(
            f"SELECT EXISTS(SELECT 1 FROM pg_roles WHERE rolname = '{ROLE}') "
            f"OR EXISTS(SELECT 1 FROM pg_database WHERE datname = '{DATABASE}');"
        )
        if existing != 'f':
            raise SystemExit('Database or role already exists without the environment file; refusing to take it over.')
        values = {
            'DATABASE_URL': f'postgres://{ROLE}:{secrets.token_hex(32)}@127.0.0.1:5432/{DATABASE}',
            'BIND_ADDR': '127.0.0.1:8084',
            'APP_ORIGIN': 'https://prx.azimuthglobal.co',
            'COOKIE_SECURE': 'true',
            'PROXY_ENCRYPTION_KEY': base64.b64encode(secrets.token_bytes(32)).decode(),
            'ADMIN_USERNAME': 'admin',
            'ADMIN_PASSWORD': secrets.token_urlsafe(32),
            'RUST_LOG': 'proxy_microservice=info',
        }
        descriptor = os.open(CONFIG, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, 'w') as config:
            config.write(''.join(f'{name}={value}\n' for name, value in values.items()))
    else:
        values = dict(
            line.split('=', 1) for line in CONFIG.read_text().splitlines()
            if line.strip() and not line.lstrip().startswith('#')
        )
    os.chmod(CONFIG, 0o600)
    url = urlsplit(values.get('DATABASE_URL', ''))
    if (url.scheme != 'postgres' or url.hostname != '127.0.0.1' or url.port != 5432
            or url.username != ROLE or url.path != '/' + DATABASE or not url.password):
        raise SystemExit('Existing DATABASE_URL does not match this deployment. File kept unchanged.')
    password = url.password.replace("'", "''")
    # These SELECTs make retries safe without resetting an existing role password.
    postgres(
        f"SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', '{ROLE}', '{password}') "
        f"WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{ROLE}')\n\\gexec\n"
        f"SELECT format('CREATE DATABASE %I OWNER %I', '{DATABASE}', '{ROLE}') "
        f"WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = '{DATABASE}')\n\\gexec\n"
    )
    owner = postgres(
        f"SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = '{DATABASE}';"
    )
    if owner != ROLE:
        raise SystemExit('Existing database has another owner; refusing to change privileges.')
    postgres(f'REVOKE ALL ON DATABASE {DATABASE} FROM PUBLIC;')
    result = subprocess.run(
        ['psql', '-XqAt', '-v', 'ON_ERROR_STOP=1', '-h', '127.0.0.1', '-p', '5432',
         '-U', ROLE, '-d', DATABASE, '-c', 'SELECT 1'],
        env={**os.environ, 'PGPASSWORD': url.password}, capture_output=True, check=False,
    )
    if result.returncode or result.stdout.strip() != b'1':
        raise SystemExit('Database login failed; existing credentials were not changed.')
    print('Database ready; production credentials kept in /etc/proxy-microservice.env.')


if __name__ == '__main__':
    configure()
