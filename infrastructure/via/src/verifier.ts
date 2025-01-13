import { Command } from 'commander';
import * as utils from 'utils';
import * as env from './env';

export async function verifier() {
    let options = '';
    await utils.spawn(`cargo run --bin via_verifier`);
}

export const verifierCommand = new Command('verifier')
    .description('start via verifier node')
    .action(async (cmd: Command) => {
        cmd.chainName ? env.reload(cmd.chainName) : env.load();
        await verifier();
    });
