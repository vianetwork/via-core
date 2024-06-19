#!/usr/bin/env bash

docker run -e NODE_TYPE=$NODE_TYPE -e P2P_NETWORK=$NETWORK \
    --name celestia-node \
    -v $HOME/celestia-volume:/home/celestia \
    -p 26656:26656 \
    -p 26657:26657 \
    -p 26658:26658 \
    -p 9090:9090 \
    -p 1317:1317 \
    -p 2121:2121 \
    ghcr.io/celestiaorg/celestia-node:v0.14.0-rc2 \
    celestia $NODE_TYPE start --core.ip $RPC_URL --p2p.network $NETWORK
