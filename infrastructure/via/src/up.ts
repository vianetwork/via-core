import { Command } from 'commander';
import * as utils from 'utils';
import { VIA_DOCKER_COMPOSE } from './docker';

export function validateUpOptions(options: { runObservability?: boolean }): void {
    if (options.runObservability) {
        throw new Error('--run-observability is unsupported by the Via container workflow.');
    }
}

export async function up(profile?: string, composeFile?: string, envFilePath?: string) {
    if (composeFile) {
        const envFile = envFilePath ? `--env-file ${envFilePath}` : '';
        let profileArg = '';
        if (profile == 'reorg') {
            profileArg = '--profile reorg';
        }
        await utils.spawn(`docker compose ${envFile} -f ${composeFile} ${profileArg} up -d`);
    } else {
        await utils.spawn('docker compose up -d');
    }
}

export const command = new Command('up')
    .description('start development containers')
    .option('--docker-file <dockerFile>', 'path to a custom docker file', VIA_DOCKER_COMPOSE)
    .option('--run-observability', 'unsupported by the Via container workflow; rejected when requested')
    .action(async (cmd) => {
        validateUpOptions(cmd);
        await up(cmd.dockerFile);
    });
