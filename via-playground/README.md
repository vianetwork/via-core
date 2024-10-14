# Sample Hardhat Project

This project demonstrates a basic Hardhat use case. It comes with a sample contract, a test for that contract, and a
Hardhat Ignition module that deploys that contract.

Try running some of the following tasks:

0. Install package using `yarn && npx hardhat compile`
1. Duplicate the `example.env` and create a `.env` file
2. Install web3 cli https://github.com/mm-zk/web3
3. Run the following command to bridge BTC to L2

```shell
via verifier deposit --amount 100 --receiver-l2-address 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049
```

4. Deploy a Crowdfunding contract with the amount of funding goals

```shell
npx hardhat deploy --network via --amount 10000
```

5. Print Crowdfunding funding goal

```shell
npx hardhat stats --network via
```

6. Contribute to the Crowdfunding

```shell
npx hardhat contribute --network via --amount 10000
```

7. Contribute to the Crowdfunding

```shell
npx hardhat withdraw --network via
```
