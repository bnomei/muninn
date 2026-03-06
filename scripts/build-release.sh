#!/usr/bin/env bash
set -euo pipefail

: "${TARGET:?TARGET is required}"
BIN_NAME="${BIN_NAME:-muninn}"
PACKAGE_NAME="${PACKAGE_NAME:-muninn-voice-to-text}"

cargo build --release -p "$PACKAGE_NAME" --bin "$BIN_NAME" --target "$TARGET"
