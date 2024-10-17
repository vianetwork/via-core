import { Command } from 'commander';
import * as utils from 'utils';
import { clean } from './clean';
import fs from 'fs';
import * as path from 'path';
import * as db from './database';
import * as env from './env';

export async function server(rebuildTree: boolean, uring: boolean, components?: string, useNodeFramework?: boolean) {
    let options = '';
    if (uring) {
        options += '--features=rocksdb/io-uring';
    }
    if (rebuildTree || components || useNodeFramework) {
        options += ' --';
    }
    if (components) {
        options += ` --components=${components}`;
    }
    await utils.spawn(`cargo run --bin via_server ${options}`);
}

async function create_genesis(cmd: string) {
    await utils.confirmAction();
    await utils.spawn(`${cmd} | tee genesis.log`);

    const date = new Date();
    const [year, month, day, hour, minute, second] = [
        date.getFullYear(),
        date.getMonth(),
        date.getDate(),
        date.getHours(),
        date.getMinutes(),
        date.getSeconds()
    ];
    const label = `${process.env.VIA_ENV}-Genesis_gen-${year}-${month}-${day}-${hour}${minute}${second}`;
    fs.mkdirSync(`logs/${label}`, { recursive: true });
    fs.copyFileSync('genesis.log', `logs/${label}/genesis.log`);
}

export async function genesisFromSources() {
    // Note that that all the chains have the same chainId at genesis. It will be changed
    // via an upgrade transaction during the registration of the chain.
    await create_genesis('cargo run --bin via_server --release -- --genesis');
}

export async function genesisFromBinary() {
    await create_genesis('via_server --genesis');
}

export const serverCommand = new Command('server')
    .description('start via server')
    .option('--genesis', 'generate genesis data via server')
    .option('--uring', 'enables uring support for RocksDB')
    .option('--components <components>', 'comma-separated list of components to run')
    .option('--chain-name <chain-name>', 'environment name')
    .action(async (cmd: Command) => {
        cmd.chainName ? env.reload(cmd.chainName) : env.load();
        if (cmd.genesis) {
            await genesisFromSources();
        } else {
            await server(cmd.rebuildTree, cmd.uring, cmd.components, cmd.useNodeFramework);
        }
    });
