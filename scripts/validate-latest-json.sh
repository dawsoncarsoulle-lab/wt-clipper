#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
endpoint="${UPDATER_ENDPOINT:-$(jq -r '.plugins.updater.endpoints[0]' "$repo_root/src-tauri/tauri.conf.json")}"

if [[ -z "$endpoint" || "$endpoint" == "null" ]]; then
  echo "No updater endpoint configured" >&2
  exit 1
fi

tmp_json="$(mktemp)"
trap 'rm -f "$tmp_json"' EXIT

curl -fsSL "$endpoint" -o "$tmp_json"
jq -e '.version | type == "string" and length > 0' "$tmp_json" >/dev/null
jq -e '.platforms["linux-x86_64"].url | type == "string" and length > 0' "$tmp_json" >/dev/null
jq -e '.platforms["linux-x86_64"].signature | type == "string" and length > 0' "$tmp_json" >/dev/null

version="$(jq -r '.version' "$tmp_json")"
artifact_url="$(jq -r '.platforms["linux-x86_64"].url' "$tmp_json")"

curl -fsI -L "$artifact_url" >/dev/null
echo "Validated updater JSON for version $version"
