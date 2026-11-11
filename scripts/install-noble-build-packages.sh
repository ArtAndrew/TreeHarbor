#!/bin/sh
# Retain Ubuntu's normal archive verification if the runner mirror is unavailable.
set -eu
if [ "$#" -eq 0 ]; then
    echo 'usage: install-noble-build-packages.sh PACKAGE...' >&2
    exit 2
fi

apt_get() {
    sudo apt-get -o Acquire::Retries=1 -o Acquire::http::Timeout=20 \
        -o Acquire::https::Timeout=20 "$@"
}

if apt_get update -qq --error-on=any && apt_get install -y "$@"; then
    exit 0
fi

if [ "$(lsb_release -cs)" != noble ]; then
    echo 'Ubuntu Noble is required for the fallback package source' >&2
    exit 2
fi
th_arch=$(dpkg --print-architecture)
case "$th_arch" in
    amd64) th_mirror=https://mirror.nju.edu.cn/ubuntu ;;
    arm64) th_mirror=https://mirror.nju.edu.cn/ubuntu-ports ;;
    *) echo "unsupported Ubuntu build architecture: $th_arch" >&2; exit 2 ;;
esac

th_source_dir=$(mktemp -d)
th_sources="$th_source_dir/ubuntu.sources"
trap 'rm -f -- "$th_sources"; rmdir -- "$th_source_dir"' EXIT
cat > "$th_sources" <<EOF
Types: deb
URIs: $th_mirror
Suites: noble noble-updates noble-security
Components: main universe
Architectures: $th_arch
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
EOF
echo "Retrying Ubuntu build dependencies with $th_mirror" >&2

fallback_apt() {
    apt_get -o "Dir::Etc::sourcelist=$th_sources" -o Dir::Etc::sourceparts=- \
        -o APT::Get::List-Cleanup=false "$@"
}

fallback_apt update -qq --error-on=any
fallback_apt install -y "$@"
