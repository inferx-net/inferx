# On server node
# sudo cat /var/lib/rancher/k3s/server/node-token
# hostname -I  # Use internal IP accessible by the joining node


sudo /usr/local/bin/k3s-agent-uninstall.sh
sudo rm -rf /etc/rancher /var/lib/rancher /var/lib/kubelet /var/lib/cni /run/flannel



curl -sfL https://get.k3s.io | K3S_URL=https://192.168.0.22:6443 \
  K3S_TOKEN=K106218814e0f9ea4c0b067750e725aee4a2921804a6867b625abb51b5c11149e9a::server:5401cee22c6fd5315c24574784b8d8a1 \
  INSTALL_K3S_EXEC="--docker --with-node-id" sh -

sudo k3s agent --docker \
  --server https://192.168.0.22:6443 \
  --token K106218814e0f9ea4c0b067750e725aee4a2921804a6867b625abb51b5c11149e9a::server:5401cee22c6fd5315c24574784b8d8a1 \
  --with-node-id \
  --node-name inferx-agent1 \
  --debug


# sudo k3s agent --docker --server https://192.168.0.22:6443 --token K106218814e0f9ea4c0b067750e725aee4a2921804a6867b625abb51b5c11149e9a::server:5401cee22c6fd5315c24574784b8d8a1 --debug

