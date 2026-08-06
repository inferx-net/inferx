#!/usr/bin/env bash
# =============================================================================
# NA-Managed CR Container Swap Test
#
# Containers:
#   - TP=2 container "swap"   (GPU 0+1, port 9010, model tp2-a)
#   - TP=2 container "swap2"  (GPU 0+1, port 9013, model tp2-b)
#   - TP=1 container "swap-a" (GPU 0,   port 9011, model tp1-a)
#   - TP=1 container "swap-b" (GPU 1,   port 9012, model tp1-b)
#
# Test phases:
#   Phase 1: TP=2 swap -> TP=2 swap2 -> TP=2 swap
#            (two TP=2 containers swap on same GPUs)
#   Phase 2: TP=2 swap -> TP=1 A -> TP=1 B -> TP=2 swap2
#            (TP=2 to sequential TP=1 to different TP=2)
#   Phase 3: TP=1 A -> TP=2 swap -> TP=1 B -> TP=2 swap2
#            (TP=1 to TP=2 to different TP=1 to different TP=2)
#   Phase 4: TP=2 swap (GPU 0,1) -> TP=2 swap (GPU 1,0)
#            (GPU migration — swap physical GPUs)
#   Phase 5: NA pod restart -> verify containers recover to SwappedOut
#            -> run Phase 1 again to verify swap works after restart
#
# NOTE: TP=1 containers cannot be swapped in simultaneously due to CRIU
# UNIX socket conflicts in host network mode. They are tested sequentially.
#
# Prerequisites:
#   - NA running at 10.42.0.37:1233
#   - ixtest binary at /opt/inferx/bin/ixtest
#   - Funcspec JSONs at /home/brad/rust/inferx/ixtest/
#   - 2x NVIDIA RTX A6000 GPUs
#   - vLLM image: localhost/vllm/vllm-openai:v0.20.0-cu129
#
# Usage: bash na_cr_swap_test.sh
# =============================================================================

NA="http://10.42.0.37:1233"
IXT="/opt/inferx/bin/ixtest"
FUNC_DIR="/home/brad/rust/inferx/ixtest"
NA_POD="k8s_nodeagent_nodeagent-cr-jm5lf_default_2755f342-1955-46ed-97cc-f65876f16969_1"

PASS=0
FAIL=0
ERRORS=()

# --- Helpers ---

log_pass() { echo "  [PASS] $1"; PASS=$((PASS+1)); }
log_fail() { echo "  [FAIL] $1"; FAIL=$((FAIL+1)); ERRORS+=("$1"); }

query() {
  local port=$1 model=$2
  sudo docker exec "$NA_POD" curl -sf --max-time 60 \
    "http://localhost:${port}/v1/chat/completions" \
    -H "Content-Type: application/json" \
    -d "{\"model\":\"${model}\",\"messages\":[{\"role\":\"user\",\"content\":\"What is one fruit that is red?\"}],\"max_tokens\":20}" \
    2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['choices'][0]['message']['content'])" 2>/dev/null
}

wait_health() {
  local port=$1 name=$2
  for i in $(seq 1 24); do
    if sudo docker exec "$NA_POD" curl -sf --max-time 5 "http://localhost:${port}/health" 2>/dev/null; then
      echo "  ${name} healthy after $((i*5))s"
      return 0
    fi
    sleep 5
  done
  echo "  ERROR: ${name} not healthy after 120s"
  return 1
}

gpu_status() {
  echo "  GPU memory:"
  nvidia-smi --query-gpu=index,memory.used --format=csv,noheader 2>/dev/null | sed 's/^/    /'
}

container_status() {
  echo "  Containers:"
  sudo docker exec "$NA_POD" podman ps -a --format '    {{.Names}} {{.Status}}' 2>/dev/null
}

