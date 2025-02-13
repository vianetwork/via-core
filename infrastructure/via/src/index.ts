#!/usr/bin/env node

import { program, Command } from 'commander';
import { spawnSync } from 'child_process';
import { serverCommand as server } from './server';
import { command as up } from './up';
import { command as down } from './down';
import { command as completion } from './completion';
import { initCommand } from './init';
import { command as run } from './run';
import { command as docker } from './docker';
import { command as config } from './config';
import { command as clean } from './clean';
import { command as db } from './database';
import * as env from './env';
import { command as transactions } from './transactions';
import { command as bootstrap } from './bootstrap';
import { verifierCommand as verifier } from './verifier';
import { command as celestia } from './celestia';
import { command as btc_explorer } from './btc_explorer';
import { command as token } from './token';
import { command as test } from './test/test';

const COMMANDS = [
    server,
    up,
    down,
    db,
    initCommand,
    run,
    docker,
    config,
    clean,
    env.command,
    transactions,
    bootstrap,
    verifier,
    celestia,
    btc_explorer,
    token,
    test,
    completion(program as Command)
];

async function main() {
    const cwd = process.cwd();
    const VIA_HOME = process.env.VIA_HOME;

    if (!VIA_HOME) {
        throw new Error('Please set $VIA_HOME to the root of Via repo!');
    } else {
        process.chdir(VIA_HOME);
    }

    env.load();

    program.version('0.1.0').name('via').description('via workflow tools');

    for (const command of COMMANDS) {
        program.addCommand(command);
    }

    // f command is special-cased because it is necessary
    // for it to run from $PWD and not from $VIA_HOME
    program
        .command('f <command...>')
        .allowUnknownOption()
        .action((command: string[]) => {
            process.chdir(cwd);
            const result = spawnSync(command[0], command.slice(1), { stdio: 'inherit' });
            if (result.error) {
                throw result.error;
            }
            process.exitCode = result.status || undefined;
        });

    await program.parseAsync(process.argv);
}

main().catch((err: Error) => {
    console.error('Error:', err.message || err);
    process.exitCode = 1;
});
