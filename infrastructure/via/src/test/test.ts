import chalk from 'chalk';
import { Command } from 'commander';
import * as utils from 'utils';
import * as db from '../database';

export async function rust(options: string[]) {
    await db.resetTest({ core: true, prover: false, verifier: false });

    let result = await utils.exec('cargo install --list');
    let test_runner = 'cargo nextest run';
    if (!result.stdout.includes('cargo-nextest')) {
        console.warn(
            chalk.bold.red(
                `cargo-nextest is missing, please run "cargo install cargo-nextest". Falling back to "cargo test".`
            )
        );
        test_runner = 'cargo test';
    }

    // Quote options containing wildcards to prevent shell expansion
    options = options.map((opt) => {
        if (opt.includes('*') || opt.includes('?')) {
            return `'${opt}'`;
        }
        return opt;
    });

    let cmd = `${test_runner} --release ${options.join(' ')}`;
    console.log(`running unit tests with '${cmd}'`);

    await utils.spawn(cmd);
}

export const command = new Command('test').description('run test suites');

command
    .command('rust [command...]')
    .allowUnknownOption()
    .description(
        "run unit-tests. the default is running all tests in all rust bins and libs. accepts optional arbitrary cargo test flags. use `-p 'via_*'` to run all unit tests for VIA crates."
    )
    .action(async (args: string[]) => {
        await rust(args);
    });
