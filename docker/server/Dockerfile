# Will work locally only after prior contracts build
# syntax=docker/dockerfile:experimental
FROM matterlabs/zksync-build-base:latest AS builder

WORKDIR /usr/src/via

COPY . .

RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*
RUN cargo build --release #--features=rocksdb/io-uring <-- investigate what is this

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y curl libpq5 liburing-dev ca-certificates && \
    rm -rf /var/lib/apt/lists/*
ENV PATH=$PATH:/usr/local/bin

EXPOSE 3000
EXPOSE 3031
EXPOSE 3030

COPY --from=builder /usr/src/via/target/release/via_server /usr/bin
COPY --from=builder /usr/src/via/target/release/block_reverter /usr/bin
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
