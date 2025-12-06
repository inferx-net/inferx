ARCH := ${shell uname -m}
VERSION := v0.1.5.beta22
NODE_NAME=${shell hostname}
UBUNTU_VERSION :=$(shell lsb_release -sr)

.PHONY: svc

all: ctl dash spdk runmodel

WATCHED_DIR := ixshare svc

svc: $(shell find $(WATCHED_DIR) -type f)
	cargo +stable build --bin svc
	-sudo cp -f onenode_logging_config.yaml /opt/inferx/config/
	-sudo cp -f nodeconfig/node*.json /opt/inferx/config/	

svcdeploy: svc
	- mkdir -p ./target/svc
	-rm ./target/svc/* -rf
	- mkdir -p ./target/svc/inferx/config
	cp -f onenode_logging_config.yaml ./target/one 
	cp ./target/debug/svc ./target/svc 
	cp ./deployment/svc.Dockerfile ./target/svc/Dockerfile
	cp nodeconfig/node*.json ./target/svc/inferx/config
	cp ./deployment/svc-entrypoint.sh ./target/svc/svc-entrypoint.sh
	sudo docker build --network=host --build-arg UBUNTU_VERSION=$(UBUNTU_VERSION) -t inferx/inferx_platform:$(VERSION) ./target/svc
	sudo docker image prune -f
	# sudo docker push inferx/inferx_platform:$(VERSION)

hf:
	- mkdir -p ./target/hf
	cp -f ./deployment/hf.Dockerfile ./target/hf/Dockerfile
	cp -f ./deployment/download.py ./target/hf/download.py
	sudo docker build -t inferx/inferx_hfdownload:v0.1.0 ./target/hf

pushhf: hf
	sudo docker push inferx/inferx_hfdownload:v0.1.0

# make download MODEL=remodlai/Qwen3-VL-30B-A3B-Instruct-AWQ
download:
	sudo docker run --rm \
	--network host \
    -v /opt/inferx/cache:/models \
    inferx/inferx_hfdownload:v0.1.0 \
        $(MODEL)

pushsvc: svcdeploy
	# sudo docker login -u inferx
	sudo docker tag inferx/inferx_platform:$(VERSION) inferx/inferx_platform:$(VERSION)
	sudo docker push inferx/inferx_platform:$(VERSION)

pushall: pushsvc pushdb pushdash

ctl:	
	# the release version has build error
	OPENSSL_STATIC=1 cargo +stable build --bin ixctl
	# sudo strip target/debug/ixctl
	-sudo cp -f ixctl_logging_config.yaml /opt/inferx/config/
	-sudo cp -f target/debug/ixctl /opt/inferx/bin/

dash:
	mkdir -p ./target/dashboard
	-rm ./target/dashboard/* -rf
	cp ./dashboard/* ./target/dashboard -rL
	cp ./deployment/dashboard.Dockerfile ./target/dashboard/Dockerfile
	-sudo docker image rm inferx/inferx_dashboard:$(VERSION)
	sudo docker build -t inferx/inferx_dashboard:$(VERSION) ./target/dashboard

pushdash: dash
	# sudo docker login -u inferx
	sudo docker tag inferx/inferx_dashboard:$(VERSION) inferx/inferx_dashboard:$(VERSION)
	sudo docker push inferx/inferx_dashboard:$(VERSION)

runmodel:
	mkdir -p ./target/runmodel
	cp ./script/run_model.py ./target/runmodel
	cp ./script/run_stablediffusion.py ./target/runmodel
	cp ./deployment/vllm-opai.Dockerfile ./target/runmodel/Dockerfile
	-sudo docker image rm vllm-openai-upgraded:$(VERSION)
	sudo docker build -t vllm-openai-upgraded:$(VERSION) ./target/runmodel

spdk:
	mkdir -p ./target/spdk
	-rm ./target/spdk/* -rf
	cp ./deployment/spdk.Dockerfile ./target/spdk/Dockerfile
	-sudo docker image rm inferx/spdk-container:$(VERSION)
	sudo docker build -t inferx/spdk-container:$(VERSION) ./target/spdk

spdk2:
	mkdir -p ./target/spdk
	-rm ./target/spdk/* -rf
	cp ./deployment/spdk2.Dockerfile ./target/spdk/Dockerfile
	cp ./deployment/spdk.script ./target/spdk/entrypoint.sh
	-sudo docker image rm inferx/spdk-container2:$(VERSION)
	sudo docker build -t inferx/spdk-container2:$(VERSION) ./target/spdk

pushspdk:
	# sudo docker login -u inferx
	sudo docker tag inferx/spdk-container:$(VERSION) inferx/spdk-container:$(VERSION)
	sudo docker push inferx/spdk-container:$(VERSION)
	sudo docker tag inferx/spdk-container2:$(VERSION) inferx/spdk-container2:$(VERSION)
	sudo docker push inferx/spdk-container2:$(VERSION)

sql:
	sudo cp ./dashboard/sql/create_table.sql /opt/inferx/config
	sudo cp ./dashboard/sql/secret.sql /opt/inferx/config

db: 
	-mkdir -p ./target/postgres
	-rm ./target/postgres/* -rf
	cp ./dashboard/sql/*.sql ./target/postgres
	cp ./deployment/postgres-entrypoint.sh ./target/postgres/postgres-entrypoint.sh
	cp ./deployment/postgres.Dockerfile ./target/postgres/Dockerfile
	sudo docker build --network=host -t inferx/inferx_postgres:$(VERSION) ./target/postgres
	sudo docker image prune -f
	# sudo docker push inferx/inferx_postgres:$(VERSION)

pushdb: db
	sudo docker push inferx/inferx_postgres:$(VERSION)

run:
	-sudo pkill -9 inferx
	@echo "LOCAL_IP=$$(hostname -I | awk '{print $$1}' | xargs)" > .env
	@echo "Version=$(VERSION)" >> .env
	@echo "HOSTNAME=$(NODE_NAME)" >> .env
	- sudo rm -f /opt/inferx/log/*.log
	# - sudo rm -f /opt/inferx/log/onenode.log
	sudo docker compose -f docker-compose.yml up -d --remove-orphans
	rm .env

runblob:
	-sudo pkill -9 inferx
	@echo "LOCAL_IP=$$(hostname -I | tr ' ' '\n' | grep -v '^172\.' | head -n 1 | xargs)" > .env
	@echo "Version=$(VERSION)" >> .env
	@echo "HOSTNAME=$(NODE_NAME)" >> .env
	sudo docker compose -f docker-compose_blob.yml  build
	- sudo rm -f /opt/inferx/log/*.log
	sudo docker compose -f docker-compose_blob.yml up -d --remove-orphans
	cat .env
	rm .env

stop:
	sudo docker compose -f docker-compose.yml down
	
stopblob:
	sudo docker compose -f docker-compose_blob.yml down

runkblob:
	-sudo rm /opt/inferx/log/*.log
	sudo kubectl apply -f k8s/gateway-servicemonitor.yaml
	sudo kubectl apply -f k8s/scheduler-servicemonitor.yaml
	# sudo kubectl apply -f k8s/spdk.yaml
	sudo kubectl apply -f k8s/jaeger.yaml
	sudo kubectl apply -f k8s/etcd.yaml
	sudo kubectl apply -f k8s/keycloak_postgres.yaml
	sudo kubectl apply -f k8s/keycloak.yaml
	VERSION=$(VERSION) envsubst < k8s/secretdb.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/db-deployment.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/statesvc.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/gateway.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/scheduler.yaml | sudo kubectl apply -f -
	# VERSION=$(VERSION) envsubst < k8s/ixproxy.yaml | sudo kubectl apply -f -
	# VERSION=$(VERSION) envsubst < k8s/nodeagent.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/dashboard.yaml | sudo kubectl apply -f -
	# sudo kubectl apply -f k8s/dashboard.yaml
	sudo kubectl apply -f k8s/ingress.yaml
stopall:
	kubectl delete all --all 

rundash:
	VERSION=$(VERSION) envsubst < k8s/dashboard.yaml | sudo kubectl apply -f -

stopdash:
	sudo kubectl delete deployment inferx-dashboard

runstatesvc:
	VERSION=$(VERSION) envsubst < k8s/statesvc.yaml | sudo kubectl apply -f -

stopstatesvc:
	sudo kubectl delete deployment statesvc

rundb:
	VERSION=$(VERSION) envsubst < k8s/db-deployment.yaml | sudo kubectl apply -f -

runkdash:
	VERSION=$(VERSION) envsubst < k8s/dashboard.yaml | sudo kubectl apply -f -

stopkdash:
	sudo kubectl delete deployment inferx-dashboard

rungateway:
	VERSION=$(VERSION) envsubst < k8s/gateway.yaml | sudo kubectl apply -f -

stopgateway:
	sudo kubectl delete deployment gateway

runscheduler:
	VERSION=$(VERSION) envsubst < k8s/scheduler.yaml | sudo kubectl apply -f -

stopscheduler:
	sudo kubectl delete deployment scheduler

runsvc:
	VERSION=$(VERSION) envsubst < k8s/statesvc.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/gateway.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/scheduler.yaml | sudo kubectl apply -f -

stopsvc:
	-sudo kubectl delete deployment scheduler
	-sudo kubectl delete deployment gateway
	-sudo kubectl delete deployment statesvc

runna:
	# -sudo rm /opt/inferx/log/*.log
	VERSION=$(VERSION) envsubst < k8s/nodeagent.yaml | sudo kubectl apply -f -
stopna:
	# sudo kubectl delete DaemonSet ixproxy
	sudo kubectl delete DaemonSet nodeagent-blob
	sudo kubectl delete DaemonSet nodeagent-file

runproxy:
	VERSION=$(VERSION) envsubst < k8s/ixproxy.yaml | sudo kubectl apply -f -
stopproxy:
	sudo kubectl delete DaemonSet ixproxy

runnaall:
	# -sudo rm /opt/inferx/log/*.log
	VERSION=$(VERSION) envsubst < k8s/ixproxy.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/nodeagent.yaml | sudo kubectl apply -f -
stopnaall:
	sudo kubectl delete DaemonSet ixproxy
	sudo kubectl delete DaemonSet nodeagent-blob
	sudo kubectl delete DaemonSet nodeagent-file

restartgw:
	sudo kubectl delete deployment gateway
	sudo kubectl apply -f k8s/gateway.yaml

runallcw:
	-rm /opt/inferx/log/*.log
	kubectl apply -f k8s/etcd.yaml
	kubectl apply -f k8s/keycloak_postgres.yaml
	kubectl apply -f k8s/keycloak.yaml
	VERSION=$(VERSION) envsubst < k8s/secretdb.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/db-deployment.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/statesvc.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/gateway.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/scheduler.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/ixproxy.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/nodeagent.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/dashboard.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/dashboard_lb.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/gw_lb.yaml | kubectl apply -f -
	kubectl apply -f k8s/ingress.yaml

runallfw:
# 	-rm /opt/inferx/log/*.log
	kubectl apply -f k8s/etcd.yaml
	kubectl apply -f k8s/keycloak_postgres.yaml
	kubectl apply -f k8s/keycloak.yaml
	VERSION=$(VERSION) envsubst < k8s/secretdb.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/db-deployment.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/statesvc.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/gateway.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/scheduler.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/ixproxy.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/nodeagent.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/dashboard.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/dashboard_lb.yaml | kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/gw_lb.yaml | kubectl apply -f -
	kubectl apply -f k8s/ingress.yaml

runallnb:
	-sudo rm /opt/inferx/log/*.log
	sudo kubectl apply -f k8s/etcd.yaml
	sudo kubectl apply -f k8s/keycloak_postgres.yaml
	sudo kubectl apply -f k8s/keycloak.yaml
	VERSION=$(VERSION) envsubst < k8s/secretdb.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/db-deployment.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/statesvc.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/gateway.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/scheduler.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/ixproxy-nb.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/nodeagent-nb.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/dashboard-nb.yaml | sudo kubectl apply -f -
	sudo kubectl apply -f k8s/ingress.yaml

runallnbmg:
	-sudo rm /opt/inferx/log/*.log
	sudo kubectl apply -f k8s/etcd.yaml
	sudo kubectl apply -f k8s/keycloak_postgres.yaml
	sudo kubectl apply -f k8s/keycloak.yaml
	VERSION=$(VERSION) envsubst < k8s/secretdb.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/db-deployment.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/statesvc.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/gateway.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/scheduler.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/ixproxy-nbmg.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/nodeagent-nbmg.yaml | sudo kubectl apply -f -
	VERSION=$(VERSION) envsubst < k8s/dashboard-nb.yaml | sudo kubectl apply -f -
	sudo kubectl apply -f k8s/ingress.yaml
