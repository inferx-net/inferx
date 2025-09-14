
ARG UBUNTU_VERSION=20.04

FROM ubuntu:${UBUNTU_VERSION}
# FROM nvidia/cuda:12.5.1-cudnn-runtime-ubuntu${UBUNTU_VERSION}

WORKDIR /

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    libnuma-dev \
    fuse3 \
    libkeyutils-dev \
    libaio-dev \
    libssl-dev \
    apt-transport-https \
    ca-certificates \
    curl \
    gnupg \
    lsb-release \
    iptables \
    uidmap \
    xz-utils \
    pigz \
    gnupg2 \
    socat \
    wget \    
    && rm -rf /var/lib/apt/lists/*


ENV TZ=Etc/UTC
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && \
    apt-get install -y software-properties-common curl gnupg && \
    curl -fsSL https://download.docker.com/linux/ubuntu/gpg | apt-key add - && \
    add-apt-repository \
      "deb [arch=amd64] https://download.docker.com/linux/ubuntu focal stable" && \
    apt-get update && \
    apt-get install -y \
      docker-ce \
      docker-ce-cli \
      containerd.io && \
    rm -rf /var/lib/apt/lists/*

RUN apt-get update && apt-get install -y vim \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /opt/
COPY inferx/ /opt/inferx/

COPY svc-entrypoint.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/svc-entrypoint.sh

# Copy remaining source/binaries
COPY . .


# Set default command to start Docker daemon and your service
ENTRYPOINT ["/usr/local/bin/svc-entrypoint.sh"]