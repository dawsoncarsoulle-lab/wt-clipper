#!/usr/bin/env bash
set -euo pipefail

# Publish a WT Clip GitHub release that is compatible with the Tauri updater.
# Usage:
#   ./scripts/release-github.sh 0.2.3
#
# Requirements:
#   - gh authenticated: gh auth login
#   - jq installed
#   - Tauri updater private key available either as:
#       TAURI_SIGNING_PRIVATE_KEY
#     or in:
#       ~/.local/share/wt-clipper/updater.key
#
# Important:
#   The installed app checks:
#   https://github.com/dawsoncarsoulle-lab/wt-clipper/releases/latest/download/latest.json
#   so this release must be published as the latest GitHub release.

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
  echo "Usage: $0 <version>" >&2
  echo "Example: $0 0.2.3" >&2
  exit 1
fi

TAG="v${VERSION#v}"
VERSION="${VERSION#v}"
REPO="${GITHUB_REPOSITORY:-dawsoncarsoulle-lab/wt-clipper}"
RELEASE_TITLE="WT Clip ${TAG}"
NOTES_FILE="${NOTES_FILE:-/tmp/wt-clip-release-notes-${VERSION}.md}"
KEY_PATH="${WT_CLIPPER_UPDATER_KEY_PATH:-$HOME/.local/share/wt-clipper/updater.key}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE_DIR="$ROOT/src-tauri/target/release/bundle"

cd "$ROOT"

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

need_cmd git
need_cmd gh
need_cmd jq
need_cmd npm
need_cmd cargo

if [[ -n "$(git status --porcelain)" ]]; then
  echo "Working tree is not clean. Commit or stash your changes before releasing." >&2
  git status --short >&2
  exit 1
fi

TAURI_VERSION="$(jq -r '.version' src-tauri/tauri.conf.json)"
if [[ "$TAURI_VERSION" != "$VERSION" ]]; then
  echo "Version mismatch:" >&2
  echo "  requested: $VERSION" >&2
  echo "  src-tauri/tauri.conf.json: $TAURI_VERSION" >&2
  echo "Fix the project version before releasing." >&2
  exit 1
fi

if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  if [[ ! -f "$KEY_PATH" ]]; then
    echo "Missing Tauri updater signing key." >&2
    echo "Expected: $KEY_PATH" >&2
    echo "Or export TAURI_SIGNING_PRIVATE_KEY manually." >&2
    exit 1
  fi
  export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_PATH")"
fi

# Empty password is valid when the key was generated without a password.
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

echo "==> Validating project"
cargo fmt --check
cargo check
cargo test
npm --prefix frontend run build

echo "==> Building signed Tauri bundles for ${TAG}"
cargo tauri build

if [[ ! -d "$BUNDLE_DIR" ]]; then
  echo "Bundle directory not found: $BUNDLE_DIR" >&2
  exit 1
fi

mapfile -t ASSETS < <(
  find "$BUNDLE_DIR" -type f \
    \( -name "*.deb" -o -name "*.rpm" -o -name "*.sig" -o -name "latest.json" \) \
    | sort
)

if [[ "${#ASSETS[@]}" -eq 0 ]]; then
  echo "No release assets found under $BUNDLE_DIR" >&2
  exit 1
fi

LATEST_JSON=""
for asset in "${ASSETS[@]}"; do
  if [[ "$(basename "$asset")" == "latest.json" ]]; then
    LATEST_JSON="$asset"
  fi
done

if [[ -z "$LATEST_JSON" ]]; then
  echo "latest.json was not generated. The updater will not work without it." >&2
  exit 1
fi

jq -e '.version | type == "string" and length > 0' "$LATEST_JSON" >/dev/null
jq -e '.platforms["linux-x86_64"].url | type == "string" and length > 0' "$LATEST_JSON" >/dev/null
jq -e '.platforms["linux-x86_64"].signature | type == "string" and length > 0' "$LATEST_JSON" >/dev/null

JSON_VERSION="$(jq -r '.version' "$LATEST_JSON")"
if [[ "$JSON_VERSION" != "$VERSION" ]]; then
  echo "latest.json version mismatch:" >&2
  echo "  requested: $VERSION" >&2
  echo "  latest.json: $JSON_VERSION" >&2
  exit 1
fi

echo "==> Assets to upload"
printf ' - %s\n' "${ASSETS[@]}"

if [[ ! -f "$NOTES_FILE" ]]; then
  cat > "$NOTES_FILE" <<NOTES
## WT Clip ${TAG}

### Highlights

- Automatic capture target strategy improvements.
- Frontend internationalization.
- Better X11 and Wayland capture flow.
- Improved waiting state before War Thunder is launched.

### Update test

This release is intended to be detected by installed WT Clip builds through the Tauri updater endpoint.
NOTES
fi

if ! git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "==> Creating git tag ${TAG}"
  git tag "$TAG"
fi

if ! git ls-remote --tags origin "$TAG" | grep -q "$TAG"; then
  echo "==> Pushing tag ${TAG}"
  git push origin "$TAG"
fi

echo "==> Pushing current branch"
git push

if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  echo "==> Release ${TAG} already exists, uploading assets with --clobber"
  gh release upload "$TAG" "${ASSETS[@]}" --repo "$REPO" --clobber
else
  echo "==> Creating GitHub release ${TAG}"
  gh release create "$TAG" \
    "${ASSETS[@]}" \
    --repo "$REPO" \
    --title "$RELEASE_TITLE" \
    --notes-file "$NOTES_FILE" \
    --latest
fi

echo "==> Validating public updater endpoint"
if [[ -x "$ROOT/scripts/validate-latest-json.sh" ]]; then
  "$ROOT/scripts/validate-latest-json.sh"
else
  ENDPOINT="$(jq -r '.plugins.updater.endpoints[0]' src-tauri/tauri.conf.json)"
  curl -fsSL "$ENDPOINT" | jq . >/dev/null
fi

echo "Release ${TAG} published successfully."
echo "Now launch the installed WT Clip ${VERSION} updater test from the old installed app, not from cargo run."
