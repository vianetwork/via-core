# Via Playground 🌐

This project demonstrates how to interact with Via network.

Try running some of the following tasks:

0. Install package using `yarn`

1. Duplicate the `example.env` and create a `.env` file

```
⚠️⚠️⚠️
If you want to deploy a contract on the deployed version of our devnet in GCP,
please ask the Devs to provide the deployed version RPC URL
and replace it in `hardhat.config.ts`.
⚠️⚠️⚠️
```

2. Run `npx hardhat compile`

3. Run the following command to bridge BTC to L2:

   ```shell
   via token deposit \
    --amount 100 \
    --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049 \
    --sender-private-key <sender_private_key> \
    --rpc-url <regtest_url> \
    --rpc-username <rcp_username> \
    --rpc-password <rpc_password>

   # if you are using local setup you don't need provide rpc related options and private key.
   ```

4. Deploy a Crowdfunding contract with the desired funding goal:
   ```shell
   npx hardhat deploy --amount 10000
   ```
5. Print the Crowdfunding funding goal:
   ```shell
   npx hardhat stats
   ```
6. Contribute to the Crowdfunding:
   ```shell
   npx hardhat contribute --amount 10000
   ```
7. Withdraw funds from the Crowdfunding to the owner:
   ```shell
   npx hardhat withdraw
   ```
