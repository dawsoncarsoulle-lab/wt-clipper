#!/usr/bin/env bash
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: $0 <version>"
  echo "Example: $0 0.2.3"
  exit 1
fi

VERSION="$1"
TAG="v${VERSION}"

REPO="dawsoncarsoulle-lab/wt-clipper"
APP_NAME="WT.Clipper"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "==> Release ${TAG}"

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

require_command() {
  if ! command_exists "$1"; then
    echo "Missing required command: $1"
    exit 1
  fi
}

require_command git
require_command cargo
require_command npm
require_command gh
require_command curl
require_command jq

echo "==> Validating git state"

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Git working tree is not clean."
  echo
  git status --short
  echo
  echo "Commit or stash your changes before releasing."
  exit 1
fi

CURRENT_BRANCH="$(git branch --show-current)"
if [ "$CURRENT_BRANCH" != "main" ]; then
  echo "You are on branch '$CURRENT_BRANCH', not 'main'."
  echo "Switch to main before releasing."
  exit 1
fi

echo "==> Validating project version"

if [ -f "src-tauri/tauri.conf.json" ]; then
  TAURI_VERSION="$(jq -r '.version // empty' src-tauri/tauri.conf.json)"
  if [ "$TAURI_VERSION" != "$VERSION" ]; then
    echo "src-tauri/tauri.conf.json version is '$TAURI_VERSION', expected '$VERSION'."
    exit 1
  fi
fi

if [ -f "frontend/package.json" ]; then
  FRONTEND_VERSION="$(jq -r '.version // empty' frontend/package.json)"
  if [ "$FRONTEND_VERSION" != "$VERSION" ]; then
    echo "frontend/package.json version is '$FRONTEND_VERSION', expected '$VERSION'."
    exit 1
  fi
fi

if [ -f "src-tauri/Cargo.toml" ]; then
  if ! grep -q "^version = \"${VERSION}\"" src-tauri/Cargo.toml; then
    echo "src-tauri/Cargo.toml does not seem to be version '$VERSION'."
    exit 1
  fi
fi

echo "==> Running checks"

cargo fmt --check
cargo check
cargo test
npm --prefix frontend run build

echo "==> Building signed Tauri bundles"

cargo tauri build

BUNDLE_DIR="src-tauri/target/release/bundle"
DEB_ORIGINAL="$(find "$BUNDLE_DIR/deb" -maxdepth 1 -type f -name "*.deb" | head -n 1 || true)"
RPM_ORIGINAL="$(find "$BUNDLE_DIR/rpm" -maxdepth 1 -type f -name "*.rpm" | head -n 1 || true)"

if [ -z "$DEB_ORIGINAL" ]; then
  echo "No .deb bundle found in $BUNDLE_DIR/deb"
  exit 1
fi

if [ ! -f "${DEB_ORIGINAL}.sig" ]; then
  echo "No updater signature found for .deb:"
  echo "${DEB_ORIGINAL}.sig"
  exit 1
fi

echo "==> Normalizing asset names"

RELEASE_ASSET_DIR="target/release-assets/${TAG}"
rm -rf "$RELEASE_ASSET_DIR"
mkdir -p "$RELEASE_ASSET_DIR"

DEB_ASSET_NAME="${APP_NAME}_${VERSION}_amd64.deb"
DEB_SIG_ASSET_NAME="${DEB_ASSET_NAME}.sig"

DEB_ASSET_PATH="${RELEASE_ASSET_DIR}/${DEB_ASSET_NAME}"
DEB_SIG_ASSET_PATH="${RELEASE_ASSET_DIR}/${DEB_SIG_ASSET_NAME}"

cp "$DEB_ORIGINAL" "$DEB_ASSET_PATH"
cp "${DEB_ORIGINAL}.sig" "$DEB_SIG_ASSET_PATH"

UPLOAD_ASSETS=(
  "$DEB_ASSET_PATH"
  "$DEB_SIG_ASSET_PATH"
)

