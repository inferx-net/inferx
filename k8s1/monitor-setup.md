helm repo add prometheus-community https://prometheus-community.github.io/helm-charts
helm repo update

kubectl create namespace monitor



helm install prometheus prometheus-community/kube-prometheus-stack \
  --namespace monitor \
  -f k8s1/kube-prometheus-stack-monitor-values.yaml
