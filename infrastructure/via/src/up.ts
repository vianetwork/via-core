import { Command } from 'commander';
import * as utils from 'utils';
import fs from 'fs';
import { VIA_DOCKER_COMPOSE } from './docker';

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
    .option('--run-observability', 'whether to run observability stack')
    .action(async (cmd) => {
        await up(cmd.dockerFile);
    });