if [ -n "$RPM_ORIGINAL" ] && [ -f "${RPM_ORIGINAL}.sig" ]; then
  RPM_ASSET_NAME="${APP_NAME}-${VERSION}-1.x86_64.rpm"
  RPM_SIG_ASSET_NAME="${RPM_ASSET_NAME}.sig"

  RPM_ASSET_PATH="${RELEASE_ASSET_DIR}/${RPM_ASSET_NAME}"
  RPM_SIG_ASSET_PATH="${RELEASE_ASSET_DIR}/${RPM_SIG_ASSET_NAME}"

  cp "$RPM_ORIGINAL" "$RPM_ASSET_PATH"
  cp "${RPM_ORIGINAL}.sig" "$RPM_SIG_ASSET_PATH"

  UPLOAD_ASSETS+=(
    "$RPM_ASSET_PATH"
    "$RPM_SIG_ASSET_PATH"
  )
fi

echo "==> Generating latest.json"

SIG_CONTENT="$(cat "$DEB_SIG_ASSET_PATH")"

LATEST_JSON="${RELEASE_ASSET_DIR}/latest.json"

cat > "$LATEST_JSON" <<EOF
{
  "version": "${VERSION}",
  "notes": "Automatic capture target strategy, X11 and Wayland capture improvements, frontend internationalization, and README improvements.",
  "pub_date": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "platforms": {
    "linux-x86_64": {
      "signature": "${SIG_CONTENT}",
      "url": "https://github.com/${REPO}/releases/download/${TAG}/${DEB_ASSET_NAME}"
    }
  }
}
EOF

jq . "$LATEST_JSON" >/dev/null

UPLOAD_ASSETS+=("$LATEST_JSON")

echo "==> Assets prepared"

printf '%s\n' "${UPLOAD_ASSETS[@]}"

echo "==> Creating or updating git tag"

if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "Local tag $TAG already exists."
else
  git tag "$TAG"
fi

if git ls-remote --tags origin "$TAG" | grep -q "$TAG"; then
  echo "Remote tag $TAG already exists."
else
  git push origin "$TAG"
fi

echo "==> Creating or updating GitHub release"

if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  echo "Release $TAG already exists. Uploading assets with --clobber."
  gh release upload "$TAG" "${UPLOAD_ASSETS[@]}" --repo "$REPO" --clobber
else
  gh release create "$TAG" \
    --repo "$REPO" \
    --title "WT Clip ${TAG}" \
    --notes "Automatic capture target strategy, X11 and Wayland capture improvements, frontend internationalization, and README improvements." \
    "${UPLOAD_ASSETS[@]}"
fi

echo "==> Marking release as latest"

gh release edit "$TAG" --repo "$REPO" --latest

echo "==> Validating public updater endpoint"

LATEST_URL="https://github.com/${REPO}/releases/latest/download/latest.json"
DEB_URL="https://github.com/${REPO}/releases/download/${TAG}/${DEB_ASSET_NAME}"

echo "Checking latest.json:"
echo "$LATEST_URL"

curl -sS -L "$LATEST_URL" -o /tmp/wtclip_latest.json

REMOTE_VERSION="$(jq -r '.version' /tmp/wtclip_latest.json)"
REMOTE_URL="$(jq -r '.platforms["linux-x86_64"].url' /tmp/wtclip_latest.json)"
REMOTE_SIGNATURE="$(jq -r '.platforms["linux-x86_64"].signature' /tmp/wtclip_latest.json)"

if [ "$REMOTE_VERSION" != "$VERSION" ]; then
  echo "Remote latest.json version is '$REMOTE_VERSION', expected '$VERSION'."
  cat /tmp/wtclip_latest.json
  exit 1
fi

if [ "$REMOTE_URL" != "$DEB_URL" ]; then
  echo "Remote latest.json URL is wrong."
  echo "Expected: $DEB_URL"
  echo "Got:      $REMOTE_URL"
  cat /tmp/wtclip_latest.json
  exit 1
fi

if [ -z "$REMOTE_SIGNATURE" ] || [ "$REMOTE_SIGNATURE" = "null" ]; then
  echo "Remote latest.json signature is missing."
  cat /tmp/wtclip_latest.json
  exit 1
fi

echo "Checking .deb download:"
echo "$DEB_URL"

HTTP_CODE="$(curl -sS -L -o /dev/null -w "%{http_code}" "$DEB_URL")"

if [ "$HTTP_CODE" != "200" ]; then
  echo ".deb URL returned HTTP $HTTP_CODE"
  exit 1
fi

echo
echo "Release ${TAG} is ready."
echo
echo "Updater endpoint:"
echo "$LATEST_URL"
echo
echo "Package URL:"
echo "$DEB_URL"
