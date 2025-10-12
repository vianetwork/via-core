import { Command } from 'commander';
import * as utils from 'utils';
import * as path from 'path';
import fs from 'fs';
import * as env from './env';
import * as clean from './clean';
import * as db from './database';

export async function server(rebuildTree: boolean, uring: boolean, components?: string, useNodeFramework?: boolean) {
    let options = '';
    if (uring) options += '--features=rocksdb/io-uring';
    if (rebuildTree || components || useNodeFramework) options += ' --';
    if (components) options += ` --components=${components}`;
    await utils.spawn(`cargo run --bin via_server --release ${options}`);
}

export async function externalNode(reinit: boolean = false, args: string[]) {
    if (process.env.VIA_ENV != 'via_ext_node') {
        console.warn(`WARNING: using ${process.env.VIA_ENV} environment for external node`);
        console.warn('If this is a mistake, set $VIA_ENV to "via_ext_node" or other environment');
    }

    // Set proper environment variables for external node.
    process.env.EN_BOOTLOADER_HASH = process.env.CHAIN_STATE_KEEPER_BOOTLOADER_HASH;
    process.env.EN_DEFAULT_AA_HASH = process.env.CHAIN_STATE_KEEPER_DEFAULT_AA_HASH;

    // On --reinit we want to reset RocksDB and Postgres before we start.
    if (reinit) {
        await utils.confirmAction();
        await db.reset({ core: true, prover: false, verifier: false, indexer: false });
        clean.clean(path.dirname(process.env.EN_MERKLE_TREE_PATH!));
    }

    await utils.spawn(`cargo run  --bin via_external_node --release -- ${args.join(' ')}`);
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
    // Note that all the chains have the same chainId at genesis. It will be changed
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

export const enCommand = new Command('external-node')
    .description('start via external node')
    .option('--reinit', 'reset postgres and rocksdb before starting')
    .action(async (cmd: Command) => {
        await externalNode(cmd.reinit, cmd.args);
    });
