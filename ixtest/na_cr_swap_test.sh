#!/usr/bin/env bash
# =============================================================================
# NA-Managed CR Container Swap Test
#
# Tests GPU scheduling with:
#   - TP=2 container "swap"   (GPU 0+1, port 9010, model tp2-a)
#   - TP=1 container "swap-a" (GPU 0,   port 9011, model tp1-a)
#   - TP=1 container "swap-b" (GPU 1,   port 9012, model tp1-b)
#
# Constraint: TP=2 uses both GPUs; TP=1 containers use separate GPUs.
# No two containers should share a GPU at the same time.
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

# =============================================================================
# STEP 0: Cleanup — remove any existing test containers
# =============================================================================
echo "============================================================"
echo "STEP 0: Cleanup"
echo "============================================================"
$IXT "$NA" terminate_pod public default swap   1 1 2>/dev/null || true
$IXT "$NA" terminate_pod public default swap-a 1 1 2>/dev/null || true
$IXT "$NA" terminate_pod public default swap-b 1 1 2>/dev/null || true
sudo docker exec "$NA_POD" bash -c 'podman rm -f swap swap-a swap-b 2>/dev/null; rm -rf /opt/inferx/podman/swap /opt/inferx/podman/swap-a /opt/inferx/podman/swap-b 2>/dev/null; echo cleaned'
echo ""
container_status
echo ""

# =============================================================================
# STEP 1: Create all three containers (they auto-swapout after warmup)
# =============================================================================
echo "============================================================"
echo "STEP 1: Create containers (auto-swapout after warmup)"
echo "============================================================"

echo "--- Creating TP=2 container 'swap' (GPU 0+1, port 9010) ---"
$IXT "$NA" create_func_pod public default swap   1 1 crcontainer "$FUNC_DIR/funcspec_cr_tp2a.json"

echo "--- Creating TP=1 container 'swap-a' (GPU 0, port 9011) ---"
$IXT "$NA" create_func_pod public default swap-a 1 1 crcontainer "$FUNC_DIR/funcspec_cr_tp1a.json"

echo "--- Creating TP=1 container 'swap-b' (GPU 1, port 9012) ---"
$IXT "$NA" create_func_pod public default swap-b 1 1 crcontainer "$FUNC_DIR/funcspec_cr_tp1b.json"

echo ""
echo "Waiting for all containers to auto-swapout (up to 3 min)..."
for i in $(seq 1 18); do
  sleep 10
  RUNNING=$(sudo docker exec "$NA_POD" podman ps --format '{{.Names}}' 2>/dev/null | grep -cE 'swap' || true)
  EXITED=$(sudo docker exec "$NA_POD" podman ps -a --filter status=exited --format '{{.Names}}' 2>/dev/null | grep -cE 'swap' || true)
  echo "  [${i}0s] running=${RUNNING} exited=${EXITED}"
  if [[ "$RUNNING" == "0" && "$EXITED" -ge 3 ]]; then
    echo "  All containers swapped out!"
    break
  fi
done

echo ""
container_status
echo ""

# =============================================================================
# STEP 2: Swapin both TP=1 containers (GPU 0 + GPU 1 simultaneously)
# =============================================================================
echo "============================================================"
echo "STEP 2: Swapin both TP=1 containers"
echo "============================================================"

echo "--- Swapin swap-a (GPU 0) ---"
$IXT "$NA" cr_swapin swap-a "[0]"

echo "--- Swapin swap-b (GPU 1) ---"
$IXT "$NA" cr_swapin swap-b "[1]"

echo ""
wait_health 9011 "swap-a"
wait_health 9012 "swap-b"

echo ""
echo "--- Query TP=1 A (port 9011, model tp1-a) ---"
echo "  Result: $(query 9011 tp1-a)"

echo "--- Query TP=1 B (port 9012, model tp1-b) ---"
echo "  Result: $(query 9012 tp1-b)"

echo ""
gpu_status
echo ""
container_status
echo ""

# =============================================================================
# STEP 3: Swapout both TP=1 containers
# =============================================================================
echo "============================================================"
echo "STEP 3: Swapout both TP=1 containers"
echo "============================================================"

echo "--- Swapout swap-a ---"
$IXT "$NA" cr_swapout swap-a

echo "--- Swapout swap-b ---"
$IXT "$NA" cr_swapout swap-b

echo ""
sleep 3
container_status
gpu_status
echo ""

# =============================================================================
# STEP 4: Swapin TP=2 container (both GPUs)
# =============================================================================
echo "============================================================"
echo "STEP 4: Swapin TP=2 container"
echo "============================================================"

echo "--- Swapin swap (GPU 0+1) ---"
$IXT "$NA" cr_swapin swap "[0,1]"

echo ""
wait_health 9010 "swap"

echo ""
echo "--- Query TP=2 (port 9010, model tp2-a) ---"
echo "  Result: $(query 9010 tp2-a)"

echo ""
gpu_status
echo ""
container_status
echo ""

# =============================================================================
# STEP 5: Swapout TP=2
# =============================================================================
echo "============================================================"
echo "STEP 5: Swapout TP=2"
echo "============================================================"

echo "--- Swapout swap ---"
$IXT "$NA" cr_swapout swap

echo ""
sleep 3
container_status
gpu_status
echo ""

# =============================================================================
# STEP 6: Swapin both TP=1 again (verify round-trip works)
# =============================================================================
echo "============================================================"
echo "STEP 6: Swapin both TP=1 again"
echo "============================================================"

echo "--- Swapin swap-a (GPU 0) ---"
$IXT "$NA" cr_swapin swap-a "[0]"

echo "--- Swapin swap-b (GPU 1) ---"
$IXT "$NA" cr_swapin swap-b "[1]"

echo ""
wait_health 9011 "swap-a"
wait_health 9012 "swap-b"

echo ""
echo "--- Query TP=1 A (port 9011, model tp1-a) ---"
echo "  Result: $(query 9011 tp1-a)"

echo "--- Query TP=1 B (port 9012, model tp1-b) ---"
echo "  Result: $(query 9012 tp1-b)"

echo ""
gpu_status
echo ""
container_status
echo ""

# =============================================================================
# STEP 7: Final cleanup — swapout all containers
# =============================================================================
echo "============================================================"
echo "STEP 7: Final cleanup"
echo "============================================================"

echo "--- Swapout swap-a ---"
$IXT "$NA" cr_swapout swap-a
echo "--- Swapout swap-b ---"
$IXT "$NA" cr_swapout swap-b

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
echo "  - TP=1 containers ran on separate GPUs simultaneously"
echo "  - TP=2 container used both GPUs exclusively"
echo "  - No GPU was shared by two containers at the same time"
echo "  - All swapin/swapout operations completed without errors"