# swapin + wait + query + validate + show GPU
do_swapin_query() {
  local name=$1 port=$2 model=$3 gpu_map=$4
  echo "--- Swapin ${name} (GPU ${gpu_map}) ---"
  $IXT "$NA" cr_swapin "$name" "$gpu_map" || { log_fail "swapin ${name}"; echo ""; return 1; }
  echo ""
  wait_health "$port" "$name" || { log_fail "health ${name}"; echo ""; return 1; }
  echo ""
  local result
  result=$(query "$port" "$model")
  echo "--- Query ${name} (port ${port}, model ${model}) ---"
  echo "  Result: ${result}"
  echo ""
  gpu_status
  echo ""
  container_status
  echo ""
  # Validate query result contains a fruit word
  if echo "$result" | grep -qiE "cherry|apple|strawberry|raspberry|fruit|red"; then
    log_pass "query ${name}"
  else
    log_fail "query ${name} (unexpected: ${result})"
  fi
}

do_swapout() {
  local name=$1
  echo "--- Swapout ${name} ---"
  $IXT "$NA" cr_swapout "$name" || { log_fail "swapout ${name}"; echo ""; return 1; }
  echo ""
  sleep 3
  container_status
  gpu_status
  echo ""
  # Verify GPU memory freed
  local gpu0 gpu1
  gpu0=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits 2>/dev/null | head -1 | tr -d ' ')
  gpu1=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits 2>/dev/null | sed -n '2p' | tr -d ' ')
  if [[ "$gpu0" -lt 1000 && "$gpu1" -lt 1000 ]]; then
    log_pass "GPU freed after swapout ${name}"
  else
    log_fail "GPU not freed after swapout ${name} (gpu0=${gpu0}MiB gpu1=${gpu1}MiB)"
  fi
}

create_and_wait() {
  local name=$1 funcspec=$2
  echo "--- Creating ${name} ---"
  $IXT "$NA" create_func_pod public default "$name" 1 1 crcontainer "$funcspec" || { log_fail "create ${name}"; return 1; }
  echo "  Waiting for ${name} to auto-swapout..."
  for i in $(seq 1 24); do
    sleep 10
    local running exited
    running=$(sudo docker exec "$NA_POD" podman ps --format '{{.Names}}' 2>/dev/null | grep -cx "$name" || true)
    if [[ "$running" == "0" ]]; then
      exited=$(sudo docker exec "$NA_POD" podman ps -a --filter status=exited --format '{{.Names}}' 2>/dev/null | grep -cx "$name" || true)
      if [[ "$exited" == "1" ]]; then
        echo "  ${name} swapped out after $((i*10))s"
        log_pass "create + auto-swapout ${name}"
        return 0
      fi
    fi
    echo "  [${i}0s] still running..."
  done
  log_fail "create ${name} (timeout waiting for auto-swapout)"
  return 1
}

# =============================================================================
# STEP 0: Check for existing containers (recovered after NA restart)
#         If all 4 exist in Exited state, skip creation.
#         Otherwise, clean up and create from scratch.
# =============================================================================
echo "============================================================"
echo "STEP 0: Check existing containers"
echo "============================================================"
container_status
echo ""

EXISTING=0
for c in swap swap2 swap-a swap-b; do
  exited=$(sudo docker exec "$NA_POD" podman ps -a --filter status=exited --format '{{.Names}}' 2>/dev/null | grep -cx "$c" || true)
  if [[ "$exited" == "1" ]]; then
    EXISTING=$((EXISTING + 1))
  fi
done

if [[ "$EXISTING" -eq 4 ]]; then
  echo "All 4 containers found in Exited state (recovered after NA restart)."
  echo "Skipping creation — testing swap directly."
  log_pass "container recovery (all 4 found)"
