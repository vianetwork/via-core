import { Command } from 'commander';
import * as utils from 'utils';
import * as env from './env';
import { updateBootstrapTxidsEnv } from './bootstrap';
import { updateEnvVariable } from './helpers';
import path from 'path';
import { load_from_file } from './env';

export async function verifier() {
    let options = '';
    await updateBootstrapTxidsEnv();

    const envFilePath = path.join(process.env.VIA_HOME!, `etc/env/target/${process.env.VIA_ENV}.env`);
    await updateEnvVariable(envFilePath, 'VIA_BTC_WATCH_ACTOR_ROLE', 'Verifier');

    console.log(`Starting verifier node...`);

    env.load_from_file();

    await console.log(`Starting verifier node...`);

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
