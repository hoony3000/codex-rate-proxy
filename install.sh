#!/bin/sh
set -eu

BIN_DIR="$HOME/.local/bin"
CONFIG_DIR="$HOME/.config/codex-rate-proxy"
CONFIG_FILE="$CONFIG_DIR/config.ini"
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SOURCE_CONFIG="$SCRIPT_DIR/config.ini"

if [ ! -f "$SOURCE_CONFIG" ]; then
    SOURCE_CONFIG="$SCRIPT_DIR/llm_rate_proxy.ini.example"
fi

mkdir -p "$BIN_DIR" "$CONFIG_DIR"
install -m 700 "$SCRIPT_DIR/codex-rate-proxy" "$BIN_DIR/codex-rate-proxy"

if [ ! -e "$CONFIG_FILE" ]; then
    install -m 600 "$SOURCE_CONFIG" "$CONFIG_FILE"
    echo "Created $CONFIG_FILE"
else
    echo "Kept existing $CONFIG_FILE"
fi

echo "Installed $BIN_DIR/codex-rate-proxy"
echo "Add $BIN_DIR to PATH if it is not already available."
