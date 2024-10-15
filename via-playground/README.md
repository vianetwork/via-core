# Via Playground

This project demonstrates how to interact with via network.

Try running some of the following tasks:

0. Install package using `yarn`
1. Duplicate the `example.env` and create a `.env` file
2. Run `npx hardhat compile`
3. Run the following command to bridge BTC to L2

```shell
via verifier deposit --amount 100 --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049
```

4. Deploy a Crowdfunding contract with the amount of funding goal

```shell
npx hardhat deploy --amount 10000
```

5. Print Crowdfunding funding goal

```shell
npx hardhat stats
```

6. Contribute to the Crowdfunding

```shell
npx hardhat contribute --amount 10000
```

7. Withdraw funds from the Crowdfunding to the owner

```shell
npx hardhat withdraw
```
