# syntax=docker/dockerfile:experimental
# Will work locally only after prior contracts build
FROM lukemathwalker/cargo-chef:0.1.77-rust-bookworm AS cargo-chef

FROM matterlabs/zksync-build-base:latest AS chef

# set of args for use of sccache
ARG SCCACHE_GCS_BUCKET=""
ARG SCCACHE_GCS_SERVICE_ACCOUNT=""
ARG SCCACHE_GCS_RW_MODE=""
ARG RUSTC_WRAPPER=""

ENV SCCACHE_GCS_BUCKET=${SCCACHE_GCS_BUCKET}
ENV SCCACHE_GCS_SERVICE_ACCOUNT=${SCCACHE_GCS_SERVICE_ACCOUNT}
ENV SCCACHE_GCS_RW_MODE=${SCCACHE_GCS_RW_MODE}
ENV RUSTC_WRAPPER=${RUSTC_WRAPPER}

WORKDIR /usr/src/via

RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev git && rm -rf /var/lib/apt/lists/*

COPY --from=cargo-chef /usr/local/cargo/bin/cargo-chef /usr/local/cargo/bin/cargo-chef

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS cacher
COPY --from=planner /usr/src/via/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --bin via_server --bin via_block_reverter_cli --bin merkle_tree_consistency_checker

FROM chef AS builder
COPY . .
COPY --from=cacher /usr/local/cargo /usr/local/cargo
COPY --from=cacher /usr/src/via/target target
RUN cargo build --release --bin via_server --bin via_block_reverter_cli --bin merkle_tree_consistency_checker

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y curl libpq5 liburing-dev ca-certificates && \
    rm -rf /var/lib/apt/lists/*
ENV PATH=$PATH:/usr/local/bin

EXPOSE 3000
EXPOSE 3031
EXPOSE 3030

COPY --from=builder /usr/src/via/target/release/via_server /usr/bin
COPY --from=builder /usr/src/via/target/release/via_block_reverter_cli /usr/bin
COPY --from=builder /usr/src/via/target/release/merkle_tree_consistency_checker /usr/bin
COPY contracts/system-contracts/bootloader/build/artifacts/ /contracts/system-contracts/bootloader/build/artifacts/
COPY contracts/system-contracts/contracts-preprocessed/artifacts/ /contracts/system-contracts/contracts-preprocessed/artifacts/
COPY contracts/system-contracts/contracts-preprocessed/precompiles/artifacts/ /contracts/system-contracts/contracts-preprocessed/precompiles/artifacts/
COPY contracts/system-contracts/artifacts-zk /contracts/system-contracts/artifacts-zk
COPY contracts/l2-contracts/artifacts-zk/ /contracts/l2-contracts/artifacts-zk/
COPY etc/tokens/ /etc/tokens/
COPY etc/ERC20/ /etc/ERC20/
COPY etc/multivm_bootloaders/ /etc/multivm_bootloaders/

ENTRYPOINT ["via_server"]
