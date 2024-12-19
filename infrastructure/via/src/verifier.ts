import { Command } from 'commander';
import * as utils from 'utils';
import * as env from './env';

export async function verifier(isCoordinator: boolean) {
    let options = '';
    console.log(isCoordinator);
    if (isCoordinator) {
        options += '--coordinator';
    }
    await utils.spawn(`cargo run --bin via_verifier -- ${options}`);
}

export const verifierCommand = new Command('verifier')
    .description('start via verifier node')
    .option('--coordinator', 'start the verifier node as coordinator', false)
    .action(async (cmd: Command) => {
        cmd.chainName ? env.reload(cmd.chainName) : env.load();
        await verifier(cmd.coordinator);
    });
