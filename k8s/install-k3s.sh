#!/bin/bash
set -e


### 2. Install K3s using Docker runtime
echo "[+] Installing K3s with Docker as container runtime..."
curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC="--docker" sh -

echo "[+] Waiting for K3s to be ready..."
sleep 10
kubectl get node

### 3. Install Helm (if not installed)
if ! command -v helm &> /dev/null; then
  echo "[+] Installing Helm..."
  curl https://raw.githubusercontent.com/helm/helm/master/scripts/get-helm-3 | bash
fi

### 4. Add NVIDIA Helm repo
echo "[+] Adding NVIDIA Helm repo..."
helm repo add nvidia https://nvidia.github.io/gpu-operator
helm repo update

### 5. Deploy NVIDIA GPU Operator with Docker runtime
echo "[+] Installing NVIDIA GPU Operator..."
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
chmod 555 /etc/rancher/k3s/k3s.yaml
helm install --wait gpu-operator \
  nvidia/gpu-operator \
  -n gpu-operator --create-namespace \
  --set operator.defaultRuntime=docker \
  --set driver.enabled=false \
  --set toolkit.enabled=true

echo "[✓] K3s with Docker runtime and NVIDIA GPU Operator installed successfully."

#kubectl apply  -f https://github.com/NVIDIA/k8s-device-plugin/releases/download/v0.17.3/nvidia-device-plugin.yml
#kubectl create -f https://raw.githubusercontent.com/NVIDIA/k8s-device-plugin/v0.17.1/deployments/static/nvidia-device-plugin.yml
