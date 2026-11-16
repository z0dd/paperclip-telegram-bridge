#!/usr/bin/env bash
set -euxo pipefail

# CI-only: installs rootless Docker from pinned static tarballs (ADR-0009 §4).
# The systemd user unit created by dockerd-rootless-setuptool.sh is the only
# daemon; never start dockerd-rootless.sh by hand (it conflicts with the unit).

DOCKER_VERSION="29.10.0"
DOCKER_SHA256="7c1ebadc65c716cdc6d35fc78759ed0e4317debf43c21dd12f066f2006999def"
EXTRAS_SHA256="a4ad65be3921ac3e45f3fe69893f5edbd078cb3b4a6499fe13288053553dce25"
BASE_URL="https://download.docker.com/linux/static/stable/x86_64"

# Ubuntu runners block unprivileged user namespaces for rootlesskit by default.
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
sudo apt-get update -q >/dev/null
sudo apt-get install -yq uidmap slirp4netns >/dev/null
grep "^$(id -un):" /etc/subuid

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/docker.tgz" "$BASE_URL/docker-$DOCKER_VERSION.tgz"
curl -fsSL -o "$tmp/extras.tgz" "$BASE_URL/docker-rootless-extras-$DOCKER_VERSION.tgz"
echo "$DOCKER_SHA256  $tmp/docker.tgz" | sha256sum -c -
echo "$EXTRAS_SHA256  $tmp/extras.tgz" | sha256sum -c -

mkdir -p "$HOME/bin"
tar -xzf "$tmp/docker.tgz" -C "$tmp"
tar -xzf "$tmp/extras.tgz" -C "$tmp"
cp -f "$tmp"/docker/* "$tmp"/docker-rootless-extras/* "$HOME/bin/"
export PATH="$HOME/bin:$PATH"

export XDG_RUNTIME_DIR="/run/user/$(id -u)"
# --force: the runner has a rootful daemon; the rootless one runs next to it.
dockerd-rootless-setuptool.sh install --force --skip-iptables

export DOCKER_HOST="unix://$XDG_RUNTIME_DIR/docker.sock"
for _ in $(seq 60); do docker info >/dev/null 2>&1 && break; sleep 1; done
docker info --format 'security={{.SecurityOptions}} server={{.ServerVersion}}'
docker compose version

if [ -n "${GITHUB_ENV:-}" ]; then
    echo "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR" >> "$GITHUB_ENV"
    echo "DOCKER_HOST=$DOCKER_HOST" >> "$GITHUB_ENV"
    echo "$HOME/bin" >> "$GITHUB_PATH"
fi
