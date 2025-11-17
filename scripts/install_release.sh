#!/usr/bin/env bash
set -euo pipefail

OWNER=${GITHUB_OWNER:-mazdak}
REPO=${GITHUB_REPO:-oracle}
INSTALL_DIR=${ORACLE_INSTALL_DIR:-$HOME/.local/bin}
TARGET_TRIPLE=${ORACLE_TARGET_TRIPLE:-}
ASSET_FILTER=${ORACLE_ASSET_FILTER:-}
RELEASE_TAG=${ORACLE_RELEASE_TAG:-latest}

detect_target() {
  if [[ -n "$TARGET_TRIPLE" ]]; then
    return
  fi

  local uname_s uname_m platform arch
  uname_s="$(uname -s)"
  uname_m="$(uname -m)"

  case "$uname_s" in
    Darwin)
      platform="apple-darwin"
      ;;
    Linux)
      platform="unknown-linux-gnu"
      ;;
    *)
      echo "error: unsupported OS '$uname_s'" >&2
      exit 1
      ;;
  esac

  case "$uname_m" in
    x86_64)
      arch="x86_64"
      ;;
    arm64|aarch64)
      arch="aarch64"
      ;;
    *)
      echo "error: unsupported architecture '$uname_m'" >&2
      exit 1
      ;;
  esac

  TARGET_TRIPLE="${arch}-${platform}"
}

detect_target

if [[ -z "$ASSET_FILTER" ]]; then
  ASSET_FILTER="$TARGET_TRIPLE"
fi

RELEASE_API_URL="https://api.github.com/repos/$OWNER/$REPO/releases/$RELEASE_TAG"

asset_info="$(curl -fsSL "$RELEASE_API_URL" | python3 - "$ASSET_FILTER" <<'PY'
import json
import sys

filter_str = sys.argv[1].lower()
raw = sys.stdin.read()
if not raw:
    print("error: could not fetch release data from GitHub", file=sys.stderr)
    sys.exit(1)

try:
    data = json.loads(raw)
except json.JSONDecodeError as exc:
    print(f"error: release API response is not valid JSON ({exc})", file=sys.stderr)
    sys.exit(1)
tag_name = data.get("tag_name", "")
for asset in data.get("assets", []):
    name = asset.get("name", "")
    name_lower = name.lower()
    if filter_str in name_lower and "oracle" in name_lower:
        print(asset["browser_download_url"])
        print(asset["name"])
        print(tag_name)
        sys.exit(0)
sys.exit(1)
PY
)"

if [[ -z "$asset_info" ]]; then
  echo "error: no release asset matching '$ASSET_FILTER' found for $TARGET_TRIPLE" >&2
  exit 1
fi

{
  read -r ASSET_URL
  read -r ASSET_NAME
  read -r RELEASE_VERSION
} <<< "$asset_info"

if [[ -z "$ASSET_URL" || -z "$ASSET_NAME" ]]; then
  echo "error: no release asset matching '$ASSET_FILTER' found for $TARGET_TRIPLE" >&2
  exit 1
fi

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT
DOWNLOAD_PATH="$TMPDIR/$ASSET_NAME"

curl -fsSL "$ASSET_URL" -o "$DOWNLOAD_PATH"

EXTRACT_DIR="$TMPDIR/extracted"
mkdir -p "$EXTRACT_DIR"
case "$ASSET_NAME" in
  *.tar.gz|*.tgz)
    tar -xzf "$DOWNLOAD_PATH" -C "$EXTRACT_DIR"
    ;;
  *.zip)
    unzip -q "$DOWNLOAD_PATH" -d "$EXTRACT_DIR"
    ;;
  *)
    mv "$DOWNLOAD_PATH" "$EXTRACT_DIR/"
    ;;
esac

BINARY_PATH=$(find "$EXTRACT_DIR" -type f -name oracle -perm /111 -print -quit)
if [[ -z "$BINARY_PATH" ]]; then
  echo "error: could not locate the oracle binary in downloaded asset" >&2
  exit 1
fi

mkdir -p "$INSTALL_DIR"
install -m 755 "$BINARY_PATH" "$INSTALL_DIR/oracle"

if [[ -n "$RELEASE_VERSION" ]]; then
  echo "Installed oracle $RELEASE_VERSION to $INSTALL_DIR/oracle"
else
  echo "Installed oracle to $INSTALL_DIR/oracle"
fi

if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  cat <<MSG
To run oracle from anywhere, add the install directory to your PATH (e.g. ~/.zshrc or ~/.bashrc):

  export PATH="$INSTALL_DIR:$PATH"
MSG
else
  echo "$INSTALL_DIR is already on your PATH."
fi
