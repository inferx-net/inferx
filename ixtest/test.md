env:

NA_POD=$(sudo docker ps --format '{{.Names}}' | grep "k8s_nodeagent.*_default" | grep -v POD | tail -1)
NA="http://10.42.0.68:1233"
IXT="/opt/inferx/bin/ixtest"
FUNC_DIR="/home/brad/rust/inferx/ixtest"
echo "NA_POD=$NA_POD"
echo "NA=$NA"

list:

/opt/inferx/bin/ixtest "http://10.42.0.68:1233" list_pods public default

create:

tp1:

$IXT "$NA" create_func_pod public default swap-a 1 1 crcontainer "$FUNC_DIR/funcspec_cr_tp1a.json"

tp2:

