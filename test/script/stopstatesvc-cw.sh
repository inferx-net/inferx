#!/usr/bin/env bash

while true; do
  PODS=($(kubectl get pods -l app=statesvc -o jsonpath='{.items[*].metadata.name}'))

  # loop through them one by one
  for POD in "${PODS[@]}"; do
      date
      echo "Start delete pod: $POD"
      kubectl delete pod "$POD"
      date
      echo "deleted"
      echo "Waiting 129s..."
      sleep 129
  done
done