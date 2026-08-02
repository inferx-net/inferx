# CRContainerSwitch E2E Test

## Build & Deploy

### 1. Build napodman image

```bash
cd /home/brad/rust/runtime/qservice/deployment
make build
```

This prepares the build context (`target/na/` with criu-src, cuda-checkpoint, napodman.Dockerfile) then runs:

```bash
docker build -t napodman-test:latest -f target/na/napodman.Dockerfile target/na/
```

### 2. Build NA binary

```bash
cd /home/brad/rust/runtime/qservice
cargo build -p qservice --bin na
```

### 2. Copy to nodeagent pod

```bash
P=$(kubectl get pods -l app=nodeagent -o name | head -1 | cut -d/ -f2)
kubectl cp target/debug/na $P:/opt/inferx/bin/nodeagent
kubectl cp target/debug/na $P:/opt/inferx/bin/current/na
```

### 3. Restart NA

```bash
kubectl exec $P -- pkill -9 na 2>/dev/null
sleep 3
kubectl exec $P -- ss -tlnp | grep 1233
```

### 4. Load vLLM image (first time only)

If the pod is fresh and vLLM image is not cached:

```bash
kubectl exec $P -- podman load -i /opt/inferx/cache/vllm-v0.20.2-cu129.tar
kubectl exec $P -- podman tag localhost/v0.20.2-cu129:latest localhost/vllm/vllm-openai:v0.20.0-cu129
```

The image persists at `/opt/inferx/podman/storage` (hostPath) across pod restarts.

### 5. Fix cuda-checkpoint (required for CRIU)

The image may have a stale/empty `cuda-checkpoint` binary. Fix once after pod creation:

```bash
kubectl cp /home/brad/rust/runtime/qservice/deployment/cuda-checkpoint $P:/usr/bin/cuda-checkpoint
kubectl exec $P -- chmod 755 /usr/bin/cuda-checkpoint
kubectl exec $P -- /usr/bin/cuda-checkpoint --help | head -2
```

## Test

### Prerequisites

```bash
P=$(kubectl get pods -l app=nodeagent -o name | head -1 | cut -d/ -f2)
IP=$(kubectl get pod $P -o jsonpath='{.status.podIP}')
IXT="/opt/inferx/bin/ixtest http://${IP}:1233"
# Or via kubectl exec:
# alias IXT="kubectl exec $P -- /opt/inferx/bin/ixtest http://127.0.0.1:1233"
```

### Initial setup (once)

```bash
$IXT cr_container_switch Zero Running           # create vLLM container + wait for ready
$IXT cr_container_switch Running GpuContextLoad  # curl /sleep?level=1
$IXT cr_container_switch GpuContextLoad MemLoaded # nvproxy snapshot
$IXT cr_container_switch MemLoaded Checkpoint    # CRIU checkpoint --tcp-established
$IXT cr_container_switch Checkpoint Stopped      # podman stop + overlayfs setup
```

### Restore + query (repeat per round)

```bash
$IXT cr_container_switch Checkpoint Stopped     # stop + remount overlay  (fresh upper)
$IXT cr_container_switch Stopped CleanUpper     # rm -rf checkpoint-upper shm-upper
$IXT cr_container_switch CleanUpper MemLoaded   # CRIU restore --tcp-established
$IXT cr_container_switch MemLoaded GpuContextLoad # nvproxy restore
$IXT cr_container_switch GpuContextLoad Running   # curl /wake_up

# Wait for API to be ready (~30s for torch compile)
sleep 30

# Query
curl -sf --max-time 60 http://${IP}:8000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"TinyLlama/TinyLlama-1.1B-Chat-v1.0","messages":[{"role":"user","content":"What is one fruit that is red?"}],"max_tokens":20}'
```

Expected output: `"content":"One classic and most recognizable red fruit is the **Strawberry**..."`

## GPU Memory per Stage

| Stage | GPU 0 (MiB) | GPU 1 (MiB) |
|-------|------------|------------|
| Initial (vLLM running) | 38565 | 38250 |
| After sleep L1 | 1385 | 1068 |
| After nvproxy snapshot | 458 | 414 |
| After CRIU checkpoint | 458 | 414 |
| After stop + overlay | 458 | 414 |
| CRIU restore | 458 | 414 |
| nvproxy restore | 1337 | 1020 |
| wake up | 38513 | 38198 |
| query | 38565 | 38250 |

## State Machine

| From | To | Action |
|------|------|--------|
| Zero | Running | `podman run` vLLM + wait ready |
| Running | GpuContextLoad | `curl /sleep?level=1` |
| GpuContextLoad | MemLoaded | nvproxy snapshot via Unix SeqPacket |
| MemLoaded | Checkpoint | `podman container checkpoint --tcp-established` |
| Checkpoint | Stopped | `podman stop` + overlayfs mount |
| Stopped | CleanUpper | `rm -rf checkpoint-upper shm-upper` |
| CleanUpper | MemLoaded | `podman container restore --tcp-established` |
| MemLoaded | GpuContextLoad | nvproxy restore via Unix SeqPacket |
| GpuContextLoad | Running | `curl /wake_up` |

## Key Files

| File | Purpose |
|------|---------|
| `na/pod_mgr/cr_container.rs` | CrContainer: podman/nvproxy/overlayfs lifecycle |
| `na/pod_mgr/nodeagent_svc.rs` | CRContainerSwitch gRPC handler |
| `inferx/ixshare/proto/na.proto` | CRState enum |
| `inferx/ixtest/main.rs` | Test client binary |
| `deployment/cuda-checkpoint` | CUDA checkpoint helper (fix if empty) |
| `deployment/napodman.Dockerfile` | Container image (vfs storage) |
| `deployment/Makefile` | Build napodman image (`make build` from deployment/) |
| `nvproxy/docker-inferx-cr/Makefile` | Build/run/test for DinD image (`make build` from that dir) |
