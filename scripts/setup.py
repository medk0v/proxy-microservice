#!/usr/bin/env python3
"""Create a local configuration with unique secrets; never overwrite existing state."""
import base64
import os
from pathlib import Path
import secrets

destination = Path(__file__).resolve().parent.parent / ".env"
database_password = secrets.token_urlsafe(24)
admin_password = secrets.token_urlsafe(18)
key = base64.b64encode(secrets.token_bytes(32)).decode()
content = f"""POSTGRES_PASSWORD={database_password}
POSTGRES_PORT=55473
DATABASE_URL=postgresql://proxy_service:{database_password}@127.0.0.1:55473/proxy_service
BIND_ADDR=127.0.0.1:8080
APP_PORT=8080
APP_ORIGIN=http://localhost:8080
COOKIE_SECURE=false
ADMIN_USERNAME=admin
ADMIN_PASSWORD={admin_password}
PROXY_ENCRYPTION_KEY={key}
"""
try:
    fd = os.open(destination, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
except FileExistsError:
    raise SystemExit(".env already exists; left unchanged.")
with os.fdopen(fd, "w") as output:
    output.write(content)
print("Created .env (permissions 0600). Administrator login and password are in ADMIN_USERNAME / ADMIN_PASSWORD.")
print("Keep PROXY_ENCRYPTION_KEY with database backups: it is required to decrypt proxy passwords.")
