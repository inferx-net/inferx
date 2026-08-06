#!/usr/bin/env bash
# =============================================================================
# NA-Managed CR Container Swap Test
#
# Tests GPU scheduling with:
#   - TP=2 container "swap"   (GPU 0+1, port 9010, model tp2-a)
#   - TP=2 container "swap2"  (GPU 0+1, port 9013, model tp2-b)
#   - TP=1 container "swap-a" (GPU 0,   port 9011, model tp1-a)
#   - TP=1 container "swap-b" (GPU 1,   port 9012, model tp1-b)
#
# Constraint: TP=2 uses both GPUs; TP=1 containers use separate GPUs.
# No two containers should share a GPU at the same time.
#
# NOTE: TP=1 containers cannot be swapped in simultaneously due to CRIU
# UNIX socket conflicts in host network mode. They are tested sequentially.
#
# Swap combinations tested:
#   Phase 1: TP=2 swap -> TP=2 swap2 -> TP=2 swap
#   Phase 2: TP=2 swap -> TP=1 A -> TP=1 B -> TP=2 swap2
#   Phase 3: TP=1 A -> TP=2 swap -> TP=1 B -> TP=2 swap2
#
# Prerequisites:
#   - NA running at 10.42.0.37:1233
#   - ixtest binary at /opt/inferx/bin/ixtest
#   - Funcspec JSONs at /home/brad/rust/inferx/ixtest/
#   - 2x NVIDIA RTX A6000 GPUs
#   - vLLM image: localhost/vllm/vllm-openai:v0.20.0-cu129
#
# Usage: Copy-paste each block step by step, or run: bash na_cr_swap_test.sh
# =============================================================================

set -euo pipefail

NA="http://10.42.0.37:1233"
IXT="/opt/inferx/bin/ixtest"
FUNC_DIR="/home/brad/rust/inferx/ixtest"
NA_POD="k8s_nodeagent_nodeagent-cr-jm5lf_default_2755f342-1955-46ed-97cc-f65876f16969_1"

# Helper: run a query against a vLLM endpoint
query() {
  local port=$1 model=$2
  sudo docker exec "$NA_POD" curl -sf --max-time 60 \
    "http://localhost:${port}/v1/chat/completions" \
    -H "Content-Type: application/json" \
    -d "{\"model\":\"${model}\",\"messages\":[{\"role\":\"user\",\"content\":\"What is one fruit that is red?\"}],\"max_tokens\":20}" \
    2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['choices'][0]['message']['content'])" 2>/dev/null
}

# Helper: wait for health
wait_health() {
  local port=$1 name=$2
  for i in $(seq 1 12); do
    if sudo docker exec "$NA_POD" curl -sf --max-time 5 "http://localhost:${port}/health" 2>/dev/null; then
      echo "  ${name} healthy after $((i*5))s"
      return 0
    fi
    sleep 5
  done
  echo "  ERROR: ${name} not healthy after 60s"
  return 1
}

# Helper: check GPU memory
gpu_status() {
  echo "  GPU memory:"
  nvidia-smi --query-gpu=index,memory.used --format=csv,noheader 2>/dev/null | sed 's/^/    /'
}

# Helper: show container status
container_status() {
  echo "  Containers:"
  sudo docker exec "$NA_POD" podman ps -a --format '    {{.Names}} {{.Status}}' 2>/dev/null
}

# Helper: swapin + wait + query + show GPU
do_swapin_query() {
  local name=$1 port=$2 model=$3 gpu_map=$4
  echo "--- Swapin ${name} (GPU ${gpu_map}) ---"
  $IXT "$NA" cr_swapin "$name" "$gpu_map"
  echo ""
  wait_health "$port" "$name"
  echo ""
  echo "--- Query ${name} (port ${port}, model ${model}) ---"
  echo "  Result: $(query "$port" "$model")"
  echo ""
  gpu_status
  echo ""
  container_status
  echo ""
}

# Helper: swapout + show status
do_swapout() {
  local name=$1
  echo "--- Swapout ${name} ---"
  $IXT "$NA" cr_swapout "$name"
  echo ""
  sleep 3
  container_status
  gpu_status
  echo ""
}

