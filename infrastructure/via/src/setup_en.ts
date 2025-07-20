import { Command } from 'commander';
import { prompt } from 'enquirer';
import chalk from 'chalk';
import { compileConfig } from './config';
import fs from 'fs';
import path from 'path';
import { set as setEnv, load_from_file } from './env';
import { setup as setupDb } from './database';
import * as utils from 'utils';
import { updateBootstrapTxidsEnv } from './bootstrap';

enum Environment {
    Mainnet = 'mainnet',
    Testnet = 'testnet',
    Local = 'local'
}

enum DataRetentionDuration {
    Hour = 'hour',
    Day = 'day',
    Week = 'week',
    Month = 'month',
    Year = 'year',
    Forever = 'forever'
}

async function selectDataRetentionDurationHours(): Promise<number | null> {
    const question = {
        type: 'select',
        name: 'retention',
        message: 'Select how long do you want to keep newest transactions data',
        choices: [
            { name: DataRetentionDuration.Hour, message: 'Hour', value: 1 },
            { name: DataRetentionDuration.Day, message: 'Day', value: 24 },
            { name: DataRetentionDuration.Week, message: 'Week', value: 24 * 7 },
            { name: DataRetentionDuration.Month, message: 'Month', value: 24 * 31 },
            { name: DataRetentionDuration.Year, message: 'Year', value: 24 * 366 },
            { name: DataRetentionDuration.Forever, message: 'Forever', value: null }
        ]
    };

    const answer: { retention: DataRetentionDuration } = await prompt(question);
    const choice = question.choices.find((choice) => choice.name === answer.retention);
    return choice ? choice.value : null;
}

async function selectEnvironment(): Promise<Environment> {
    const question = {
        type: 'select',
        name: 'environment',
        message: 'Select the environment:',
        choices: [
            { name: Environment.Testnet, message: 'Testnet' },
            { name: Environment.Mainnet, message: 'Mainnet' },
            { name: Environment.Local, message: 'Local' }
        ]
    };

    const answer: { environment: Environment } = await prompt(question);
    return answer.environment;
}

async function removeConfigKey(env: string, key: string) {
    const filePath = path.join(path.join(process.env.VIA_HOME as string, `etc/env/configs/${env}.toml`));
    const contents = await fs.promises.readFile(filePath, { encoding: 'utf-8' });

    const modifiedContents = contents
        .split('\n')
        .filter((line) => !line.startsWith(`${key} =`) && !line.startsWith(`${key}=`))
        .join('\n');
    await fs.promises.writeFile(filePath, modifiedContents);
}

async function changeConfigKey(env: string, key: string, newValue: string | number | boolean, section: string) {
    const filePath = path.join(path.join(process.env.VIA_HOME as string, `etc/env/configs/${env}.toml`));
    let contents = await fs.promises.readFile(filePath, { encoding: 'utf-8' });

    const keyExists =
        contents.split('\n').find((line) => line.startsWith(`${key} =`) || line.startsWith(`${key}=`)) !== undefined;

    if (!keyExists) {
        contents = contents.replace(`\n[${section}]\n`, `\n[${section}]\n${key} =\n`);
    }

    const modifiedContents = contents
        .split('\n')
        .map((line) => (line.startsWith(`${key} =`) ? `${key} = ${JSON.stringify(newValue)}` : line))
        .map((line) => (line.startsWith(`${key}=`) ? `${key}=${JSON.stringify(newValue)}` : line))
        .join('\n');
    await fs.promises.writeFile(filePath, modifiedContents);
}

async function clearIfNeeded() {
    const filePath = path.join(path.join(process.env.VIA_HOME as string, `etc/env/target/via_ext_node.env`));
    if (!fs.existsSync(filePath)) {
        return true;
    }

    const question = {
        type: 'confirm',
        name: 'cleanup',
        message: 'Do you want to clear the external node database?'
    };

    const answer: { cleanup: boolean } = await prompt(question);
    if (!answer.cleanup) {
        return false;
    }
    const cmd = chalk.yellow;
    console.log(`cleaning up database (${cmd('via clean --config via_ext_node --database')})`);
    await utils.exec('via clean --config via_ext_node --database');
    console.log(`cleaning up db (${cmd('via db drop --core')})`);
    await utils.exec('via db drop --core');
    return true;
}

