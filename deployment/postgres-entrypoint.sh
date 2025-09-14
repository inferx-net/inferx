set -e

# Clean default init dir
rm -f /docker-entrypoint-initdb.d/*

# Pick which SQL to use (default = db1.sql)
if [ -n "$INIT_SQL" ]; then
    cp "/$INIT_SQL" /docker-entrypoint-initdb.d/
fi

# Now run the original entrypoint
exec docker-entrypoint.sh "$@"