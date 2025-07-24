#!/bin/bash
set -e

# Start Docker daemon in background
dockerd-entrypoint.sh &

# Wait until Docker daemon is ready
until docker info >/dev/null 2>&1; do
  echo "Waiting for Docker to start..."
  sleep 1
done

echo "Docker is running!"

# Run your onenode service
exec ./onenode
