import { Command } from 'commander';
import * as utils from 'utils';
import * as env from './env';
import { updateBootstrapTxidsEnv } from './bootstrap';
import { updateEnvVariable } from './helpers';
import path from 'path';
import { load_from_file } from './env';

export async function verifier() {
    await updateBootstrapTxidsEnv();

    console.log(`Starting verifier node...`);

    env.load_from_file();

    await utils.spawn(`cargo run --bin via_verifier`);
}

export const verifierCommand = new Command('verifier')
    .description('start via verifier node')
    .action(async (cmd: Command) => {
        cmd.chainName ? env.reload(cmd.chainName) : env.load();
        await env.load();
        env.get(true);
        await verifier();
    });
