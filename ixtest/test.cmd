######################################### Create L1 Container:

sudo docker run --name inferx-cr --rm --detach \
  --privileged --cgroupns=host \
  --device /dev/nvidiactl --device /dev/nvidia0 --device /dev/nvidia1 \
  --device /dev/nvidia-uvm --device /dev/nvidia-uvm-tools \
  -v /opt/inferx:/opt/inferx \
  -v /opt/inferx/shm:/dev/shm \
  -v /dev/pts:/dev/pts \
  --entrypoint bash \
  napodman-test:latest  -c "sleep infinity"

sudo docker exec -it inferx-cr bash

########################### Create vllm container:

CONTAINER_IP=10.88.0.200
PORT=9010
NVPROXY_SOCK=/opt/inferx/podman/swap/nvproxy.sock

podman run --name swap --detach \
  --network podman --ip "$CONTAINER_IP" \
  -p "${PORT}:${PORT}" \
  --device /dev/nvidiactl --device /dev/nvidia0 --device /dev/nvidia1 \
  --device /dev/nvidia-uvm --device /dev/nvidia-uvm-tools \
  --device /dev/nvidia-caps/nvidia-cap1 --device /dev/nvidia-caps/nvidia-cap2 \
  --log-driver k8s-file \
  -v /opt/inferx/cache:/root/.cache/huggingface \
  -v /opt/inferx/bin:/opt/inferx/bin \
  -v /opt/inferx/podman/swap:/opt/inferx/podman/swap \
  -v /opt/inferx/log:/opt/inferx/log \
  -v /opt/inferx/checkpoint:/opt/inferx/checkpoint \
  -v /opt/inferx/nvlibs:/opt/inferx/nvlibs:ro \
  -v /opt/inferx/nvlibs:/usr/local/nvidia/lib64:ro \
  -v /opt/inferx/podman/swap/shm:/dev/shm \
  -v /dev/pts:/dev/pts \
  --env "HF_HUB_OFFLINE=1" \
  --env "LD_LIBRARY_PATH=/usr/local/nvidia/lib64:/opt/inferx/bin/:$LD_LIBRARY_PATH" \
  --env "LD_PRELOAD=/opt/inferx/bin/libnvproxy.so" \
  --env "VLLM_SERVER_DEV_MODE=1" \
  --env "NVPROXY_SOCKET=${NVPROXY_SOCK}" \
  localhost/vllm/vllm-openai:v0.20.0-cu129 \
  Qwen/Qwen3.5-2B \
  --trust-remote-code --max-model-len 50000 --gpu-memory-utilization 0.80 \
  --tensor-parallel-size 2 --disable-custom-all-reduce --max-num-seqs 8 \
  --enable-sleep-mode \
  --port $PORT

@REM podman run --name swap --detach \
@REM   --network podman --ip "$CONTAINER_IP" \
@REM   -p "${PORT}:${PORT}" \
@REM   --device /dev/nvidiactl --device /dev/nvidia0 --device /dev/nvidia1 \
@REM   --device /dev/nvidia-uvm --device /dev/nvidia-uvm-tools \
@REM   --device /dev/nvidia-caps/nvidia-cap1 --device /dev/nvidia-caps/nvidia-cap2 \
@REM   --log-driver k8s-file \
@REM   -v /opt/inferx/cache:/root/.cache/huggingface \
@REM   -v /opt/inferx/bin:/opt/inferx/bin \
@REM   -v /opt/inferx/podman/swap:/opt/inferx/podman/swap \
@REM   -v /opt/inferx/log:/opt/inferx/log \
@REM   -v /opt/inferx/checkpoint:/opt/inferx/checkpoint \
@REM   -v /opt/inferx/nvlibs:/opt/inferx/nvlibs:ro \
@REM   -v /opt/inferx/nvlibs:/usr/local/nvidia/lib64:ro \
@REM   -v /opt/inferx/podman/swap/shm:/dev/shm \
@REM   -v /dev/pts:/dev/pts \
@REM   --env "HF_HUB_OFFLINE=1" \
@REM   --env "LD_LIBRARY_PATH=/usr/local/nvidia/lib64:/opt/inferx/bin/:$LD_LIBRARY_PATH" \
@REM   --env "LD_PRELOAD=/opt/inferx/bin/libnvproxy.so" \
@REM   --env "VLLM_SERVER_DEV_MODE=1" \
@REM   --env "NVPROXY_SOCKET=${NVPROXY_SOCK}" \
@REM   localhost/vllm/vllm-openai:v0.20.0-cu129 \
@REM   Qwen/Qwen3.8-27B-FP8 \
@REM   --trust-remote-code --max-model-len 50000 --gpu-memory-utilization 0.80 \
@REM   --tensor-parallel-size 1 --disable-custom-all-reduce --max-num-seqs 8 \
@REM   --enable-sleep-mode \
@REM   --port $PORT

