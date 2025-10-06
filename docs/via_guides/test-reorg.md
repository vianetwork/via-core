# Testing reorg on localhost

## Test Part 1:

1. Start the sequencer `make via-multi`, this will start the sequencer and 2 Bitcoin node that we can use to simulate
   reorgs.
2. Start the coordinator and verifier.
3. Make some deposits, make sure to create 1 batch per deposit. So the last created batch should be batch 2.

```sh
# Deposit 1 BTC
via token deposit \
    --amount 1 \
    --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 \
    --bridge-address bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq \
    --l1-rpc-url http://0.0.0.0:18443

# Deposit 2 BTC
via token deposit \
    --amount 2 \
    --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 \
    --bridge-address bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq \
    --l1-rpc-url http://0.0.0.0:18443
```

4. Disconnect the 2 nodes

```sh
# Disconnect the node2 from node1
docker exec -it via-core-bitcoin-cli2-1 bash -c \
"IP_BITCOIND=\$(getent hosts bitcoind | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND add"

# Disconnect the node1 from node2
docker exec -it via-core-bitcoin-cli-1 bash -c \
"IP_BITCOIND2=\$(getent hosts bitcoind2 | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND2 add"
```

5. Make some deposits in the second node, you should not see any transaction or new batch created because this node is
   isolated from the main one running in the port `http://0.0.0.0:18443`

```sh
# Deposit 1 BTC
via token deposit \
    --amount 3 \
    --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 \
    --bridge-address bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq \
    --sender-private-key cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm \
    --l1-rpc-url http://0.0.0.0:19443

# Deposit 2 BTC
via token deposit \
    --amount 4 \
    --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 \
    --bridge-address bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq \
    --sender-private-key cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm \
    --l1-rpc-url http://0.0.0.0:19443
```

6. Make a deposit and a withdrawal in the first node, to create a block conflict which requires a reorg

```sh
# Delete the depositor inscriber context
rm core/lib/via_btc_client/depositor_inscriber_context.json

# Deposit 5 BTC
via token deposit \
    --amount 5 \
    --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 \
    --bridge-address bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq \
    --sender-private-key cQ4UHjdsGWFMcQ8zXcaSr7m4Kxq9x7g9EKqguTaFH7fA34mZAnqW \
    --l1-rpc-url http://0.0.0.0:18443

# Create a withdrawal
via token withdraw --amount 1 --receiver-l1-address bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56
```

7. Create dummy blocks on both nodes

```sh
# Create dummy blocks in node 1
docker exec -it via-core-bitcoin-cli-1 bash -c \
"ADDR=\$(bitcoin-cli \$RPC_ARGS -rpcwallet=Alice getnewaddress) && bitcoin-cli \$RPC_ARGS generatetoaddress 2 \$ADDR"

# Create dummy blocks in node 2
docker exec -it via-core-bitcoin-cli2-1 bash -c \
"ADDR=\$(bitcoin-cli \$RPC_ARGS -rpcwallet=Bob getnewaddress) && bitcoin-cli \$RPC_ARGS generatetoaddress 20 \$ADDR"
```

8. Stop the sequencer and update the ENVs then restart the sequencer

```sh
# Increase the time to commit the proof so we have time to revert
VIA_BTC_SENDER_BLOCK_TIME_TO_PROOF=300
```

9. Reconnect both nodes

```sh
# Reconnect the node1 from node2
docker exec -it via-core-bitcoin-cli-1 bash -c \
"IP_BITCOIND2=\$(getent hosts bitcoind2 | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND2 remove"

# Reconnect the node2 from node1
docker exec -it via-core-bitcoin-cli2-1 bash -c \
"IP_BITCOIND=\$(getent hosts bitcoind | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND remove"
```

10. Here you should see some warns on the sequencer, coordinator and verifier terminals. The sequencer needs to be
    stopped to execute a revert. For the coordinator and verifier we don't need to do anything.

11. Execute the sequencer block reverter using the CLI, the last valid should be batch 2.

```sh
# Export the VIA ENVs
export $(grep -v '^#' etc/env/target/via.env | xargs)

# Run the revert
make rollback to_batch=2
```

12. Update the sequencer ENV

```sh
VIA_BTC_SENDER_BLOCK_TIME_TO_PROOF=0
```

13. Restart the sequencer and wait the verifier network to verify the batches.

## Test Part 2:

14. Disconnect the 2 nodes

```sh
# Disconnect the node2 from node1
docker exec -it via-core-bitcoin-cli2-1 bash -c \
"IP_BITCOIND=\$(getent hosts bitcoind | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND add"

# Disconnect the node1 from node2
docker exec -it via-core-bitcoin-cli-1 bash -c \
"IP_BITCOIND2=\$(getent hosts bitcoind2 | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND2 add"
```

15. Connect the verifier with the node 2 and keep the sequencer and coordinator connected with node 1.

```sh
# Set the verifier node url
VIA_BTC_CLIENT_RPC_URL=http://0.0.0.0:19443

# Set the sequencer commit block time
VIA_BTC_SENDER_BLOCK_TIME_TO_PROOF=500
```

16. Deposit on the node 1, wait between each deposit to create 2 batches.

```sh
# Deposit 1 BTC
via token deposit \
    --amount 1 \
    --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 \
    --bridge-address bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq \
    --l1-rpc-url http://0.0.0.0:18443

# Deposit 2 BTC
via token deposit \
    --amount 2 \
    --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 \
    --bridge-address bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq \
    --l1-rpc-url http://0.0.0.0:18443
```

16. Make a deposit and a withdrawal in the second node, to create a block conflict which requires a reorg

```sh
# Delete the depositor inscriber context
rm core/lib/via_btc_client/depositor_inscriber_context.json

# Deposit 3 BTC
via token deposit \
    --amount 3 \
    --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 \
    --bridge-address bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq \
    --sender-private-key cQ4UHjdsGWFMcQ8zXcaSr7m4Kxq9x7g9EKqguTaFH7fA34mZAnqW \
    --l1-rpc-url http://0.0.0.0:19443

# Create a withdrawal
via token withdraw --amount 1 --receiver-l1-address bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56
```

17. Create dummy blocks on both nodes

```sh
# Create dummy blocks in node 1
docker exec -it via-core-bitcoin-cli-1 bash -c \
"ADDR=\$(bitcoin-cli \$RPC_ARGS -rpcwallet=Alice getnewaddress) && bitcoin-cli \$RPC_ARGS generatetoaddress 20 \$ADDR"

# Create dummy blocks in node 2
docker exec -it via-core-bitcoin-cli2-1 bash -c \
"ADDR=\$(bitcoin-cli \$RPC_ARGS -rpcwallet=Bob getnewaddress) && bitcoin-cli \$RPC_ARGS generatetoaddress 2 \$ADDR"
```

18. Reconnect both nodes

```sh
# Reconnect the node1 from node2
docker exec -it via-core-bitcoin-cli-1 bash -c \
"IP_BITCOIND2=\$(getent hosts bitcoind2 | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND2 remove"

# Reconnect the node2 from node1
docker exec -it via-core-bitcoin-cli2-1 bash -c \
"IP_BITCOIND=\$(getent hosts bitcoind | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP_BITCOIND remove"
```

19. The verifier should reorg but not the sequencer and the verifier because those are connected to the node 1 which is
    the chain with the most computation work.

20. The blocks are verified and withdrawal executed.
21. Done :)
