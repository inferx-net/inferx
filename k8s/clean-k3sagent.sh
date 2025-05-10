# Stop K3s service if running
sudo systemctl stop k3s-agent || true

# Run the uninstall script if present
sudo /usr/local/bin/k3s-agent-uninstall.sh || true

# Clean residual data
sudo rm -rf /etc/rancher/k3s /var/lib/rancher/k3s /var/lib/kubelet /etc/systemd/system/k3s-agent.service /usr/local/bin/k3s*

# Optionally clean containerd data if used before
# sudo rm -rf /var/lib/containerd

echo "K3s agent cleanup complete."