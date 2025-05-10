#!/usr/bin/env bash
set -euxo pipefail

### 1. Stop & Uninstall K3s
if command -v k3s-uninstall.sh &> /dev/null; then
  sudo /usr/local/bin/k3s-uninstall.sh           # Stops k3s, removes services, data, etc. :contentReference[oaicite:0]{index=0}
fi
if command -v k3s-agent-uninstall.sh &> /dev/null; then
  sudo /usr/local/bin/k3s-agent-uninstall.sh     # Removes agent components on workers :contentReference[oaicite:1]{index=1}
fi

### 2. Kill any remaining processes
if command -v k3s-killall.sh &> /dev/null; then
  sudo /usr/local/bin/k3s-killall.sh             # Kills k3s-related processes, containerd, etc. :contentReference[oaicite:2]{index=2}
fi

### 3. Remove leftover dirs and configs
# sudo rm -rf /etc/rancher/k3s /var/lib/rancher/k3s /var/lib/kubelet 
            # /etc/containerd /var/lib/containerd           # Clean containerd and K3s state :contentReference[oaicite:3]{index=3}

sudo rm -rf /etc/rancher /var/lib/rancher /var/lib/kubelet /etc/systemd/system/k3s* /var/lib/containerd /etc/cni /opt/cni


### 4. Restart containerd to clear any stuck state
sudo systemctl restart containerd                     # Ensures containerd is fresh :contentReference[oaicite:4]{index=4}

echo "✔️  K3s and related components have been fully removed."
