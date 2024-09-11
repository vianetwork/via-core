# Building solidity from sources so binary is available for any architecture
FROM debian:bookworm AS solidity-builder

# Link Docker Image with repository
# https://docs.github.com/en/packages/learn-github-packages/connecting-a-repository-to-a-package#connecting-a-repository-to-a-container-image-using-the-command-line
LABEL org.opencontainers.image.source=https://github.com/vianetwork/via-core
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"

# Install prerequisites
RUN apt-get update && apt-get install -y \
    build-essential \
    cmake \
    git \
    libboost-all-dev && \
    rm -rf /var/lib/apt/lists/*

# Clone Solidity repository
RUN git clone https://github.com/ethereum/solidity.git

# Checkout specific version
WORKDIR /solidity
RUN git checkout v0.8.20

# Initialize and update submodules
RUN git submodule update --init --recursive

# Build Solidity
WORKDIR /solidity/build
# The default compilation is Release with Debug symbols, which is quite large.
RUN cmake .. -DCMAKE_BUILD_TYPE="Release"
RUN make -j

FROM debian:bookworm AS rust-lightweight

ARG ARCH=amd64

WORKDIR /usr/src/via
ENV DEBIAN_FRONTEND=noninteractive

# Install required dependencies
RUN apt-get update && apt-get install -y \
    cmake \
    make \
    bash \
    git \
    openssl \
    libssl-dev \
    gcc \
    g++ \
    curl \
    pkg-config \
    software-properties-common \
    jq \
    openssh-server \
    openssh-client \
    vim \
    ca-certificates \
    gnupg2 \
    postgresql-client \
    wget \
    bzip2 \
    hub \
    unzip \
    protobuf-compiler && \
    rm -rf /var/lib/apt/lists/*

# Install dependencies for RocksDB.
RUN apt-get update && \
    apt-get install -y \
    gnutls-bin git \
    build-essential \
    clang-15 \
    lldb-15 \
    lld-15 \
    liburing-dev \
    libclang-dev && \
    rm -rf /var/lib/apt/lists/*

# Install Docker
RUN apt-get update && \
    apt-get install ca-certificates gnupg && \
    install -m 0755 -d /etc/apt/keyrings && \
    wget -c -O - https://download.docker.com/linux/debian/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg && \
    chmod a+r /etc/apt/keyrings/docker.gpg && \
    echo \
    "deb [arch="$(dpkg --print-architecture)" signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/debian \
    "$(. /etc/os-release && echo "$VERSION_CODENAME")" stable" | \
    tee /etc/apt/sources.list.d/docker.list > /dev/null && \
    apt-get update && apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin && \
    rm -rf /var/lib/apt/lists/*

# Install Node and yarn
ENV NODE_MAJOR=18
RUN mkdir -p /etc/apt/keyrings && \
    wget -c -O - https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key | gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg && \
    echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_$NODE_MAJOR.x nodistro main" | tee /etc/apt/sources.list.d/nodesource.list && \
    apt-get update && apt-get install nodejs -y && \
    npm install -g yarn

# Install Rust and required cargo packages
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# Install gloud for GCR/GAR login
# Google was super lazy, and their package is around 1 GB.
# So we trim it a little bit based on info from `https://github.com/GoogleCloudPlatform/gsutil/issues/1732`
ENV GCLOUD_VERSION=451.0.1
RUN echo "deb [arch=${ARCH}] http://packages.cloud.google.com/apt cloud-sdk main" > /etc/apt/sources.list.d/google-cloud-sdk.list && \
    wget -c -O - https://packages.cloud.google.com/apt/doc/apt-key.gpg | apt-key add - && \
    apt-get update -y && apt-get install google-cloud-cli=${GCLOUD_VERSION}-0 --no-install-recommends -y && \
    gcloud config set core/disable_usage_reporting true && \
    gcloud config set component_manager/disable_update_check true && \
    gcloud config set metrics/environment github_docker_image && \
    rm -rf $(find /usr/lib/google-cloud-sdk/ -regex ".*/__pycache__") && \
    rm -rf /usr/lib/google-cloud-sdk/bin/anthoscli && \
    rm -rf /usr/lib/google-cloud-sdk/platform/bundledpythonunix && \
    rm -rf /usr/lib/google-cloud-sdk/data/gcloud.json

COPY --from=rust-builder /usr/local/cargo/bin/sqlx \
    /usr/local/cargo/bin/cargo-sqlx \
    /usr/local/cargo/bin/cargo-nextest \
    /usr/local/cargo/bin/cargo-spellcheck \
    /usr/local/cargo/bin/sccache \
    /usr/local/cargo/bin/forge \
    /usr/local/cargo/bin/cast /usr/local/cargo/bin/
    
# Copy compiler (both solc and zksolc) binaries
# Obtain `solc` 0.8.20.
COPY --from=solidity-builder /solidity/build/solc/solc /usr/bin/
RUN chmod +x /usr/bin/solc
# Obtain `zksolc` 1.3.16.
RUN wget -c https://github.com/matter-labs/zksolc-bin/raw/main/linux-${ARCH}/zksolc-linux-${ARCH}-musl-v1.3.16 \
    && mv zksolc-linux-${ARCH}-musl-v1.3.16 /usr/bin/zksolc \
    && chmod +x /usr/bin/zksolc

# Somehow it is installed with some other packages
RUN apt-get remove valgrind -y

# We need valgrind 3.20, which is unavailable in repos or ppa, so we will build it from source
RUN wget -c https://sourceware.org/pub/valgrind/valgrind-3.20.0.tar.bz2 && \
    tar -xf valgrind-3.20.0.tar.bz2 && \
    cd valgrind-3.20.0 && ./configure && make -j && make install && \
    cd ../ && rm -rf valgrind-3.20.0.tar.bz2 && rm -rf valgrind-3.20.0

# Install bitcoin core
RUN wget -c https://bitcoin.org/bin/bitcoin-core-27.0/bitcoin-27.0-x86_64-linux-gnu.tar.gz && \
    tar -xf bitcoin-27.0-x86_64-linux-gnu.tar.gz && \
    mv bitcoin-27.0/bin/* /usr/local/bin/ && \
    rm -rf bitcoin-27.0 && rm -rf bitcoin-27.0-x86_64-linux-gnu.tar.gz

# Setup the environment
ENV VIA_HOME=/usr/src/via
ENV PATH="${VIA_HOME}/bin:${PATH}"
ENV CI=1
ENV RUSTC_WRAPPER=/usr/local/cargo/bin/sccache

FROM rust-lightweight AS rust-lightweight-nightly


# If target is nightly - then install only nightly rust.
FROM rust-lightweight-base as rust-lightweight-nightly
RUN wget -c -O - https://sh.rustup.rs | bash -s -- -y --default-toolchain nightly-2024-08-01