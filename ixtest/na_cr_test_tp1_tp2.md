# NA CR Test — 1×TP=2 + 1×TP=1 (clean, manual, from scratch)

Runnable by hand from the host. Creates a fresh TP=2 and a fresh TP=1 CR container through
the NA, then runs swapin / query / swapout. **Containers must be created via `create_func_pod`**
— this registers the NA CR agent (a mere leftover podman container is NOT tracked and cannot be
swapin'd).

## Containers (created fresh)

| role | funcname | funcspec (existing JSON) | TP | served | port | gpu_map |
|------|----------|--------------------------|----|--------|------|---------|
| TP=2 | `swap`   | `funcspec_cr_tp2a.json` | 2 | `tp2-a` | 9010 | `[0,1]` |
| TP=1 | `swap-a` | `funcspec_cr_tp1a.json` | 1 | `tp1-a` | 9011 | `[0]`   |

> TP=1 has no NCCL (single rank). NCCL / CUDA-graph rebuild only applies to TP=2.

## 0. Environment (one-time)

Run ixtest from the **host** (it reads the funcspec from the host filesystem and reaches NA
over the network).

```bash
NA_POD=$(sudo docker ps --format '{{.Names}}' | grep "k8s_nodeagent.*_default" | grep -v POD | tail -1)
NA="http://10.42.0.68:1233"
IXT="/opt/inferx/bin/ixtest"
FUNC_DIR="/home/brad/rust/inferx/ixtest"
echo "NA_POD=$NA_POD"
echo "NA=$NA"
```

If `10.42.0.68` not reachable, rediscover:

```bash
sudo docker exec "$NA_POD" cat /etc/hosts | grep nodeagent
NA="http://10.42.0.68:1233"     # set from IP above
```

## 1. Preflight

```bash
sudo docker exec "$NA_POD" pgrep -x na
timeout 3 bash -c 'cat < /dev/null > /dev/tcp/10.42.0.68/1233' && echo OPEN || echo CLOSED
sudo docker exec "$NA_POD" podman ps -a --format '{{.Names}} {{.Status}}'
$IXT "$NA" list_pods public default
```

Expected: podman empty or only old Exited containers; `list_pods` shows 0-1 pods.

## 2. Full cleanup (start from scratch)

Removes orphaned containers, CR dirs, tracked pods, and CNI leases.

```bash
export NA_POD
# Stop tracking known CR pods (ignore errors)
for c in swap swap-a; do $IXT "$NA" terminate_pod public default "$c" 1 1 2>/dev/null || true; done

# Remove any podman containers + CR dirs + CNI leases straight from the NA pod
sudo docker exec "$NA_POD" bash -c '
  podman rm -f swap swap-a 2>/dev/null;
  rm -rf /opt/inferx/podman/swap /opt/inferx/podman/swap-a;
  rm -f /var/lib/cni/networks/podman/10.88.0.* 2>/dev/null;
  echo CLEANED'
```

Verify:

```bash
sudo docker exec "$NA_POD" podman ps -a --format '{{.Names}} {{.Status}}'
sudo docker exec "$NA_POD" ls /opt/inferx/podman/
$IXT "$NA" list_pods public default
```

For a fully clean NA (optional, if agents are stale):

```bash
sudo docker restart "$NA_POD"
for i in $(seq 1 24); do
  sudo docker exec "$NA_POD" pgrep -x na >/dev/null 2>&1 && { echo "NA up after $((i*5))s"; break; }
  sleep 5
done
```

## 3. Create the two containers (auto-swapout after warmup)

```bash
# TP=2
$IXT "$NA" create_func_pod public default swap 1 1 crcontainer "$FUNC_DIR/funcspec_cr_tp2a.json"

# TP=1
$IXT "$NA" create_func_pod public default swap-a 1 1 crcontainer "$FUNC_DIR/funcspec_cr_tp1a.json"
```

Wait for each to auto-swapout to `Exited (0)` (vLLM boots ~2.5 min, then sleeps + checkpoints):

```bash
for c in swap swap-a; do
  echo "--- $c ---"
  for i in $(seq 1 60); do
    st=$(sudo docker exec "$NA_POD" podman ps -a --format '{{.Names}} {{.Status}}' 2>/dev/null | grep -E "^$c " || echo MISSING)
    echo "[$((i*5))s] $st"
    echo "$st" | grep -q Exited && break
    sleep 5
  done
done
```

Verify checkpoint + registration:

```bash
sudo docker exec "$NA_POD" ls /opt/inferx/podman/swap/          # checkpoint/, checkpoint.ready
sudo docker exec "$NA_POD" ls /opt/inferx/podman/swap-a/        # checkpoint/, checkpoint.ready
$IXT "$NA" list_pods public default                             # both CrSwappedOut
```

## 4. Baseline TP=2 swap in / query / swap out

```bash
$IXT "$NA" cr_swapin swap "[0,1]"

sudo docker exec "$NA_POD" \
  bash -c 'for i in $(seq 1 48); do curl -sf --max-time 5 http://localhost:9010/health && break; sleep 5; done'

sudo docker exec "$NA_POD" curl -sf --max-time 60 \
  "http://localhost:9010/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{"model":"tp2-a","messages":[{"role":"user","content":"What is one fruit that is red?"}],"max_tokens":20}'

nvidia-smi --query-gpu=index,memory.used --format=csv,noheader

$IXT "$NA" cr_swapout swap
```

## 5. Baseline TP=1 swap in / query / swap out

```bash
$IXT "$NA" cr_swapin swap-a "[0]"

sudo docker exec "$NA_POD" \
  bash -c 'for i in $(seq 1 48); do curl -sf --max-time 5 http://localhost:9011/health && break; sleep 5; done'

sudo docker exec "$NA_POD" curl -sf --max-time 60 \
  "http://localhost:9011/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{"model":"tp1-a","messages":[{"role":"user","content":"What is one fruit that is red?"}],"max_tokens":20}'

nvidia-smi --query-gpu=index,memory.used --format=csv,noheader

$IXT "$NA" cr_swapout swap-a
```

## 6. Multi-round (optional)

```bash
$IXT "$NA" cr_swap_test swap   9010 3 "[0,1]"
$IXT "$NA" cr_swap_test swap-a 9011 3 "[0]"
```

## 7. Final cleanup

```bash
$IXT "$NA" cr_swapout swap   2>/dev/null || true
$IXT "$NA" cr_swapout swap-a 2>/dev/null || true
```

## Known limitation (observed 2026-08-13, TP=2 on this node)

On a fresh single TP=2 `swap` run (no TP=1 involved), swapin succeeded and the container
served correctly for several requests, but **`VllmWorker-1` (Worker_TP1, rank 1, GPU 1)
died unexpectedly** during concurrent generation (`multiproc_executor` → EngineCore
`EngineDeadError`). The NA's health check then failed and `on_failure()` tore the container
down entirely. This is a post-restore TP=2 worker crash (NCCL / CUDA-graph path) — TP=1 is
unaffected (no NCCL). If rank-1 dies, expect the container to be auto-removed; that is
expected NA behavior on failure, not a CLI issue.

## Gotchas

- **Create is required** — swapin/swapout only works on containers the NA has an agent for
  (created this session via `create_func_pod`). Old leftover podman containers are not usable.
- **ixtest runs on host** — funcspec path is host-side; NA reachable at its pod IP:1233.
- **TP=1 and TP=2 are two separate containers** — they do not share GPUs (TP=2 uses
  GPU 0+1, TP=1 uses GPU 0). Swap in serially to avoid GPU contention.
- **NA IP may change** after pod restart — re-verify in section 0.
