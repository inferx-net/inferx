#!/bin/bash
set -e # Exit on error

echo "--- Generating Config ---"
# Use -u for unbuffered output so it shows up in docker logs immediately
python3 -u /app/gen_config.py "$@"

echo "--- Generated supervisord.conf ---"
cat /app/supervisord.conf

echo "--- Starting Supervisor ---"
# Use the -n flag for nodaemon (already in your config, but good for safety)
exec /usr/bin/supervisord -n -c /app/supervisord.conf
