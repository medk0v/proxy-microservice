#!/usr/bin/env bash
# Runs as proxy-deploy. sudo permits restarting/stopping this service only.
set -Eeuo pipefail
umask 022

release=${1:-}
if [[ $# != 1 || ! $release =~ ^[a-f0-9]{40}-[0-9]+-[0-9]+$ ]]; then
    echo 'Usage: activate.sh <commit-sha>-<run-id>-<attempt>' >&2
    exit 2
fi

base=${DEPLOY_ROOT:-/opt/proxy-microservice}
service=proxy-microservice.service
incoming="$base/incoming/$release"
target="$base/releases/$release"
previous=''
activated=false

exec 9>"$base/.deploy.lock"
flock -w 300 9
test -f "$incoming/proxy-microservice"
test ! -L "$incoming/proxy-microservice"
read -r expected filename < "$incoming/SHA256SUMS"
[[ $expected =~ ^[a-f0-9]{64}$ && $filename == proxy-microservice ]]
printf '%s  %s\n' "$expected" "$incoming/proxy-microservice" | sha256sum --check --status
systemctl cat "$service" > /dev/null

if [[ -L $base/current ]]; then
    previous=$(readlink "$base/current")
    [[ $previous =~ ^releases/[a-f0-9]{40}-[0-9]+-[0-9]+$ ]]
    test -x "$base/$previous/proxy-microservice"
elif [[ -e $base/current ]]; then
    echo 'current must be a release symlink.' >&2
    exit 1
fi

switch_release() {
    rm -f "$base/.current-$release" &&
    ln -s "$1" "$base/.current-$release" &&
    mv -Tf "$base/.current-$release" "$base/current"
}

wait_healthy() {
    local attempt response
    for attempt in {1..30}; do
        if systemctl is-active --quiet "$service" &&
            response=$(curl --fail --silent --max-time 2 http://127.0.0.1:8084/health) &&
            [[ $response == ok ]]; then
            # Also catch a process that exits just after opening the listener.
            sleep 2
            if systemctl is-active --quiet "$service" &&
                response=$(curl --fail --silent --max-time 2 http://127.0.0.1:8084/health) &&
                [[ $response == ok ]]; then
                return 0
            fi
        fi
        sleep 2
    done
    return 1
}

finish() {
    local status=$?
    trap - EXIT HUP INT TERM
    set +e
    if [[ $status != 0 && $activated == true ]]; then
        if [[ -n $previous ]]; then
            if switch_release "$previous" &&
                sudo -n /usr/bin/systemctl reset-failed "$service" &&
                sudo -n /usr/bin/systemctl restart "$service" && wait_healthy; then
                echo "Activation failed. Previous release restored: $previous" >&2
            else
                echo 'Activation AND rollback failed. Inspect the service on the server.' >&2
            fi
        else
            sudo -n /usr/bin/systemctl stop "$service"
            rm -f "$base/current"
            echo 'First release failed; service stopped.' >&2
        fi
    fi
    rm -f "$base/.current-$release"
    rm -rf "$incoming"
    exit "$status"
}
trap finish EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir "$target"
install -m 755 "$incoming/proxy-microservice" "$target/proxy-microservice"
activated=true
switch_release "releases/$release"
sudo -n /usr/bin/systemctl reset-failed "$service"
sudo -n /usr/bin/systemctl restart "$service"
if ! wait_healthy; then
    echo 'New release did not become healthy.' >&2
    exit 1
fi
echo "Active release: $release"
