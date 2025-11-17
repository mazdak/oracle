#!/usr/bin/env bash
set -euo pipefail

OWNER=${GITHUB_OWNER:-mazdak}
REPO=${GITHUB_REPO:-oracle}
INSTALL_DIR=${ORACLE_INSTALL_DIR:-$HOME/.local/bin}
TARGET_TRIPLE=${ORACLE_TARGET_TRIPLE:-}
RELEASE_TAG=${ORACLE_RELEASE_TAG:-latest}
TARGET_ASSET=${ORACLE_ASSET_NAME:-}

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

if [[ "$RELEASE_TAG" == "latest" ]]; then
  RELEASE_TAG=$(curl -fsSL -o /dev/null -w "%{url_effective}" "https://github.com/$OWNER/$REPO/releases/latest")
  RELEASE_TAG=${RELEASE_TAG##*/}
  if [[ -z "$RELEASE_TAG" ]]; then
    echo "error: could not determine latest release tag" >&2
    exit 1
  fi
fi

if [[ -n "${ORACLE_ASSET_NAME:-}" ]]; then
  ASSET_NAME="$ORACLE_ASSET_NAME"
else
  ASSET_NAME="oracle-${RELEASE_TAG}-${TARGET_TRIPLE}.tar.gz"
fi
ASSET_URL="https://github.com/$OWNER/$REPO/releases/download/$RELEASE_TAG/$ASSET_NAME"

RELEASE_VERSION="$RELEASE_TAG"

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

BINARY_PATH=$(find "$EXTRACT_DIR" -type f -name oracle -perm -111 -print -quit)
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
