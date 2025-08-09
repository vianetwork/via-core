## Update the bridge address on localhost

1. Create a new bridge address, follow this [doc](musig2.md). You should have a json file on your local `my_wallet.json`
2. Create a proposal update bridge.

```sh
cargo run --example propose_new_bridge \
    regtest \
    http://0.0.0.0:18443 \
    rpcuser \
    rpcpassword \
    cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R \
    bcrt1pfk264lnycy2v48h3we2jajyg7kyuvha9yfkd4qmxfrgywz3meyhqhdhmj8 \
    bcrt1q08v0vm5w3rftefqutgtwlyslhy35ms8ftuay80,bcrt1q50xmdcwlmt8qhwczxptaq2h5cn3zchcrvqd35v
```

3. Copy the txid of the upgrade proposal and create an upgrade using the governance wallet. Follow the doc on how to
   sign a multisig tx (update just the 2 with the following cmd) [here](#How-to-execute-an-upgrade-proposal)

```sh
via multisig create-update-bridge \
--inputTxId 519f8f471f9ea26408935107a8ee4cb10cd9573ded2671c11a6e88af97ea9071 \
--inputVout 1 \
--inputAmount 100000000 \
--proposalTxid bf8731b79d50b8b4862ae91ee6e3e2beae805ba534dd03daa78de0734a3dc8b8 \
--fee 500
```

4. The verifier should start throwing an Error because the current signer doesn't match the new bridge address.

```error
Failed to process verifier withdrawal task: Verifier address not found in the verifiers set, expected one of [bcrt1q08v0vm5w3rftefqutgtwlyslhy35ms8ftuay80, bcrt1q50xmdcwlmt8qhwczxptaq2h5cn3zchcrvqd35v], found bcrt1qw2mvkvm6alfhe86yf328kgvr7mupdx4vln7kpv
```

5. Transfer some BTC to the new verifier addresses

```sh
curl --user rpcuser:rpcpassword \
     --data-binary '{"jsonrpc":"1.0","id":"sendbtc","method":"sendtoaddress","params":["bcrt1q50xmdcwlmt8qhwczxptaq2h5cn3zchcrvqd35v", 0.1]}' \
     -H 'content-type: text/plain;' \
     http://127.0.0.1:18443/wallet/Alice

curl --user rpcuser:rpcpassword \
     --data-binary '{"jsonrpc":"1.0","id":"sendbtc","method":"sendtoaddress","params":["bcrt1q08v0vm5w3rftefqutgtwlyslhy35ms8ftuay80", 0.1]}' \
     -H 'content-type: text/plain;' \
     http://127.0.0.1:18443/wallet/Alice
```

6. Update the ENVs for verifier and coordinator

```sh
VIA_BTC_SENDER_PRIVATE_KEY=cQnW8oDqEME4gxJHC4MC9HvJECcF7Ju8oanWdjWLGxDbkfWo7vZa
VIA_BTC_SENDER_WALLET_ADDRESS=bcrt1q08v0vm5w3rftefqutgtwlyslhy35ms8ftuay80
VIA_VERIFIER_PRIVATE_KEY=cQnW8oDqEME4gxJHC4MC9HvJECcF7Ju8oanWdjWLGxDbkfWo7vZa
VIA_VERIFIER_WALLET_ADDRESS=bcrt1q08v0vm5w3rftefqutgtwlyslhy35ms8ftuay80
VIA_VERIFIER_BRIDGE_ADDRESS_MERKLE_ROOT=2aa187093ce1f9e55ad02aa804480cc01beb9c570781133b768d8cfb12177e25
VIA_BRIDGE_VERIFIERS_PUB_KEYS=025b3c069378f860cc4dae864a491e0cd33cc559b9f82fc856d4dcc74d3d763241,03c2871e18d4fb503ead90461da747b40df5e28da0fd3e067f3731f1a28da60ddf
VIA_BRIDGE_COORDINATOR_PUB_KEY=025b3c069378f860cc4dae864a491e0cd33cc559b9f82fc856d4dcc74d3d763241
VIA_BRIDGE_BRIDGE_ADDRESS=bcrt1pfk264lnycy2v48h3we2jajyg7kyuvha9yfkd4qmxfrgywz3meyhqhdhmj8
```

and coordinator:

```sh
VIA_BTC_SENDER_PRIVATE_KEY=cVJYEHTzmfdRPoX6fL3vRnZVmqy4D1sWaT5WL9U25oZhQktoeHgo
VIA_BTC_SENDER_WALLET_ADDRESS=bcrt1q50xmdcwlmt8qhwczxptaq2h5cn3zchcrvqd35v
VIA_VERIFIER_PRIVATE_KEY=cVJYEHTzmfdRPoX6fL3vRnZVmqy4D1sWaT5WL9U25oZhQktoeHgo
VIA_VERIFIER_WALLET_ADDRESS=bcrt1q50xmdcwlmt8qhwczxptaq2h5cn3zchcrvqd35v
VIA_VERIFIER_BRIDGE_ADDRESS_MERKLE_ROOT=2aa187093ce1f9e55ad02aa804480cc01beb9c570781133b768d8cfb12177e25
VIA_GENESIS_VERIFIERS_PUB_KEYS=025b3c069378f860cc4dae864a491e0cd33cc559b9f82fc856d4dcc74d3d763241,03c2871e18d4fb503ead90461da747b40df5e28da0fd3e067f3731f1a28da60ddf
VIA_GENESIS_COORDINATOR_PUB_KEY=025b3c069378f860cc4dae864a491e0cd33cc559b9f82fc856d4dcc74d3d763241
VIA_BRIDGE_BRIDGE_ADDRESS=bcrt1pfk264lnycy2v48h3we2jajyg7kyuvha9yfkd4qmxfrgywz3meyhqhdhmj8
```

7. Deposit BTC to the **new bridge address**

```sh
via token deposit --amount 10 --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 --bridge-address bcrt1pfk264lnycy2v48h3we2jajyg7kyuvha9yfkd4qmxfrgywz3meyhqhdhmj8
```

8. Withdraw 1 BTC.
9. The verifier and coordinator process the batch and sequencer finalize the batch.
