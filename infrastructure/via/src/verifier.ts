import * as utils from 'utils';
import { Command } from 'commander';

const verifyBatch = async (batchRefRevealTxId: string) => {
    process.chdir(`${process.env.VIA_HOME}`);
    await utils.spawn(`cargo run --example verify_batch -- ${batchRefRevealTxId}`);
}

export const command = new Command('verifier').description('verifier network mock');

command.command('verify-batch')
    .description('verify batch by batch da ref reveal tx id')
    .option('--batch-ref-reveal-tx-id <batchRefRevealTxId>', 'reveal tx id for the l1 batch to verify')
    .action((cmd: Command) => verifyBatch(cmd.batchRefRevealTxId));

command.command('deposit').description('deposit BTC to l2').action(() => {
    console.log('Depositing BTC...');
});