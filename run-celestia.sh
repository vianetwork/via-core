#!/usr/bin/env bash

docker run -e NODE_TYPE=$NODE_TYPE -e P2P_NETWORK=$NETWORK \
    -v $HOME/celestia-volume:/home/celestia \
    ghcr.io/celestiaorg/celestia-node:v0.14.0-rc2 \
    celestia $NODE_TYPE start --core.ip $RPC_URL --p2p.network $NETWORK