else
  echo "Only ${EXISTING}/4 containers found. Cleaning up and creating from scratch."
  log_fail "container recovery (only ${EXISTING}/4 found)"
  echo ""
  for c in swap swap2 swap-a swap-b; do
    $IXT "$NA" terminate_pod public default "$c" 1 1 2>/dev/null || true
  done
  sudo docker exec "$NA_POD" bash -c 'podman rm -f swap swap2 swap-a swap-b 2>/dev/null; rm -rf /opt/inferx/podman/swap /opt/inferx/podman/swap2 /opt/inferx/podman/swap-a /opt/inferx/podman/swap-b 2>/dev/null; echo cleaned'
  echo ""
  echo "============================================================"
  echo "STEP 1: Create containers (sequential, auto-swapout after warmup)"
  echo "============================================================"
  create_and_wait "swap"   "$FUNC_DIR/funcspec_cr_tp2a.json"
  create_and_wait "swap2"  "$FUNC_DIR/funcspec_cr_tp2b.json"
  create_and_wait "swap-a" "$FUNC_DIR/funcspec_cr_tp1a.json"
  create_and_wait "swap-b" "$FUNC_DIR/funcspec_cr_tp1b.json"
fi
container_status
echo ""

# =============================================================================
# PHASE 1: TP=2 swap -> TP=2 swap2 -> TP=2 swap
# =============================================================================
echo "============================================================"
echo "PHASE 1: TP=2 swap  ->  TP=2 swap2  ->  TP=2 swap"
echo "============================================================"
echo ""
echo "----- 1a: Swapin TP=2 swap -----"
do_swapin_query "swap" 9010 "tp2-a" "[0,1]"
echo "----- 1b: Swapout TP=2 swap -----"
do_swapout "swap"
echo "----- 1c: Swapin TP=2 swap2 -----"
do_swapin_query "swap2" 9013 "tp2-b" "[0,1]"
echo "----- 1d: Swapout TP=2 swap2 -----"
do_swapout "swap2"
echo "----- 1e: Swapin TP=2 swap again -----"
do_swapin_query "swap" 9010 "tp2-a" "[0,1]"
echo "----- 1f: Swapout TP=2 swap -----"
do_swapout "swap"
echo ""

# =============================================================================
# PHASE 2: TP=2 swap -> TP=1 A -> TP=1 B -> TP=2 swap2
# =============================================================================
echo "============================================================"
echo "PHASE 2: TP=2 swap  ->  TP=1 A  ->  TP=1 B  ->  TP=2 swap2"
echo "============================================================"
echo ""
echo "----- 2a: Swapin TP=2 swap -----"
do_swapin_query "swap" 9010 "tp2-a" "[0,1]"
echo "----- 2b: Swapout TP=2 swap -----"
do_swapout "swap"
echo "----- 2c: Swapin TP=1 A (GPU 0) -----"
do_swapin_query "swap-a" 9011 "tp1-a" "[0]"
echo "----- 2d: Swapout TP=1 A -----"
do_swapout "swap-a"
echo "----- 2e: Swapin TP=1 B (GPU 1) -----"
do_swapin_query "swap-b" 9012 "tp1-b" "[1]"
echo "----- 2f: Swapout TP=1 B -----"
do_swapout "swap-b"
echo "----- 2g: Swapin TP=2 swap2 -----"
do_swapin_query "swap2" 9013 "tp2-b" "[0,1]"
echo "----- 2h: Swapout TP=2 swap2 -----"
do_swapout "swap2"
echo ""

# =============================================================================
# PHASE 3: TP=1 A -> TP=2 swap -> TP=1 B -> TP=2 swap2
# =============================================================================
echo "============================================================"
echo "PHASE 3: TP=1 A  ->  TP=2 swap  ->  TP=1 B  ->  TP=2 swap2"
echo "============================================================"
echo ""
echo "----- 3a: Swapin TP=1 A (GPU 0) -----"
do_swapin_query "swap-a" 9011 "tp1-a" "[0]"
echo "----- 3b: Swapout TP=1 A -----"
do_swapout "swap-a"
echo "----- 3c: Swapin TP=2 swap -----"
do_swapin_query "swap" 9010 "tp2-a" "[0,1]"
echo "----- 3d: Swapout TP=2 swap -----"
do_swapout "swap"
echo "----- 3e: Swapin TP=1 B (GPU 1) -----"
do_swapin_query "swap-b" 9012 "tp1-b" "[1]"
echo "----- 3f: Swapout TP=1 B -----"
do_swapout "swap-b"
echo "----- 3g: Swapin TP=2 swap2 -----"
do_swapin_query "swap2" 9013 "tp2-b" "[0,1]"
echo "----- 3h: Swapout TP=2 swap2 -----"
do_swapout "swap2"
echo ""

