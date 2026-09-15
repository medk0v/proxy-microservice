#!/usr/bin/env python3
"""Run PostgreSQL integration tests against a local database only."""
import argparse
import os
from pathlib import Path
import subprocess
from urllib.parse import urlsplit

parser = argparse.ArgumentParser()
parser.add_argument("--fixture", type=Path, help="Optional private 1000-line proxy list to validate without storing it")
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
environment = os.environ.copy()
config = root / ".env"
if config.exists():
    for line in config.read_text().splitlines():
        if line.strip() and not line.lstrip().startswith("#") and "=" in line:
            name, value = line.split("=", 1)
            environment.setdefault(name, value)
url = environment.get("DATABASE_URL", "")
if urlsplit(url).hostname not in {"127.0.0.1", "localhost", "::1"}:
    raise SystemExit("Tests require a local DATABASE_URL. SQLx creates isolated temporary test databases.")
command = ["cargo", "test", "--locked", "--test", "api", "--", "--ignored"]
if args.fixture:
    environment["PROXY_IMPORT_FIXTURE"] = str(args.fixture.resolve())
else:
    command += ["--skip", "private_import_fixture"]
raise SystemExit(subprocess.run(command, cwd=root, env=environment).returncode)
