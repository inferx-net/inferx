#!/bin/bash
set -e

if [ "${ENABLE_COREDUMP}" = "true" ]; then
  echo "Enable coredump"
  # allow unlimited core file size
  ulimit -c unlimited
  # set core dump file location & naming
  echo "/opt/inferx/log/core.na.%e.%p" > /proc/sys/kernel/core_pattern
else
  echo "Disable coredump"
  # disallow core dumps
  ulimit -c 0
fi

./svc "$@"