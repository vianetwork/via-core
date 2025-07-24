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
--rpcUser rpcuser \
--rpcPass rpcpassword
```

## Upgrade execution example

1. Start the Sequencer.
2. Deposit 1 BTC.
3. cd via-playground exec the following cmd, you should see ETH. as token symbol.
```sh
cd via-playground && source .env.example && npx hardhat balance --address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 && cd ..
```
4. Delete the submodule to `branch` key to use the `main` branch, this allows us to use the protocol version 26.
5. Build git submodule using
```sh
git submodule update --remote --recursive
```
6. Build the system contracts
```sh
cd contracts && yarn sc build && cd ..
```
7. Create a new upgrade config
```sh
cd infrastructure/via-protocol-upgrade && yarn start upgrades create via-network --protocol-version 0.26.0
```
8. Publish the new system contract for version 
```sh
yarn start system-contracts publish \
    --private-key 0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110 \
    --l2rpc http://0.0.0.0:3050 \
    --environment devnet-2 \
    --new-protocol-version 0.26.0 \
    --recursion-scheduler-level-vk-hash 0x14f97b81e54b35fe673d8708cc1a19e1ea5b5e348e12d31e39824ed4f42bbca2 \
    --bootloader \
    --default-aa \
    --system-contracts
```
The above cmd created a new upgrade file at this location etc/upgrades/1742370950-via-network with the new system contracts we are going to deploy. Wait the transactions to be processed on L2 and included in L1 batches before execute the next steps.
10. When all the l1_batches are processed execute the next cmd to send an upgrade inscription to the L1.
```sh
yarn start l2-transaction upgrade-system-contracts --environment devnet-2 --private-key cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R
```
11. Copy the `tx_id` of the proposal created in the previous step and follow this doc to create a multisig [GOV transaction](#How-to-execute-an-upgrade-proposal).

12. When the Gov tx is created and minted in a block, execute another deposit 1 BTC, this because a batch can not include only an upgrade transaction.
13. Deposit 1 BTC. 
14. Check the database, new protocol version should be 26, the last batch should be processed with the new bootloader
hash and version 26.
```sql
-- You should see that the last miniblocks where processed using the version 26
select protocol_version from miniblocks order by number DESC

-- The last batches (check number), should have different bootloader_code_hash and default_aa_code_hash.
select number, encode(bootloader_code_hash, 'hex'), encode(default_aa_code_hash, 'hex') from l1_batches order by number DESC
```
15. In another terminal starts the coordinator (by default version 26 ). You will notice that all the batches before the one includes the upgrade are processing with VK (verifying key version 25) and after upgrade VK-26

16. Start a verifier with the (change the version to 25 [const SEQUENCER_MINOR: ProtocolVersionId = ProtocolVersionId::Version26;](https://github.com/vianetwork/via-core/blob/22bbfd3e5dae6f01a5cbecc629a748798e66cd16/via_verifier/lib/via_verifier_types/src/protocol_version.rs#L6))
17. The coordinator will reject this verifier as the protocol version used is 25 and it requires 26
18. Start the verifier and restart it with version 26. The withdrawal is processed
19. Execute a withdrawal.
20. Done :)
