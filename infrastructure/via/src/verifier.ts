import { Command } from 'commander';
import * as utils from 'utils';
import * as env from './env';
import { updateBootstrapTxidsEnv } from './bootstrap';

const DEFAULT_NETWORK = 'regtest';

export async function verifier(network: string) {
    await updateBootstrapTxidsEnv(network);

    console.log(`Starting verifier node...`);

    env.load_from_file();

    await utils.spawn(`cargo run --bin via_verifier --release`);
}

export const verifierCommand = new Command('verifier')
    .description('start via verifier node')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .action(async (cmd: Command) => {
        cmd.chainName ? env.reload(cmd.chainName) : env.load();
        env.get(true);
        await verifier(cmd.network);
    });
