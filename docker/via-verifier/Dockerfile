# Will work locally only after prior contracts build
# syntax=docker/dockerfile:experimental
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
RUN cargo chef cook --release --recipe-path recipe.json --bin via_verifier

FROM chef AS builder
RUN apt-get update && apt-get install -y --no-install-recommends mold && rm -rf /var/lib/apt/lists/*
COPY . .
COPY --from=cacher /usr/local/cargo /usr/local/cargo
COPY --from=cacher /usr/src/via/target target
RUN mold -run cargo build --release --bin via_verifier

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y curl libpq5 liburing-dev ca-certificates && \
    rm -rf /var/lib/apt/lists/*
ENV PATH=$PATH:/usr/local/bin

EXPOSE 6060

COPY --from=builder /usr/src/via/via_verifier/lib/via_verification/keys/protocol_version /keys/protocol_version
ENV VIA_VK_KEY_PATH=/keys

COPY --from=builder /usr/src/via/target/release/via_verifier /usr/bin

ENTRYPOINT ["via_verifier"]
