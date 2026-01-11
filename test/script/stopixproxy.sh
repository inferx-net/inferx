#!/usr/bin/env bash

while true; do
  # get list of pods with label app=ixproxy
  PODS=($(sudo kubectl get pods -l app=ixproxy -o jsonpath='{.items[*].metadata.name}'))

  # loop through them one by one
  for POD in "${PODS[@]}"; do
      echo "Deleting pod: $POD"
      sudo kubectl delete pod "$POD"
      echo "Waiting 79s..."
      sleep 79
  done
done