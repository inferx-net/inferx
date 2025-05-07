#!/bin/bash

PARENT_DIR="/opt/inferx/sandbox/"
INFERX_BIN="/opt/inferx/bin/inferx"

# pkill -9 inferx

for SUBDIR in "$PARENT_DIR"/*; do
  if [ -d "$SUBDIR" ]; then
    SUBFOLDER_NAME=$(basename "$SUBDIR")
    echo "Running inferx on: $SUBFOLDER_NAME"
    "$INFERX_BIN" \
      --root "/var/run/docker/runtime-runc/moby" \
      --log-format json \
      --systemd-cgroup delete "$SUBFOLDER_NAME"
    
  fi
done