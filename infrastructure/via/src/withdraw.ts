import * as fs from 'fs';
import { Command } from 'commander';
import { Wallet, Provider, Contract } from 'zksync-ethers';
import { ethers } from 'ethers';

// 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049
const DEFAULT_L2_PRIVATE_KEY = '0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110';
const DEFAULT_L2_RPC_URL = 'http://0.0.0.0:3050';
const L2_BASE_TOKEN = '0x000000000000000000000000000000000000800a';

async function withdraw(amount: number, receiverL1Address: string, userL2PrivateKey: string, rpcUrl: string) {
    if (isNaN(amount)) {
        console.error('Error: Invalid withdraw amount. Please provide a valid number.');
        return;
    }

    const abi = [
        {
            inputs: [
                {
                    internalType: 'uint256',
                    name: '_account',
                    type: 'uint256'
                }
            ],
            name: 'balanceOf',
            outputs: [
                {
                    internalType: 'uint256',
                    name: '',
                    type: 'uint256'
                }
            ],
            stateMutability: 'view',
            type: 'function'
        },
        {
            inputs: [
                {
                    internalType: 'bytes',
                    name: '_l1Receiver',
                    type: 'bytes'
                }
            ],
            name: 'withdraw',
            outputs: [],
            stateMutability: 'payable',
            type: 'function'
        }
    ];

    const provider = new Provider(rpcUrl);
    const wallet = new Wallet(userL2PrivateKey, provider);
    const btcAddress = ethers.toUtf8Bytes(receiverL1Address);
    const contract = new Contract(L2_BASE_TOKEN, abi, wallet) as any;

    let balance = await contract.balanceOf(wallet.address);
    console.log('Balance before withdraw', ethers.formatUnits(balance, 8));
    const tx = await contract.connect(wallet).withdraw(btcAddress, { value: ethers.parseUnits(String(amount), 8) });
    await tx.wait();
    balance = await contract.balanceOf(wallet.address);
    console.log('Balance after withdraw', ethers.formatUnits(balance, 8));
}

export const command = new Command('token').description('Bridge BTC L2>L1');

command
    .command('withdraw')
    .description('withdraw BTC to l1')
    .requiredOption('--amount <amount>', 'amount of BTC to withdraw', parseFloat)
    .requiredOption('--receiver-l1-address <receiverL1Address>', 'receiver l1 address')
    .option('--user-private-key <userPrivateKey>', 'user private key', DEFAULT_L2_PRIVATE_KEY)
    .option('--rpc-url <rcpUrl>', 'RPC URL', DEFAULT_L2_RPC_URL)
    .action((cmd: Command) => withdraw(cmd.amount, cmd.receiverL1Address, cmd.userPrivateKey, cmd.rpcUrl));