# =============================================================================
# PHASE 4: GPU migration — swap physical GPUs 0<->1
# =============================================================================
echo "============================================================"
echo "PHASE 4: GPU migration (TP=2 swap GPU 0,1 -> GPU 1,0)"
echo "============================================================"
echo ""
echo "----- 4a: Swapin TP=2 swap (GPU 0,1) -----"
do_swapin_query "swap" 9010 "tp2-a" "[0,1]"
echo "----- 4b: Swapout TP=2 swap -----"
do_swapout "swap"
echo "----- 4c: Swapin TP=2 swap with GPU migration (GPU 1,0) -----"
do_swapin_query "swap" 9010 "tp2-a" "[1,0]"
echo "----- 4d: Swapout TP=2 swap -----"
do_swapout "swap"
echo ""

# =============================================================================
# PHASE 5: NA pod restart -> verify recovery -> run swap
# =============================================================================
echo "============================================================"
echo "PHASE 5: NA pod restart -> container recovery -> swap test"
echo "============================================================"
echo ""
echo "----- 5a: Swapout all containers -----"
for c in swap swap2 swap-a swap-b; do
  $IXT "$NA" cr_swapout "$c" 2>/dev/null || true
done
sleep 3
container_status
echo ""

echo "----- 5b: Restart NA pod -----"
echo "  Restarting ${NA_POD}..."
sudo docker restart "$NA_POD" 2>&1
echo "  Waiting for NA to be ready..."
for i in $(seq 1 24); do
  if sudo docker exec "$NA_POD" pgrep -x na >/dev/null 2>&1; then
    echo "  NA process running after $((i*5))s"
    sleep 5
    break
  fi
  sleep 5
done
echo ""

echo "----- 5c: Verify container recovery -----"
container_status
echo ""
RECOVERED=0
for c in swap swap2 swap-a swap-b; do
  exited=$(sudo docker exec "$NA_POD" podman ps -a --filter status=exited --format '{{.Names}}' 2>/dev/null | grep -cx "$c" || true)
  if [[ "$exited" == "1" ]]; then
    RECOVERED=$((RECOVERED + 1))
  fi
done
if [[ "$RECOVERED" -eq 4 ]]; then
  log_pass "NA restart recovery (all 4 containers recovered to SwappedOut)"
else
  log_fail "NA restart recovery (only ${RECOVERED}/4 recovered)"
fi
echo ""

echo "----- 5d: Swap test after restart (TP=2 swap -> TP=2 swap2) -----"
do_swapin_query "swap" 9010 "tp2-a" "[0,1]"
do_swapout "swap"
do_swapin_query "swap2" 9013 "tp2-b" "[0,1]"
do_swapout "swap2"
echo ""

# =============================================================================
# Final cleanup
# =============================================================================
echo "============================================================"
echo "FINAL CLEANUP"
echo "============================================================"
for c in swap swap2 swap-a swap-b; do
  $IXT "$NA" cr_swapout "$c" 2>/dev/null || true
done
sleep 3
container_status
gpu_status
echo ""

# =============================================================================
# Summary
# =============================================================================
echo "============================================================"
echo "TEST SUMMARY"
echo "============================================================"
echo "  PASSED: ${PASS}"
echo "  FAILED: ${FAIL}"
if [[ ${FAIL} -gt 0 ]]; then
  echo ""
  echo "  Failures:"
  for e in "${ERRORS[@]}"; do
    echo "    - ${e}"
  done
fi
echo ""
if [[ ${FAIL} -eq 0 ]]; then
  echo "  *** ALL TESTS PASSED ***"
else
  echo "  *** SOME TESTS FAILED ***"
fi
echo "============================================================"
