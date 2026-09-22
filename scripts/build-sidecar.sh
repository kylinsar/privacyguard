#!/usr/bin/env bash
# 构建 pg 启动器并放到 Tauri sidecar 目录（src-tauri/binaries/pg-<triple>）。
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="${1:-release}"
TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"

if [ "$PROFILE" = "release" ]; then
  cargo build -p pg-cli --release
  SRC="$TARGET_DIR/release/pg"
else
  cargo build -p pg-cli
  SRC="$TARGET_DIR/debug/pg"
fi

mkdir -p src-tauri/binaries
cp "$SRC" "src-tauri/binaries/pg-$TRIPLE"
chmod +x "src-tauri/binaries/pg-$TRIPLE"
echo "sidecar -> src-tauri/binaries/pg-$TRIPLE"
