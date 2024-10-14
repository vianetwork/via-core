import { Wallet } from 'zksync-ethers';
import { Deployer } from '@matterlabs/hardhat-zksync-deploy';
import dotenv from 'dotenv';
import * as fs from 'fs';
import { ethers } from 'ethers';

import '@matterlabs/hardhat-zksync-node/dist/type-extensions';
import '@matterlabs/hardhat-zksync-verify/dist/src/type-extensions';

// Load env file
dotenv.config();

import { Provider } from 'zksync-ethers';

export const getProvider = (rpcUrl: string, name: string) => {
    if (!rpcUrl)
        throw `⛔️ RPC URL wasn't found in "${name}"! Please add a "url" field to the network config in hardhat.config.ts`;

    // Initialize ZKsync Provider
    const provider = new Provider(rpcUrl);

    return provider;
};

export const getWallet = (provider: any, privateKey?: string) => {
    if (!privateKey) {
        // Get wallet private key from .env file
        if (!process.env.PK) throw "⛔️ Wallet private key wasn't found in .env file!";
    }

    // Initialize ZKsync Wallet
    const wallet = new Wallet(privateKey ?? process.env.PK!, provider);

    return wallet;
};

export const verifyEnoughBalance = async (wallet: Wallet, amount: bigint) => {
    // Check if the wallet has enough balance
    const balance = await wallet.getBalance();
    if (balance < amount)
        throw `⛔️ Wallet balance is too low! Required ${ethers.formatUnits(amount, 8)} BTC, but current ${
            wallet.address
        } balance is ${ethers.formatUnits(balance, 8)} BTC`;
};

type DeployContractOptions = {
    /**
     * If true, the deployment process will not print any logs
     */
    silent?: boolean;
    /**
     * If true, the contract will not be verified on Block Explorer
     */
    noVerify?: boolean;
    /**
     * If specified, the contract will be deployed using this wallet
     */
    wallet?: Wallet;
};
export const deployContract = async (
    hre: any,
    contractArtifactName: string,
    constructorArguments?: any[],
    options?: DeployContractOptions
) => {
    const log = (message: string) => {
        if (!options?.silent) console.log(message);
    };

    log(`\nStarting deployment process of "${contractArtifactName}"...`);
    const provider = getProvider(hre.network.config.url, hre.network.name);
    const wallet = options?.wallet ?? getWallet(provider);
    const deployer = new Deployer(hre, wallet);
    const artifact = await deployer.loadArtifact(contractArtifactName).catch((error) => {
        if (error?.message?.includes(`Artifact for contract "${contractArtifactName}" not found.`)) {
            console.error(error.message);
            throw `⛔️ Please make sure you have compiled your contracts or specified the correct contract name!`;
        } else {
            throw error;
        }
    });

    // Estimate contract deployment fee
    const deploymentFee = await deployer.estimateDeployFee(artifact, constructorArguments || []);
    log(`Estimated deployment cost: ${ethers.formatUnits(deploymentFee, 8)} BTC`);

    // Check if the wallet has enough balance
    await verifyEnoughBalance(wallet, deploymentFee);

    // Deploy the contract to ZKsync
    const contract = await deployer.deploy(artifact, constructorArguments, undefined, {
        // maxFeePerBlobGas: 1500,
        // maxFeePerGas: 20000
    });
    const address = await contract.getAddress();
    const constructorArgs = contract.interface.encodeDeploy(constructorArguments);
    const fullContractSource = `${artifact.sourceName}:${artifact.contractName}`;

    // Display contract deployment info
    log(`\n"${artifact.contractName}" was successfully deployed:`);
    log(` - Contract address: ${address}`);
    log(` - Contract source: ${fullContractSource}`);
    log(` - Encoded constructor arguments: ${constructorArgs}\n`);

    fs.writeFileSync(
        './config.json',
        JSON.stringify({
            contract: address,
            source: fullContractSource
        })
    );

    return contract;
};
