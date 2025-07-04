ARCH := ${shell uname -m}
VERSION := v0.1.2
NODE_NAME=${shell hostname}

all: ctl dash spdk runmodel

ctl:	
	cargo +stable build --bin ixctl --release
	sudo cp -f ixctl_logging_config.yaml /opt/inferx/config/
	sudo cp -f target/release/ixctl /opt/inferx/bin/

dash:
	mkdir -p ./target/dashboard
	-rm ./target/dashboard/* -rf
	cp ./dashboard/* ./target/dashboard -rL
	cp ./deployment/dashboard.Dockerfile ./target/dashboard/Dockerfile
	-sudo docker image rm inferx/inferx_dashboard:$(VERSION)
	sudo docker build -t inferx/inferx_dashboard:$(VERSION) ./target/dashboard

pushdash:
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

run:
	-sudo pkill -9 inferx
	@echo "LOCAL_IP=$$(hostname -I | awk '{print $$1}' | xargs)" > .env
	@echo "Version=$(VERSION)" >> .env
	@echo "HOSTNAME=$(NODE_NAME)" >> .env
	sudo docker compose -f docker-compose.yml  build
	- sudo rm -f /opt/inferx/log/inferx.log
	- sudo rm -f /opt/inferx/log/onenode.log
	sudo docker compose -f docker-compose.yml up -d --remove-orphans
	rm .env

runblob:
	-sudo pkill -9 inferx
	@echo "LOCAL_IP=$$(hostname -I | tr ' ' '\n' | grep -v '^172\.' | head -n 1 | xargs)" > .env
	@echo "Version=$(VERSION)" >> .env
	@echo "HOSTNAME=$(NODE_NAME)" >> .env
	sudo docker compose -f docker-compose_blob.yml  build
	- sudo rm -f /opt/inferx/log/inferx.log
	- sudo rm -f /opt/inferx/log/onenode.log
	sudo docker compose -f docker-compose_blob.yml up -d --remove-orphans
	cat .env
	rm .env

stop:
	sudo docker compose -f docker-compose.yml down
	
stopblob:
	sudo docker compose -f docker-compose_blob.yml down

rundash:
	sudo docker run --net=host --name inferx_dashboard --env "KEYCLOAK_URL=http://192.168.0.22:1260/authn" \
	-v /etc/letsencrypt/:/etc/letsencrypt/ --rm  inferx/inferx_dashboard:$(VERSION)

stopdash:
	sudo docker stop inferx_dashboard

runkblob:
	sudo kubectl apply -f k8s/spdk.yaml
	sudo kubectl apply -f k8s/etcd.yaml
	sudo kubectl apply -f k8s/secretdb.yaml
	sudo kubectl apply -f k8s/db-deployment.yaml
	sudo kubectl apply -f k8s/keycloak_postgres.yaml
	sudo kubectl apply -f k8s/keycloak.yaml
	sudo kubectl apply -f k8s/statesvc.yaml
	sudo kubectl apply -f k8s/scheduler.yaml
	sudo kubectl apply -f k8s/nodeagent.yaml
	sudo kubectl apply -f k8s/dashboard.yaml
	sudo kubectl apply -f k8s/ingress.yaml

stopnodeagent:
	sudo kubectl delete DaemonSet nodeagent-blob
	sudo kubectl delete DaemonSet nodeagent-file