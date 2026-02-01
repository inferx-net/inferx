FROM alpine:3.19

RUN apk add --no-cache \
    bash \
    vim \
    openssh-client \
    curl \
    wget \
    iproute2 \
    net-tools \
    bind-tools \
    tcpdump \
    traceroute \
    jq \
    postgresql-client \
    ca-certificates

CMD ["bash", "-c", "sleep infinity"]
