#!/usr/bin/env bash

while true; do
  # get list of pods with label app=scheduler
  PODS=($(kubectl get pods -l app=scheduler -o jsonpath='{.items[*].metadata.name}'))

  # loop through them one by one
  for POD in "${PODS[@]}"; do
      date
      echo "Start delete pod: $POD"
      kubectl delete pod "$POD"
      date
      echo "deleted"
      echo "Waiting 129..."
      sleep 129
  done
done