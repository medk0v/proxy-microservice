#!/usr/bin/env bash
# ONE-TIME, EXPLICIT SERVER SETUP. Never called by GitHub Actions.
# Usage on the production server: sudo bash deploy/bootstrap.sh /path/to/deploy-key.pub
set -Eeuo pipefail
umask 077

if [[ $EUID != 0 || $# != 1 ]]; then
    echo 'Usage as root: bash deploy/bootstrap.sh /path/to/deploy-key.pub' >&2
    exit 2
fi
[[ $(uname -m) == aarch64 ]] || { echo 'Expected an ARM64 server.' >&2; exit 1; }
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
public_key=$(cat -- "$1")
[[ $public_key =~ ^ssh-ed25519\ [A-Za-z0-9+/=]+(\ [^$'\n']*)?$ ]] || {
    echo 'Expected exactly one Ed25519 public key, not a private key.' >&2
    exit 1
}
ssh-keygen -l -f "$1" > /dev/null
for command in python3 psql runuser systemctl visudo flock ss curl; do
    command -v "$command" > /dev/null
done
exec 9>/run/lock/proxy-microservice-bootstrap.lock
flock -n 9

# Do not assign a port already used by a different service.
if ss -H -ltn 'sport = :8084' | grep -q . && ! systemctl is-active --quiet proxy-microservice.service; then
    echo 'Port 8084 is already occupied. Resolve the conflict before setup.' >&2
    exit 1
fi

if ! id proxy-microservice > /dev/null 2>&1; then
    useradd --system --user-group --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin proxy-microservice
fi
if ! id proxy-deploy > /dev/null 2>&1; then
    useradd --create-home --user-group --shell /bin/bash proxy-deploy
fi
install -d -o proxy-deploy -g proxy-deploy -m 700 /home/proxy-deploy/.ssh
authorized_keys=/home/proxy-deploy/.ssh/authorized_keys
touch "$authorized_keys"
key_line="restrict $public_key"
if ! grep -qxF "$key_line" "$authorized_keys"; then
    printf '%s\n' "$key_line" >> "$authorized_keys"
fi
chown proxy-deploy:proxy-deploy "$authorized_keys"
chmod 600 "$authorized_keys"

install -d -o proxy-deploy -g proxy-deploy -m 755 /opt/proxy-microservice /opt/proxy-microservice/releases
install -d -o proxy-deploy -g proxy-deploy -m 700 /opt/proxy-microservice/incoming
python3 "$source_dir/configure_database.py"

sudoers=$(mktemp)
trap 'rm -f "$sudoers"' EXIT
cat > "$sudoers" <<'SUDOERS'
proxy-deploy ALL=(root) NOPASSWD: /usr/bin/systemctl restart proxy-microservice.service, /usr/bin/systemctl stop proxy-microservice.service, /usr/bin/systemctl reset-failed proxy-microservice.service
SUDOERS
visudo -cf "$sudoers"
install -o root -g root -m 440 "$sudoers" /etc/sudoers.d/proxy-microservice
install -o root -g root -m 644 "$source_dir/proxy-microservice.service" /etc/systemd/system/proxy-microservice.service
systemctl daemon-reload
systemctl enable proxy-microservice.service
echo 'Application setup ready. No release started. Configure Nginx/TLS before enabling deployment.'
echo 'Production secrets: /etc/proxy-microservice.env (root only; back up with the database).'
