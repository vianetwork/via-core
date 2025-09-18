#! /bin/bash

# Run the docker-compose-via.yml with reorg profile, this will start 2 Bitcoin node that you can test the reorg.

# Disconnect the node2 from node1
docker exec -it via-core-bitcoin-cli2-1 bash -c \
"IP_BITCOIND=\$(getent hosts bitcoind | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND add"

# Disconnect the node1 from node2
docker exec -it via-core-bitcoin-cli-1 bash -c \
"IP_BITCOIND2=\$(getent hosts bitcoind2 | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND2 add"

# Mint new blocks in node 1
docker exec -it via-core-bitcoin-cli-1 bash -c \
"ADDR=\$(bitcoin-cli \$RPC_ARGS -rpcwallet=Alice getnewaddress) && bitcoin-cli \$RPC_ARGS generatetoaddress 2 \$ADDR"

# Get the block height
docker exec -it via-core-bitcoin-cli-1 bash -c "bitcoin-cli \$RPC_ARGS getblockcount"

# Mint new blocks in node 2
docker exec -it via-core-bitcoin-cli2-1 bash -c \
"ADDR=\$(bitcoin-cli \$RPC_ARGS -rpcwallet=Bob getnewaddress) && bitcoin-cli \$RPC_ARGS generatetoaddress 20 \$ADDR"

# Get the block height
docker exec -it via-core-bitcoin-cli2-1 bash -c "bitcoin-cli \$RPC_ARGS getblockcount"

# Reconnect the node1 from node2
docker exec -it via-core-bitcoin-cli-1 bash -c \
"IP_BITCOIND2=\$(getent hosts bitcoind2 | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND2 remove"

# Reconnect the node2 from node1
docker exec -it via-core-bitcoin-cli2-1 bash -c \
"IP_BITCOIND=\$(getent hosts bitcoind | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND remove"
