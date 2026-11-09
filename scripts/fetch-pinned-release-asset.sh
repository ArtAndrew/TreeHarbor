#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 6 ]]; then
    echo 'Usage: fetch-pinned-release-asset.sh OWNER/REPO TAG ASSET ASSET_ID SHA256 OUTPUT' >&2
    exit 2
fi

repository=$1
tag=$2
asset=$3
asset_id=$4
expected=$5
output=$6
if ! [[ "$asset_id" =~ ^[0-9]+$ && "$expected" =~ ^[0-9a-f]{64}$ ]]; then
    echo 'A GitHub asset ID and an exact SHA-256 digest are required.' >&2
    exit 2
fi

download="https://github.com/$repository/releases/download/$tag/$asset"
api="https://api.github.com/repos/$repository/releases/assets/$asset_id"
temporary=$(mktemp "${output}.partial.XXXXXX")
trap 'rm -f -- "$temporary"' EXIT
nonce="$(date +%s)-$$"
attempt=0

for url in "$download" "$api" "$download" "$api"; do
    attempt=$((attempt + 1))
    # Get a fresh signed redirect on each attempt, including after an HTTP 4xx.
    if curl --disable --fail --silent --show-error --location --max-redirs 5 \
        --proto '=https' --proto-redir '=https' --connect-timeout 15 --max-time 120 \
        --header 'Accept: application/octet-stream' --output "$temporary" \
        -- "${url}?treeharbor_ci=${nonce}-${attempt}"; then
        if digest=$(sha256sum < "$temporary") && [[ "${digest%% *}" == "$expected" ]]; then
            mv -f -- "$temporary" "$output"
            printf 'Verified %s with SHA-256 %s\n' "$asset" "$expected"
            exit 0
        fi
        printf 'SHA-256 mismatch for %s on attempt %s\n' "$asset" "$attempt" >&2
    fi
    if (( attempt < 4 )); then
        sleep "$attempt"
    fi
done

printf 'Could not download a verified copy of %s\n' "$asset" >&2
exit 1
