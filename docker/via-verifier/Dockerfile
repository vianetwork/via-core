# Will work locally only after prior contracts build
# syntax=docker/dockerfile:experimental
FROM matterlabs/zksync-build-base:latest AS builder

WORKDIR /usr/src/via

COPY . .

RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --bin via_verifier

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y curl libpq5 liburing-dev ca-certificates && \
    rm -rf /var/lib/apt/lists/*
ENV PATH=$PATH:/usr/local/bin

EXPOSE 6060

ARG PROTOCOL_VERSION=26
COPY --from=builder /usr/src/via/via_verifier/lib/via_verification/keys/protocol_version/${PROTOCOL_VERSION}/scheduler_key.json \
                    /keys/protocol_version/${PROTOCOL_VERSION}/scheduler_key.json
ENV VIA_VK_KEY_PATH=/keys

COPY --from=builder /usr/src/via/target/release/via_verifier /usr/bin

ENTRYPOINT ["via_verifier"]
