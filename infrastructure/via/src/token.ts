import { Command } from 'commander';
import { Wallet, Provider, Contract } from 'zksync-ethers';
import { ethers } from 'ethers';
import * as utils from 'utils';

const DEFAULT_DEPOSITOR_PRIVATE_KEY = 'cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R';
const DEFAULT_NETWORK = 'regtest';
const DEFAULT_RPC_URL = 'http://0.0.0.0:18443';
const DEFAULT_RPC_USERNAME = 'rpcuser';
const DEFAULT_RPC_PASSWORD = 'rpcpassword';

// 0x36615Cf349d7F6344891B1e7CA7C72883F5dc049
const DEFAULT_L2_PRIVATE_KEY = '0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110';
const DEFAULT_L2_RPC_URL = 'http://0.0.0.0:3050';
const L2_BASE_TOKEN = '0x000000000000000000000000000000000000800a';

async function deposit(
    amount: number,
    receiverL2Address: string,
    senderPrivateKey: string,
    network: String,
    rcpUrl: string,
    rpcUsername: string,
    rpcPassword: string
) {
    if (isNaN(amount)) {
        console.error('Error: Invalid deposit amount. Please provide a valid number.');
        return;
    }
    process.chdir(`${process.env.VIA_HOME}`);
    await utils.spawn(
        `cargo run --example deposit -- ${amount} ${receiverL2Address} ${senderPrivateKey} ${network} ${rcpUrl} ${rpcUsername} ${rpcPassword}`
    );
}

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

export const command = new Command('token').description('Bridge BTC L2<>L1');
command
    .command('deposit')
    .description('deposit BTC to l2')
    .requiredOption('--amount <amount>', 'amount of BTC to deposit', parseFloat)
    .requiredOption('--receiver-l2-address <receiverL2Address>', 'receiver l2 address')
    .option('--sender-private-key <senderPrivateKey>', 'sender private key', DEFAULT_DEPOSITOR_PRIVATE_KEY)
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .option('--rpc-url <rcpUrl>', 'RPC URL', DEFAULT_RPC_URL)
    .option('--rpc-username <rcpUsername>', 'RPC username', DEFAULT_RPC_USERNAME)
    .option('--rpc-password <rpcPassword>', 'RPC password', DEFAULT_RPC_PASSWORD)
    .action((cmd: Command) =>
        deposit(
            cmd.amount,
            cmd.receiverL2Address,
            cmd.senderPrivateKey,
            cmd.network,
            cmd.rpcUrl,
            cmd.rpcUsername,
            cmd.rpcPassword
        )
    );

command
    .command('withdraw')
    .description('withdraw BTC to l1')
    .requiredOption('--amount <amount>', 'amount of BTC to withdraw', parseFloat)
    .requiredOption('--receiver-l1-address <receiverL1Address>', 'receiver l1 address')
    .option('--user-private-key <userPrivateKey>', 'user private key', DEFAULT_L2_PRIVATE_KEY)
    .option('--rpc-url <rcpUrl>', 'RPC URL', DEFAULT_L2_RPC_URL)
    .action((cmd: Command) => withdraw(cmd.amount, cmd.receiverL1Address, cmd.userPrivateKey, cmd.rpcUrl));
