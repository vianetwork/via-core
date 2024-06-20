#!/usr/bin/env bash

docker run --network host -e NODE_TYPE=$NODE_TYPE -e P2P_NETWORK=$NETWORK \
    --name celestia-node \
    -v $HOME/celestia-volume:/home/celestia \
    -p 26656:26656 \
    -p 26656:26656/udp \
    -p 26657:26657 \
    -p 26657:26657/udp \
    -p 26658:26658 \
    -p 26658:26658/udp \
    -p 9090:9090 \
    -p 9090:9090/udp \
    -p 1317:1317 \
    -p 1317:1317/udp \
    -p 2121:2121 \
    -p 2121:2121/udp \
    ghcr.io/celestiaorg/celestia-node:v0.14.0-rc2 \
    celestia $NODE_TYPE start --core.ip $RPC_URL --p2p.network $NETWORK

# eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJBbGxvdyI6WyJwdWJsaWMiLCJyZWFkIiwid3JpdGUiLCJhZG1pbiJdfQ.ut1X4u9XG5cbV0yaRAKfGp9xWVrz3NoEPGGRch13dFU
# address:
# celestia14aa9asfwdheasrc5q8kl4vz7kp4k6leaz7wuph
