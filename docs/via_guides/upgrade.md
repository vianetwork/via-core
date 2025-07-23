# VIA upgrade flow

The VIA upgrade flow is split in 2 main parts:
1. The upgrade proposal inscription.
2. The Governance proposal execution inscription.

## How to create an upgrade proposal?
Creating a proposal can be done by any P2PKH wallet. The process should inscribe the data using the btc client inscriber ProtocolUpgradeProposal inscription.

1. Build the system contracts, `cd contracts` && `yarn sc build` && `cd ..`
2. Create a new upgrade config, `cd infrastructure/via-protocol-upgrade` and `yarn start upgrades create via-network --protocol-version <new-version>`.
3. Publish the new system contract for <version>
```sh
yarn start system-contracts publish \
    --private-key <l2-private-key> \
    --l2rpc http://0.0.0.0:3050 \
    --environment devnet-2 \
    --new-protocol-version <version> \
    --recursion-scheduler-level-vk-hash <hash> \
    --bootloader \
    --default-aa \
    --system-contracts
```

4. The previous cmd created a new upgrade file at this location `etc/upgrades/1742370950-via-network` with the new system contracts we are going to deploy.
5. When all the l1_batches are processed execute the next cmd to create an upgrade proposal.
```sh
yarn start l2-transaction upgrade-system-contracts --environment devnet-2 --private-key <l1-private-key>
```

## How to execute an upgrade proposal?
Use the VIA CLI to create a multisig transaction that execute the proposal stored in `txid`.

For this example we will use those wallets on regtest:

```json
{
  "mnemonic": "what same drill eight camp market ill month inform urge february duck",
  "privateKey": "cQnW8oDqEME4gxJHC4MC9HvJECcF7Ju8oanWdjWLGxDbkfWo7vZa",
  "address": "bcrt1q08v0vm5w3rftefqutgtwlyslhy35ms8ftuay80",
  "publicKey": "025b3c069378f860cc4dae864a491e0cd33cc559b9f82fc856d4dcc74d3d763241",
  "network": "regtest"
}

{
  "mnemonic": "beef rigid brick input hint coyote gap earn march affair tissue major",
  "privateKey": "cVJYEHTzmfdRPoX6fL3vRnZVmqy4D1sWaT5WL9U25oZhQktoeHgo",
  "address": "bcrt1q50xmdcwlmt8qhwczxptaq2h5cn3zchcrvqd35v",
  "publicKey": "03c2871e18d4fb503ead90461da747b40df5e28da0fd3e067f3731f1a28da60ddf",
  "network": "regtest"
}

{
  "mnemonic": "fragile loyal suffer fashion about insane expose body siege brother control action",
  "privateKey": "cPytijNj4VAnczJD5a21bboiPavYDCLmM9AW6cmjUxrDUYnJXQaf",
  "address": "bcrt1q9l2wcyaquvvxuzxenae75q24yx4uhzhq3mrlfe",
  "publicKey": "03445c516584d751643442bea558be2c5d77a6c3377e86fe6e78e3b992dd68ac62",
  "network": "regtest"
}
```

1. Compute the multisig wallet with 2 signers as minimum.
```sh,
via multisig compute-multisig \
--pubkeys 025b3c069378f860cc4dae864a491e0cd33cc559b9f82fc856d4dcc74d3d763241,03c2871e18d4fb503ead90461da747b40df5e28da0fd3e067f3731f1a28da60ddf,03445c516584d751643442bea558be2c5d77a6c3377e86fe6e78e3b992dd68ac62 \
--minimumSigners 2
```
A new file is created `upgrade_tx_exec.json`

2. Create an unsigned upgrade transaction. Make sure to select the input you want to use and the `upgradeProposalTxId`.
To fetch the UTXOs from regtest use this cmd
```sh
curl --user rpcuser:rpcpassword \
  --data-binary '{
    "jsonrpc": "1.0",
    "id": "scan_utxo",
    "method": "scantxoutset",
    "params": [
      "start", 
      [
        { "desc": "addr(bcrt1q92gkfme6k9dkpagrkwt76etkaq29hvf02w5m38f6shs4ddpw7hzqp347zm)", "range": 1000 }
      ]
    ]
  }' \
  -H 'content-type: text/plain;' \
  http://127.0.0.1:18443/
```

```sh
via multisig create-upgrade-tx \
--inputTxId <tx_id> \
--inputVout <vout> \
--inputAmount <amount> \
--upgradeProposalTxId <upgradeProposalTxId> \
--fee 500
```

3. Sign the transaction using the signer-1 `Privatekey`.
```sh
via multisig sign-upgrade-tx --privateKey cQnW8oDqEME4gxJHC4MC9HvJECcF7Ju8oanWdjWLGxDbkfWo7vZa
```
After signing the tx send the `upgrade_tx_exec.json` to signer-2


4. Sign the transaction using the signer-2 `Privatekey`.
```sh
via multisig sign-upgrade-tx --privateKey cVJYEHTzmfdRPoX6fL3vRnZVmqy4D1sWaT5WL9U25oZhQktoeHgo
```

5. The signer-2 finalize the transaction
```sh
via multisig finalize-upgrade-tx
```

6. The signer-2 broadcast the transaction
```sh
via multisig broadcast-tx \
--rpcUrl http://0.0.0.0:18443 \
--rpcUser rpcuser\
--rpcPass rpcpassword
```
