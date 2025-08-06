import { Command } from 'commander';
import { deposit, getWallet, withdraw } from './token';
import { generateBitcoinWallet } from './helpers';
import {
    DEFAULT_DEPOSITOR_PRIVATE_KEY,
    DEFAULT_NETWORK,
    DEFAULT_L1_RPC_URL,
    DEFAULT_RPC_USERNAME,
    DEFAULT_RPC_PASSWORD,
    DEFAULT_L2_PRIVATE_KEY,
    DEFAULT_L2_RPC_URL
} from './constants';

export async function depositMany(
    count: number,
    amount: number,
    receiverL2Address: string,
    senderPrivateKey: string,
    network: String,
    l1RpcUrl: string,
    l2RpcUrl: string,
    rpcUsername: string,
    rpcPassword: string,
    bridgeAddress: string
) {
    const amountPerDeposit = Number(amount) / count;
    for (let i = 0; i < count; i++) {
        await deposit(
            amountPerDeposit,
            receiverL2Address,
            senderPrivateKey,
            network,
            l1RpcUrl,
            l2RpcUrl,
            rpcUsername,
            rpcPassword,
            bridgeAddress
        );

        console.log(`\n\nDeposit ${i + 1}/${count}`);
    }
}

export async function withdrawMany(
    count: number,
    network: string,
    amount: string,
    userL2PrivateKey: string,
    rpcUrl: string
) {
    const wallet = getWallet(rpcUrl, userL2PrivateKey);
    const nonce = await wallet.getNonce();
    const amountPerWithdraw = Number(amount) / count;

    for (let i = 0; i < count; i++) {
        const wallet = await generateBitcoinWallet(network);
        if (wallet) {
            await withdraw(String(amountPerWithdraw), wallet.address, userL2PrivateKey, rpcUrl, nonce + i);
            console.log('\n\nReceiver: ', wallet.address);
            console.log(`Withdrawal ${i + 1}/${count}`);
        }
    }
}

export const command = new Command('debug').description('Debug cmds used for testing');

command
    .command('deposit-many')
    .description('deposit BTC to l2')
    .requiredOption('--amount <amount>', 'amount of BTC to deposit', parseFloat)
    .requiredOption('--receiver-l2-address <receiverL2Address>', 'receiver l2 address')
    .requiredOption('--bridge-address <bridgeAddress>', 'The bridge address')
    .option('--sender-private-key <senderPrivateKey>', 'sender private key', DEFAULT_DEPOSITOR_PRIVATE_KEY)
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .option('--l1-rpc-url <l1RcpUrl>', 'RPC URL', DEFAULT_L1_RPC_URL)
    .option('--l2-rpc-url <l2RcpUrl>', 'RPC URL', DEFAULT_L2_RPC_URL)
    .option('--rpc-username <rcpUsername>', 'RPC username', DEFAULT_RPC_USERNAME)
    .option('--rpc-password <rpcPassword>', 'RPC password', DEFAULT_RPC_PASSWORD)
    .option('--count <count>', 'Number of deposits', '100')
    .action((cmd: Command) =>
        depositMany(
            cmd.count,
            cmd.amount,
            cmd.receiverL2Address,
            cmd.senderPrivateKey,
            cmd.network,
            cmd.l1RpcUrl,
            cmd.l2RpcUrl,
            cmd.rpcUsername,
            cmd.rpcPassword,
            cmd.bridgeAddress
        )
    );

command
    .command('withdraw-many')
    .description('TEST: withdraw many BTC to l1')
    .requiredOption('--amount <amount>', 'amount of BTC to withdraw')
    .option('--count <count>', 'Number of request withdrawal', '10')
    .option('--user-private-key <userPrivateKey>', 'user private key', DEFAULT_L2_PRIVATE_KEY)
    .option('--rpc-url <rcpUrl>', 'RPC URL', DEFAULT_L2_RPC_URL)
    .option('--network <network>', 'The L1 network', 'regtest')
    .action((cmd: Command) => withdrawMany(Number(cmd.count), cmd.network, cmd.amount, cmd.userPrivateKey, cmd.rpcUrl));
