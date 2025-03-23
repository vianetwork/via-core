import { Command } from 'commander';
import * as utils from 'utils';
import fs from 'fs';

// installs all dependencies
export async function yarn() {
    await utils.spawn('run_retried yarn install --frozen-lockfile');
}

export async function catLogs(exitCode?: number) {
    utils.allowFailSync(() => {
        console.log('\nSERVER LOGS:\n', fs.readFileSync('server.log').toString());
        console.log('\nPROVER LOGS:\n', fs.readFileSync('dummy_verifier.log').toString());
    });
    if (exitCode !== undefined) {
        process.exit(exitCode);
    }
}

export async function snapshots_creator() {
    process.chdir(`${process.env.VIA_HOME}`);
    let logLevel = 'RUST_LOG=snapshots_creator=debug';
    await utils.spawn(`${logLevel} cargo run --bin snapshots_creator --release`);
}

export const command = new Command('run').description('Run miscellaneous applications');

command.command('yarn').description('Install all JS dependencies').action(yarn);
command.command('cat-logs [exit_code]').description('Print server logs').action(catLogs);

command.command('snapshots-creator').action(snapshots_creator);