# Helper: create one container and wait for auto-swapout
create_and_wait() {
  local name=$1 funcspec=$2
  echo "--- Creating ${name} ---"
  $IXT "$NA" create_func_pod public default "$name" 1 1 crcontainer "$funcspec"
  echo "  Waiting for ${name} to auto-swapout..."
  for i in $(seq 1 24); do
    sleep 10
    RUNNING=$(sudo docker exec "$NA_POD" podman ps --format '{{.Names}}' 2>/dev/null | grep -cx "$name" || true)
    if [[ "$RUNNING" == "0" ]]; then
      EXITED=$(sudo docker exec "$NA_POD" podman ps -a --filter status=exited --format '{{.Names}}' 2>/dev/null | grep -cx "$name" || true)
      if [[ "$EXITED" == "1" ]]; then
        echo "  ${name} swapped out after $((i*10))s"
        break
      fi
    fi
    echo "  [${i}0s] still running..."
  done
  echo ""
}

# =============================================================================
# STEP 0: Check for existing containers (recovered after NA restart)
#         If all 4 exist in Exited/SwappedOut state, skip creation.
#         Otherwise, clean up and create from scratch.
# =============================================================================
echo "============================================================"
echo "STEP 0: Check existing containers"
echo "============================================================"
container_status
echo ""

EXISTING=0
for c in swap swap2 swap-a swap-b; do
  EXITED=$(sudo docker exec "$NA_POD" podman ps -a --filter status=exited --format '{{.Names}}' 2>/dev/null | grep -cx "$c" || true)
  if [[ "$EXITED" == "1" ]]; then
    EXISTING=$((EXISTING + 1))
  fi
done

if [[ "$EXISTING" -eq 4 ]]; then
  echo "All 4 containers found in Exited state (recovered after NA restart)."
  echo "Skipping creation — testing swap directly."
  echo ""
else
  echo "Only ${EXISTING}/4 containers found. Cleaning up and creating from scratch."
  echo ""
  for c in swap swap2 swap-a swap-b; do
    $IXT "$NA" terminate_pod public default "$c" 1 1 2>/dev/null || true
  done
  sudo docker exec "$NA_POD" bash -c 'podman rm -f swap swap2 swap-a swap-b 2>/dev/null; rm -rf /opt/inferx/podman/swap /opt/inferx/podman/swap2 /opt/inferx/podman/swap-a /opt/inferx/podman/swap-b 2>/dev/null; echo cleaned'
  echo ""

  # =============================================================================
  # STEP 1: Create all four containers (sequentially)
  # =============================================================================
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
#   Tests: two different TP=2 containers swapping on same GPUs
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

# =============================================================================
# PHASE 2: TP=2 swap -> TP=1 A -> TP=1 B -> TP=2 swap2
#   Tests: TP=2 to sequential TP=1 containers, then to different TP=2
#   (TP=1 containers run one at a time due to CRIU host-network limitation)
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

# =============================================================================
# PHASE 3: TP=1 A -> TP=2 swap -> TP=1 B -> TP=2 swap2
#   Tests: TP=1 to TP=2 to different TP=1 to different TP=2
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

# =============================================================================
# STEP 7: Final cleanup — swapout all containers
# =============================================================================
echo "============================================================"
echo "STEP 7: Final cleanup"
echo "============================================================"
echo "--- Swapout swap2 ---"
$IXT "$NA" cr_swapout swap2 2>/dev/null || true
echo ""
sleep 3
container_status
gpu_status
echo ""

echo "============================================================"
echo "ALL TESTS COMPLETE"
echo "============================================================"
echo ""
echo "Expected results:"
echo "  - All queries returned correct fruit answers (cherry/apple)"
echo "  - TP=2 containers used both GPUs exclusively"
echo "  - Two TP=2 containers swapped sequentially on same GPUs"
echo "  - TP=1 containers ran one at a time (CRIU host-net limitation)"
echo "  - No GPU was shared by two containers at the same time"
echo "  - All swapin/swapout operations completed without errors"
