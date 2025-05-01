ARCH := ${shell uname -m}

LOCAL_IP=${hostname -I | awk '{print $$1}' | xargs}

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
	-sudo docker image rm inferx/inferx_dashboard:v0.1.0
	sudo docker build -t inferx/inferx_dashboard:v0.1.0 ./target/dashboard
	# sudo docker push inferx/inferx_dashboard:v0.1.0

pushdash:
	# sudo docker login -u inferx
	sudo docker tag inferx/inferx_dashboard:v0.1.0 inferx/inferx_dashboard:v0.1.0
	sudo docker push inferx/inferx_dashboard:v0.1.0

runmodel:
	mkdir -p ./target/runmodel
	cp ./script/run_model.py ./target/runmodel
	cp ./script/run_llava.py ./target/runmodel
	cp ./script/run_stablediffusion.py ./target/runmodel
	cp ./deployment/vllm-opai.Dockerfile ./target/runmodel/Dockerfile
	-sudo docker image rm vllm-openai-upgraded:v0.1.0
	sudo docker build -t vllm-openai-upgraded:v0.1.0 ./target/runmodel

spdk:
	mkdir -p ./target/spdk
	-rm ./target/spdk/* -rf
	cp ./deployment/spdk.Dockerfile ./target/spdk/Dockerfile
	-sudo docker image rm inferx/spdk-container:v0.1.0
	sudo docker build -t inferx/spdk-container:v0.1.0 ./target/spdk

spdk2:
	mkdir -p ./target/spdk
	-rm ./target/spdk/* -rf
	cp ./deployment/spdk2.Dockerfile ./target/spdk/Dockerfile
	cp ./deployment/spdk.script ./target/spdk/entrypoint.sh
	-sudo docker image rm inferx/spdk-container2:v0.1.0
	sudo docker build -t inferx/spdk-container2:v0.1.0 ./target/spdk

pushspdk:
	# sudo docker login -u inferx
	sudo docker tag inferx/spdk-container:v0.1.0 inferx/spdk-container:v0.1.0
	sudo docker push inferx/spdk-container:v0.1.0
	sudo docker tag inferx/spdk-container2:v0.1.0 inferx/spdk-container2:v0.1.0
	sudo docker push inferx/spdk-container2:v0.1.0
sql:
	sudo cp ./dashboard/sql/create_table.sql /opt/inferx/config
	sudo cp ./dashboard/sql/secret.sql /opt/inferx/config

run:
	-sudo pkill -9 inferx
	@echo "LOCAL_IP=$$(hostname -I | awk '{print $$1}' | xargs)" > .env
	sudo docker compose -f docker-compose.yml  build
	- sudo rm -f /opt/inferx/log/inferx.log
	- sudo rm -f /opt/inferx/log/onenode.log
	sudo docker compose -f docker-compose.yml up -d --remove-orphans
	rm .env

runblob:
	-sudo pkill -9 inferx
	@echo "LOCAL_IP=$$(hostname -I | tr ' ' '\n' | grep -v '^172\.' | head -n 1 | xargs)" > .env
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
	sudo docker run --net=host --name inferx_dashboard -v /etc/letsencrypt/:/etc/letsencrypt/ --rm  inferx/inferx_dashboard:v0.1.0

stopdash:
	sudo docker stop inferx_dashboard