async function runEnIfAskedTo() {
    const question = {
        type: 'confirm',
        name: 'runRequested',
        message: 'Do you want to run external-node now?'
    };
    const answer: { runRequested: boolean } = await prompt(question);
    if (!answer.runRequested) {
        return false;
    }
    await utils.spawn('via external-node');
}

async function commentOutConfigKey(env: string, key: string) {
    const filePath = path.join(path.join(process.env.VIA_HOME as string, `etc/env/configs/${env}.toml`));
    const contents = await fs.promises.readFile(filePath, { encoding: 'utf-8' });
    const modifiedContents = contents
        .split('\n')
        .map((line) => (line.startsWith(`${key} =`) || line.startsWith(`${key}=`) ? `#${line}` : line))
        .join('\n');
    await fs.promises.writeFile(filePath, modifiedContents);
}

async function configExternalNode() {
    const cmd = chalk.yellow;

    console.log(`Changing active env to via_ext_node (${cmd('via env via_ext_node')})`);
    setEnv('via_ext_node');

    await clearIfNeeded();
    const env = await selectEnvironment();

    const retention = await selectDataRetentionDurationHours();
    await commentOutConfigKey('via_ext_node', 'template_database_url');
    await changeConfigKey('via_ext_node', 'mode', 'GCSAnonymousReadOnly', 'en.snapshots.object_store');
    if (retention !== null) {
        await changeConfigKey('via_ext_node', 'pruning_data_retention_hours', retention, 'en');
    } else {
        await removeConfigKey('via_ext_node', 'pruning_data_retention_hours');
    }

    let network = 'regtest';
    switch (env) {
        case Environment.Mainnet:
            await changeConfigKey('via_ext_node', 'l1_chain_id', 1, 'en');
            await changeConfigKey('via_ext_node', 'l2_chain_id', 324, 'en');
            await changeConfigKey('via_ext_node', 'main_node_url', 'https://mainnet.era.zksync.io', 'en');
            await changeConfigKey('via_ext_node', 'eth_client_url', 'https://ethereum-rpc.publicnode.com', 'en');
            await changeConfigKey(
                'via_ext_node',
                'bucket_base_url',
                'zksync-era-mainnet-external-node-snapshots',
                'en.snapshots.object_store'
            );
            network = 'bitcoin';
            break;
        case Environment.Testnet:
            await changeConfigKey('via_ext_node', 'l1_chain_id', 11155111, 'en');
            await changeConfigKey('via_ext_node', 'l2_chain_id', 300, 'en');
            await changeConfigKey('via_ext_node', 'main_node_url', 'https://sepolia.era.zksync.dev', 'en');
            await changeConfigKey(
                'via_ext_node',
                'eth_client_url',
                'https://ethereum-sepolia-rpc.publicnode.com',
                'en'
            );
            await changeConfigKey(
                'via_ext_node',
                'bucket_base_url',
                'zksync-era-boojnet-external-node-snapshots',
                'en.snapshots.object_store'
            );
            network = 'testnet';
            break;
        case Environment.Local:
            await changeConfigKey('via_ext_node', 'l2_chain_id', 25223, 'en');
            await changeConfigKey('via_ext_node', 'mode', 'FileBacked', 'en.snapshots.object_store_mode');
            break;
    }
    compileConfig('via_ext_node');
    await updateBootstrapTxidsEnv(network);
    load_from_file();
    console.log(`Setting up postgres (${cmd('via db setup')})`);
    await setupDb({ prover: false, core: true, verifier: false, indexer: false });
    await runEnIfAskedTo();
}

export const command = new Command('setup-external-node')
    .description('prepare local setup for running external-node on mainnet/testnet')
    .action(async (_: Command) => {
        await configExternalNode();
    });
