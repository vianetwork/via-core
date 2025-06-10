import { Command } from 'commander';
import * as utils from 'utils';
import * as env from './env';
import { updateBootstrapTxidsEnv } from './bootstrap';

export async function indexer(network: string) {
    await updateBootstrapTxidsEnv(network);

    console.log(`Starting l1 indexer...`);
    env.load_from_file();

    await utils.spawn(`cargo run --bin via_indexer_bin`);
}

export const indexerCommand = new Command('indexer')
    .description('start via indexer node')
    .option('--network <network>', 'network', 'regtest')
    .action(async (cmd: Command) => {
        cmd.chainName ? env.reload(cmd.chainName) : env.load();
        env.get(true);
        await indexer(cmd.network);
    });
