import * as utils from 'utils';
import { Command } from 'commander';

async function verifyBatch(batchRefRevealTxId: string) {
    process.chdir(`${process.env.VIA_HOME}`);
    await utils.spawn(`cargo run --example verify_batch -- ${batchRefRevealTxId}`);
}

async function deposit(amount: number) {
    if (isNaN(amount)) {
        console.error('Error: Invalid deposit amount. Please provide a valid number.');
        return;
    }
    process.chdir(`${process.env.VIA_HOME}`);
    await utils.spawn(`cargo run --example deposit -- ${amount}`);
}

export const command = new Command('verifier').description('verifier network mock');

command.command('verify-batch')
    .description('verify batch by batch da ref reveal tx id')
    .option('--batch-ref-reveal-tx-id <batchRefRevealTxId>', 'reveal tx id for the l1 batch to verify')
    .action((cmd: Command) => verifyBatch(cmd.batchRefRevealTxId));

command.command('deposit')
    .description('deposit BTC to l2')
    .option('--amount <amount>', 'amount of BTC to deposit', parseFloat)
    .action((cmd: Command) => deposit(cmd.amount));