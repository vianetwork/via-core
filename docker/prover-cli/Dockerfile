FROM matterlabs/zksync-build-base:latest AS builder

ARG DEBIAN_FRONTEND=noninteractive
ARG RUST_FLAGS=""
ENV RUSTFLAGS=${RUST_FLAGS}

WORKDIR /usr/src/zksync
COPY . .

RUN cd prover && cargo build --release --bin prover_cli

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y curl libpq5 ca-certificates && rm -rf /var/lib/apt/lists/*

COPY prover/crates/bin/vk_setup_data_generator_server_fri/data/ /prover/crates/bin/vk_setup_data_generator_server_fri/data/

COPY --from=builder /usr/src/zksync/prover/target/release/prover_cli /usr/bin/

ENTRYPOINT ["prover_cli"]
