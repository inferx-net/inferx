#!/usr/bin/env bash

while true; do
  PODS=($(kubectl get pods -l app=ixproxy -o jsonpath='{.items[*].metadata.name}'))

  # loop through them one by one
  for POD in "${PODS[@]}"; do
      date
      echo "Start delete pod: $POD"
      kubectl delete pod "$POD"
      date
      echo "deleted"
      echo "Waiting 1259s..."
      sleep 1259
  done
done