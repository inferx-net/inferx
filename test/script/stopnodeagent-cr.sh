#!/usr/bin/env bash

while true; do
  # get list of pods with label app=nodeagent-cr
  PODS=($(sudo kubectl get pods -l app=nodeagent-cr -o jsonpath='{.items[*].metadata.name}'))

  # loop through them one by one
  for POD in "${PODS[@]}"; do
      echo "Executing pkill in pod: $POD"
      sudo kubectl exec "$POD" -- pkill -x na
      echo "Waiting 79s..."
      sleep 79
  done
done
