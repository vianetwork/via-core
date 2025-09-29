import { Command } from 'commander';
import * as utils from 'utils';
import fs from 'fs';
import { VIA_DOCKER_COMPOSE } from './docker';

// Make sure that the volumes exists before starting the containers.
export function createVolumes(profile: string) {
    fs.mkdirSync(`${process.env.VIA_HOME}/volumes/postgres`, {
        recursive: true
    });
    fs.mkdirSync(`${process.env.VIA_HOME}/volumes/bitcoin`, {
        recursive: true
    });
    fs.mkdirSync(`${process.env.VIA_HOME}/volumes/celestia`, {
        recursive: true
    });

    fs.mkdirSync(`${process.env.VIA_HOME}/volumes/btc-explorer/data`, {
        recursive: true
    });

    fs.mkdirSync(`${process.env.VIA_HOME}/volumes/btc-explorer/mysql`, {
        recursive: true
    });
    fs.mkdirSync(`${process.env.VIA_HOME}/volumes/celestia-keys`, {
        recursive: true
    });

    if (profile === 'reorg') {
        fs.mkdirSync(`${process.env.VIA_HOME}/volumes/bitcoin2`, {
            recursive: true
        });
    }
}

export async function up(profile: string, composeFile?: string, envFilePath?: string) {
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
