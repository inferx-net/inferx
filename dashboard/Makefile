# pip install grpcio grpcio-tools
# pip install psycopg2-binary 
all: protoc
run:
	KEYCLOAK_URL=http://192.168.0.22:1260/authn python3 ./app.py

protoc:
	python3 -m grpc_tools.protoc -I ../qshare/proto --python_out=. --grpc_python_out=. qobjs.proto
	python3 -m grpc_tools.protoc -I ../qshare/proto --python_out=. --grpc_python_out=. na.proto