############# Monitor ###############

podman logs -f swap

########### query ######
curl -sf --max-time 60 "http://localhost:9010/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"What is one fruit that is red?"}],"max_tokens":20}'


##########   Sleep L1 + nvproxy Snapshot

curl -sf --max-time 120 -X POST -H "Content-Type: application/json" -d "{}" "http://localhost:9010/sleep?level=1"


# Then snapshot (saves GPU state)
/opt/inferx/bin/nvproxycli --socket "$NVPROXY_SOCK" snapshot


###########  CRIU Checkpoint
rm -f /opt/inferx/podman/swap/shm/link_remap.* /opt/inferx/podman/swap/shm/sem.*
podman container checkpoint --tcp-established swap
podman stop swap



################## setup snapshot

bash -c '
CID=$(podman inspect swap --format "{{.Id}}")
CHECKPOINT_PATH=$(ls -d /opt/inferx/podman/storage/{overlay,vfs}-containers/$CID/userdata/checkpoint 2>/dev/null | head -1)
[ -z "$CHECKPOINT_PATH" ] && { echo "ERROR: checkpoint path not found for CID=$CID"; exit 1; }
echo "Checkpoint path: $CHECKPOINT_PATH"
SHM_PATH=/opt/inferx/podman/swap/shm
BASE_DIR=/opt/inferx/podman/swap
mkdir -p ${BASE_DIR}/checkpoint-{base,upper,work}
cp -a "$CHECKPOINT_PATH"/. ${BASE_DIR}/checkpoint-base/
mkdir -p ${BASE_DIR}/shm-{base,upper,work}
cp -a "$SHM_PATH"/. ${BASE_DIR}/shm-base/
mkdir -p "$CHECKPOINT_PATH"
umount "$CHECKPOINT_PATH" 2>/dev/null || true
mount -t overlay overlay -o "lowerdir=$BASE_DIR/checkpoint-base,upperdir=$BASE_DIR/checkpoint-upper,workdir=$BASE_DIR/checkpoint-work" "$CHECKPOINT_PATH"
mkdir -p "$SHM_PATH" 2>/dev/null || true
umount "$SHM_PATH" 2>/dev/null || true
mount -t overlay overlay -o "lowerdir=$BASE_DIR/shm-base,upperdir=$BASE_DIR/shm-upper,workdir=$BASE_DIR/shm-work" "$SHM_PATH"
echo "overlays mounted"
'


################## restore

SHM_PATH=/opt/inferx/podman/swap/shm
BASE_DIR=/opt/inferx/podman/swap
CID=$(podman inspect swap --format "{{.Id}}")
CHECKPOINT_PATH=$(ls -d /opt/inferx/podman/storage/{overlay,vfs}-containers/$CID/userdata/checkpoint 2>/dev/null | head -1)

cleanup() {
  # Unmount, clean upper, remount — ensures pristine base each round
  umount "$CHECKPOINT_PATH" 2>/dev/null
  umount "$SHM_PATH" 2>/dev/null
  rm -rf ${BASE_DIR}/checkpoint-upper/* ${BASE_DIR}/shm-upper/* 2>/dev/null
  mount -t overlay overlay -o lowerdir=${BASE_DIR}/checkpoint-base,upperdir=${BASE_DIR}/checkpoint-upper,workdir=${BASE_DIR}/checkpoint-work "$CHECKPOINT_PATH"
  mount -t overlay overlay -o lowerdir=${BASE_DIR}/shm-base,upperdir=${BASE_DIR}/shm-upper,workdir=${BASE_DIR}/shm-work "$SHM_PATH"
}

cleanup

podman container restore --tcp-established swap

#################### swap in/out

CONTAINER_IP=10.88.0.200
PORT=9010
NVPROXY_SOCK=/opt/inferx/podman/swap/nvproxy.sock


/opt/inferx/bin/nvproxycli --socket "$NVPROXY_SOCK" restore "[0,1]"  

curl -sf --max-time 30 -X POST -H "Content-Type: application/json" -d "{}" "http://localhost:9010/wake_up"

curl -sf --max-time 60 "http://localhost:9010/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"What is one fruit that is red?"}],"max_tokens":20}'

curl -sf --max-time 120 -X POST -H "Content-Type: application/json" -d "{}" "http://localhost:9010/sleep?level=1"

/opt/inferx/bin/nvproxycli --socket "$NVPROXY_SOCK" snapshot   


################## 

sudo docker kill inferx-cr


################## 


################## 


################## 


################## 


