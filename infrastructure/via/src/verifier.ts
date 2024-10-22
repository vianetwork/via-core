import * as utils from 'utils';
import { Command, option } from 'commander';

const DEFAULT_DEPOSITOR_PRIVATE_KEY = 'cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R';
const DEFAULT_NETWORK = 'regtest';
const DEFAULT_RPC_URL = "http://0.0.0.0:18443";
const DEFAULT_RPC_USERNAME = "rpcuser";
const DEFAULT_RPC_PASSWORD = "rpcpassword";

async function verifyBatch(batchProofRefRevealTxId: string) {
    process.chdir(`${process.env.VIA_HOME}`);
    await utils.spawn(`cargo run --example verify_batch -- ${batchProofRefRevealTxId}`);
}

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
    await utils.spawn(`cargo run --example deposit -- ${amount} ${receiverL2Address} ${senderPrivateKey} ${network} ${rcpUrl} ${rpcUsername} ${rpcPassword}`);
}

export const command = new Command('verifier').description('verifier network mock');

command
    .command('verify-batch')
    .description('verify batch by batch da ref reveal tx id')
    .requiredOption(
        '--batch-proof-ref-reveal-tx-id <batchProofRefRevealTxId>',
        'reveal tx id for the l1 batch proof to verify'
    )
    .action((cmd: Command) => verifyBatch(cmd.batchProofRefRevealTxId));

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
    .action((cmd: Command) => deposit(cmd.amount, cmd.receiverL2Address, cmd.senderPrivateKey, cmd.network, cmd.rpcUrl, cmd.rpcUsername, cmd.rpcPassword));
