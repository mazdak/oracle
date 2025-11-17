#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${ORACLE_INSTALL_DIR:-$HOME/.local/bin}"
PROJECT_ROOT="$(pwd)"

if [[ ! -f "$PROJECT_ROOT/Cargo.toml" ]]; then
  echo "error: this script must be run from the project root." >&2
  exit 1
fi

cargo build --release

mkdir -p "$INSTALL_DIR"
install_bin="$INSTALL_DIR/oracle"
cp -f "$PROJECT_ROOT/target/release/oracle" "$install_bin"
chmod +x "$install_bin"

echo "\nOracle binary written to: $install_bin"
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  cat <<MSG
To put Oracle on your PATH, add the following to your shell init (zsh/bash/fish):

  export PATH="$INSTALL_DIR:$PATH"

Then restart your shell or run:

  source ~/.zshrc  # or ~/.bashrc, ~/.config/fish/config.fish
MSG
else
  echo "Oracle is already on your PATH because $INSTALL_DIR is already listed." 
fi
